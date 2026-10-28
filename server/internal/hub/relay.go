package hub

import (
	"context"
	"crypto/subtle"
	"net/http"
	"sync"
	"sync/atomic"
	"time"

	"github.com/coder/websocket"

	"aegiron/server/internal/auth"
)

// relay pairs one viewer and one agent connection for a single remote session.
type relay struct {
	sid       string
	deviceID  string
	userID    int64
	viewerIP  string
	viewerTok string
	agentTok  string
	created   time.Time

	mu     sync.Mutex
	viewer *websocket.Conn
	agent  *websocket.Conn
	paired chan struct{}
	done   chan struct{}
	once   sync.Once

	toViewer atomic.Int64
	toAgent  atomic.Int64

	// direct is set when the viewer asked for, and the agent supports, a direct path.
	direct *directState
}

// directStarted reports whether the session moved to a direct path (no relay audit then).
func (rl *relay) directStarted() bool { return rl.direct != nil && rl.direct.isStarted() }

// Ticket is what the console needs to open the viewer side of a session.
type Ticket struct {
	SID       string `json:"sid"`
	Token     string `json:"token"`
	RelayPath string `json:"relay_path"`
	// Direct is present only when the viewer asked for it and the agent offered candidates.
	Direct *DirectTicket `json:"direct,omitempty"`
}

type ViewerInfo struct {
	Email string `json:"email"`
	Name  string `json:"name"`
	IP    string `json:"ip"`
}

// StartSession asks an online agent to join a fresh relay and returns the viewer ticket.
// With wantDirect (native viewers that can race a direct path), an agent that supports it
// also gets a direct token and the ticket carries the agent's candidates.
func (h *Hub) StartSession(ctx context.Context, deviceID string, userID int64, viewer ViewerInfo, wantDirect bool) (*Ticket, error) {
	h.mu.Lock()
	a := h.agents[deviceID]
	h.mu.Unlock()
	if a == nil {
		return nil, ErrOffline
	}
	rl := &relay{
		sid:       auth.RandomToken(18),
		deviceID:  deviceID,
		userID:    userID,
		viewerIP:  viewer.IP,
		viewerTok: auth.RandomToken(24),
		agentTok:  auth.RandomToken(24),
		created:   time.Now(),
		paired:    make(chan struct{}),
		done:      make(chan struct{}),
	}
	if wantDirect && h.P2P && a.direct {
		rl.direct = newDirectState()
	}
	if err := h.store.CreateRemoteSession(ctx, rl.sid, deviceID, userID, viewer.IP); err != nil {
		return nil, err
	}
	h.mu.Lock()
	h.relays[rl.sid] = rl
	if rl.direct != nil {
		h.directs[rl.sid] = rl
	}
	h.mu.Unlock()

	msg := map[string]any{"t": "session", "sid": rl.sid, "token": rl.agentTok, "relay": "/ws/relay", "viewer": viewer}
	if rl.direct != nil {
		d := map[string]any{"token": rl.direct.token}
		if h.UDPPort > 0 {
			d["punch_key"], d["udp_port"] = rl.direct.agentPunch, h.UDPPort
		}
		msg["direct"] = d
	}
	if !a.send(msg) {
		h.endRelay(rl, "agent_busy")
		return nil, ErrBusy
	}
	time.AfterFunc(pairTimeout, func() {
		select {
		case <-rl.paired:
		default:
			h.endRelay(rl, "pair_timeout")
		}
	})
	t := &Ticket{SID: rl.sid, Token: rl.viewerTok, RelayPath: "/ws/relay"}
	if rl.direct != nil {
		time.AfterFunc(directGrace, func() {
			if !rl.direct.isStarted() {
				h.mu.Lock()
				delete(h.directs, rl.sid)
				h.mu.Unlock()
			}
		})
		t.Direct = h.awaitOffer(ctx, a, rl)
	}
	return t, nil
}

func (h *Hub) rejectRelay(sid, deviceID, reason string) {
	h.mu.Lock()
	rl := h.relays[sid]
	h.mu.Unlock()
	if rl != nil && rl.deviceID == deviceID {
		h.endRelay(rl, "agent_rejected: "+trunc(reason, 64))
	}
}

func (h *Hub) endRelay(rl *relay, reason string) {
	rl.once.Do(func() {
		close(rl.done)
		h.mu.Lock()
		delete(h.relays, rl.sid)
		h.mu.Unlock()

		rl.mu.Lock()
		for _, c := range []*websocket.Conn{rl.viewer, rl.agent} {
			if c != nil {
				c.Close(websocket.StatusNormalClosure, trunc(reason, 120))
			}
		}
		rl.mu.Unlock()

		if rl.directStarted() {
			// The session went direct before pairing: the direct path writes its own records.
			return
		}
		ctx, cancel := context.WithTimeout(context.Background(), 5*time.Second)
		defer cancel()
		tv, ta := rl.toViewer.Load(), rl.toAgent.Load()
		_ = h.store.MarkSessionEnded(ctx, rl.sid, tv, ta, reason)
		uid := rl.userID
		_ = h.store.AddAudit(ctx, &uid, rl.viewerIP, "session.end", rl.deviceID, map[string]any{
			"sid": rl.sid, "path": "relay", "reason": reason, "seconds": int(time.Since(rl.created).Seconds()),
			"bytes_to_viewer": tv, "bytes_to_agent": ta,
		})
		h.log.Info("session ended", "sid", rl.sid, "device", rl.deviceID, "reason", reason)
	})
}

// HandleRelay: GET /ws/relay?sid=..&token=..
func (h *Hub) HandleRelay(w http.ResponseWriter, r *http.Request) {
	sid, tok := r.URL.Query().Get("sid"), r.URL.Query().Get("token")
	h.mu.Lock()
	rl := h.relays[sid]
	h.mu.Unlock()
	if rl == nil {
		http.Error(w, "unknown session", http.StatusNotFound)
		return
	}
	isViewer := subtle.ConstantTimeCompare([]byte(tok), []byte(rl.viewerTok)) == 1
	isAgent := subtle.ConstantTimeCompare([]byte(tok), []byte(rl.agentTok)) == 1
	if !isViewer && !isAgent {
		http.Error(w, "forbidden", http.StatusForbidden)
		return
	}
	// The viewer token is only ever handed to the logged-in requester; require that same login.
	if isViewer && (h.ViewerUserID == nil || h.ViewerUserID(r) != rl.userID) {
		http.Error(w, "forbidden", http.StatusForbidden)
		return
	}
	if rl.directStarted() {
		http.Error(w, "session continues on a direct path", http.StatusConflict)
		return
	}
	var directCh chan struct{}
	if rl.direct != nil {
		directCh = rl.direct.started
	}

	c, err := websocket.Accept(w, r, &websocket.AcceptOptions{OriginPatterns: h.origins})
	if err != nil {
		return
	}
	c.SetReadLimit(32 << 20)

	rl.mu.Lock()
	slot := &rl.agent
	if isViewer {
		slot = &rl.viewer
	}
	if *slot != nil {
		rl.mu.Unlock()
		c.Close(websocket.StatusPolicyViolation, "already connected")
		return
	}
	*slot = c
	both := rl.viewer != nil && rl.agent != nil
	rl.mu.Unlock()

	if both {
		// Notify both ends before any forwarding starts so "paired" is always the first message.
		ctx, cancel := context.WithTimeout(r.Context(), writeTimeout)
		e1 := writeJSON(ctx, rl.viewer, map[string]string{"t": "paired"})
		e2 := writeJSON(ctx, rl.agent, map[string]string{"t": "paired"})
		cancel()
		if e1 != nil || e2 != nil {
			h.endRelay(rl, "pair_write_failed")
			return
		}
		close(rl.paired)
		_ = h.store.MarkSessionStarted(r.Context(), rl.sid)
		uid := rl.userID
		_ = h.store.AddAudit(r.Context(), &uid, rl.viewerIP, "session.start", rl.deviceID, map[string]any{"sid": rl.sid, "path": "relay"})
		h.log.Info("session started", "sid", rl.sid, "device", rl.deviceID)
	}

	select {
	case <-rl.paired:
	case <-rl.done:
		return
	case <-directCh:
		c.Close(websocket.StatusNormalClosure, "direct")
		return
	case <-r.Context().Done():
		if rl.direct != nil {
			// A peer that won a direct path drops its relay slot; wait for direct_started
			// or the pairing timeout instead of ending the session here.
			rl.mu.Lock()
			if *slot == c {
				*slot = nil
			}
			rl.mu.Unlock()
			return
		}
		h.endRelay(rl, "disconnected_before_pair")
		return
	}

	rl.mu.Lock()
	dst, counter := rl.agent, &rl.toAgent
	if isAgent {
		dst, counter = rl.viewer, &rl.toViewer
	}
	rl.mu.Unlock()

	reason := forward(r.Context(), c, dst, counter)
	if isViewer {
		reason = "viewer_" + reason
	} else {
		reason = "agent_" + reason
	}
	h.endRelay(rl, reason)
}

// forward copies messages from src to dst until either side fails.
// Writes block on a slow reader, which back-pressures the sender over TCP.
func forward(ctx context.Context, src, dst *websocket.Conn, counter *atomic.Int64) string {
	for {
		typ, data, err := src.Read(ctx)
		if err != nil {
			if websocket.CloseStatus(err) == websocket.StatusNormalClosure {
				return "closed"
			}
			return "disconnected"
		}
		wctx, cancel := context.WithTimeout(ctx, 30*time.Second)
		err = dst.Write(wctx, typ, data)
		cancel()
		if err != nil {
			return "peer_write_failed"
		}
		counter.Add(int64(len(data)))
	}
}

// ActiveSessions lists relays currently open, for the dashboard.
func (h *Hub) ActiveSessions() []map[string]any {
	h.mu.Lock()
	defer h.mu.Unlock()
	out := []map[string]any{}
	for _, rl := range h.relays {
		state := "pending"
		select {
		case <-rl.paired:
			state = "active"
		default:
		}
		if rl.directStarted() {
			continue // listed below
		}
		out = append(out, map[string]any{"sid": rl.sid, "device_id": rl.deviceID, "user_id": rl.userID, "state": state, "path": "relay", "created_at": rl.created})
	}
	for _, rl := range h.directs {
		if rl.directStarted() {
			out = append(out, map[string]any{"sid": rl.sid, "device_id": rl.deviceID, "user_id": rl.userID, "state": "active", "path": rl.direct.pathName(), "created_at": rl.created})
		}
	}
	return out
}
