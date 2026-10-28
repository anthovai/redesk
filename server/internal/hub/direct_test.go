package hub

import (
	"context"
	"encoding/json"
	"log/slog"
	"net"
	"net/netip"
	"strings"
	"testing"
	"time"
)

func TestPunchMACVector(t *testing.T) {
	// Same vector as agent/src/direct.rs (tests::helpers).
	if got := PunchMAC("key", "sid", "agent", 1700000000); got != "G+czfC5NMvtWfd2kon2rnIRV/1GMh4HL7VhEBL4GcEA=" {
		t.Fatalf("PunchMAC = %s", got)
	}
}

func TestCleanCandidates(t *testing.T) {
	got := cleanCandidates([]string{
		"192.168.1.5:21118", "192.168.1.5:21118", "0.0.0.0:1", "224.0.0.1:5", "169.254.1.1:5",
		"10.0.0.2:0", "garbage", "[fe80::1]:5", "10.0.0.7:21118",
	}, "203.0.113.9")
	want := []string{"192.168.1.5:21118", "10.0.0.7:21118", "203.0.113.9:21118"}
	if strings.Join(got, ",") != strings.Join(want, ",") {
		t.Fatalf("got %v want %v", got, want)
	}
	many := []string{}
	for i := 1; i <= 20; i++ {
		many = append(many, netip.AddrPortFrom(netip.AddrFrom4([4]byte{10, 0, 0, byte(i)}), 1).String())
	}
	if n := len(cleanCandidates(many, "")); n != maxCandidates {
		t.Fatalf("kept %d candidates", n)
	}
}

func udpHub(t *testing.T) (*Hub, int) {
	t.Helper()
	h := New(nil, nil, slog.New(slog.DiscardHandler), nil)
	// Find two consecutive free UDP ports.
	for p := 41000; p < 41100; p += 2 {
		ctx, cancel := context.WithCancel(context.Background())
		if err := h.ServeUDP(ctx, p); err == nil {
			t.Cleanup(cancel)
			return h, p
		}
		cancel()
	}
	t.Skip("no free UDP ports")
	return nil, 0
}

func udpAsk(t *testing.T, c *net.UDPConn, to int, msg map[string]any) map[string]string {
	t.Helper()
	msg["pad"] = strings.Repeat("x", 120)
	b, _ := json.Marshal(msg)
	if _, err := c.WriteToUDP(b, &net.UDPAddr{IP: net.IPv4(127, 0, 0, 1), Port: to}); err != nil {
		t.Fatal(err)
	}
	c.SetReadDeadline(time.Now().Add(time.Second))
	buf := make([]byte, 1500)
	n, _, err := c.ReadFromUDP(buf)
	if err != nil {
		return nil
	}
	var r map[string]string
	json.Unmarshal(buf[:n], &r)
	return r
}

func TestNATProbeAndRendezvous(t *testing.T) {
	h, port := udpHub(t)
	c, err := net.ListenUDP("udp4", &net.UDPAddr{IP: net.IPv4(127, 0, 0, 1)})
	if err != nil {
		t.Fatal(err)
	}
	defer c.Close()
	me := c.LocalAddr().String()

	// NAT probe answers on both ports with the observed address.
	for _, p := range []int{port, port + 1} {
		if r := udpAsk(t, c, p, map[string]any{"t": "nat"}); r["addr"] != me {
			t.Fatalf("nat probe on %d: %v (want %s)", p, r, me)
		}
	}
	// Short requests are ignored (no amplification).
	c.WriteToUDP([]byte(`{"t":"nat"}`), &net.UDPAddr{IP: net.IPv4(127, 0, 0, 1), Port: port})
	c.SetReadDeadline(time.Now().Add(200 * time.Millisecond))
	if _, _, err := c.ReadFromUDP(make([]byte, 100)); err == nil {
		t.Fatal("answered an unpadded request")
	}

	rl := &relay{sid: "S1", deviceID: "123456789", direct: newDirectState()}
	h.directs["S1"] = rl
	ts := time.Now().Unix()
	punch := func(conn *net.UDPConn, role, key string) map[string]string {
		return udpAsk(t, conn, port, map[string]any{"t": "punch", "sid": "S1", "role": role, "ts": ts, "mac": PunchMAC(key, "S1", role, ts)})
	}
	// Wrong key: no answer, nothing recorded.
	if r := punch(c, "agent", "wrong"); r != nil {
		t.Fatalf("accepted a bad MAC: %v", r)
	}
	r := punch(c, "agent", rl.direct.agentPunch)
	if r["addr"] != me || r["peer"] != "" {
		t.Fatalf("agent rendezvous: %v", r)
	}
	v, _ := net.ListenUDP("udp4", &net.UDPAddr{IP: net.IPv4(127, 0, 0, 1)})
	defer v.Close()
	r = punch(v, "viewer", rl.direct.viewerPunch)
	if r["peer"] != me {
		t.Fatalf("viewer did not learn the agent address: %v", r)
	}
	if r = punch(c, "agent", rl.direct.agentPunch); r["peer"] != v.LocalAddr().String() {
		t.Fatalf("agent did not learn the viewer address: %v", r)
	}
	// Once the session is direct, the rendezvous closes.
	rl.direct.startOnce.Do(func() { close(rl.direct.started) })
	if r = punch(v, "viewer", rl.direct.viewerPunch); r != nil {
		t.Fatalf("rendezvous still open: %v", r)
	}
}
