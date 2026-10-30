package hub

// Direct and P2P sessions (see docs/PROTOCOL.md, "Direct connections").
//
// The server never carries the traffic of a direct session; it only
//   - hands both ends a per-session secret (the direct token) and per-role
//     rendezvous keys through the already authenticated channels,
//   - collects the agent's TCP candidates and QUIC certificate hash and returns
//     them to the viewer with its ticket,
//   - tells each peer the other's public UDP address (rendezvous for hole punching),
//   - answers NAT-type probes, and
//   - writes the audit records (session.start / session.end with "path").

import (
	"context"
	"crypto/hmac"
	"crypto/sha256"
	"encoding/base64"
	"encoding/hex"
	"encoding/json"
	"errors"
	"net"
	"net/netip"
	"strconv"
	"sync"
	"sync/atomic"
	"time"

	"github.com/coder/websocket"

	"aegiron/server/internal/auth"
)

// FeatureDirect is advertised by agents that accept direct/P2P sessions.
const FeatureDirect = "direct1"

const (
	// How long StartSession waits for the agent's candidates before answering the viewer.
	directOfferWait = 2500 * time.Millisecond
	maxCandidates   = 8
	// Keep a direct-offered session findable this long after creation if it never starts.
	directGrace = pairTimeout + 15*time.Second
	// A rendezvous timestamp must be this fresh.
	punchMaxSkew = 2 * time.Minute
)

// directState tracks the direct side of one session; guarded by the hub's mu where noted.
type directState struct {
	token       string // shared secret proving to the agent that a viewer holds this ticket
	agentPunch  string // rendezvous key of the agent
	viewerPunch string // rendezvous key of the viewer
	offer       chan directOffer

	started   chan struct{}
	startOnce sync.Once
	endOnce   sync.Once
	path      atomic.Value // "direct" | "p2p"
	startedAt time.Time

	// Public UDP addresses seen by the rendezvous (hub.mu).
	agentAddr, viewerAddr netip.AddrPort
}

func (d *directState) isStarted() bool {
	select {
	case <-d.started:
		return true
	default:
		return false
	}
}

func (d *directState) pathName() string {
	if p, ok := d.path.Load().(string); ok {
		return p
	}
	return ""
}

type directOffer struct {
	TCP      []string
	Cert     string
	NAT      string
	Disabled bool
}

// DirectTicket is what a viewer needs to try a direct or P2P path.
type DirectTicket struct {
	Token string      `json:"token"`
	TCP   []string    `json:"tcp"`
	QUIC  *QUICTicket `json:"quic,omitempty"`
	NAT   string      `json:"nat,omitempty"` // agent's NAT type, informational
}

type QUICTicket struct {
	Cert     string `json:"cert"`      // SHA-256 (hex) of the agent's per-session certificate
	PunchKey string `json:"punch_key"` // the viewer's rendezvous key
	UDPPort  int    `json:"udp_port"`  // rendezvous port on the server's host
}

func newDirectState() *directState {
	return &directState{
		token:       auth.RandomToken(24),
		agentPunch:  auth.RandomToken(24),
		viewerPunch: auth.RandomToken(24),
		offer:       make(chan directOffer, 1),
		started:     make(chan struct{}),
	}
}

// awaitOffer is called by StartSession: it waits briefly for the agent's candidates.
func (h *Hub) awaitOffer(ctx context.Context, a *agentConn, rl *relay) *DirectTicket {
	var off directOffer
	select {
	case off = <-rl.direct.offer:
	case <-time.After(directOfferWait):
		return nil
	case <-ctx.Done():
		return nil
	}
	if off.Disabled {
		return nil
	}
	t := &DirectTicket{Token: rl.direct.token, TCP: cleanCandidates(off.TCP, a.ip), NAT: trunc(off.NAT, 16)}
	if h.UDPPort > 0 && isCertHash(off.Cert) {
		t.QUIC = &QUICTicket{Cert: off.Cert, PunchKey: rl.direct.viewerPunch, UDPPort: h.UDPPort}
	}
	if len(t.TCP) == 0 && t.QUIC == nil {
		return nil
	}
	return t
}

func isCertHash(s string) bool {
	b, err := hex.DecodeString(s)
	return err == nil && len(b) == sha256.Size
}

// cleanCandidates keeps valid unicast ip:port pairs and adds the agent's public
// address (as this server sees it) with the listening port.
func cleanCandidates(in []string, publicIP string) []string {
	out := []string{}
	seen := map[netip.AddrPort]bool{}
	add := func(ap netip.AddrPort) {
		ip := ap.Addr().Unmap()
		ap = netip.AddrPortFrom(ip, ap.Port())
		if ap.Port() == 0 || !ip.IsValid() || ip.IsUnspecified() || ip.IsMulticast() || ip.IsLinkLocalUnicast() || seen[ap] || len(out) >= maxCandidates {
			return
		}
		seen[ap] = true
		out = append(out, ap.String())
	}
	var port uint16
	for _, s := range in {
		if ap, err := netip.ParseAddrPort(s); err == nil {
			add(ap)
			if port == 0 {
				port = ap.Port()
			}
		}
	}
	if ip, err := netip.ParseAddr(publicIP); err == nil && port != 0 {
		add(netip.AddrPortFrom(ip, port))
	}
	return out
}

// --- agent messages ---

func (h *Hub) directFor(sid, deviceID string) *relay {
	h.mu.Lock()
	defer h.mu.Unlock()
	if rl := h.directs[sid]; rl != nil && rl.deviceID == deviceID {
		return rl
	}
	return nil
}

func (h *Hub) handleDirectOffer(a *agentConn, m agentMsg) {
	rl := h.directFor(m.SID, a.id)
	if rl == nil {
		return
	}
	select {
	case rl.direct.offer <- directOffer{TCP: m.TCP, Cert: m.Cert, NAT: m.NAT, Disabled: m.Disabled}:
	default:
	}
}

// directStarted: the agent accepted the viewer's hello on a direct path.
func (h *Hub) directStarted(ctx context.Context, a *agentConn, m agentMsg) {
	rl := h.directFor(m.SID, a.id)
	if rl == nil || (m.Path != "direct" && m.Path != "p2p") {
		return
	}
	d := rl.direct
	first := false
	d.startOnce.Do(func() {
		d.path.Store(m.Path)
		d.startedAt = time.Now()
		close(d.started)
		first = true
	})
	if !first {
		return
	}
	// The agent no longer needs its relay slot; the viewer may not join it any more.
	rl.mu.Lock()
	ac := rl.agent
	rl.mu.Unlock()
	if ac != nil {
		ac.Close(websocket.StatusNormalClosure, "direct")
	}
	_ = h.store.MarkSessionStarted(ctx, rl.sid)
	uid := rl.userID
	_ = h.store.AddAudit(ctx, &uid, rl.viewerIP, "session.start", rl.deviceID, rl.tagged(map[string]any{"sid": rl.sid, "path": m.Path}))
	h.log.Info("session started", "sid", rl.sid, "device", rl.deviceID, "path", m.Path)
}

// directEnded writes the end of a direct session once (from the agent, or when it goes offline).
func (h *Hub) directEnded(rl *relay, reason string, toViewer, toAgent int64) {
	d := rl.direct
	if !d.isStarted() {
		return
	}
	d.endOnce.Do(func() {
		h.mu.Lock()
		delete(h.directs, rl.sid)
		h.mu.Unlock()
		if toViewer < 0 {
			toViewer = 0
		}
		if toAgent < 0 {
			toAgent = 0
		}
		ctx, cancel := context.WithTimeout(context.Background(), 5*time.Second)
		defer cancel()
		_ = h.store.MarkSessionEnded(ctx, rl.sid, toViewer, toAgent, reason)
		uid := rl.userID
		_ = h.store.AddAudit(ctx, &uid, rl.viewerIP, "session.end", rl.deviceID, rl.tagged(map[string]any{
			"sid": rl.sid, "path": d.pathName(), "reason": reason, "seconds": int(time.Since(d.startedAt).Seconds()),
			"bytes_to_viewer": toViewer, "bytes_to_agent": toAgent,
		}))
		h.log.Info("session ended", "sid", rl.sid, "device", rl.deviceID, "path", d.pathName(), "reason", reason)
	})
}

func (h *Hub) handleDirectEnded(a *agentConn, m agentMsg) {
	if rl := h.directFor(m.SID, a.id); rl != nil {
		reason := "agent_" + trunc(m.Reason, 48)
		if m.Reason == "" {
			reason = "closed"
		}
		h.directEnded(rl, reason, m.BytesToViewer, m.BytesToAgent)
	}
}

// endDirectSessionsOf closes the audit record of every running direct session of an
// agent whose signaling connection is gone (its end can no longer be reported).
func (h *Hub) endDirectSessionsOf(deviceID string) {
	h.mu.Lock()
	var list []*relay
	for _, rl := range h.directs {
		if rl.deviceID == deviceID && rl.direct.isStarted() {
			list = append(list, rl)
		}
	}
	h.mu.Unlock()
	for _, rl := range list {
		h.directEnded(rl, "agent_offline", 0, 0)
	}
}

// --- UDP: NAT-type probe and hole-punching rendezvous ---

// ServeUDP answers NAT probes on port and port+1 and runs the rendezvous on port.
// Comparing the mapped port seen on both tells cone (endpoint-independent) from
// symmetric NATs, like RustDesk's NAT test port.
func (h *Hub) ServeUDP(ctx context.Context, port int) error {
	var conns []*net.UDPConn
	for _, p := range []int{port, port + 1} {
		c, err := net.ListenUDP("udp", &net.UDPAddr{Port: p})
		if err != nil {
			for _, o := range conns {
				o.Close()
			}
			return err
		}
		conns = append(conns, c)
	}
	lim := newUDPLimiter()
	for i, c := range conns {
		go h.udpLoop(c, i == 0, lim)
	}
	go func() {
		<-ctx.Done()
		for _, c := range conns {
			c.Close()
		}
	}()
	return nil
}

type udpMsg struct {
	T    string `json:"t"`
	SID  string `json:"sid"`
	Role string `json:"role"`
	TS   int64  `json:"ts"`
	MAC  string `json:"mac"`
}

// Requests must be at least this long so that replies never amplify spoofed traffic.
const minUDPRequest = 96

func (h *Hub) udpLoop(c *net.UDPConn, rendezvous bool, lim *udpLimiter) {
	buf := make([]byte, 1500)
	for {
		n, from, err := c.ReadFromUDPAddrPort(buf)
		if err != nil {
			if errors.Is(err, net.ErrClosed) {
				return
			}
			continue
		}
		from = netip.AddrPortFrom(from.Addr().Unmap(), from.Port())
		if n < minUDPRequest || n > 512 || !lim.allow(from.Addr()) {
			continue
		}
		var m udpMsg
		if json.Unmarshal(buf[:n], &m) != nil {
			continue
		}
		var reply map[string]string
		switch {
		case m.T == "nat":
			reply = map[string]string{"t": "nat", "addr": from.String()}
		case m.T == "punch" && rendezvous:
			reply = h.punch(m, from)
		}
		if reply != nil {
			b, _ := json.Marshal(reply)
			if len(b) <= n {
				_, _ = c.WriteToUDPAddrPort(b, from)
			}
		}
	}
}

// PunchMAC is HMAC-SHA256(key, "aegiron-punch-v1\n" + sid + "\n" + role + "\n" + ts), base64.
func PunchMAC(key, sid, role string, ts int64) string {
	m := hmac.New(sha256.New, []byte(key))
	m.Write([]byte("aegiron-punch-v1\n" + sid + "\n" + role + "\n" + strconv.FormatInt(ts, 10)))
	return base64.StdEncoding.EncodeToString(m.Sum(nil))
}

// punch records the sender's public address for its role and returns the peer's, if known.
func (h *Hub) punch(m udpMsg, from netip.AddrPort) map[string]string {
	if d := time.Since(time.Unix(m.TS, 0)); d > punchMaxSkew || d < -punchMaxSkew {
		return nil
	}
	h.mu.Lock()
	defer h.mu.Unlock()
	rl := h.directs[m.SID]
	if rl == nil || rl.direct.isStarted() {
		return nil
	}
	d := rl.direct
	var key string
	var mine, peer *netip.AddrPort
	switch m.Role {
	case "agent":
		key, mine, peer = d.agentPunch, &d.agentAddr, &d.viewerAddr
	case "viewer":
		key, mine, peer = d.viewerPunch, &d.viewerAddr, &d.agentAddr
	default:
		return nil
	}
	if !hmac.Equal([]byte(m.MAC), []byte(PunchMAC(key, m.SID, m.Role, m.TS))) {
		return nil
	}
	if !mine.IsValid() { // first authenticated address wins
		*mine = from
	}
	r := map[string]string{"t": "punch", "addr": from.String()}
	if peer.IsValid() {
		r["peer"] = peer.String()
	}
	return r
}

// udpLimiter allows a few packets per second per source address.
type udpLimiter struct {
	mu    sync.Mutex
	win   time.Time
	count map[netip.Addr]int
}

func newUDPLimiter() *udpLimiter { return &udpLimiter{count: map[netip.Addr]int{}} }

func (l *udpLimiter) allow(ip netip.Addr) bool {
	l.mu.Lock()
	defer l.mu.Unlock()
	if now := time.Now(); now.Sub(l.win) > time.Second {
		l.win = now
		clear(l.count)
	}
	l.count[ip]++
	return l.count[ip] <= 20
}
