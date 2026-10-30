package api

// Strategies: bundles of agent settings that admins assign to device groups or single devices.
// Agents receive the effective strategy over signaling (see hub/policy.go) and lock those
// settings locally; RecordSessions is enforced here (recordingToken).

import (
	"context"
	"net/http"
	"strconv"
	"strings"

	"aegiron/server/internal/store"
)

func (a *API) listStrategies(w http.ResponseWriter, r *http.Request) {
	s, err := a.store.ListStrategies(r.Context())
	if err != nil {
		a.internal(w, err)
		return
	}
	writeJSON(w, http.StatusOK, map[string]any{"strategies": s})
}

type strategyReq struct {
	Name        string                 `json:"name"`
	Description string                 `json:"description"`
	Settings    store.StrategySettings `json:"settings"`
	// GroupIDs: groups that use this strategy (replaces the previous list).
	GroupIDs []int64 `json:"group_ids"`
}

func validStrategy(s *store.StrategySettings) string {
	if m := s.ApproveMode; m != nil && *m != "password" && *m != "click" && *m != "both" {
		return "approve_mode must be password, click or both"
	}
	// "Not recorded" is the server-wide default; a strategy can only add recording.
	if s.RecordSessions != nil && !*s.RecordSessions {
		s.RecordSessions = nil
	}
	return ""
}

func (a *API) saveStrategy(w http.ResponseWriter, r *http.Request, id int64) {
	var req strategyReq
	if !readJSON(w, r, &req) {
		return
	}
	name := trim(req.Name, 64)
	if name == "" {
		writeErr(w, http.StatusBadRequest, "name is required")
		return
	}
	if msg := validStrategy(&req.Settings); msg != "" {
		writeErr(w, http.StatusBadRequest, msg)
		return
	}
	created := id == 0
	id, err := a.store.SaveStrategy(r.Context(), id, name, trim(req.Description, 500), req.Settings)
	if err != nil {
		if strings.Contains(err.Error(), "duplicate key") {
			writeErr(w, http.StatusConflict, "strategy name already exists")
			return
		}
		storeErr(a, w, err)
		return
	}
	if req.GroupIDs != nil {
		if err := a.store.SetStrategyGroups(r.Context(), id, req.GroupIDs); err != nil {
			a.internal(w, err)
			return
		}
	}
	action := "strategy.updated"
	if created {
		action = "strategy.created"
	}
	a.audit(r, action, strconv.FormatInt(id, 10), map[string]any{"name": name, "settings": req.Settings, "group_ids": req.GroupIDs})
	a.refreshPolicies()
	writeJSON(w, http.StatusOK, map[string]any{"id": id})
}

func (a *API) createStrategy(w http.ResponseWriter, r *http.Request) { a.saveStrategy(w, r, 0) }

func (a *API) updateStrategy(w http.ResponseWriter, r *http.Request) {
	id, err := strconv.ParseInt(r.PathValue("id"), 10, 64)
	if err != nil || id <= 0 {
		writeErr(w, http.StatusBadRequest, "invalid id")
		return
	}
	a.saveStrategy(w, r, id)
}

func (a *API) deleteStrategy(w http.ResponseWriter, r *http.Request) {
	id, err := strconv.ParseInt(r.PathValue("id"), 10, 64)
	if err != nil {
		writeErr(w, http.StatusBadRequest, "invalid id")
		return
	}
	st, err := a.store.GetStrategy(r.Context(), id)
	if err != nil {
		storeErr(a, w, err)
		return
	}
	if err := a.store.DeleteStrategy(r.Context(), id); err != nil {
		storeErr(a, w, err)
		return
	}
	a.audit(r, "strategy.deleted", r.PathValue("id"), map[string]any{"name": st.Name})
	a.refreshPolicies()
	writeJSON(w, http.StatusOK, map[string]bool{"ok": true})
}

// refreshPolicies pushes changed strategies to online agents without holding up the request.
func (a *API) refreshPolicies() {
	go a.hub.RefreshPolicies(context.Background())
}
