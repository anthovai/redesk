package api

import (
	"errors"
	"net/http"
	"regexp"
	"strings"
	"time"

	"aegiron/server/internal/hub"
	"aegiron/server/internal/store"
)

var deviceIDPattern = regexp.MustCompile(`^[0-9]{9}$`)

type deviceView struct {
	*store.Device
	Online  bool `json:"online"`
	CanWake bool `json:"can_wake"` // offline, MAC known and an online helper on its LAN
}

// viewCtx renders devices for one request: online state, wake helpers and
// admin-only network fields.
type viewCtx struct {
	online  map[string]bool
	helpers []*store.Device
	admin   bool
}

func (a *API) viewCtx(r *http.Request) *viewCtx {
	v := &viewCtx{online: a.hub.OnlineSet(), admin: userFrom(r).Role == "admin"}
	helpers, err := a.hub.OnlineDevices(r.Context())
	if err != nil {
		a.log.Warn("cannot load online devices", "err", err)
	}
	v.helpers = helpers
	return v
}

func (v *viewCtx) view(d *store.Device) *deviceView {
	if d == nil {
		return nil
	}
	dv := &deviceView{Device: d, Online: v.online[d.ID]}
	dv.CanWake = !dv.Online && !d.Disabled && len(d.Macs) > 0 && hub.PickWakeHelper(d, v.helpers) != nil
	if !v.admin {
		// network location is admin-only
		d.LastIP, d.Macs, d.LAN = "", nil, nil
	}
	return dv
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
	vc := a.viewCtx(r)
	out := make([]*deviceView, len(devs))
	for i, d := range devs {
		out[i] = vc.view(d)
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
	vc := a.viewCtx(r)
	out := make([]*deviceView, len(devs))
	for i, d := range devs {
		out[i] = vc.view(d)
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
	writeJSON(w, http.StatusOK, map[string]any{"device": a.viewCtx(r).view(d)})
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

// wakeDevice asks an online agent on the target's LAN to send a Wake-on-LAN packet.
func (a *API) wakeDevice(w http.ResponseWriter, r *http.Request) {
	u := userFrom(r)
	if !a.limiter.allow("wake:"+u.Email, 10, time.Minute) {
		writeCodeErr(w, http.StatusTooManyRequests, "rate_limited", "ส่งคำสั่งปลุกเครื่องบ่อยเกินไป กรุณารอสักครู่")
		return
	}
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
	if d.Disabled {
		writeCodeErr(w, http.StatusForbidden, "disabled", "เครื่องนี้ถูกปิดใช้งาน")
		return
	}
	if a.hub.Online(id) {
		writeCodeErr(w, http.StatusConflict, "online_already", "เครื่องนี้ออนไลน์อยู่แล้ว")
		return
	}
	if len(d.Macs) == 0 {
		writeCodeErr(w, http.StatusConflict, "no_mac", "ยังไม่ทราบ MAC address ของเครื่องนี้ (ต้องเปิด agent รุ่นใหม่ให้ลงทะเบียนอย่างน้อยหนึ่งครั้ง)")
		return
	}
	online, err := a.hub.OnlineDevices(r.Context())
	if err != nil {
		a.internal(w, err)
		return
	}
	helper := hub.PickWakeHelper(d, online)
	if helper == nil {
		writeCodeErr(w, http.StatusConflict, "no_helper", "ไม่มีเครื่องอื่นที่ออนไลน์อยู่ในเครือข่ายเดียวกันเพื่อช่วยปลุกเครื่องนี้")
		return
	}
	if err := a.hub.SendWol(helper.ID, d.Macs, d.ID); err != nil {
		writeCodeErr(w, http.StatusServiceUnavailable, "helper_busy", "เครื่องที่ช่วยปลุกไม่ว่าง กรุณาลองใหม่")
		return
	}
	a.audit(r, "device.wake", d.ID, map[string]any{"helper": helper.ID, "macs": len(d.Macs)})
	resp := map[string]any{"ok": true}
	if u.Role == "admin" {
		resp["helper"] = helper.ID
	}
	writeJSON(w, http.StatusOK, resp)
}

func writeCodeErr(w http.ResponseWriter, status int, code, msg string) {
	writeJSON(w, status, map[string]string{"error": msg, "code": code})
}

// --- address book ---

func (a *API) listAddressBook(w http.ResponseWriter, r *http.Request) {
	entries, err := a.store.ListAddressBook(r.Context(), userFrom(r).ID)
	if err != nil {
		a.internal(w, err)
		return
	}
	vc := a.viewCtx(r)
	type entryView struct {
		*store.AddressEntry
		Device *deviceView `json:"device"`
	}
	out := make([]entryView, len(entries))
	for i, e := range entries {
		out[i] = entryView{AddressEntry: e, Device: vc.view(e.Device)}
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
	t, err := a.hub.StartSession(r.Context(), d.ID, u.ID, hub.ViewerInfo{Email: u.Email, Name: u.Name, IP: ip}, r.URL.Query().Get("direct") == "1")
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
