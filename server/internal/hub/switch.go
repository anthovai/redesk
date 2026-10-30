package hub

import (
	"context"
	"errors"
	"sync"
	"time"

	"aegiron/server/internal/auth"
)

// Switch sides (RustDesk's "switch sides"): the viewer of a running session lets the person at
// the controlled device control the viewer's own device in return. The viewer asks for a grant
// (POST /api/sessions/{sid}/switch), hands it to the controlled agent inside the end-to-end
// channel, and that agent redeems it once over its signaling connection for a session to the
// viewer's device. See docs/PROTOCOL.md §11.

const switchGrantTTL = 60 * time.Second

var (
	ErrNoSession   = errors.New("no such active session for this user")
	ErrSameDevice  = errors.New("cannot switch sides with the same device")
	ErrBadGrant    = errors.New("invalid or expired switch grant")
	errGrantFormat = errors.New("bad grant")
)

type switchGrant struct {
	from    string // device that may redeem it (the controlled device of the original session)
	to      string // device the new session goes to (the viewer's own device)
	userID  int64  // user who offered; the new session is audited under this user
	origSID string
	name    string         // shown on the target as the controlling side
	device  map[string]any // ticket.device for the redeeming viewer
	expires time.Time
}

type switchGrants struct {
	mu sync.Mutex
	m  map[string]*switchGrant // by SHA-256 of the grant token
}

func (g *switchGrants) put(tok string, sg *switchGrant) {
	g.mu.Lock()
	defer g.mu.Unlock()
	if g.m == nil {
		g.m = map[string]*switchGrant{}
	}
	now := time.Now()
	for k, v := range g.m {
		if now.After(v.expires) {
			delete(g.m, k)
		}
	}
	g.m[string(auth.HashToken(tok))] = sg
}

// take removes and returns the grant if it exists, is unexpired and belongs to device `from`.
// A grant presented by another device stays valid for its owner.
func (g *switchGrants) take(tok, from string) (*switchGrant, error) {
	if tok == "" || len(tok) > 128 {
		return nil, errGrantFormat
	}
	g.mu.Lock()
	defer g.mu.Unlock()
	k := string(auth.HashToken(tok))
	sg := g.m[k]
	if sg == nil || sg.from != from {
		return nil, ErrBadGrant
	}
	delete(g.m, k)
	if time.Now().After(sg.expires) {
		return nil, ErrBadGrant
	}
	return sg, nil
}

// SessionOf returns the controlled device of an active session `sid` whose viewer is userID.
func (h *Hub) SessionOf(sid string, userID int64) (string, error) {
	h.mu.Lock()
	rl := h.relays[sid]
	if rl == nil {
		rl = h.directs[sid]
	}
	h.mu.Unlock()
	// Only remote-control sessions: tunnels and terminals have no switch sides.
	if rl == nil || rl.userID != userID || rl.kind != KindScreen {
		return "", ErrNoSession
	}
	select {
	case <-rl.paired:
	default:
		if !rl.directStarted() {
			return "", ErrNoSession
		}
	}
	return rl.deviceID, nil
}

// OfferSwitch creates a one-time grant (valid 60 s) that lets the controlled device of session
// `sid` open one session to device `to`. The caller has checked that userID may offer `to`.
func (h *Hub) OfferSwitch(sid string, userID int64, to, name string, device map[string]any) (string, error) {
	from, err := h.SessionOf(sid, userID)
	if err != nil {
		return "", err
	}
	if from == to {
		return "", ErrSameDevice
	}
	if !h.Online(to) {
		return "", ErrOffline
	}
	tok := auth.RandomToken(24)
	h.grants.put(tok, &switchGrant{from: from, to: to, userID: userID, origSID: sid, name: name, device: device, expires: time.Now().Add(switchGrantTTL)})
	return tok, nil
}

// switchConnect redeems a grant for agent `a` and answers on its signaling connection with
// {"t":"switch_ticket","req",ticket,device} or {"t":"switch_ticket","req","error"}.
func (h *Hub) switchConnect(ctx context.Context, a *agentConn, m agentMsg) {
	reply := func(v map[string]any) {
		v["t"], v["req"] = "switch_ticket", m.Req
		a.send(v)
	}
	sg, err := h.grants.take(m.Grant, a.id)
	if err != nil {
		reply(map[string]any{"error": "invalid_grant"})
		return
	}
	viewer := ViewerInfo{Name: sg.name, IP: a.ip}
	t, err := h.startSession(ctx, sg.to, sg.userID, viewer, m.Direct, SessionOptions{}, a.id)
	if err != nil {
		code := "unavailable"
		if errors.Is(err, ErrOffline) {
			code = "offline"
		}
		reply(map[string]any{"error": code})
		return
	}
	uid := sg.userID
	_ = h.store.AddAudit(ctx, &uid, a.ip, "session.request", sg.to, map[string]any{"sid": t.SID, "switch_from": sg.origSID, "viewer_device": a.id})
	h.log.Info("switch sides", "from", a.id, "to", sg.to, "sid", t.SID)
	reply(map[string]any{"ticket": t, "device": sg.device})
}
