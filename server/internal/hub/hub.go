// Package hub implements agent signaling (rendezvous) and the session relay.
//
// The server never sees remote-desktop content: viewer and agent run an
// end-to-end key exchange over the relay, which only forwards opaque frames.
package hub

import (
	"context"
	"crypto/ed25519"
	"crypto/rand"
	"encoding/base64"
	"encoding/json"
	"errors"
	"log/slog"
	"net/http"
	"regexp"
	"sync"
	"time"

	"github.com/coder/websocket"

	"aegiron/server/internal/auth"
	"aegiron/server/internal/store"
)

var (
	ErrOffline = errors.New("device is offline")
	ErrBusy    = errors.New("device signaling channel is busy")
	idPattern  = regexp.MustCompile(`^[0-9]{9}$`)
)

const (
	agentIdleTimeout = 75 * time.Second // agents ping every 20s
	pairTimeout      = 60 * time.Second
	writeTimeout     = 15 * time.Second
)

type Hub struct {
	store   *store.Store
	key     ed25519.PrivateKey
	log     *slog.Logger
	origins []string

	// ClientIP resolves the caller address (honours trusted proxy headers).
	ClientIP func(*http.Request) string
	// ViewerUserID returns the logged-in console user for a request, or 0.
	ViewerUserID func(*http.Request) int64
	// RecordingToken decides whether a new session must be recorded and, if so, returns the
	// device's one-time upload token ("" = not recorded).
	RecordingToken func(ctx context.Context, sid, deviceID string, userID int64, viewerIP string) string

	// P2P allows direct/P2P sessions (AEGIRON_P2P); UDPPort is the rendezvous/NAT-probe
	// port (0 = no UDP, so no hole punching; LAN/TCP direct still works).
	P2P     bool
	UDPPort int

	mu      sync.Mutex
	agents  map[string]*agentConn
	relays  map[string]*relay
	directs map[string]*relay // sessions offered a direct path, by sid
}

func New(s *store.Store, key ed25519.PrivateKey, log *slog.Logger, origins []string) *Hub {
	return &Hub{
		store: s, key: key, log: log, origins: origins,
		ClientIP: func(r *http.Request) string { return r.RemoteAddr },
		agents:   map[string]*agentConn{},
		relays:   map[string]*relay{},
		directs:  map[string]*relay{},
	}
}

type agentConn struct {
	id     string
	ip     string
	conn   *websocket.Conn
	out    chan []byte
	direct bool // advertised FeatureDirect
	// Every advertised feature, e.g. FeatureTunnel.
	features map[string]bool
}

func (a *agentConn) send(v any) bool {
	b, _ := json.Marshal(v)
	select {
	case a.out <- b:
		return true
	default:
		return false
	}
}

// Online reports which of the given device ids currently hold a signaling connection.
func (h *Hub) Online(id string) bool {
	h.mu.Lock()
	defer h.mu.Unlock()
	return h.agents[id] != nil
}

func (h *Hub) OnlineSet() map[string]bool {
	h.mu.Lock()
	defer h.mu.Unlock()
	m := make(map[string]bool, len(h.agents))
	for id := range h.agents {
		m[id] = true
	}
	return m
}

// Disconnect drops an agent's signaling connection (e.g. after disable/delete).
func (h *Hub) Disconnect(id, reason string) {
	h.mu.Lock()
	a := h.agents[id]
	h.mu.Unlock()
	if a != nil {
		a.conn.Close(websocket.StatusPolicyViolation, reason)
	}
}

// --- agent signaling ---

type registerMsg struct {
	T           string `json:"t"`
	ID          string `json:"id"`
	PublicKey   string `json:"pk"`
	Nonce       string `json:"nonce"`
	Sig         string `json:"sig"`
	Hostname    string `json:"hostname"`
	OS          string `json:"os"`
	Version     string `json:"version"`
	EnrollToken string `json:"enroll_token,omitempty"`
	// LAN facts for Wake-on-LAN (optional; older agents omit them).
	Macs []string        `json:"macs,omitempty"`
	LAN  []store.LanAddr `json:"lan,omitempty"`
	// Optional capabilities, e.g. "direct1" (older agents omit them).
	Features []string `json:"features,omitempty"`
	NAT      string   `json:"nat,omitempty"`
}

type agentMsg struct {
	T      string         `json:"t"`
	SID    string         `json:"sid,omitempty"`
	Reason string         `json:"reason,omitempty"`
	Kind   string         `json:"kind,omitempty"`
	Detail map[string]any `json:"detail,omitempty"`
	// direct_offer / direct_started / direct_ended
	TCP           []string `json:"tcp,omitempty"`
	Cert          string   `json:"cert,omitempty"`
	NAT           string   `json:"nat,omitempty"`
	Disabled      bool     `json:"disabled,omitempty"`
	Path          string   `json:"path,omitempty"`
	BytesToViewer int64    `json:"bytes_to_viewer,omitempty"`
	BytesToAgent  int64    `json:"bytes_to_agent,omitempty"`
}

// agentEvents are the session events an agent may report into the audit log.
var agentEvents = map[string]bool{
	"auth_failed": true, "auth_ok": true, "denied": true, "locked": true,
	"file_upload": true, "file_download": true, "wol_sent": true,
	"recording_started": true, "recording_failed": true,
	"file_delete": true, "file_rename": true, "resolution_change": true,
	"tunnel_open": true, "tunnel_closed": true, "terminal_open": true, "terminal_closed": true,
}

func (h *Hub) HandleAgent(w http.ResponseWriter, r *http.Request) {
	c, err := websocket.Accept(w, r, &websocket.AcceptOptions{OriginPatterns: h.origins})
	if err != nil {
		return
	}
	c.SetReadLimit(64 << 10)
	ctx := r.Context()
	ip := h.ClientIP(r)

	a, err := h.registerAgent(ctx, c, ip)
	if err != nil {
		h.log.Info("agent registration failed", "ip", ip, "err", err)
		c.Close(websocket.StatusPolicyViolation, "registration failed")
		return
	}

	h.mu.Lock()
	if old := h.agents[a.id]; old != nil {
		old.conn.Close(websocket.StatusPolicyViolation, "replaced by a newer connection")
	}
	h.agents[a.id] = a
	h.mu.Unlock()
	h.log.Info("agent online", "id", a.id, "ip", ip)

	defer func() {
		h.mu.Lock()
		current := h.agents[a.id] == a
		if current {
			delete(h.agents, a.id)
		}
		h.mu.Unlock()
		if current {
			h.endDirectSessionsOf(a.id)
		}
		h.log.Info("agent offline", "id", a.id)
	}()

	writerCtx, stopWriter := context.WithCancel(ctx)
	defer stopWriter()
	go func() {
		for {
			select {
			case <-writerCtx.Done():
				return
			case b := <-a.out:
				wctx, cancel := context.WithTimeout(writerCtx, writeTimeout)
				err := c.Write(wctx, websocket.MessageText, b)
				cancel()
				if err != nil {
					c.CloseNow()
					return
				}
			}
		}
	}()

	lastTouch := time.Now()
	for {
		rctx, cancel := context.WithTimeout(ctx, agentIdleTimeout)
		_, data, err := c.Read(rctx)
		cancel()
		if err != nil {
			return
		}
		var m agentMsg
		if json.Unmarshal(data, &m) != nil {
			continue
		}
		switch m.T {
		case "ping":
			a.send(map[string]string{"t": "pong"})
			if time.Since(lastTouch) > time.Minute {
				lastTouch = time.Now()
				_ = h.store.TouchDevice(ctx, a.id)
			}
		case "session_reject":
			h.rejectRelay(m.SID, a.id, m.Reason)
		case "event":
			h.agentEvent(ctx, a, m)
		case "direct_offer":
			h.handleDirectOffer(a, m)
		case "direct_started":
			h.directStarted(ctx, a, m)
		case "direct_ended":
			h.handleDirectEnded(a, m)
		}
	}
}

func (h *Hub) registerAgent(ctx context.Context, c *websocket.Conn, ip string) (*agentConn, error) {
	serverNonce := make([]byte, 32)
	rand.Read(serverNonce)
	sn := base64.StdEncoding.EncodeToString(serverNonce)

	wctx, cancel := context.WithTimeout(ctx, writeTimeout)
	defer cancel()
	if err := writeJSON(wctx, c, map[string]string{"t": "challenge", "nonce": sn}); err != nil {
		return nil, err
	}
	rctx, cancel2 := context.WithTimeout(ctx, 15*time.Second)
	defer cancel2()
	_, data, err := c.Read(rctx)
	if err != nil {
		return nil, err
	}
	var m registerMsg
	if err := json.Unmarshal(data, &m); err != nil || m.T != "register" {
		return nil, errors.New("expected register message")
	}

	fail := func(code, msg string) (*agentConn, error) {
		_ = writeJSON(wctx, c, map[string]string{"t": "error", "code": code, "message": msg})
		return nil, errors.New(code + ": " + msg)
	}

	pk, err := base64.StdEncoding.DecodeString(m.PublicKey)
	if err != nil || len(pk) != ed25519.PublicKeySize {
		return fail("bad_request", "invalid public key")
	}
	sig, _ := base64.StdEncoding.DecodeString(m.Sig)
	if !idPattern.MatchString(m.ID) || len(m.Nonce) > 64 {
		return fail("bad_request", "invalid id")
	}
	if !ed25519.Verify(pk, []byte("redesk-register-v1\n"+m.ID+"\n"+sn+"\n"+m.Nonce), sig) {
		return fail("bad_signature", "registration signature invalid")
	}

	st, err := h.store.GetSettings(ctx)
	if err != nil {
		return fail("internal", "settings unavailable")
	}
	existing, err := h.store.GetDevice(ctx, m.ID)
	if err != nil && !errors.Is(err, store.ErrNotFound) {
		return fail("internal", "database error")
	}
	var grant store.EnrollGrant
	if (st.RequireEnroll || m.EnrollToken != "") && (existing == nil || existing.PublicKey == nil) {
		ok, g, err := h.store.ConsumeEnrollToken(ctx, auth.HashToken(m.EnrollToken))
		grant = g
		if (err != nil || !ok) && st.RequireEnroll {
			_ = h.store.AddAudit(ctx, nil, ip, "device.enroll_denied", m.ID, nil)
			return fail("enroll_required", "a valid enrollment token is required")
		}
	}

	dev, created, err := h.store.RegisterDevice(ctx, m.ID, pk, trunc(m.Hostname, 128), trunc(m.OS, 64), trunc(m.Version, 32), ip, CleanMacs(m.Macs), cleanLAN(m.LAN), grant)
	switch {
	case errors.Is(err, store.ErrKeyMismatch):
		_ = h.store.AddAudit(ctx, nil, ip, "device.key_mismatch", m.ID, nil)
		return fail("id_taken", "this id is bound to another key")
	case errors.Is(err, store.ErrDeviceDisabled):
		return fail("disabled", "device disabled by administrator")
	case err != nil:
		return fail("internal", "database error")
	}
	if created {
		_ = h.store.AddAudit(ctx, grant.OwnerID, ip, "device.registered", dev.ID, map[string]any{"hostname": dev.Hostname, "os": dev.OS})
	}
	if grant.OwnerID != nil {
		// Enrolled by signing in from the app: show it in that person's address book.
		_ = h.store.AddAddressEntryIfMissing(ctx, *grant.OwnerID, dev.ID, dev.Hostname)
	}

	// Prove server identity: agents pin this key and abort on mismatch.
	serverSig := ed25519.Sign(h.key, []byte("redesk-server-v1\n"+m.ID+"\n"+m.Nonce+"\n"+sn))
	reg := map[string]any{"t": "registered", "sig": base64.StdEncoding.EncodeToString(serverSig)}
	if h.P2P && h.UDPPort > 0 {
		reg["udp_port"] = h.UDPPort // for the agent's NAT-type probe
	}
	if err := writeJSON(wctx, c, reg); err != nil {
		return nil, err
	}
	direct := false
	features := map[string]bool{}
	for _, f := range m.Features {
		direct = direct || f == FeatureDirect
		if len(features) < 32 {
			features[trunc(f, 32)] = true
		}
	}
	return &agentConn{id: m.ID, ip: ip, conn: c, out: make(chan []byte, 16), direct: direct, features: features}, nil
}

// agentEvent records an agent-reported session event, attributed to the session's viewer.
func (h *Hub) agentEvent(ctx context.Context, a *agentConn, m agentMsg) {
	if !agentEvents[m.Kind] {
		return
	}
	detail := map[string]any{"sid": m.SID}
	for k, v := range m.Detail {
		if len(detail) < 8 {
			if s, ok := v.(string); ok {
				v = trunc(s, 256)
			}
			detail[k] = v
		}
	}
	var actor *int64
	ip := ""
	h.mu.Lock()
	rl := h.relays[m.SID]
	if rl == nil {
		rl = h.directs[m.SID]
	}
	if rl != nil && rl.deviceID == a.id {
		uid := rl.userID
		actor, ip = &uid, rl.viewerIP
		if _, ok := detail["target"]; !ok {
			rl.tagged(detail)
		}
	}
	h.mu.Unlock()
	_ = h.store.AddAudit(ctx, actor, ip, "agent."+m.Kind, a.id, detail)
	if m.Kind == "recording_started" {
		_ = h.store.MarkRecordingStarted(ctx, m.SID, a.id)
	}
}

func writeJSON(ctx context.Context, c *websocket.Conn, v any) error {
	b, err := json.Marshal(v)
	if err != nil {
		return err
	}
	return c.Write(ctx, websocket.MessageText, b)
}

func trunc(s string, n int) string {
	if len(s) > n {
		return s[:n]
	}
	return s
}
