package api

import (
	"net/http"
	"strconv"
	"strings"
	"time"

	"aegiron/server/internal/auth"
	"aegiron/server/internal/store"
)

// Trusted devices: "remember this device for 30 days" on the 2FA step.
//
// After a correct TOTP code the client may ask to be remembered. The server returns a random
// 32-byte token (web: HttpOnly cookie scoped to /api/auth, app: trust_token in the JSON reply)
// and stores only its SHA-256, bound to the user. Presenting it with the right password skips
// the TOTP step until it expires (fixed, not sliding) or is revoked. It is never a substitute
// for the password and never creates a session on its own.

const (
	trustCookie   = "aegiron_trust"
	trustLifetime = 30 * 24 * time.Hour
)

func (a *API) trustCookieFor(tok string, exp time.Time) *http.Cookie {
	return &http.Cookie{
		Name: trustCookie, Value: tok, Path: "/api/auth", Expires: exp, MaxAge: int(time.Until(exp).Seconds()),
		HttpOnly: true, Secure: a.cfg.CookieSecure, SameSite: http.SameSiteStrictMode,
	}
}

// rememberDevice issues a trust token for u and audits it.
func (a *API) rememberDevice(r *http.Request, u *store.User, label, ua string) (string, *store.TrustedDevice, error) {
	tok := auth.RandomToken(32)
	d, err := a.store.CreateTrustedDevice(r.Context(), auth.HashToken(tok), u.ID, trim(label, 100), trim(ua, 300),
		a.clientIP(r), time.Now().Add(trustLifetime))
	if err != nil {
		return "", nil, err
	}
	_ = a.store.AddAudit(r.Context(), &u.ID, a.clientIP(r), "auth.trusted_device_added", u.Email,
		map[string]any{"id": d.ID, "label": d.Label})
	return tok, d, nil
}

// revokeTrustedDevices forgets every remembered device of target (password or 2FA changed,
// account disabled, or on request) and audits how many were removed.
func (a *API) revokeTrustedDevices(r *http.Request, target *store.User, reason string) int64 {
	n, err := a.store.DeleteUserTrustedDevices(r.Context(), target.ID)
	if err != nil {
		a.log.Error("revoke trusted devices", "user", target.ID, "err", err)
		return 0
	}
	if n > 0 || reason == "user" || reason == "admin" {
		a.audit(r, "user.trusted_devices_revoked", target.Email, map[string]any{"count": n, "reason": reason})
	}
	return n
}

// browserLabel turns a User-Agent into a short "Chrome บน macOS" style name.
func browserLabel(ua string) string {
	browser := "เบราว์เซอร์"
	switch {
	case strings.Contains(ua, "Edg/"):
		browser = "Edge"
	case strings.Contains(ua, "Firefox/"):
		browser = "Firefox"
	case strings.Contains(ua, "OPR/"):
		browser = "Opera"
	case strings.Contains(ua, "Chrome/"):
		browser = "Chrome"
	case strings.Contains(ua, "Safari/"):
		browser = "Safari"
	}
	os := ""
	switch {
	case strings.Contains(ua, "iPhone"), strings.Contains(ua, "iPad"):
		os = "iOS"
	case strings.Contains(ua, "Android"):
		os = "Android"
	case strings.Contains(ua, "Windows"):
		os = "Windows"
	case strings.Contains(ua, "Mac OS X"), strings.Contains(ua, "Macintosh"):
		os = "macOS"
	case strings.Contains(ua, "Linux"):
		os = "Linux"
	}
	if os == "" {
		return browser
	}
	return browser + " บน " + os
}

type trustedDeviceView struct {
	*store.TrustedDevice
	// Current marks the browser making the request (its cookie matches).
	Current bool `json:"current"`
}

func (a *API) trustedDeviceList(w http.ResponseWriter, r *http.Request, userID int64) {
	list, err := a.store.ListTrustedDevices(r.Context(), userID)
	if err != nil {
		a.internal(w, err)
		return
	}
	var mine []byte
	if c, err := r.Cookie(trustCookie); err == nil && c.Value != "" {
		mine = auth.HashToken(c.Value)
	}
	out := make([]trustedDeviceView, 0, len(list))
	for _, d := range list {
		out = append(out, trustedDeviceView{TrustedDevice: d, Current: mine != nil && string(mine) == string(d.TokenHash)})
	}
	writeJSON(w, http.StatusOK, map[string]any{"devices": out})
}

func (a *API) trustedDeviceDelete(w http.ResponseWriter, r *http.Request, target *store.User) {
	id, err := strconv.ParseInt(r.PathValue("tid"), 10, 64)
	if err != nil {
		writeErr(w, http.StatusBadRequest, "invalid id")
		return
	}
	d, err := a.store.DeleteTrustedDevice(r.Context(), target.ID, id)
	if err != nil {
		storeErr(a, w, err)
		return
	}
	by := "user"
	if userFrom(r).ID != target.ID {
		by = "admin"
	}
	a.audit(r, "user.trusted_device_revoked", target.Email, map[string]any{"id": d.ID, "label": d.Label, "by": by})
	writeJSON(w, http.StatusOK, map[string]bool{"ok": true})
}

// GET /api/auth/trusted-devices
func (a *API) listMyTrustedDevices(w http.ResponseWriter, r *http.Request) {
	a.trustedDeviceList(w, r, userFrom(r).ID)
}

// DELETE /api/auth/trusted-devices/{tid}
func (a *API) deleteMyTrustedDevice(w http.ResponseWriter, r *http.Request) {
	a.trustedDeviceDelete(w, r, userFrom(r))
}

// DELETE /api/auth/trusted-devices
func (a *API) deleteMyTrustedDevices(w http.ResponseWriter, r *http.Request) {
	n := a.revokeTrustedDevices(r, userFrom(r), "user")
	http.SetCookie(w, &http.Cookie{Name: trustCookie, Value: "", Path: "/api/auth", MaxAge: -1, HttpOnly: true, Secure: a.cfg.CookieSecure, SameSite: http.SameSiteStrictMode})
	writeJSON(w, http.StatusOK, map[string]any{"ok": true, "revoked": n})
}

// GET /api/users/{id}/trusted-devices (admin)
func (a *API) adminListTrustedDevices(w http.ResponseWriter, r *http.Request) {
	target, ok := a.userParam(w, r)
	if !ok {
		return
	}
	a.trustedDeviceList(w, r, target.ID)
}

// DELETE /api/users/{id}/trusted-devices/{tid} (admin)
func (a *API) adminDeleteTrustedDevice(w http.ResponseWriter, r *http.Request) {
	target, ok := a.userParam(w, r)
	if !ok {
		return
	}
	a.trustedDeviceDelete(w, r, target)
}

// DELETE /api/users/{id}/trusted-devices (admin)
func (a *API) adminDeleteTrustedDevices(w http.ResponseWriter, r *http.Request) {
	target, ok := a.userParam(w, r)
	if !ok {
		return
	}
	n := a.revokeTrustedDevices(r, target, "admin")
	writeJSON(w, http.StatusOK, map[string]any{"ok": true, "revoked": n})
}
