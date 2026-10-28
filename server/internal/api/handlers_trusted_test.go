package api

import (
	"bytes"
	"context"
	"encoding/json"
	"net/http"
	"net/http/cookiejar"
	"testing"
	"time"

	"github.com/pquerna/otp/totp"

	"aegiron/server/internal/auth"
	"aegiron/server/internal/store"
)

func TestBrowserLabel(t *testing.T) {
	cases := map[string]string{
		"Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/130.0 Safari/537.36":         "Chrome บน macOS",
		"Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/130.0 Safari/537.36 Edg/130.0":     "Edge บน Windows",
		"Mozilla/5.0 (X11; Linux x86_64; rv:131.0) Gecko/20100101 Firefox/131.0":                                                    "Firefox บน Linux",
		"Mozilla/5.0 (iPhone; CPU iPhone OS 18_0 like Mac OS X) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/18.0 Safari/604.1": "Safari บน iOS",
		"curl/8.7.1": "เบราว์เซอร์",
	}
	for ua, want := range cases {
		if got := browserLabel(ua); got != want {
			t.Errorf("browserLabel(%q) = %q, want %q", ua, got, want)
		}
	}
}

// totpUser creates a user with 2FA enabled and returns it with its secret.
func (e *testEnv) totpUser(prefix string) (*store.User, string) {
	u := e.createUser(uniqueEmail(prefix, "trust.test"), "user", false)
	key, err := totp.Generate(totp.GenerateOpts{Issuer: "Aegiron", AccountName: u.Email})
	if err != nil {
		e.t.Fatal(err)
	}
	secret := key.Secret()
	if err := e.st.SetTOTP(context.Background(), u.ID, &secret, true); err != nil {
		e.t.Fatal(err)
	}
	u.TOTPEnabled, u.TOTPSecret = true, &secret
	return u, secret
}

func code(t *testing.T, secret string) string {
	c, err := totp.GenerateCode(secret, time.Now())
	if err != nil {
		t.Fatal(err)
	}
	return c
}

// post sends JSON with the CSRF header; tok is an optional bearer token.
func (e *testEnv) post(c *http.Client, method, path, tok string, body any) (int, map[string]any) {
	e.t.Helper()
	// Many sign-ins from one address in one test: the brute-force limiter is tested elsewhere.
	e.api.limiter = newRateLimiter()
	var buf bytes.Buffer
	if body != nil {
		json.NewEncoder(&buf).Encode(body)
	}
	req, _ := http.NewRequest(method, e.srv.URL+path, &buf)
	req.Header.Set("X-Aegiron", "1")
	req.Header.Set("Content-Type", "application/json")
	req.Header.Set("User-Agent", "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) Chrome/130.0 Safari/537.36")
	if tok != "" {
		req.Header.Set("Authorization", "Bearer "+tok)
	}
	if c == nil {
		c = http.DefaultClient
	}
	res, err := c.Do(req)
	if err != nil {
		e.t.Fatal(err)
	}
	defer res.Body.Close()
	out := map[string]any{}
	json.NewDecoder(res.Body).Decode(&out)
	return res.StatusCode, out
}

func (e *testEnv) appLogin(u *store.User, password string, extra map[string]any) (int, map[string]any) {
	body := map[string]any{"email": u.Email, "password": password, "device": "test-mac"}
	for k, v := range extra {
		body[k] = v
	}
	return e.post(nil, "POST", "/api/agent/login", "", body)
}

func (e *testEnv) auditCount(action, target string) int {
	var n int
	if err := e.st.DB.QueryRow(context.Background(), `SELECT count(*) FROM audit_log WHERE action=$1 AND target=$2`, action, target).Scan(&n); err != nil {
		e.t.Fatal(err)
	}
	return n
}

func TestTrustedDeviceAppFlow(t *testing.T) {
	e := newTestEnv(t)
	u, secret := e.totpUser("app")
	const pw = "password-123456"

	// No code: TOTP is asked for.
	if st, r := e.appLogin(u, pw, nil); st != 200 || r["need_totp"] != true {
		t.Fatalf("want need_totp, got %d %v", st, r)
	}
	// Correct code without remember: no trust token.
	if st, r := e.appLogin(u, pw, map[string]any{"totp": code(t, secret)}); st != 200 || r["token"] == nil || r["trust_token"] != nil {
		t.Fatalf("plain login: %d %v", st, r)
	}
	// Wrong code with remember: refused, nothing remembered.
	if st, _ := e.appLogin(u, pw, map[string]any{"totp": "000000", "remember_device": true}); st != 401 {
		t.Fatalf("bad code: %d", st)
	}
	st, r := e.appLogin(u, pw, map[string]any{"totp": code(t, secret), "remember_device": true})
	trust, _ := r["trust_token"].(string)
	if st != 200 || len(trust) < 40 {
		t.Fatalf("remember: %d %v", st, r)
	}
	// Only the hash is stored.
	var stored []byte
	e.st.DB.QueryRow(context.Background(), `SELECT token_hash FROM trusted_devices WHERE user_id=$1`, u.ID).Scan(&stored)
	if !bytes.Equal(stored, auth.HashToken(trust)) {
		t.Fatal("stored value is not the token hash")
	}

	// Next sign-in: password + trust token, no code.
	st, r = e.appLogin(u, pw, map[string]any{"trust_token": trust})
	if st != 200 || r["token"] == nil || r["trust_token"] != nil {
		t.Fatalf("trusted login: %d %v", st, r)
	}
	bearer := r["token"].(string)
	// The trust token never replaces the password.
	if st, _ := e.appLogin(u, "wrong-password-1", map[string]any{"trust_token": trust}); st != 401 {
		t.Fatalf("wrong password with trust token: %d", st)
	}
	// Another user cannot use it.
	other, _ := e.totpUser("other")
	if st, r := e.appLogin(other, pw, map[string]any{"trust_token": trust}); st != 200 || r["need_totp"] != true || r["trust_rejected"] != true {
		t.Fatalf("other user's token: %d %v", st, r)
	}
	if e.auditCount("auth.trusted_device_added", u.Email) != 1 {
		t.Fatal("missing auth.trusted_device_added audit")
	}

	// The user sees the device and revokes it.
	st, r = e.post(nil, "GET", "/api/auth/trusted-devices", bearer, nil)
	devs, _ := r["devices"].([]any)
	if st != 200 || len(devs) != 1 {
		t.Fatalf("list: %d %v", st, r)
	}
	d := devs[0].(map[string]any)
	if d["label"] != "แอป Aegiron: test-mac" || d["last_used_at"] == nil || d["token_hash"] != nil {
		t.Fatalf("device view: %v", d)
	}
	id := int64(d["id"].(float64))
	// Another user cannot revoke it.
	if st, _ := e.post(nil, "DELETE", "/api/auth/trusted-devices/"+itoa(id), e.bearer(other), nil); st != 404 {
		t.Fatalf("foreign revoke: %d", st)
	}
	if st, _ := e.post(nil, "DELETE", "/api/auth/trusted-devices/"+itoa(id), bearer, nil); st != 200 {
		t.Fatalf("revoke: %d", st)
	}
	if st, r := e.appLogin(u, pw, map[string]any{"trust_token": trust}); r["need_totp"] != true || r["trust_rejected"] != true {
		t.Fatalf("revoked token still accepted: %d %v", st, r)
	}
	if e.auditCount("user.trusted_device_revoked", u.Email) != 1 || e.auditCount("auth.trusted_device_rejected", u.Email) != 1 {
		t.Fatal("missing revoke/reject audit")
	}

	// Expired tokens are refused and not listed.
	exp := auth.RandomToken(32)
	if _, err := e.st.CreateTrustedDevice(context.Background(), auth.HashToken(exp), u.ID, "old", "", "", time.Now().Add(-time.Minute)); err != nil {
		t.Fatal(err)
	}
	if _, r := e.appLogin(u, pw, map[string]any{"trust_token": exp}); r["need_totp"] != true {
		t.Fatalf("expired token accepted: %v", r)
	}
	if _, r := e.post(nil, "GET", "/api/auth/trusted-devices", bearer, nil); len(r["devices"].([]any)) != 0 {
		t.Fatalf("expired device listed: %v", r)
	}
}

func TestTrustedDeviceWithoutTOTP(t *testing.T) {
	e := newTestEnv(t)
	u := e.createUser(uniqueEmail("nototp", "trust.test"), "user", false)
	st, r := e.appLogin(u, "password-123456", map[string]any{"remember_device": true})
	if st != 200 || r["token"] == nil || r["trust_token"] != nil {
		t.Fatalf("no 2FA: %d %v", st, r)
	}
}

func TestTrustedDeviceBrowserFlow(t *testing.T) {
	e := newTestEnv(t)
	u, secret := e.totpUser("web")
	const pw = "password-123456"
	jar, _ := cookiejar.New(nil)
	c := &http.Client{Jar: jar}
	login := func(extra map[string]any) (int, map[string]any) {
		body := map[string]any{"email": u.Email, "password": pw}
		for k, v := range extra {
			body[k] = v
		}
		return e.post(c, "POST", "/api/auth/login", "", body)
	}
	trustCookieSet := func() bool {
		req, _ := http.NewRequest("GET", e.srv.URL+"/api/auth/login", nil)
		for _, ck := range jar.Cookies(req.URL) {
			if ck.Name == trustCookie && ck.Value != "" {
				return true
			}
		}
		return false
	}

	if _, r := login(nil); r["need_totp"] != true {
		t.Fatalf("want need_totp: %v", r)
	}
	if st, r := login(map[string]any{"totp": code(t, secret), "remember_device": true}); st != 200 || r["user"] == nil {
		t.Fatalf("remember: %d %v", st, r)
	}
	if !trustCookieSet() {
		t.Fatal("trust cookie not set")
	}
	// The cookie is scoped to /api/auth only.
	other, _ := http.NewRequest("GET", e.srv.URL+"/api/devices", nil)
	for _, ck := range jar.Cookies(other.URL) {
		if ck.Name == trustCookie {
			t.Fatal("trust cookie sent outside /api/auth")
		}
	}
	e.post(c, "POST", "/api/auth/logout", "", nil)
	if st, r := login(nil); st != 200 || r["user"] == nil {
		t.Fatalf("trusted browser still asked for code: %d %v", st, r)
	}
	// The list marks this browser as current.
	_, r := e.post(c, "GET", "/api/auth/trusted-devices", "", nil)
	devs := r["devices"].([]any)
	if len(devs) != 1 || devs[0].(map[string]any)["current"] != true || devs[0].(map[string]any)["label"] != "Chrome บน macOS" {
		t.Fatalf("list: %v", r)
	}

	// Changing the password forgets every remembered device.
	if st, _ := e.post(c, "POST", "/api/auth/password", "", map[string]any{"current": pw, "new": "new-password-123"}); st != 200 {
		t.Fatalf("change password: %d", st)
	}
	if n := e.auditCount("user.trusted_devices_revoked", u.Email); n != 1 {
		t.Fatalf("revocation audit count %d", n)
	}
	body := map[string]any{"email": u.Email, "password": "new-password-123"}
	if _, r := e.post(c, "POST", "/api/auth/login", "", body); r["need_totp"] != true {
		t.Fatalf("password change did not revoke: %v", r)
	}
	// A rejected cookie is cleared.
	if trustCookieSet() {
		t.Fatal("rejected trust cookie kept")
	}
}

func TestTrustedDeviceAdminAndReset(t *testing.T) {
	e := newTestEnv(t)
	admin := e.bearer(e.createUser(uniqueEmail("admin", "trust.test"), "admin", false))
	u, secret := e.totpUser("managed")
	const pw = "password-123456"
	remember := func() string {
		_, r := e.appLogin(u, pw, map[string]any{"totp": code(t, secret), "remember_device": true})
		tok, _ := r["trust_token"].(string)
		if tok == "" {
			t.Fatalf("remember: %v", r)
		}
		return tok
	}
	trusted := func(tok string) bool {
		_, r := e.appLogin(u, pw, map[string]any{"trust_token": tok})
		return r["token"] != nil
	}
	t1, t2 := remember(), remember()

	// A normal user cannot use the admin endpoints.
	if st, _ := e.post(nil, "GET", "/api/users/"+itoa(u.ID)+"/trusted-devices", e.bearer(u), nil); st != 403 {
		t.Fatalf("non-admin list: %d", st)
	}
	_, r := e.post(nil, "GET", "/api/users/"+itoa(u.ID)+"/trusted-devices", admin, nil)
	devs := r["devices"].([]any)
	if len(devs) != 2 {
		t.Fatalf("admin list: %v", r)
	}
	// Revoke one.
	id := int64(devs[0].(map[string]any)["id"].(float64))
	if st, _ := e.post(nil, "DELETE", "/api/users/"+itoa(u.ID)+"/trusted-devices/"+itoa(id), admin, nil); st != 200 {
		t.Fatalf("admin revoke one: %d", st)
	}
	if trusted(t1) == trusted(t2) {
		t.Fatal("exactly one token should remain valid")
	}
	// Revoke all.
	if st, r := e.post(nil, "DELETE", "/api/users/"+itoa(u.ID)+"/trusted-devices", admin, nil); st != 200 || r["revoked"] != float64(1) {
		t.Fatalf("admin revoke all: %d %v", st, r)
	}
	if trusted(t1) || trusted(t2) {
		t.Fatal("token valid after revoke all")
	}

	// Admin 2FA reset and password reset both forget remembered devices.
	t3 := remember()
	if st, _ := e.post(nil, "POST", "/api/users/"+itoa(u.ID)+"/password", admin, map[string]any{"password": pw}); st != 200 {
		t.Fatalf("password reset: %d", st)
	}
	if trusted(t3) {
		t.Fatal("token valid after admin password reset")
	}
	t4 := remember()
	if st, _ := e.post(nil, "DELETE", "/api/users/"+itoa(u.ID)+"/totp", admin, nil); st != 200 {
		t.Fatalf("totp reset: %d", st)
	}
	var n int
	e.st.DB.QueryRow(context.Background(), `SELECT count(*) FROM trusted_devices WHERE user_id=$1`, u.ID).Scan(&n)
	if n != 0 {
		t.Fatalf("%d devices left after 2FA reset", n)
	}
	_ = t4
	if e.auditCount("user.trusted_devices_revoked", u.Email) < 3 {
		t.Fatal("missing revocation audits")
	}
}

func TestTrustedDeviceCap(t *testing.T) {
	e := newTestEnv(t)
	u, _ := e.totpUser("cap")
	ctx := context.Background()
	for i := 0; i < store.MaxTrustedDevices+5; i++ {
		if _, err := e.st.CreateTrustedDevice(ctx, auth.HashToken(auth.RandomToken(32)), u.ID, "x", "", "", time.Now().Add(time.Hour)); err != nil {
			t.Fatal(err)
		}
	}
	list, err := e.st.ListTrustedDevices(ctx, u.ID)
	if err != nil || len(list) != store.MaxTrustedDevices {
		t.Fatalf("got %d devices, err %v", len(list), err)
	}
}
