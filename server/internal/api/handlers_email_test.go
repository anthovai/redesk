package api

import (
	"context"
	"net/http"
	"net/http/cookiejar"
	"regexp"
	"strings"
	"testing"

	"aegiron/server/internal/mail/mailtest"
)

// configureSMTP points the server at a mailtest server through the settings API.
func (e *testEnv) configureSMTP(adm string, srv *mailtest.Server, extra map[string]any) {
	e.t.Helper()
	body := map[string]any{
		"host": "127.0.0.1", "port": srv.Port(), "security": "starttls", "ca_cert": srv.CAPEM,
		"username": "aegiron", "password": "smtp-secret", "from": "aegiron@mail.test", "enabled": true,
	}
	for k, v := range extra {
		body[k] = v
	}
	if st, r := e.post(nil, "PUT", "/api/smtp/settings", adm, body); st != 200 {
		e.t.Fatalf("smtp settings: %d %v", st, r)
	}
}

var sixDigits = regexp.MustCompile(`\b[0-9]{6}\b`)

func TestSMTPSettings(t *testing.T) {
	e := newTestEnv(t)
	adm := e.admin()
	srv := mailtest.New(t, true)
	if st, _ := e.post(nil, "PUT", "/api/smtp/settings", adm, map[string]any{"host": "smtp.example.com", "port": 25, "security": "none", "from": "a@example.com", "enabled": true}); st != 400 {
		t.Fatalf("plaintext to a remote relay accepted: %d", st)
	}
	e.configureSMTP(adm, srv, nil)
	st, r := e.post(nil, "GET", "/api/smtp/settings", adm, nil)
	s := r["settings"].(map[string]any)
	if st != 200 || s["has_password"] != true || s["password"] != nil || s["security"] != "starttls" {
		t.Fatalf("get: %d %v", st, r)
	}
	var sealed []byte
	e.st.DB.QueryRow(context.Background(), `SELECT password FROM smtp_settings`).Scan(&sealed)
	if len(sealed) == 0 || strings.Contains(string(sealed), "smtp-secret") {
		t.Fatal("SMTP password not sealed")
	}
	if pw, err := e.v.Open(sealed); err != nil || pw != "smtp-secret" {
		t.Fatalf("sealed password: %v", err)
	}
	if st, r := e.post(nil, "POST", "/api/smtp/test", adm, map[string]any{"to": "ops@mail.test"}); st != 200 || r["ok"] != true {
		t.Fatalf("test mail: %d %v", st, r)
	}
	m := srv.Wait(t, 0)
	if !m.TLS || m.Auth != "\x00aegiron\x00smtp-secret" || m.To[0] != "ops@mail.test" {
		t.Fatalf("test message: %+v", m)
	}
	// Keeping the password: omit it.
	e.configureSMTP(adm, srv, map[string]any{"password": nil})
	e.st.DB.QueryRow(context.Background(), `SELECT password FROM smtp_settings`).Scan(&sealed)
	if pw, _ := e.v.Open(sealed); pw != "smtp-secret" {
		t.Fatal("password lost when omitted")
	}
}

func TestEmailSecondFactor(t *testing.T) {
	e := newTestEnv(t)
	adm := e.admin()
	srv := mailtest.New(t, true)
	u := e.createUser(uniqueEmail("mailer", "mail.test"), "user", false)
	const pw = "password-123456"
	tok := e.bearer(u)

	// Without SMTP the user cannot switch it on.
	e.st.DB.Exec(context.Background(), `DELETE FROM smtp_settings`)
	if st, _ := e.post(nil, "POST", "/api/auth/email-otp", tok, map[string]any{"enabled": true, "password": pw}); st != 409 {
		t.Fatalf("without SMTP: %d", st)
	}
	e.configureSMTP(adm, srv, nil)
	if st, _ := e.post(nil, "POST", "/api/auth/email-otp", tok, map[string]any{"enabled": true, "password": "wrong-password"}); st != 401 {
		t.Fatalf("wrong password: %d", st)
	}
	if st, _ := e.post(nil, "POST", "/api/auth/email-otp", tok, map[string]any{"enabled": true, "password": pw}); st != 200 {
		t.Fatalf("enable: %d", st)
	}

	jar, _ := cookiejar.New(nil)
	c := &http.Client{Jar: jar}
	login := func(extra map[string]any) (int, map[string]any) {
		body := map[string]any{"email": u.Email, "password": pw}
		for k, v := range extra {
			body[k] = v
		}
		return e.post(c, "POST", "/api/auth/login", "", body)
	}
	st, r := login(nil)
	if st != 200 || r["need_totp"] != true || r["method"] != "email" {
		t.Fatalf("first step: %d %v", st, r)
	}
	m := srv.Wait(t, 0)
	code := sixDigits.FindString(m.Body())
	if m.To[0] != u.Email || code == "" {
		t.Fatalf("code mail: %+v %q", m, m.Body())
	}
	// Asking again within a minute does not mail another code.
	login(nil)
	if n := len(srv.Messages()); n != 1 {
		t.Fatalf("resent too soon: %d messages", n)
	}
	wrong := "000000"
	if wrong == code {
		wrong = "111111"
	}
	if st, _ := login(map[string]any{"totp": wrong}); st != 401 {
		t.Fatalf("wrong code: %d", st)
	}
	st, r = login(map[string]any{"totp": code, "remember_device": true})
	if st != 200 || r["user"] == nil {
		t.Fatalf("right code: %d %v", st, r)
	}
	// Single use.
	if st, _ := login(map[string]any{"totp": code}); st != 401 {
		t.Fatalf("code reused: %d", st)
	}
	// The remembered browser skips the emailed code.
	if st, r := login(nil); st != 200 || r["user"] == nil {
		t.Fatalf("trusted browser: %d %v", st, r)
	}

	// Desktop app: same flow through need_totp/totp, so older apps work unchanged.
	st, r = e.appLogin(u, pw, nil)
	if st != 200 || r["need_totp"] != true || r["method"] != "email" {
		t.Fatalf("app first step: %d %v", st, r)
	}
	code = sixDigits.FindString(srv.Wait(t, 1).Body())
	if st, r := e.appLogin(u, pw, map[string]any{"totp": code}); st != 200 || r["token"] == nil {
		t.Fatalf("app code: %d %v", st, r)
	}

	// Five wrong guesses burn the code.
	e.appLogin(u, pw, nil)
	code = sixDigits.FindString(srv.Wait(t, 2).Body())
	for range 5 {
		e.appLogin(u, pw, map[string]any{"totp": "999999"})
	}
	if st, _ := e.appLogin(u, pw, map[string]any{"totp": code}); st != 401 {
		t.Fatalf("code still valid after 5 wrong guesses: %d", st)
	}

	// An admin reset turns it off.
	uid := u.ID
	if st, _ := e.post(nil, "DELETE", "/api/users/"+itoa(uid)+"/totp", adm, nil); st != 200 {
		t.Fatalf("reset: %d", st)
	}
	if st, r := e.appLogin(u, pw, nil); st != 200 || r["token"] == nil {
		t.Fatalf("after reset: %d %v", st, r)
	}
	if got, _ := e.st.GetUser(context.Background(), uid); got.EmailOTP {
		t.Fatal("email_otp still on")
	}
}

func TestEmailCodeNeedsMail(t *testing.T) {
	e := newTestEnv(t)
	u := e.createUser(uniqueEmail("nomail", "mail.test"), "user", false)
	e.st.SetEmailOTP(context.Background(), u.ID, true)
	e.st.DB.Exec(context.Background(), `DELETE FROM smtp_settings`)
	// Fails closed: no second factor available means no sign-in, not a skipped factor.
	if st, r := e.appLogin(u, "password-123456", nil); st != 503 || r["code"] != "mail_unavailable" {
		t.Fatalf("got %d %v", st, r)
	}
}

func TestAdminAlerts(t *testing.T) {
	e := newTestEnv(t)
	adm := e.admin()
	srv := mailtest.New(t, true)
	e.configureSMTP(adm, srv, map[string]any{"alert_new_device": true, "alert_lockout": true, "alert_recipients": []string{"SOC@mail.test"}})

	id := randomDeviceID()
	if _, r := e.register(id, newKey()); r["t"] != "registered" {
		t.Fatal(r)
	}
	m := srv.Wait(t, 0)
	if m.To[0] != "soc@mail.test" || !strings.Contains(m.Body(), id) {
		t.Fatalf("new device alert: %+v %q", m, m.Body())
	}

	// Lockout: the 11th attempt within 10 minutes is refused and alerts once.
	u := e.createUser(uniqueEmail("victim", "mail.test"), "user", false)
	e.api.limiter = newRateLimiter()
	for i := range 13 {
		var buf strings.Builder
		buf.WriteString(`{"email":"` + u.Email + `","password":"wrong-password-x"}`)
		req, _ := http.NewRequest("POST", e.srv.URL+"/api/auth/login", strings.NewReader(buf.String()))
		req.Header.Set("X-Aegiron", "1")
		res, err := http.DefaultClient.Do(req)
		if err != nil {
			t.Fatal(err)
		}
		res.Body.Close()
		if i >= 10 && res.StatusCode != 429 {
			t.Fatalf("attempt %d: %d", i+1, res.StatusCode)
		}
	}
	m = srv.Wait(t, 1)
	if !strings.Contains(m.Body(), u.Email) {
		t.Fatalf("lockout alert: %q", m.Body())
	}
	if n := e.auditCount("auth.lockout", u.Email); n != 1 {
		t.Fatalf("lockout audited %d times", n)
	}
	if n := len(srv.Messages()); n != 2 {
		t.Fatalf("%d messages, want 2 (one lockout alert per window)", n)
	}
}
