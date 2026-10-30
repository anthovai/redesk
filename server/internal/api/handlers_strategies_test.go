package api

import (
	"context"
	"crypto/ed25519"
	"crypto/rand"
	"encoding/base64"
	"encoding/json"
	"fmt"
	"math/big"
	"strings"
	"testing"
	"time"

	"github.com/coder/websocket"

	"aegiron/server/internal/auth"
	"aegiron/server/internal/store"
)

// fakeAgent is a device on the signaling WebSocket, as the Rust agent speaks it.
type fakeAgent struct {
	t    *testing.T
	conn *websocket.Conn
	key  ed25519.PrivateKey
	id   string
}

func randomDeviceID() string {
	n, _ := rand.Int(rand.Reader, big.NewInt(900_000_000))
	return fmt.Sprint(100_000_000 + n.Int64())
}

// register connects and registers id with key; it returns the server's reply
// ("registered" or "error"). The connection stays open after "registered".
func (e *testEnv) register(id string, key ed25519.PrivateKey) (*fakeAgent, map[string]any) {
	e.t.Helper()
	ctx, cancel := context.WithTimeout(context.Background(), 10*time.Second)
	defer cancel()
	c, _, err := websocket.Dial(ctx, "ws"+strings.TrimPrefix(e.srv.URL, "http")+"/ws/agent", nil)
	if err != nil {
		e.t.Fatal(err)
	}
	a := &fakeAgent{t: e.t, conn: c, key: key, id: id}
	e.t.Cleanup(func() { c.CloseNow() })
	ch := a.read(ctx)
	nonce := "client-nonce-" + auth.RandomToken(8)
	sig := ed25519.Sign(key, []byte("redesk-register-v1\n"+id+"\n"+ch["nonce"].(string)+"\n"+nonce))
	a.write(ctx, map[string]any{"t": "register", "id": id, "pk": base64.StdEncoding.EncodeToString(key.Public().(ed25519.PublicKey)),
		"nonce": nonce, "sig": base64.StdEncoding.EncodeToString(sig), "hostname": "test-host", "os": "testos", "version": "0.6.0"})
	return a, a.read(ctx)
}

func newKey() ed25519.PrivateKey {
	_, k, _ := ed25519.GenerateKey(rand.Reader)
	return k
}

func (a *fakeAgent) write(ctx context.Context, v any) {
	b, _ := json.Marshal(v)
	if err := a.conn.Write(ctx, websocket.MessageText, b); err != nil {
		a.t.Fatal(err)
	}
}

func (a *fakeAgent) read(ctx context.Context) map[string]any {
	a.t.Helper()
	_, b, err := a.conn.Read(ctx)
	if err != nil {
		a.t.Fatalf("read: %v", err)
	}
	m := map[string]any{}
	json.Unmarshal(b, &m)
	return m
}

// next waits for the next message of type typ (skipping pongs), up to 5 s.
func (a *fakeAgent) next(typ string) map[string]any {
	a.t.Helper()
	ctx, cancel := context.WithTimeout(context.Background(), 5*time.Second)
	defer cancel()
	for {
		m := a.read(ctx)
		if m["t"] == typ {
			return m
		}
	}
}

func (e *testEnv) admin() string {
	return e.bearer(e.createUser(uniqueEmail("admin", "strategy.test"), "admin", false))
}

func TestStrategies(t *testing.T) {
	e := newTestEnv(t)
	adm := e.admin()
	viewer := e.createUser(uniqueEmail("user", "strategy.test"), "user", false)
	usr := e.bearer(viewer)

	if st, _ := e.post(nil, "GET", "/api/strategies", usr, nil); st != 403 {
		t.Fatalf("non-admin list: %d", st)
	}
	if st, r := e.post(nil, "POST", "/api/strategies", adm, map[string]any{"name": "bad", "settings": map[string]any{"approve_mode": "anyone"}}); st != 400 {
		t.Fatalf("bad approve_mode: %d %v", st, r)
	}
	st, r := e.post(nil, "POST", "/api/groups", adm, map[string]any{"name": "Kiosks " + auth.RandomToken(4)})
	if st != 200 {
		t.Fatalf("group: %d %v", st, r)
	}
	gid := int64(r["id"].(float64))
	name := "Locked down " + auth.RandomToken(4)
	st, r = e.post(nil, "POST", "/api/strategies", adm, map[string]any{
		"name": name, "group_ids": []int64{gid},
		"settings": map[string]any{"approve_mode": "click", "allow_file_transfer": false, "allow_permanent_password": false, "direct_enabled": false, "record_sessions": true},
	})
	if st != 200 {
		t.Fatalf("create: %d %v", st, r)
	}
	sid := int64(r["id"].(float64))
	if e.auditCount("strategy.created", fmt.Sprint(sid)) != 1 {
		t.Fatal("not audited")
	}

	// A device without a strategy registers with policy null (behaves as before).
	id := randomDeviceID()
	key := newKey()
	ag, reg := e.register(id, key)
	if reg["t"] != "registered" || reg["policy"] != nil {
		t.Fatalf("no strategy: %v", reg)
	}
	if _, ok := reg["policy"]; !ok {
		t.Fatal("registered lacks the policy field")
	}

	// Moving it into the group pushes the group's strategy.
	if st, r := e.post(nil, "PATCH", "/api/devices/"+id, adm, map[string]any{"alias": "", "note": "front desk", "disabled": false, "group_id": gid}); st != 200 {
		t.Fatalf("patch: %d %v", st, r)
	}
	p := ag.next("policy")["policy"].(map[string]any)
	s := p["settings"].(map[string]any)
	if p["name"] != name || s["approve_mode"] != "click" || s["allow_file_transfer"] != false || s["direct_enabled"] != false {
		t.Fatalf("pushed policy: %v", p)
	}
	if _, managed := s["allow_input"]; managed {
		t.Fatal("unset setting sent as managed")
	}

	// Editing the strategy pushes the change.
	if st, r := e.post(nil, "PUT", fmt.Sprintf("/api/strategies/%d", sid), adm, map[string]any{
		"name": name, "group_ids": []int64{gid}, "settings": map[string]any{"approve_mode": "both", "allow_input": false},
	}); st != 200 {
		t.Fatalf("update: %d %v", st, r)
	}
	s = ag.next("policy")["policy"].(map[string]any)["settings"].(map[string]any)
	if s["approve_mode"] != "both" || s["allow_input"] != false || s["allow_file_transfer"] != nil {
		t.Fatalf("updated policy: %v", s)
	}

	// The device's own strategy wins over its group's.
	st, r = e.post(nil, "POST", "/api/strategies", adm, map[string]any{"name": "Own " + auth.RandomToken(4), "settings": map[string]any{"allow_audio": false}})
	if st != 200 {
		t.Fatalf("create 2: %d", st)
	}
	own := int64(r["id"].(float64))
	e.post(nil, "PATCH", "/api/devices/"+id, adm, map[string]any{"alias": "", "note": "", "disabled": false, "group_id": gid, "strategy_id": own})
	if p := ag.next("policy")["policy"].(map[string]any); int64(p["id"].(float64)) != own {
		t.Fatalf("device strategy: %v", p)
	}
	// PATCH without strategy_id keeps it.
	e.post(nil, "PATCH", "/api/devices/"+id, adm, map[string]any{"alias": "x", "note": "", "disabled": false, "group_id": gid})
	if d, _ := e.st.GetDevice(context.Background(), id); d.StrategyID == nil || *d.StrategyID != own || d.Alias != "x" {
		t.Fatalf("strategy_id dropped: %+v", d)
	}
	// Deleting it falls back to the group's.
	e.post(nil, "DELETE", fmt.Sprintf("/api/strategies/%d", own), adm, nil)
	if p := ag.next("policy")["policy"].(map[string]any); int64(p["id"].(float64)) != sid {
		t.Fatalf("fallback: %v", p)
	}

	// Re-registering gets the current strategy in "registered".
	ag.conn.CloseNow()
	time.Sleep(100 * time.Millisecond)
	ag, reg = e.register(id, key)
	if p, _ := reg["policy"].(map[string]any); p == nil || int64(p["id"].(float64)) != sid {
		t.Fatalf("registered policy: %v", reg)
	}

	// record_sessions forces recording although the server-wide mode is off.
	if tok := e.api.recordingToken(context.Background(), "sid-"+auth.RandomToken(8), id, viewer.ID, "127.0.0.1"); tok != "" {
		t.Fatal("recording after record_sessions was removed")
	}
	e.post(nil, "PUT", fmt.Sprintf("/api/strategies/%d", sid), adm, map[string]any{"name": name, "group_ids": []int64{gid}, "settings": map[string]any{"record_sessions": true}})
	ag.next("policy")
	if tok := e.api.recordingToken(context.Background(), "sid-"+auth.RandomToken(8), id, viewer.ID, "127.0.0.1"); tok == "" {
		t.Fatal("record_sessions did not record")
	}

	// Removing the group's strategy sends null.
	e.post(nil, "PUT", fmt.Sprintf("/api/strategies/%d", sid), adm, map[string]any{"name": name, "group_ids": []int64{}, "settings": map[string]any{}})
	if m := ag.next("policy"); m["policy"] != nil {
		t.Fatalf("want null policy, got %v", m)
	}
	var list struct {
		Strategies []store.Strategy `json:"strategies"`
	}
	st, raw := e.post(nil, "GET", "/api/strategies", adm, nil)
	b, _ := json.Marshal(raw)
	json.Unmarshal(b, &list)
	if st != 200 || len(list.Strategies) == 0 {
		t.Fatalf("list: %d %v", st, raw)
	}
}

func TestChangeDeviceID(t *testing.T) {
	e := newTestEnv(t)
	adm := e.admin()
	old, key := randomDeviceID(), newKey()
	ag, reg := e.register(old, key)
	if reg["t"] != "registered" {
		t.Fatalf("register: %v", reg)
	}
	u := e.createUser(uniqueEmail("book", "id.test"), "user", false)
	e.st.UpsertAddressEntry(context.Background(), u.ID, old, "Reception", nil, false)

	if st, _ := e.post(nil, "POST", "/api/devices/"+old+"/change-id", adm, map[string]any{"id": "12-34"}); st != 400 {
		t.Fatalf("invalid id accepted: %d", st)
	}
	other := randomDeviceID()
	if _, r := e.register(other, newKey()); r["t"] != "registered" {
		t.Fatal(r)
	}
	if st, r := e.post(nil, "POST", "/api/devices/"+old+"/change-id", adm, map[string]any{"id": other}); st != 409 || r["code"] != "id_taken" {
		t.Fatalf("taken id: %d %v", st, r)
	}

	newID := fmt.Sprintf("front-desk-%d", time.Now().UnixNano()%1e9)
	st, r := e.post(nil, "POST", "/api/devices/"+old+"/change-id", adm, map[string]any{"id": strings.ToUpper(newID)})
	if st != 200 || r["id"] != newID {
		t.Fatalf("change: %d %v", st, r)
	}
	// The online agent is told.
	if m := ag.next("id_changed"); m["id"] != newID {
		t.Fatalf("id_changed: %v", m)
	}
	if e.auditCount("device.id_changed", newID) != 1 {
		t.Fatal("not audited")
	}
	// History follows the device.
	entries, _ := e.st.ListAddressBook(context.Background(), u.ID)
	if len(entries) != 1 || entries[0].DeviceID != newID {
		t.Fatalf("address book: %+v", entries)
	}

	// An agent that was offline registers with the old id: it is told the new one.
	if _, r := e.register(old, key); r["t"] != "error" || r["code"] != "id_changed" || r["id"] != newID {
		t.Fatalf("old id, same key: %v", r)
	}
	// Someone else cannot take the old id meanwhile, nor learn the new one.
	if _, r := e.register(old, newKey()); r["code"] != "id_taken" || r["id"] != nil {
		t.Fatalf("old id, other key: %v", r)
	}
	// The agent registers under the new id; the old id is then free.
	if _, r := e.register(newID, key); r["t"] != "registered" {
		t.Fatalf("new id: %v", r)
	}
	if _, err := e.st.RenamedDevice(context.Background(), old); err == nil {
		t.Fatal("old id still reserved after the agent confirmed")
	}
	if _, r := e.register(old, newKey()); r["t"] != "registered" {
		t.Fatalf("old id reuse: %v", r)
	}
}
