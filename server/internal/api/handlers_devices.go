package api

import (
	"errors"
	"net/http"
	"regexp"
	"strings"
	"time"

	"redesk/server/internal/hub"
	"redesk/server/internal/store"
)

var deviceIDPattern = regexp.MustCompile(`^[0-9]{9}$`)

type deviceView struct {
	*store.Device
	Online bool `json:"online"`
}

func (a *API) view(d *store.Device, online map[string]bool) *deviceView {
	if d == nil {
		return nil
	}
	return &deviceView{Device: d, Online: online[d.ID]}
}

func (a *API) listDevices(w http.ResponseWriter, r *http.Request) {
	if userFrom(r).Role != "admin" {
		writeErr(w, http.StatusForbidden, "admin only; use the address book")
		return
	}
	devs, err := a.store.ListDevices(r.Context())
	if err != nil {
		a.internal(w, err)
		return
	}
	online := a.hub.OnlineSet()
	out := make([]*deviceView, len(devs))
	for i, d := range devs {
		out[i] = a.view(d, online)
	}
	writeJSON(w, http.StatusOK, map[string]any{"devices": out})
}

// canAccess applies the access policy: admins reach everything; with "groups",
// other users only reach devices in a group they belong to.
func (a *API) canAccess(r *http.Request, deviceID string) (bool, error) {
	u := userFrom(r)
	if u.Role == "admin" {
		return true, nil
	}
	st, err := a.store.GetSettings(r.Context())
	if err != nil {
		return false, err
	}
	if st.AccessPolicy != "groups" {
		return true, nil
	}
	return a.store.UserInDeviceGroup(r.Context(), u.ID, deviceID)
}

func (a *API) myDevices(w http.ResponseWriter, r *http.Request) {
	devs, err := a.store.DevicesForUser(r.Context(), userFrom(r).ID)
	if err != nil {
		a.internal(w, err)
		return
	}
	online := a.hub.OnlineSet()
	out := make([]*deviceView, len(devs))
	for i, d := range devs {
		if userFrom(r).Role != "admin" {
			d.LastIP = ""
		}
		out[i] = a.view(d, online)
	}
	writeJSON(w, http.StatusOK, map[string]any{"devices": out})
}

func (a *API) getDevice(w http.ResponseWriter, r *http.Request) {
	id := r.PathValue("id")
	if ok, err := a.canAccess(r, id); err != nil {
		a.internal(w, err)
		return
	} else if !ok {
		writeErr(w, http.StatusNotFound, "not found")
		return
	}
	d, err := a.store.GetDevice(r.Context(), id)
	if err != nil {
		storeErr(a, w, err)
		return
	}
	if userFrom(r).Role != "admin" {
		d.LastIP = "" // network location is admin-only
	}
	writeJSON(w, http.StatusOK, map[string]any{"device": a.view(d, a.hub.OnlineSet())})
}

func (a *API) updateDevice(w http.ResponseWriter, r *http.Request) {
	var req struct {
		Alias    string `json:"alias"`
		Note     string `json:"note"`
		Disabled bool   `json:"disabled"`
		GroupID  *int64 `json:"group_id"`
	}
	if !readJSON(w, r, &req) {
		return
	}
	id := r.PathValue("id")
	if err := a.store.UpdateDevice(r.Context(), id, trim(req.Alias, 64), trim(req.Note, 1000), req.Disabled, req.GroupID); err != nil {
		storeErr(a, w, err)
		return
	}
	if req.Disabled {
		a.hub.Disconnect(id, "device disabled")
	}
	a.audit(r, "device.updated", id, map[string]any{"alias": req.Alias, "disabled": req.Disabled, "group_id": req.GroupID})
	writeJSON(w, http.StatusOK, map[string]bool{"ok": true})
}

func (a *API) deleteDevice(w http.ResponseWriter, r *http.Request) {
	id := r.PathValue("id")
	if err := a.store.DeleteDevice(r.Context(), id); err != nil {
		storeErr(a, w, err)
		return
	}
	a.hub.Disconnect(id, "device deleted")
	a.audit(r, "device.deleted", id, nil)
	writeJSON(w, http.StatusOK, map[string]bool{"ok": true})
}

func (a *API) resetDeviceKey(w http.ResponseWriter, r *http.Request) {
	id := r.PathValue("id")
	if err := a.store.ResetDeviceKey(r.Context(), id); err != nil {
		storeErr(a, w, err)
		return
	}
	a.hub.Disconnect(id, "device key reset")
	a.audit(r, "device.key_reset", id, nil)
	writeJSON(w, http.StatusOK, map[string]bool{"ok": true})
}

// --- address book ---

func (a *API) listAddressBook(w http.ResponseWriter, r *http.Request) {
	entries, err := a.store.ListAddressBook(r.Context(), userFrom(r).ID)
	if err != nil {
		a.internal(w, err)
		return
	}
	online := a.hub.OnlineSet()
	type entryView struct {
		*store.AddressEntry
		Device *deviceView `json:"device"`
	}
	out := make([]entryView, len(entries))
	for i, e := range entries {
		if e.Device != nil && userFrom(r).Role != "admin" {
			e.Device.LastIP = ""
		}
		out[i] = entryView{AddressEntry: e, Device: a.view(e.Device, online)}
	}
	writeJSON(w, http.StatusOK, map[string]any{"entries": out})
}

func (a *API) putAddressEntry(w http.ResponseWriter, r *http.Request) {
	var req struct {
		Alias    string   `json:"alias"`
		Tags     []string `json:"tags"`
		Favorite bool     `json:"favorite"`
	}
	if !readJSON(w, r, &req) {
		return
	}
	id := r.PathValue("id")
	if !deviceIDPattern.MatchString(id) {
		writeErr(w, http.StatusBadRequest, "device id must be 9 digits")
		return
	}
	tags := []string{}
	for _, t := range req.Tags {
		if t = trim(t, 32); t != "" && len(tags) < 16 {
			tags = append(tags, t)
		}
	}
	if err := a.store.UpsertAddressEntry(r.Context(), userFrom(r).ID, id, trim(req.Alias, 64), tags, req.Favorite); err != nil {
		a.internal(w, err)
		return
	}
	writeJSON(w, http.StatusOK, map[string]bool{"ok": true})
}

func (a *API) deleteAddressEntry(w http.ResponseWriter, r *http.Request) {
	if err := a.store.DeleteAddressEntry(r.Context(), userFrom(r).ID, r.PathValue("id")); err != nil {
		a.internal(w, err)
		return
	}
	writeJSON(w, http.StatusOK, map[string]bool{"ok": true})
}

// --- remote sessions ---

func (a *API) createSession(w http.ResponseWriter, r *http.Request) {
	var req struct {
		DeviceID string `json:"device_id"`
	}
	if !readJSON(w, r, &req) {
		return
	}
	u := userFrom(r)
	if !a.limiter.allow("session:"+u.Email, 30, time.Minute) {
		writeErr(w, http.StatusTooManyRequests, "too many connection attempts")
		return
	}
	id := strings.ReplaceAll(req.DeviceID, " ", "")
	if ok, err := a.canAccess(r, id); err != nil {
		a.internal(w, err)
		return
	} else if !ok {
		a.audit(r, "session.denied", id, map[string]any{"reason": "not_in_group"})
		writeErr(w, http.StatusForbidden, "you are not allowed to connect to this device")
		return
	}
	d, err := a.store.GetDevice(r.Context(), id)
	switch {
	case errors.Is(err, store.ErrNotFound):
		writeErr(w, http.StatusNotFound, "unknown device id")
		return
	case err != nil:
		a.internal(w, err)
		return
	case d.Disabled:
		writeErr(w, http.StatusForbidden, "device is disabled")
		return
	case d.PublicKey == nil:
		writeErr(w, http.StatusConflict, "device key was reset; waiting for the agent to re-register")
		return
	}
	ip := a.clientIP(r)
	t, err := a.hub.StartSession(r.Context(), d.ID, u.ID, hub.ViewerInfo{Email: u.Email, Name: u.Name, IP: ip})
	switch {
	case errors.Is(err, hub.ErrOffline):
		writeErr(w, http.StatusConflict, "device is offline")
		return
	case errors.Is(err, hub.ErrBusy):
		writeErr(w, http.StatusServiceUnavailable, "device is busy, try again")
		return
	case err != nil:
		a.internal(w, err)
		return
	}
	a.audit(r, "session.request", d.ID, map[string]any{"sid": t.SID})
	writeJSON(w, http.StatusOK, map[string]any{
		"ticket": t,
		"device": map[string]any{"id": d.ID, "public_key": d.PublicKey, "hostname": d.Hostname, "os": d.OS, "alias": d.Alias},
	})
}

func (a *API) listSessions(w http.ResponseWriter, r *http.Request) {
	s, err := a.store.ListRemoteSessions(r.Context(), 200)
	if err != nil {
		a.internal(w, err)
		return
	}
	writeJSON(w, http.StatusOK, map[string]any{"sessions": s})
}

func (a *API) activeSessions(w http.ResponseWriter, r *http.Request) {
	writeJSON(w, http.StatusOK, map[string]any{"sessions": a.hub.ActiveSessions(), "online_devices": len(a.hub.OnlineSet())})
}

func trim(s string, n int) string {
	s = strings.TrimSpace(s)
	if len(s) > n {
		s = s[:n]
	}
	return s
}
