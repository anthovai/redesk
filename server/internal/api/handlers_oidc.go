package api

import (
	"context"
	"crypto/subtle"
	"errors"
	"fmt"
	"html/template"
	"net"
	"net/http"
	"net/mail"
	"net/url"
	"regexp"
	"strings"
	"sync"
	"time"

	"github.com/coreos/go-oidc/v3/oidc"
	"golang.org/x/oauth2"

	"aegiron/server/internal/auth"
	"aegiron/server/internal/store"
)

// Single sign-on with an OpenID Connect provider (Authorization Code + PKCE).
//
// Browser:     GET /api/oidc/login?next=/x -> IdP -> GET /api/oidc/callback -> console cookie.
// Desktop app: POST /api/oidc/auth {device} -> {code, url}; the app opens url in the browser
//              (a confirmation page, then the IdP) and polls GET /api/oidc/auth-query?code=...
//              until it receives the same payload as POST /api/agent/login.
// The poll code never appears in the browser URL; the browser only sees a separate app id.

const (
	oidcCookie     = "aegiron_oidc"
	oidcFlowTTL    = 10 * time.Minute
	oidcAppTTL     = 5 * time.Minute
	oidcMaxPending = 5000
	oidcProvTTL    = time.Hour
)

var oidcHTTP = &http.Client{Timeout: 15 * time.Second} // default TLS verification, never relaxed

type oidcFlow struct {
	binding     string // browser cookie that started the flow (login CSRF protection)
	verifier    string // PKCE
	nonce       string
	next        string
	redirectURI string
	appID       string // desktop app sign-in, else ""
	exp         time.Time
}

type appAuth struct {
	pollCode string
	device   string
	ip       string
	confirm  string // token shown only on the confirmation page
	binding  string // browser that saw the confirmation page
	userID   int64  // set when the browser finished
	failed   string // set when the browser flow failed
	exp      time.Time
}

type oidcState struct {
	mu    sync.Mutex
	flows map[string]*oidcFlow // by state
	apps  map[string]*appAuth  // by app id (in the browser URL)
	polls map[string]string    // poll code -> app id

	prov       *oidc.Provider
	provIssuer string
	provAt     time.Time
}

func newOIDCState() *oidcState {
	return &oidcState{flows: map[string]*oidcFlow{}, apps: map[string]*appAuth{}, polls: map[string]string{}}
}

// purge drops expired entries; callers hold mu.
func (o *oidcState) purge() {
	now := time.Now()
	for k, f := range o.flows {
		if now.After(f.exp) {
			delete(o.flows, k)
		}
	}
	for k, a := range o.apps {
		if now.After(a.exp) {
			delete(o.apps, k)
			delete(o.polls, a.pollCode)
		}
	}
}

func (o *oidcState) putFlow(state string, f *oidcFlow) bool {
	o.mu.Lock()
	defer o.mu.Unlock()
	o.purge()
	if len(o.flows) >= oidcMaxPending {
		return false
	}
	o.flows[state] = f
	return true
}

// takeFlow returns a flow exactly once.
func (o *oidcState) takeFlow(state string) *oidcFlow {
	o.mu.Lock()
	defer o.mu.Unlock()
	f, ok := o.flows[state]
	delete(o.flows, state)
	if !ok || time.Now().After(f.exp) {
		return nil
	}
	return f
}

func (o *oidcState) newApp(device, ip string) (pollCode, appID string, ok bool) {
	o.mu.Lock()
	defer o.mu.Unlock()
	o.purge()
	if len(o.apps) >= oidcMaxPending {
		return "", "", false
	}
	pollCode, appID = auth.RandomToken(32), auth.RandomToken(16)
	o.apps[appID] = &appAuth{pollCode: pollCode, device: device, ip: ip, exp: time.Now().Add(oidcAppTTL)}
	o.polls[pollCode] = appID
	return pollCode, appID, true
}

// withApp runs fn on a live app entry under the lock.
func (o *oidcState) withApp(appID string, fn func(*appAuth)) bool {
	o.mu.Lock()
	defer o.mu.Unlock()
	a, ok := o.apps[appID]
	if !ok || time.Now().After(a.exp) {
		return false
	}
	fn(a)
	return true
}

type appPoll int

const (
	appUnknown appPoll = iota
	appPending
	appFailed
	appDone
)

// poll reports the state of a desktop sign-in; a finished or failed entry is consumed.
func (o *oidcState) poll(code string) (appPoll, *appAuth) {
	o.mu.Lock()
	defer o.mu.Unlock()
	id, ok := o.polls[code]
	if !ok {
		return appUnknown, nil
	}
	a := o.apps[id]
	if a == nil || time.Now().After(a.exp) {
		delete(o.polls, code)
		delete(o.apps, id)
		return appUnknown, nil
	}
	switch {
	case a.failed != "":
	case a.userID != 0:
	default:
		return appPending, a
	}
	delete(o.polls, code)
	delete(o.apps, id)
	if a.failed != "" {
		return appFailed, a
	}
	return appDone, a
}

func (o *oidcState) provider(ctx context.Context, issuer string) (*oidc.Provider, error) {
	o.mu.Lock()
	if o.prov != nil && o.provIssuer == issuer && time.Since(o.provAt) < oidcProvTTL {
		p := o.prov
		o.mu.Unlock()
		return p, nil
	}
	o.mu.Unlock()
	ctx, cancel := context.WithTimeout(oidc.ClientContext(ctx, oidcHTTP), 15*time.Second)
	defer cancel()
	p, err := oidc.NewProvider(ctx, issuer)
	if err != nil {
		return nil, err
	}
	o.mu.Lock()
	o.prov, o.provIssuer, o.provAt = p, issuer, time.Now()
	o.mu.Unlock()
	return p, nil
}

func (o *oidcState) forgetProvider() {
	o.mu.Lock()
	o.prov = nil
	o.mu.Unlock()
}

// --- configuration ---

// oidcEnabled returns the settings when SSO is configured and switched on.
func (a *API) oidcEnabled(ctx context.Context) (*store.OIDCSettings, error) {
	s, err := a.store.GetOIDCSettings(ctx)
	if errors.Is(err, store.ErrNotFound) {
		return nil, nil
	} else if err != nil {
		return nil, err
	}
	if !s.Enabled || s.Issuer == "" || s.ClientID == "" {
		return nil, nil
	}
	return s, nil
}

// oauthConfig builds the client config (discovery is cached).
func (a *API) oauthConfig(ctx context.Context, s *store.OIDCSettings, redirectURI string) (*oauth2.Config, *oidc.Provider, error) {
	p, err := a.oidc.provider(ctx, s.Issuer)
	if err != nil {
		return nil, nil, fmt.Errorf("discovery: %w", err)
	}
	secret := ""
	if len(s.ClientSecret) > 0 {
		if secret, err = a.vault.Open(s.ClientSecret); err != nil {
			return nil, nil, fmt.Errorf("open client secret: %w", err)
		}
	}
	return &oauth2.Config{
		ClientID:     s.ClientID,
		ClientSecret: secret,
		Endpoint:     p.Endpoint(),
		RedirectURL:  redirectURI,
		Scopes:       []string{oidc.ScopeOpenID, "email", "profile"},
	}, p, nil
}

// publicBase is the external origin used for the redirect URI.
func (a *API) publicBase(r *http.Request) string {
	if a.cfg.PublicURL != "" {
		return a.cfg.PublicURL
	}
	scheme := "http"
	if r.TLS != nil {
		scheme = "https"
	}
	if a.cfg.TrustProxy {
		if p := strings.ToLower(strings.TrimSpace(strings.Split(r.Header.Get("X-Forwarded-Proto"), ",")[0])); p == "https" || p == "http" {
			scheme = p
		}
	}
	return scheme + "://" + r.Host
}

// safeNext keeps only same-site paths (same rule as useHandoff).
func safeNext(next string) string {
	if !strings.HasPrefix(next, "/") || strings.HasPrefix(next, "//") || strings.ContainsAny(next, "\\\r\n") {
		return "/"
	}
	return next
}

// browserBinding returns the flow-binding cookie, creating it if needed.
func (a *API) browserBinding(w http.ResponseWriter, r *http.Request) string {
	if c, err := r.Cookie(oidcCookie); err == nil && len(c.Value) >= 32 && len(c.Value) <= 64 {
		return c.Value
	}
	v := auth.RandomToken(32)
	// Lax: the IdP returns with a top-level cross-site GET.
	http.SetCookie(w, &http.Cookie{
		Name: oidcCookie, Value: v, Path: "/api/oidc/", MaxAge: int(oidcFlowTTL / time.Second),
		HttpOnly: true, Secure: a.cfg.CookieSecure, SameSite: http.SameSiteLaxMode,
	})
	return v
}

func sameToken(a, b string) bool {
	return a != "" && subtle.ConstantTimeCompare([]byte(a), []byte(b)) == 1
}

// --- browser flow ---

// oidcLogin: GET /api/oidc/login?next=/path  or  ?app=<id>[&confirm=<token>]
func (a *API) oidcLogin(w http.ResponseWriter, r *http.Request) {
	ip := a.clientIP(r)
	if !a.limiter.allow("oidc-login:"+ip, 30, 10*time.Minute) {
		oidcPage(w, http.StatusTooManyRequests, "ลองหลายครั้งเกินไป", "กรุณารอสักครู่แล้วลองใหม่")
		return
	}
	s, err := a.oidcEnabled(r.Context())
	if err != nil {
		a.internal(w, err)
		return
	}
	q := r.URL.Query()
	appID := q.Get("app")
	if s == nil {
		if appID != "" {
			oidcPage(w, http.StatusBadRequest, "ยังไม่ได้เปิดใช้ SSO", "ผู้ดูแลระบบยังไม่ได้เปิดการเข้าสู่ระบบด้วย SSO")
			return
		}
		http.Redirect(w, r, "/login?oidc_error=disabled", http.StatusSeeOther)
		return
	}
	binding := a.browserBinding(w, r)
	next := safeNext(q.Get("next"))

	if appID != "" {
		// Desktop sign-in: show which device asked first, so a link sent by someone else
		// cannot silently sign that person's app in as the victim.
		var device, appIP, confirm string
		ok := a.oidc.withApp(appID, func(x *appAuth) {
			if c := q.Get("confirm"); c != "" && sameToken(c, x.confirm) && sameToken(binding, x.binding) {
				x.confirm = "" // single use
				confirm = "ok"
				return
			}
			x.confirm, x.binding = auth.RandomToken(16), binding
			device, appIP, confirm = x.device, x.ip, x.confirm
		})
		if !ok {
			oidcPage(w, http.StatusNotFound, "ลิงก์หมดอายุ", "ลิงก์เข้าสู่ระบบนี้หมดอายุหรือถูกใช้ไปแล้ว กรุณากดเข้าสู่ระบบจากโปรแกรม Aegiron ใหม่อีกครั้ง")
			return
		}
		if confirm != "ok" {
			oidcConfirmPage(w, s.Name, device, appIP, "/api/oidc/login?app="+url.QueryEscape(appID)+"&confirm="+url.QueryEscape(confirm))
			return
		}
		next = ""
	}

	redirectURI := a.publicBase(r) + "/api/oidc/callback"
	oc, _, err := a.oauthConfig(r.Context(), s, redirectURI)
	if err != nil {
		a.log.Warn("oidc login", "err", err)
		a.oidcFail(w, r, &oidcFlow{appID: appID}, "provider_unavailable", "", map[string]any{"error": err.Error()})
		return
	}
	state, nonce, verifier := auth.RandomToken(24), auth.RandomToken(24), oauth2.GenerateVerifier()
	if !a.oidc.putFlow(state, &oidcFlow{binding: binding, verifier: verifier, nonce: nonce, next: next,
		redirectURI: redirectURI, appID: appID, exp: time.Now().Add(oidcFlowTTL)}) {
		oidcPage(w, http.StatusServiceUnavailable, "ระบบไม่ว่าง", "กรุณาลองใหม่ภายหลัง")
		return
	}
	http.Redirect(w, r, oc.AuthCodeURL(state, oidc.Nonce(nonce), oauth2.S256ChallengeOption(verifier)), http.StatusSeeOther)
}

type oidcClaims struct {
	Email             string `json:"email"`
	EmailVerified     any    `json:"email_verified"`
	Name              string `json:"name"`
	PreferredUsername string `json:"preferred_username"`
}

// oidcCallback: GET /api/oidc/callback?code=&state=
func (a *API) oidcCallback(w http.ResponseWriter, r *http.Request) {
	q := r.URL.Query()
	f := a.oidc.takeFlow(q.Get("state"))
	if f == nil {
		a.oidcFail(w, r, &oidcFlow{}, "bad_state", "", nil)
		return
	}
	c, err := r.Cookie(oidcCookie)
	if err != nil || !sameToken(c.Value, f.binding) {
		a.oidcFail(w, r, f, "bad_state", "", map[string]any{"why": "browser mismatch"})
		return
	}
	if e := q.Get("error"); e != "" {
		a.oidcFail(w, r, f, "idp_error", "", map[string]any{"error": trim(e, 100), "description": trim(q.Get("error_description"), 300)})
		return
	}
	s, err := a.oidcEnabled(r.Context())
	if err != nil {
		a.internal(w, err)
		return
	}
	if s == nil {
		a.oidcFail(w, r, f, "disabled", "", nil)
		return
	}
	oc, p, err := a.oauthConfig(r.Context(), s, f.redirectURI)
	if err != nil {
		a.oidcFail(w, r, f, "provider_unavailable", "", map[string]any{"error": err.Error()})
		return
	}
	ctx, cancel := context.WithTimeout(context.WithValue(r.Context(), oauth2.HTTPClient, oidcHTTP), 20*time.Second)
	defer cancel()
	tok, err := oc.Exchange(ctx, q.Get("code"), oauth2.VerifierOption(f.verifier))
	if err != nil {
		a.oidcFail(w, r, f, "exchange_failed", "", map[string]any{"error": trim(err.Error(), 300)})
		return
	}
	raw, _ := tok.Extra("id_token").(string)
	if raw == "" {
		a.oidcFail(w, r, f, "bad_token", "", map[string]any{"error": "no id_token"})
		return
	}
	// Checks signature, issuer, audience and expiry.
	idt, err := p.Verifier(&oidc.Config{ClientID: s.ClientID}).Verify(ctx, raw)
	if err != nil {
		a.oidcFail(w, r, f, "bad_token", "", map[string]any{"error": trim(err.Error(), 300)})
		return
	}
	if !sameToken(idt.Nonce, f.nonce) {
		a.oidcFail(w, r, f, "bad_token", "", map[string]any{"error": "nonce mismatch"})
		return
	}
	var cl oidcClaims
	if err := idt.Claims(&cl); err != nil {
		a.oidcFail(w, r, f, "bad_token", "", map[string]any{"error": "claims"})
		return
	}
	u, created, reason, email := a.oidcResolveUser(r.Context(), s, &cl)
	if reason != "" {
		a.oidcFail(w, r, f, reason, email, map[string]any{"subject": trim(idt.Subject, 100)})
		return
	}
	ip := a.clientIP(r)
	detail := map[string]any{"subject": trim(idt.Subject, 100), "created": created}
	if created {
		_ = a.store.AddAudit(r.Context(), &u.ID, ip, "user.created", u.Email, map[string]any{"role": u.Role, "via": "oidc"})
	}

	if f.appID != "" {
		var device string
		if !a.oidc.withApp(f.appID, func(x *appAuth) { x.userID, device = u.ID, x.device }) {
			a.oidcFail(w, r, f, "expired", u.Email, nil)
			return
		}
		detail["app"], detail["device"] = true, device
		_ = a.store.AddAudit(r.Context(), &u.ID, ip, "auth.oidc_login", u.Email, detail)
		oidcPage(w, http.StatusOK, "เข้าสู่ระบบสำเร็จ", "กลับไปที่โปรแกรม Aegiron ได้เลย หน้านี้ปิดได้")
		return
	}

	st, err := a.store.GetSettings(r.Context())
	if err != nil {
		a.internal(w, err)
		return
	}
	sess := auth.RandomToken(32)
	exp := time.Now().Add(time.Duration(st.SessionHours) * time.Hour)
	if err := a.store.CreateAuthSession(r.Context(), auth.HashToken(sess), u.ID, ip, r.UserAgent(), exp); err != nil {
		a.internal(w, err)
		return
	}
	_ = a.store.TouchLogin(r.Context(), u.ID)
	_ = a.store.AddAudit(r.Context(), &u.ID, ip, "auth.oidc_login", u.Email, detail)
	http.SetCookie(w, &http.Cookie{
		Name: cookieName, Value: sess, Path: "/", Expires: exp,
		HttpOnly: true, Secure: a.cfg.CookieSecure, SameSite: http.SameSiteStrictMode,
	})
	http.Redirect(w, r, f.next, http.StatusSeeOther)
}

// oidcResolveUser maps verified claims to a Aegiron account. reason != "" means rejected.
func (a *API) oidcResolveUser(ctx context.Context, s *store.OIDCSettings, cl *oidcClaims) (u *store.User, created bool, reason, email string) {
	email = cl.Email
	if email == "" && strings.Contains(cl.PreferredUsername, "@") {
		email = cl.PreferredUsername // Microsoft Entra ID: UPN when no mail attribute
	}
	email = store.NormalizeEmail(email)
	if addr, err := mail.ParseAddress(email); err != nil || addr.Address != email || len(email) > 254 {
		return nil, false, "no_email", email
	}
	if cl.EmailVerified != nil {
		if v, ok := cl.EmailVerified.(bool); !(ok && v) && cl.EmailVerified != "true" {
			return nil, false, "email_unverified", email
		}
	}
	if !domainAllowed(email, s.AllowedDomains) {
		return nil, false, "domain_not_allowed", email
	}
	u, err := a.store.GetUserByEmail(ctx, email)
	if errors.Is(err, store.ErrNotFound) {
		if !s.AutoCreate {
			return nil, false, "no_account", email
		}
		// Unusable random password: the account signs in through SSO until an admin sets one.
		hash, err := auth.HashPassword(auth.RandomToken(32))
		if err != nil {
			return nil, false, "internal", email
		}
		if u, err = a.store.CreateUser(ctx, email, trim(cl.Name, 100), "user", hash); err != nil {
			if u, err = a.store.GetUserByEmail(ctx, email); err != nil { // lost a race
				return nil, false, "internal", email
			}
		} else {
			created = true
		}
	} else if err != nil {
		return nil, false, "internal", email
	}
	if u.Disabled {
		return nil, false, "disabled_account", email
	}
	return u, created, "", email
}

func domainAllowed(email string, domains []string) bool {
	if len(domains) == 0 {
		return true
	}
	_, d, _ := strings.Cut(email, "@")
	for _, x := range domains {
		if d == x {
			return true
		}
	}
	return false
}

var oidcReasonText = map[string]string{
	"bad_state":            "คำขอเข้าสู่ระบบไม่ถูกต้องหรือหมดอายุ กรุณาลองใหม่",
	"idp_error":            "ผู้ให้บริการ SSO ปฏิเสธการเข้าสู่ระบบ",
	"disabled":             "ยังไม่ได้เปิดการเข้าสู่ระบบด้วย SSO",
	"provider_unavailable": "ติดต่อผู้ให้บริการ SSO ไม่ได้",
	"exchange_failed":      "ยืนยันตัวตนกับผู้ให้บริการ SSO ไม่สำเร็จ",
	"bad_token":            "ข้อมูลยืนยันตัวตนจากผู้ให้บริการ SSO ไม่ถูกต้อง",
	"no_email":             "บัญชี SSO ไม่มีอีเมล",
	"email_unverified":     "อีเมลของบัญชี SSO ยังไม่ได้รับการยืนยัน",
	"domain_not_allowed":   "โดเมนอีเมลนี้ไม่ได้รับอนุญาต",
	"no_account":           "ยังไม่มีบัญชี Aegiron สำหรับอีเมลนี้ กรุณาติดต่อผู้ดูแลระบบ",
	"disabled_account":     "บัญชีนี้ถูกปิดใช้งาน",
	"expired":              "คำขอเข้าสู่ระบบจากโปรแกรมหมดอายุ กรุณาลองใหม่",
	"internal":             "เกิดข้อผิดพลาดภายในเซิร์ฟเวอร์",
}

// oidcFail audits, tells a waiting desktop app, and answers the browser.
func (a *API) oidcFail(w http.ResponseWriter, r *http.Request, f *oidcFlow, reason, email string, detail map[string]any) {
	if detail == nil {
		detail = map[string]any{}
	}
	detail["reason"] = reason
	if f.appID != "" {
		detail["app"] = true
		a.oidc.withApp(f.appID, func(x *appAuth) { x.failed = reason })
	}
	_ = a.store.AddAudit(r.Context(), nil, a.clientIP(r), "auth.oidc_failed", email, detail)
	if f.appID != "" {
		oidcPage(w, http.StatusForbidden, "เข้าสู่ระบบไม่สำเร็จ", oidcReasonText[reason])
		return
	}
	http.Redirect(w, r, "/login?oidc_error="+url.QueryEscape(reason), http.StatusSeeOther)
}

var oidcTmpl = template.Must(template.New("p").Parse(`<!doctype html><html lang="th"><head><meta charset="utf-8">
<meta name="viewport" content="width=device-width,initial-scale=1"><title>{{.Title}} · Aegiron</title>
<style>body{font-family:system-ui,sans-serif;background:#f6f7f9;color:#1b1f24;display:grid;place-items:center;min-height:100vh;margin:0;padding:16px;box-sizing:border-box}
.c{background:#fff;border:1px solid #e2e5e9;border-radius:12px;padding:24px;max-width:420px;width:100%}h1{font-size:1.15rem;margin:0 0 8px}
p{color:#4b5563;line-height:1.5}dl{display:grid;grid-template-columns:auto 1fr;gap:4px 12px;font-size:.9rem}dt{color:#6b7280}dd{margin:0;word-break:break-all}
a.b{display:inline-block;margin-top:12px;background:#2563eb;color:#fff;padding:8px 16px;border-radius:8px;text-decoration:none}
@media (prefers-color-scheme:dark){body{background:#0f1115;color:#e5e7eb}.c{background:#181b21;border-color:#2a2f37}p{color:#9ca3af}}</style>
</head><body><div class="c"><h1>{{.Title}}</h1><p>{{.Msg}}</p>
{{if .Link}}<dl><dt>เครื่อง</dt><dd>{{.Device}}</dd><dt>IP</dt><dd>{{.IP}}</dd></dl>
<p>ถ้าคุณไม่ได้กดเข้าสู่ระบบจากโปรแกรม Aegiron บนเครื่องนี้เอง ให้ปิดหน้านี้ทันที</p>
<a class="b" href="{{.Link}}">ดำเนินการต่อด้วย {{.Name}}</a>{{end}}</div></body></html>`))

func oidcPage(w http.ResponseWriter, status int, title, msg string) {
	w.Header().Set("Content-Type", "text/html; charset=utf-8")
	w.Header().Set("Cache-Control", "no-store")
	w.WriteHeader(status)
	_ = oidcTmpl.Execute(w, map[string]string{"Title": title, "Msg": msg})
}

func oidcConfirmPage(w http.ResponseWriter, name, device, ip, link string) {
	w.Header().Set("Content-Type", "text/html; charset=utf-8")
	w.Header().Set("Cache-Control", "no-store")
	_ = oidcTmpl.Execute(w, map[string]string{
		"Title": "เข้าสู่ระบบโปรแกรม Aegiron", "Msg": "มีคำขอเข้าสู่ระบบโปรแกรม Aegiron จากเครื่องต่อไปนี้",
		"Device": device, "IP": ip, "Link": link, "Name": name,
	})
}

// --- desktop app flow ---

// oidcAppStart: POST /api/oidc/auth {device} -> {code, url, expires_in}
func (a *API) oidcAppStart(w http.ResponseWriter, r *http.Request) {
	var req struct {
		Device string `json:"device"`
	}
	if !readJSON(w, r, &req) {
		return
	}
	ip := a.clientIP(r)
	if !a.limiter.allow("oidc-app:"+ip, 10, 10*time.Minute) {
		writeErr(w, http.StatusTooManyRequests, "too many attempts, try again later")
		return
	}
	s, err := a.oidcEnabled(r.Context())
	if err != nil {
		a.internal(w, err)
		return
	}
	if s == nil {
		writeErr(w, http.StatusBadRequest, "single sign-on is not enabled")
		return
	}
	code, appID, ok := a.oidc.newApp(trim(req.Device, 80), ip)
	if !ok {
		writeErr(w, http.StatusServiceUnavailable, "too many pending sign-ins")
		return
	}
	writeJSON(w, http.StatusOK, map[string]any{
		"code": code, "url": "/api/oidc/login?app=" + appID, "expires_in": int(oidcAppTTL / time.Second),
	})
}

// oidcAppQuery: GET /api/oidc/auth-query?code= -> {pending:true} | /api/agent/login payload
func (a *API) oidcAppQuery(w http.ResponseWriter, r *http.Request) {
	if !a.limiter.allow("oidc-poll:"+a.clientIP(r), 120, time.Minute) {
		writeErr(w, http.StatusTooManyRequests, "too many requests")
		return
	}
	st, app := a.oidc.poll(r.URL.Query().Get("code"))
	switch st {
	case appUnknown:
		writeErr(w, http.StatusNotFound, "unknown or expired code")
	case appPending:
		writeJSON(w, http.StatusOK, map[string]bool{"pending": true})
	case appFailed:
		writeErr(w, http.StatusForbidden, "sign-in failed: "+app.failed)
	case appDone:
		u, err := a.store.GetUser(r.Context(), app.userID)
		if err != nil || u.Disabled {
			writeErr(w, http.StatusForbidden, "sign-in failed: disabled_account")
			return
		}
		a.issueAppSession(w, r, u, app.device, map[string]any{"method": "oidc"})
	}
}

// --- admin settings ---

func (a *API) getOIDCSettings(w http.ResponseWriter, r *http.Request) {
	s, err := a.store.GetOIDCSettings(r.Context())
	if errors.Is(err, store.ErrNotFound) {
		s, err = &store.OIDCSettings{AllowedDomains: []string{}}, nil
	}
	if err != nil {
		a.internal(w, err)
		return
	}
	writeJSON(w, http.StatusOK, map[string]any{"settings": s, "redirect_uri": a.publicBase(r) + "/api/oidc/callback"})
}

var domainRe = regexp.MustCompile(`^[a-z0-9]([a-z0-9-]*[a-z0-9])?(\.[a-z0-9]([a-z0-9-]*[a-z0-9])?)+$`)

// validIssuer requires https (plain http only for loopback, e.g. a local test IdP).
func validIssuer(s string) error {
	u, err := url.Parse(s)
	if err != nil || u.Host == "" || u.User != nil || u.RawQuery != "" || u.Fragment != "" || len(s) > 512 {
		return errors.New("invalid issuer URL")
	}
	if u.Scheme == "https" {
		return nil
	}
	if ip := net.ParseIP(u.Hostname()); u.Scheme == "http" && (u.Hostname() == "localhost" || (ip != nil && ip.IsLoopback())) {
		return nil
	}
	return errors.New("issuer URL must use https://")
}

func (a *API) putOIDCSettings(w http.ResponseWriter, r *http.Request) {
	var req struct {
		Name           string   `json:"name"`
		Issuer         string   `json:"issuer"`
		ClientID       string   `json:"client_id"`
		ClientSecret   *string  `json:"client_secret"` // nil = keep
		AllowedDomains []string `json:"allowed_domains"`
		AutoCreate     bool     `json:"auto_create"`
		Enabled        bool     `json:"enabled"`
	}
	if !readJSON(w, r, &req) {
		return
	}
	s := &store.OIDCSettings{
		Name:       trim(req.Name, 64),
		Issuer:     strings.TrimRight(strings.TrimSpace(req.Issuer), "/"),
		ClientID:   trim(req.ClientID, 256),
		AutoCreate: req.AutoCreate,
		Enabled:    req.Enabled,
	}
	if s.Name == "" {
		s.Name = "SSO"
	}
	if strings.ContainsAny(s.Name+s.ClientID, "\r\n") {
		writeErr(w, http.StatusBadRequest, "invalid characters")
		return
	}
	if s.Issuer != "" {
		if err := validIssuer(s.Issuer); err != nil {
			writeErr(w, http.StatusBadRequest, err.Error())
			return
		}
	}
	seen := map[string]bool{}
	for _, d := range req.AllowedDomains {
		d = strings.TrimPrefix(strings.ToLower(strings.TrimSpace(d)), "@")
		if d == "" || seen[d] {
			continue
		}
		if !domainRe.MatchString(d) || len(d) > 253 {
			writeErr(w, http.StatusBadRequest, "invalid domain: "+d)
			return
		}
		seen[d] = true
		s.AllowedDomains = append(s.AllowedDomains, d)
	}
	if len(s.AllowedDomains) > 50 {
		writeErr(w, http.StatusBadRequest, "too many domains")
		return
	}
	var sealed []byte
	if req.ClientSecret != nil {
		sec := strings.TrimSpace(*req.ClientSecret)
		if len(sec) > 1024 {
			writeErr(w, http.StatusBadRequest, "client secret too long")
			return
		}
		sealed = []byte{} // empty = explicit clear (public client, PKCE only)
		if sec != "" {
			var err error
			if sealed, err = a.vault.Seal(sec); err != nil {
				a.internal(w, err)
				return
			}
		}
	}
	if s.Enabled {
		if s.Issuer == "" || s.ClientID == "" {
			writeErr(w, http.StatusBadRequest, "issuer and client ID are required")
			return
		}
		a.oidc.forgetProvider()
		if _, err := a.oidc.provider(r.Context(), s.Issuer); err != nil {
			writeErr(w, http.StatusBadRequest, "OIDC discovery failed: "+trim(err.Error(), 300))
			return
		}
	}
	if err := a.store.SaveOIDCSettings(r.Context(), s, sealed); err != nil {
		a.internal(w, err)
		return
	}
	a.oidc.forgetProvider()
	a.audit(r, "oidc.settings_saved", s.Issuer, map[string]any{
		"enabled": s.Enabled, "name": s.Name, "client_id": s.ClientID, "allowed_domains": s.AllowedDomains,
		"auto_create": s.AutoCreate, "secret_changed": req.ClientSecret != nil,
	})
	writeJSON(w, http.StatusOK, map[string]bool{"ok": true})
}
