package hub

import (
	"context"
	"encoding/json"
	"time"

	"github.com/coder/websocket"

	"aegiron/server/internal/store"
)

// AgentPolicy is the strategy as sent to agents ({"t":"policy","policy":...} and in
// "registered"). Settings the strategy leaves out are not managed.
type AgentPolicy struct {
	ID       int64                  `json:"id"`
	Name     string                 `json:"name"`
	Settings store.StrategySettings `json:"settings"`
}

func policyOf(st *store.Strategy) string {
	if st == nil {
		return "null"
	}
	b, _ := json.Marshal(AgentPolicy{ID: st.ID, Name: st.Name, Settings: st.Settings})
	return string(b)
}

// policyJSON is the effective strategy of a device as JSON ("null" when it has none).
func (h *Hub) policyJSON(ctx context.Context, deviceID string) (string, error) {
	st, err := h.store.EffectiveStrategy(ctx, deviceID)
	if err != nil {
		return "", err
	}
	return policyOf(st), nil
}

// RefreshPolicies recomputes the strategy of every online agent and sends it to those whose
// strategy changed (after an admin edited strategies, groups or devices).
func (h *Hub) RefreshPolicies(ctx context.Context) {
	h.mu.Lock()
	conns := make([]*agentConn, 0, len(h.agents))
	ids := make([]string, 0, len(h.agents))
	for id, a := range h.agents {
		conns = append(conns, a)
		ids = append(ids, id)
	}
	h.mu.Unlock()
	if len(ids) == 0 {
		return
	}
	eff, err := h.store.EffectiveStrategies(ctx, ids)
	if err != nil {
		h.log.Error("cannot load strategies", "err", err)
		return
	}
	for _, a := range conns {
		p := policyOf(eff[a.id])
		a.policyMu.Lock()
		changed := a.policy != p
		if changed && a.send(map[string]any{"t": "policy", "policy": json.RawMessage(p)}) {
			a.policy = p
		}
		a.policyMu.Unlock()
		if changed {
			h.log.Info("strategy sent to agent", "id", a.id)
		}
	}
}

// IDChanged tells an online agent that an admin changed its id and drops its connection; it
// reconnects under the new id. Offline agents learn it when they next register.
func (h *Hub) IDChanged(oldID, newID string) {
	h.mu.Lock()
	a := h.agents[oldID]
	h.mu.Unlock()
	if a == nil {
		return
	}
	a.send(map[string]string{"t": "id_changed", "id": newID})
	// The agent closes the connection itself; this is for agents that predate id changes (they
	// then fail to register under the old id until updated). The delay lets the writer flush.
	time.AfterFunc(3*time.Second, func() { a.conn.Close(websocket.StatusNormalClosure, "device id changed") })
}
