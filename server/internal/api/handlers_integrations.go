package api

import (
	"context"
	"encoding/json"
	"errors"
	"net/http"
	"time"

	"aegiron/server/internal/integrations"
	"aegiron/server/internal/store"
)

func supportedIntegration(w http.ResponseWriter, r *http.Request) (string, bool) {
	p := r.PathValue("provider")
	switch p {
	case integrations.TeamViewer:
		return p, true
	case "anydesk":
		writeErr(w, http.StatusBadRequest, "AnyDesk does not publicly document a REST API for clients or sessions; import AnyDesk devices with CSV instead")
	default:
		writeErr(w, http.StatusNotFound, "unknown integration")
	}
	return "", false
}

func (a *API) getIntegration(w http.ResponseWriter, r *http.Request) {
	p, ok := supportedIntegration(w, r)
	if !ok {
		return
	}
	in, err := a.store.GetIntegration(r.Context(), p)
	if errors.Is(err, store.ErrNotFound) {
		writeJSON(w, http.StatusOK, map[string]any{"integration": nil})
		return
	} else if err != nil {
		a.internal(w, err)
		return
	}
	writeJSON(w, http.StatusOK, map[string]any{"integration": in, "has_token": len(in.Config) > 0})
}

func (a *API) putIntegration(w http.ResponseWriter, r *http.Request) {
	p, ok := supportedIntegration(w, r)
	if !ok {
		return
	}
	var req struct {
		Token   *string `json:"token"` // nil = keep the stored token
		Enabled bool    `json:"enabled"`
		GroupID *int64  `json:"group_id"`
	}
	if !readJSON(w, r, &req) {
		return
	}
	var sealed []byte
	if req.Token != nil {
		b, _ := json.Marshal(integrations.Config{Token: trim(*req.Token, 512)})
		var err error
		if sealed, err = a.vault.Seal(string(b)); err != nil {
			a.internal(w, err)
			return
		}
	} else if _, err := a.store.GetIntegration(r.Context(), p); errors.Is(err, store.ErrNotFound) {
		writeErr(w, http.StatusBadRequest, "token is required")
		return
	}
	if err := a.store.SaveIntegration(r.Context(), p, sealed, req.Enabled, req.GroupID); err != nil {
		a.internal(w, err)
		return
	}
	a.audit(r, "integration.saved", p, map[string]any{"enabled": req.Enabled, "token_changed": req.Token != nil})
	writeJSON(w, http.StatusOK, map[string]bool{"ok": true})
}

func (a *API) deleteIntegration(w http.ResponseWriter, r *http.Request) {
	p, ok := supportedIntegration(w, r)
	if !ok {
		return
	}
	if err := a.store.DeleteIntegration(r.Context(), p); err != nil {
		a.internal(w, err)
		return
	}
	a.audit(r, "integration.deleted", p, nil)
	writeJSON(w, http.StatusOK, map[string]bool{"ok": true})
}

func (a *API) syncIntegration(w http.ResponseWriter, r *http.Request) {
	if _, ok := supportedIntegration(w, r); !ok {
		return
	}
	ctx, cancel := context.WithTimeout(r.Context(), 2*time.Minute)
	defer cancel()
	res, err := integrations.SyncTeamViewer(ctx, a.store, a.vault, a.cfg.TeamViewerAPI)
	a.audit(r, "integration.sync", integrations.TeamViewer, map[string]any{"result": res, "error": errString(err)})
	if err != nil {
		writeErr(w, http.StatusBadGateway, err.Error())
		return
	}
	writeJSON(w, http.StatusOK, map[string]any{"result": res})
}

// SyncAll runs enabled integrations; called periodically from main.
func (a *API) SyncAll(ctx context.Context) {
	in, err := a.store.GetIntegration(ctx, integrations.TeamViewer)
	if err != nil || !in.Enabled {
		return
	}
	if res, err := integrations.SyncTeamViewer(ctx, a.store, a.vault, a.cfg.TeamViewerAPI); err != nil {
		a.log.Warn("teamviewer sync failed", "err", err)
	} else {
		a.log.Info("teamviewer sync", "devices", res.Devices, "new", res.NewDevices, "sessions", res.Sessions)
	}
}

func (a *API) listExternalSessions(w http.ResponseWriter, r *http.Request) {
	s, err := a.store.ListExternalSessions(r.Context(), 500)
	if err != nil {
		a.internal(w, err)
		return
	}
	writeJSON(w, http.StatusOK, map[string]any{"sessions": s})
}

func errString(err error) string {
	if err == nil {
		return ""
	}
	return err.Error()
}
