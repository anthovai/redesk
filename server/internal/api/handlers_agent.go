package api

import (
	"errors"
	"net/http"
	"net/url"
	"strings"
	"sync"
	"time"

	"github.com/pquerna/otp/totp"

	"aegiron/server/internal/auth"
	"aegiron/server/internal/store"
)

const agentSessionLifetime = 30 * 24 * time.Hour

// credentials is what both sign-in endpoints accept besides their own fields.
type credentials struct {
	Email    string
	Password string
	TOTP     string
	// TrustToken is a "remember this device" token from an earlier sign-in (web: cookie,
	// app: request body). It replaces the TOTP code only; the password is always checked.
	TrustToken string
}

// loginResult says how the second factor was satisfied.
type loginResult struct {
	user *store.User
	// trusted is set when a remembered device skipped the TOTP step.
	trusted *store.TrustedDevice
	// totpVerified is true when a correct TOTP code was entered in this request, which is
	// the only time a new device may be remembered.
	totpVerified bool
}

// checkCredentials verifies the sign-in name and password (local account, or the LDAP
// directory when enabled), then the second factor: a TOTP code, a code sent by email, or a
// trusted device token. It writes the error response itself. A missing code answers
// {"need_totp": true} (with "method": "email" when the code was mailed; plus
// "trust_rejected": true when a token was offered but is no longer valid, so the client can
// forget it). Older clients that only know need_totp/totp work with emailed codes unchanged.
func (a *API) checkCredentials(w http.ResponseWriter, r *http.Request, c credentials) (*loginResult, bool) {
	ip := a.clientIP(r)
	email := store.NormalizeEmail(c.Email) // an email, or a directory user name with LDAP
	if !a.limiter.allow("ip:"+ip, 20, 10*time.Minute) {
		writeErr(w, http.StatusTooManyRequests, "too many attempts, try again later")
		return nil, false
	}
	if !a.limiter.allow("email:"+email, 10, 10*time.Minute) {
		a.lockedOut(r.Context(), email, ip)
		writeErr(w, http.StatusTooManyRequests, "too many attempts, try again later")
		return nil, false
	}
	fail := func(uid *int64, reason string) (*loginResult, bool) {
		_ = a.store.AddAudit(r.Context(), uid, ip, "auth.login_failed", email, map[string]any{"reason": reason})
		if reason == "ldap_unavailable" {
			writeErr(w, http.StatusServiceUnavailable, "the directory server is unavailable, try again later")
		} else {
			writeErr(w, http.StatusUnauthorized, "invalid email or password")
		}
		return nil, false
	}
	u, err := a.store.GetUserByEmail(r.Context(), email)
	if err != nil && !errors.Is(err, store.ErrNotFound) {
		a.internal(w, err)
		return nil, false
	}
	if u == nil || u.AuthSource == "ldap" {
		l, err := a.ldapEnabled(r.Context())
		if err != nil {
			a.internal(w, err)
			return nil, false
		}
		if l == nil {
			auth.VerifyDummy(c.Password)
			if u != nil {
				return fail(&u.ID, "ldap_disabled")
			}
			return fail(nil, "bad_credentials")
		}
		lu, created, reason, err := a.ldapLogin(r.Context(), l, c.Email, c.Password)
		if err != nil {
			a.internal(w, err)
			return nil, false
		}
		if reason != "" {
			var uid *int64
			if u != nil {
				uid = &u.ID
			}
			return fail(uid, reason)
		}
		if created {
			_ = a.store.AddAudit(r.Context(), &lu.ID, ip, "user.created", lu.Email, map[string]any{"role": lu.Role, "via": "ldap"})
		}
		u = lu
		if u.Disabled {
			return fail(&u.ID, "bad_credentials")
		}
	} else if !auth.VerifyPassword(u.PasswordHash, c.Password) || u.Disabled {
		return fail(&u.ID, "bad_credentials")
	}
	email = u.Email
	res := &loginResult{user: u}
	byEmail := !u.TOTPEnabled && u.EmailOTP
	if !u.TOTPEnabled && !byEmail {
		return res, true
	}
	needCode := func(extra map[string]any) (*loginResult, bool) {
		out := map[string]any{"need_totp": true}
		if byEmail {
			switch err := a.issueEmailCode(r.Context(), u, ip); {
			case errors.Is(err, errMailUnavailable):
				writeCodeErr(w, http.StatusServiceUnavailable, "mail_unavailable", "cannot send the sign-in code by email; ask an administrator")
				return nil, false
			case err != nil:
				a.internal(w, err)
				return nil, false
			}
			out["method"] = "email"
		}
		for k, v := range extra {
			out[k] = v
		}
		writeJSON(w, http.StatusOK, out)
		return nil, false
	}
	if c.TOTP == "" {
		if c.TrustToken != "" && len(c.TrustToken) <= 128 {
			d, err := a.store.UseTrustedDevice(r.Context(), auth.HashToken(c.TrustToken), u.ID, ip)
			if err == nil {
				res.trusted = d
				return res, true
			}
			if !errors.Is(err, store.ErrNotFound) {
				a.internal(w, err)
				return nil, false
			}
			_ = a.store.AddAudit(r.Context(), &u.ID, ip, "auth.trusted_device_rejected", email, nil)
			// Expired or revoked: the browser should stop sending it (the app ignores cookies
			// and forgets its copy on trust_rejected).
			http.SetCookie(w, a.trustCookieFor("", time.Now().Add(-time.Hour)))
			return needCode(map[string]any{"trust_rejected": true})
		}
		return needCode(nil)
	}
	if byEmail {
		ok, err := a.checkEmailCode(r.Context(), u, c.TOTP)
		if err != nil {
			a.internal(w, err)
			return nil, false
		}
		if !ok {
			_ = a.store.AddAudit(r.Context(), &u.ID, ip, "auth.login_failed", email, map[string]any{"reason": "bad_email_code"})
			writeErr(w, http.StatusUnauthorized, "invalid authentication code")
			return nil, false
		}
	} else if u.TOTPSecret == nil || !totp.Validate(c.TOTP, *u.TOTPSecret) {
		_ = a.store.AddAudit(r.Context(), &u.ID, ip, "auth.login_failed", email, map[string]any{"reason": "bad_totp"})
		writeErr(w, http.StatusUnauthorized, "invalid authentication code")
		return nil, false
	}
	res.totpVerified = true
	return res, true
}

// agentLogin: POST /api/agent/login — sign-in from the desktop app.
// Returns a bearer token for the app, the server's identity key (trusted because this
// request arrived over the server's TLS) and a one-time enrollment token that makes the
// new device belong to this user. With remember_device after a correct TOTP code it also
// returns trust_token, which the app sends back as trust_token instead of a code next time.
func (a *API) agentLogin(w http.ResponseWriter, r *http.Request) {
	var req struct {
		Email          string `json:"email"`
		Password       string `json:"password"`
		TOTP           string `json:"totp"`
		Device         string `json:"device"`
		TrustToken     string `json:"trust_token"`
		RememberDevice bool   `json:"remember_device"`
	}
	if !readJSON(w, r, &req) {
		return
	}
	res, ok := a.checkCredentials(w, r, credentials{Email: req.Email, Password: req.Password, TOTP: req.TOTP, TrustToken: req.TrustToken})
	if !ok {
		return
	}
	device := trim(req.Device, 80)
	audit := map[string]any{}
	reply := map[string]any{}
	if res.trusted != nil {
		audit["trusted_device"] = res.trusted.ID
	} else if req.RememberDevice && res.totpVerified {
		tok, d, err := a.rememberDevice(r, res.user, "แอป Aegiron: "+device, "Aegiron app: "+device)
		if err != nil {
			a.internal(w, err)
			return
		}
		audit["trusted_device_added"] = d.ID
		reply["trust_token"] = tok
	}
	a.issueAppSession(w, r, res.user, device, audit, reply)
}

// issueAppSession answers a successful desktop-app sign-in (password or OIDC) with
// {user, token, server_key, enroll_token}. extra is merged into the audit detail.
// reply is merged into the JSON answer.
func (a *API) issueAppSession(w http.ResponseWriter, r *http.Request, u *store.User, device string, extra, reply map[string]any) {
	ip := a.clientIP(r)
	device = trim(device, 80)
	tok := auth.RandomToken(32)
	if err := a.store.CreateAuthSession(r.Context(), auth.HashToken(tok), u.ID, ip, "Aegiron app: "+device,
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
	out := map[string]any{
		"user":         u,
		"token":        tok,
		"server_key":   auth.PublicKeyB64(a.key),
		"enroll_token": enroll,
	}
	for k, v := range reply {
		out[k] = v
	}
	writeJSON(w, http.StatusOK, out)
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
