package api

import (
	"net/http"
	"net/url"
	"strings"
	"sync"
	"time"

	"github.com/pquerna/otp/totp"

	"redesk/server/internal/auth"
	"redesk/server/internal/store"
)

const agentSessionLifetime = 30 * 24 * time.Hour

// checkCredentials verifies email, password and (if enabled) the TOTP code, writing the
// error response itself. A missing TOTP code answers {"need_totp": true}.
func (a *API) checkCredentials(w http.ResponseWriter, r *http.Request, email, password, code string) (*store.User, bool) {
	ip := a.clientIP(r)
	email = store.NormalizeEmail(email)
	if !a.limiter.allow("ip:"+ip, 20, 10*time.Minute) || !a.limiter.allow("email:"+email, 10, 10*time.Minute) {
		writeErr(w, http.StatusTooManyRequests, "too many attempts, try again later")
		return nil, false
	}
	u, err := a.store.GetUserByEmail(r.Context(), email)
	if err != nil {
		auth.VerifyDummy(password)
		_ = a.store.AddAudit(r.Context(), nil, ip, "auth.login_failed", email, map[string]any{"reason": "bad_credentials"})
		writeErr(w, http.StatusUnauthorized, "invalid email or password")
		return nil, false
	}
	if !auth.VerifyPassword(u.PasswordHash, password) || u.Disabled {
		_ = a.store.AddAudit(r.Context(), &u.ID, ip, "auth.login_failed", email, map[string]any{"reason": "bad_credentials"})
		writeErr(w, http.StatusUnauthorized, "invalid email or password")
		return nil, false
	}
	if u.TOTPEnabled {
		if code == "" {
			writeJSON(w, http.StatusOK, map[string]any{"need_totp": true})
			return nil, false
		}
		if u.TOTPSecret == nil || !totp.Validate(code, *u.TOTPSecret) {
			_ = a.store.AddAudit(r.Context(), &u.ID, ip, "auth.login_failed", email, map[string]any{"reason": "bad_totp"})
			writeErr(w, http.StatusUnauthorized, "invalid authentication code")
			return nil, false
		}
	}
	return u, true
}

// agentLogin: POST /api/agent/login — sign-in from the desktop app.
// Returns a bearer token for the app, the server's identity key (trusted because this
// request arrived over the server's TLS) and a one-time enrollment token that makes the
// new device belong to this user.
func (a *API) agentLogin(w http.ResponseWriter, r *http.Request) {
	var req struct {
		Email    string `json:"email"`
		Password string `json:"password"`
		TOTP     string `json:"totp"`
		Device   string `json:"device"`
	}
	if !readJSON(w, r, &req) {
		return
	}
	u, ok := a.checkCredentials(w, r, req.Email, req.Password, req.TOTP)
	if !ok {
		return
	}
	a.issueAppSession(w, r, u, req.Device, nil)
}

// issueAppSession answers a successful desktop-app sign-in (password or OIDC) with
// {user, token, server_key, enroll_token}. extra is merged into the audit detail.
func (a *API) issueAppSession(w http.ResponseWriter, r *http.Request, u *store.User, device string, extra map[string]any) {
	ip := a.clientIP(r)
	device = trim(device, 80)
	tok := auth.RandomToken(32)
	if err := a.store.CreateAuthSession(r.Context(), auth.HashToken(tok), u.ID, ip, "ReDesk app: "+device,
		time.Now().Add(agentSessionLifetime)); err != nil {
		a.internal(w, err)
		return
	}
	enroll := "rdk_" + auth.RandomToken(24)
	exp := time.Now().Add(time.Hour)
	if _, err := a.store.CreateOwnedEnrollToken(r.Context(), auth.HashToken(enroll), "เข้าสู่ระบบจากโปรแกรม: "+device, u.ID, &exp); err != nil {
		a.internal(w, err)
		return
	}
	_ = a.store.TouchLogin(r.Context(), u.ID)
	detail := map[string]any{"device": device}
	for k, v := range extra {
		detail[k] = v
	}
	_ = a.store.AddAudit(r.Context(), &u.ID, ip, "auth.agent_login", u.Email, detail)
	writeJSON(w, http.StatusOK, map[string]any{
		"user":         u,
		"token":        tok,
		"server_key":   auth.PublicKeyB64(a.key),
		"enroll_token": enroll,
	})
}

// --- handoff: open the web console from the app without signing in again ---

type handoff struct {
	userID int64
	exp    time.Time
}

type handoffs struct {
	mu sync.Mutex
	m  map[string]handoff
}

func newHandoffs() *handoffs { return &handoffs{m: map[string]handoff{}} }

func (h *handoffs) put(userID int64) string {
	code := auth.RandomToken(24)
	h.mu.Lock()
	defer h.mu.Unlock()
	now := time.Now()
	for k, v := range h.m {
		if now.After(v.exp) {
			delete(h.m, k)
		}
	}
	h.m[code] = handoff{userID: userID, exp: now.Add(60 * time.Second)}
	return code
}

// take returns the user for a code exactly once.
func (h *handoffs) take(code string) (int64, bool) {
	h.mu.Lock()
	defer h.mu.Unlock()
	v, ok := h.m[code]
	delete(h.m, code)
	if !ok || time.Now().After(v.exp) {
		return 0, false
	}
	return v.userID, true
}

// createHandoff: POST /api/auth/handoff (app bearer token) -> single-use code, valid 60 seconds.
func (a *API) createHandoff(w http.ResponseWriter, r *http.Request) {
	writeJSON(w, http.StatusOK, map[string]string{"code": a.handoffs.put(userFrom(r).ID)})
}

// useHandoff: GET /api/auth/handoff/{code}?next=/connect/123 — signs the browser in and redirects.
func (a *API) useHandoff(w http.ResponseWriter, r *http.Request) {
	next := r.URL.Query().Get("next")
	// Only same-site paths: no scheme, no protocol-relative "//host", no backslashes.
	if !strings.HasPrefix(next, "/") || strings.HasPrefix(next, "//") || strings.ContainsAny(next, "\\\r\n") {
		next = "/"
	}
	uid, ok := a.handoffs.take(r.PathValue("code"))
	if !ok {
		http.Redirect(w, r, "/login?next="+url.QueryEscape(next), http.StatusSeeOther)
		return
	}
	u, err := a.store.GetUser(r.Context(), uid)
	if err != nil || u.Disabled {
		http.Redirect(w, r, "/login", http.StatusSeeOther)
		return
	}
	st, err := a.store.GetSettings(r.Context())
	if err != nil {
		a.internal(w, err)
		return
	}
	tok := auth.RandomToken(32)
	exp := time.Now().Add(time.Duration(st.SessionHours) * time.Hour)
	ip := a.clientIP(r)
	if err := a.store.CreateAuthSession(r.Context(), auth.HashToken(tok), u.ID, ip, r.UserAgent(), exp); err != nil {
		a.internal(w, err)
		return
	}
	_ = a.store.AddAudit(r.Context(), &u.ID, ip, "auth.handoff", u.Email, map[string]any{"next": next})
	http.SetCookie(w, &http.Cookie{
		Name: cookieName, Value: tok, Path: "/", Expires: exp,
		HttpOnly: true, Secure: a.cfg.CookieSecure, SameSite: http.SameSiteLaxMode,
	})
	http.Redirect(w, r, next, http.StatusSeeOther)
}
