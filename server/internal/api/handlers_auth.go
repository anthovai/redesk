package api

import (
	"net/http"
	"time"

	"github.com/pquerna/otp/totp"

	"redesk/server/internal/auth"
)

func (a *API) serverInfo(w http.ResponseWriter, r *http.Request) {
	st, err := a.store.GetSettings(r.Context())
	if err != nil {
		a.internal(w, err)
		return
	}
	oidcInfo := map[string]any{"enabled": false, "name": ""}
	if o, err := a.oidcEnabled(r.Context()); err == nil && o != nil {
		oidcInfo = map[string]any{"enabled": true, "name": o.Name}
	}
	writeJSON(w, http.StatusOK, map[string]any{
		"oidc":           oidcInfo,
		"name":           "ReDesk",
		"version":        Version,
		"public_key":     auth.PublicKeyB64(a.key),
		"require_enroll": st.RequireEnroll,
		"gateway":        a.cfg.GuacdAddr != "",
	})
}

func (a *API) login(w http.ResponseWriter, r *http.Request) {
	var req struct {
		Email    string `json:"email"`
		Password string `json:"password"`
		TOTP     string `json:"totp"`
	}
	if !readJSON(w, r, &req) {
		return
	}
	u, ok := a.checkCredentials(w, r, req.Email, req.Password, req.TOTP)
	if !ok {
		return
	}
	ip, email := a.clientIP(r), u.Email

	st, err := a.store.GetSettings(r.Context())
	if err != nil {
		a.internal(w, err)
		return
	}
	tok := auth.RandomToken(32)
	exp := time.Now().Add(time.Duration(st.SessionHours) * time.Hour)
	if err := a.store.CreateAuthSession(r.Context(), auth.HashToken(tok), u.ID, ip, r.UserAgent(), exp); err != nil {
		a.internal(w, err)
		return
	}
	_ = a.store.TouchLogin(r.Context(), u.ID)
	_ = a.store.AddAudit(r.Context(), &u.ID, ip, "auth.login", email, nil)
	http.SetCookie(w, &http.Cookie{
		Name: cookieName, Value: tok, Path: "/", Expires: exp,
		HttpOnly: true, Secure: a.cfg.CookieSecure, SameSite: http.SameSiteStrictMode,
	})
	writeJSON(w, http.StatusOK, map[string]any{"user": u})
}

func (a *API) logout(w http.ResponseWriter, r *http.Request) {
	if c, err := r.Cookie(cookieName); err == nil {
		_ = a.store.DeleteAuthSession(r.Context(), auth.HashToken(c.Value))
	}
	if tok := bearerToken(r); tok != "" {
		_ = a.store.DeleteAuthSession(r.Context(), auth.HashToken(tok))
	}
	a.audit(r, "auth.logout", "", nil)
	http.SetCookie(w, &http.Cookie{Name: cookieName, Value: "", Path: "/", MaxAge: -1, HttpOnly: true, Secure: a.cfg.CookieSecure, SameSite: http.SameSiteStrictMode})
	writeJSON(w, http.StatusOK, map[string]bool{"ok": true})
}

func (a *API) me(w http.ResponseWriter, r *http.Request) {
	writeJSON(w, http.StatusOK, map[string]any{"user": userFrom(r)})
}

func (a *API) changePassword(w http.ResponseWriter, r *http.Request) {
	var req struct {
		Current string `json:"current"`
		New     string `json:"new"`
	}
	if !readJSON(w, r, &req) {
		return
	}
	u := userFrom(r)
	if !auth.VerifyPassword(u.PasswordHash, req.Current) {
		writeErr(w, http.StatusUnauthorized, "current password is incorrect")
		return
	}
	if err := auth.ValidatePassword(req.New); err != nil {
		writeErr(w, http.StatusBadRequest, err.Error())
		return
	}
	hash, err := auth.HashPassword(req.New)
	if err != nil {
		a.internal(w, err)
		return
	}
	if err := a.store.SetUserPassword(r.Context(), u.ID, hash); err != nil {
		a.internal(w, err)
		return
	}
	a.audit(r, "user.password_changed", u.Email, nil)
	writeJSON(w, http.StatusOK, map[string]bool{"ok": true})
}

// totpSetup stores a new, not-yet-enabled secret and returns it for the authenticator app.
func (a *API) totpSetup(w http.ResponseWriter, r *http.Request) {
	u := userFrom(r)
	if u.TOTPEnabled {
		writeErr(w, http.StatusConflict, "two-factor authentication is already enabled")
		return
	}
	key, err := totp.Generate(totp.GenerateOpts{Issuer: "ReDesk", AccountName: u.Email})
	if err != nil {
		a.internal(w, err)
		return
	}
	secret := key.Secret()
	if err := a.store.SetTOTP(r.Context(), u.ID, &secret, false); err != nil {
		a.internal(w, err)
		return
	}
	writeJSON(w, http.StatusOK, map[string]string{"secret": secret, "uri": key.URL()})
}

func (a *API) totpEnable(w http.ResponseWriter, r *http.Request) {
	var req struct {
		Code string `json:"code"`
	}
	if !readJSON(w, r, &req) {
		return
	}
	u := userFrom(r)
	if u.TOTPSecret == nil || !totp.Validate(req.Code, *u.TOTPSecret) {
		writeErr(w, http.StatusBadRequest, "invalid code")
		return
	}
	if err := a.store.SetTOTP(r.Context(), u.ID, u.TOTPSecret, true); err != nil {
		a.internal(w, err)
		return
	}
	a.audit(r, "user.totp_enabled", u.Email, nil)
	writeJSON(w, http.StatusOK, map[string]bool{"ok": true})
}

func (a *API) totpDisable(w http.ResponseWriter, r *http.Request) {
	var req struct {
		Password string `json:"password"`
	}
	if !readJSON(w, r, &req) {
		return
	}
	u := userFrom(r)
	if !auth.VerifyPassword(u.PasswordHash, req.Password) {
		writeErr(w, http.StatusUnauthorized, "password is incorrect")
		return
	}
	if err := a.store.SetTOTP(r.Context(), u.ID, nil, false); err != nil {
		a.internal(w, err)
		return
	}
	a.audit(r, "user.totp_disabled", u.Email, nil)
	writeJSON(w, http.StatusOK, map[string]bool{"ok": true})
}
