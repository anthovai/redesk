package api

import (
	"bytes"
	"context"
	"crypto/ed25519"
	"crypto/rand"
	"crypto/rsa"
	"crypto/sha256"
	"encoding/base64"
	"encoding/hex"
	"encoding/json"
	"html"
	"io"
	"log/slog"
	"mime/multipart"
	"net/http"
	"net/http/cookiejar"
	"net/http/httptest"
	"net/url"
	"os"
	"regexp"
	"strconv"
	"strings"
	"sync"
	"testing"
	"time"

	"github.com/go-jose/go-jose/v4"

	"redesk/server/internal/auth"
	"redesk/server/internal/config"
	"redesk/server/internal/hub"
	"redesk/server/internal/store"
	"redesk/server/internal/vault"
)

// DB-backed tests run when REDESK_TEST_DATABASE_URL points at a scratch database.

type testEnv struct {
	t   *testing.T
	api *API
	srv *httptest.Server
	st  *store.Store
	v   *vault.Vault
}

func newTestEnv(t *testing.T) *testEnv {
	t.Helper()
	dsn := os.Getenv("REDESK_TEST_DATABASE_URL")
	if dsn == "" {
		t.Skip("REDESK_TEST_DATABASE_URL not set")
	}
	ctx := context.Background()
	st, err := store.Open(ctx, dsn)
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(st.Close)
	if err := st.Migrate(ctx); err != nil {
		t.Fatal(err)
	}
	if _, err := st.DB.Exec(ctx, `DELETE FROM oidc_settings; DELETE FROM agent_releases`); err != nil {
		t.Fatal(err)
	}
	dir := t.TempDir()
	v, err := vault.Load("", dir)
	if err != nil {
		t.Fatal(err)
	}
	_, key, _ := ed25519.GenerateKey(rand.Reader)
	log := slog.New(slog.NewTextHandler(io.Discard, nil))
	cfg := config.Config{DataDir: dir}
	a := New(cfg, st, hub.New(st, key, log, nil), key, v, log)
	srv := httptest.NewServer(a.Handler())
	t.Cleanup(srv.Close)
	return &testEnv{t: t, api: a, srv: srv, st: st, v: v}
}

func uniqueEmail(prefix, domain string) string {
	return prefix + "-" + auth.RandomToken(6) + "@" + domain
}

func (e *testEnv) createUser(email, role string, disabled bool) *store.User {
	ctx := context.Background()
	h, _ := auth.HashPassword("password-123456")
	u, err := e.st.CreateUser(ctx, email, "Test", role, h)
	if err != nil {
		e.t.Fatal(err)
	}
	if disabled {
		_ = e.st.UpdateUser(ctx, u.ID, u.Name, role, true)
	}
	return u
}

// bearer returns a session token for u.
func (e *testEnv) bearer(u *store.User) string {
	tok := auth.RandomToken(32)
	if err := e.st.CreateAuthSession(context.Background(), auth.HashToken(tok), u.ID, "", "test", time.Now().Add(time.Hour)); err != nil {
		e.t.Fatal(err)
	}
	return tok
}

// --- mock OpenID provider ---

type mockIdP struct {
	srv    *httptest.Server
	key    *rsa.PrivateKey
	mu     sync.Mutex
	codes  map[string]map[string]any // code -> claims
	authz  map[string]url.Values     // code_challenge -> authorize query
	client string
	secret string
}

func newMockIdP(t *testing.T) *mockIdP {
	key, _ := rsa.GenerateKey(rand.Reader, 2048)
	m := &mockIdP{key: key, codes: map[string]map[string]any{}, authz: map[string]url.Values{}, client: "redesk-test", secret: "s3cret"}
	mux := http.NewServeMux()
	mux.HandleFunc("GET /.well-known/openid-configuration", func(w http.ResponseWriter, r *http.Request) {
		json.NewEncoder(w).Encode(map[string]any{
			"issuer": m.srv.URL, "authorization_endpoint": m.srv.URL + "/authorize", "token_endpoint": m.srv.URL + "/token",
			"jwks_uri": m.srv.URL + "/jwks", "id_token_signing_alg_values_supported": []string{"RS256"},
			"response_types_supported": []string{"code"}, "subject_types_supported": []string{"public"},
		})
	})
	mux.HandleFunc("GET /jwks", func(w http.ResponseWriter, r *http.Request) {
		json.NewEncoder(w).Encode(jose.JSONWebKeySet{Keys: []jose.JSONWebKey{{Key: &key.PublicKey, KeyID: "k1", Algorithm: "RS256", Use: "sig"}}})
	})
	mux.HandleFunc("POST /token", func(w http.ResponseWriter, r *http.Request) {
		r.ParseForm()
		id, sec, ok := r.BasicAuth()
		if !ok {
			id, sec = r.Form.Get("client_id"), r.Form.Get("client_secret")
		}
		m.mu.Lock()
		claims, found := m.codes[r.Form.Get("code")]
		delete(m.codes, r.Form.Get("code"))
		sum := sha256.Sum256([]byte(r.Form.Get("code_verifier")))
		_, pkceOK := m.authz[base64.RawURLEncoding.EncodeToString(sum[:])]
		m.mu.Unlock()
		if id != m.client || sec != m.secret || !found || !pkceOK {
			w.WriteHeader(http.StatusBadRequest)
			json.NewEncoder(w).Encode(map[string]string{"error": "invalid_grant"})
			return
		}
		w.Header().Set("Content-Type", "application/json")
		json.NewEncoder(w).Encode(map[string]any{"access_token": "at", "token_type": "Bearer", "expires_in": 300, "id_token": m.sign(t, claims)})
	})
	m.srv = httptest.NewServer(mux)
	t.Cleanup(m.srv.Close)
	return m
}

func (m *mockIdP) sign(t *testing.T, claims map[string]any) string {
	s, err := jose.NewSigner(jose.SigningKey{Algorithm: jose.RS256, Key: m.key}, (&jose.SignerOptions{}).WithType("JWT").WithHeader("kid", "k1"))
	if err != nil {
		t.Fatal(err)
	}
	b, _ := json.Marshal(claims)
	obj, err := s.Sign(b)
	if err != nil {
		t.Fatal(err)
	}
	out, _ := obj.CompactSerialize()
	return out
}

// authorize simulates the user signing in at the IdP: records the request and issues a code.
func (m *mockIdP) authorize(t *testing.T, loc string, claims map[string]any) (redirect string) {
	u, err := url.Parse(loc)
	if err != nil || !strings.HasPrefix(loc, m.srv.URL+"/authorize") {
		t.Fatalf("not an IdP redirect: %q", loc)
	}
	q := u.Query()
	if q.Get("code_challenge_method") != "S256" || q.Get("code_challenge") == "" || q.Get("nonce") == "" || q.Get("state") == "" {
		t.Fatalf("missing PKCE/nonce/state: %v", q)
	}
	full := map[string]any{"iss": m.srv.URL, "aud": m.client, "sub": "sub-1", "iat": time.Now().Unix(),
		"exp": time.Now().Add(5 * time.Minute).Unix(), "nonce": q.Get("nonce")}
	for k, v := range claims {
		if v == nil {
			delete(full, k)
		} else {
			full[k] = v
		}
	}
	code := auth.RandomToken(12)
	m.mu.Lock()
	m.codes[code] = full
	m.authz[q.Get("code_challenge")] = q
	m.mu.Unlock()
	return q.Get("redirect_uri") + "?code=" + code + "&state=" + url.QueryEscape(q.Get("state"))
}

func (e *testEnv) configureOIDC(m *mockIdP, domains []string, autoCreate bool) {
	sealed, err := e.v.Seal(m.secret)
	if err != nil {
		e.t.Fatal(err)
	}
	if err := e.st.SaveOIDCSettings(context.Background(), &store.OIDCSettings{
		Name: "Microsoft 365", Issuer: m.srv.URL, ClientID: m.client, AllowedDomains: domains, AutoCreate: autoCreate, Enabled: true,
	}, sealed); err != nil {
		e.t.Fatal(err)
	}
}

func browser() *http.Client {
	jar, _ := cookiejar.New(nil)
	return &http.Client{Jar: jar, CheckRedirect: func(*http.Request, []*http.Request) error { return http.ErrUseLastResponse }}
}

func get(t *testing.T, c *http.Client, u string) *http.Response {
	t.Helper()
	res, err := c.Get(u)
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() { res.Body.Close() })
	return res
}

// browserLogin runs /api/oidc/login and the IdP, returning the callback response.
func (e *testEnv) browserLogin(m *mockIdP, c *http.Client, claims map[string]any) *http.Response {
	res := get(e.t, c, e.srv.URL+"/api/oidc/login?next=/devices")
	if res.StatusCode != http.StatusSeeOther {
		e.t.Fatalf("login status %d", res.StatusCode)
	}
	return get(e.t, c, m.authorize(e.t, res.Header.Get("Location"), claims))
}

func wantOIDCError(t *testing.T, res *http.Response, reason string) {
	t.Helper()
	if loc := res.Header.Get("Location"); res.StatusCode != http.StatusSeeOther || loc != "/login?oidc_error="+reason {
		t.Fatalf("want error %s, got %d %q", reason, res.StatusCode, loc)
	}
	for _, c := range res.Cookies() {
		if c.Name == cookieName && c.Value != "" {
			t.Fatal("session cookie set on failure")
		}
	}
}

func TestOIDCBrowserLogin(t *testing.T) {
	e := newTestEnv(t)
	m := newMockIdP(t)
	e.configureOIDC(m, []string{"example.com"}, false)

	var info struct {
		OIDC struct {
			Enabled bool   `json:"enabled"`
			Name    string `json:"name"`
		} `json:"oidc"`
	}
	json.NewDecoder(get(t, browser(), e.srv.URL+"/api/server-info").Body).Decode(&info)
	if !info.OIDC.Enabled || info.OIDC.Name != "Microsoft 365" {
		t.Fatalf("server-info oidc: %+v", info.OIDC)
	}

	email := uniqueEmail("Somchai", "Example.com")
	u := e.createUser(store.NormalizeEmail(email), "user", false)

	t.Run("success", func(t *testing.T) {
		c := browser()
		res := e.browserLogin(m, c, map[string]any{"email": email, "email_verified": true})
		if res.StatusCode != http.StatusSeeOther || res.Header.Get("Location") != "/devices" {
			t.Fatalf("callback: %d %q", res.StatusCode, res.Header.Get("Location"))
		}
		var sess *http.Cookie
		for _, ck := range res.Cookies() {
			if ck.Name == cookieName {
				sess = ck
			}
		}
		if sess == nil || !sess.HttpOnly || sess.SameSite != http.SameSiteStrictMode || sess.Path != "/" {
			t.Fatalf("session cookie: %+v", sess)
		}
		me := get(t, c, e.srv.URL+"/api/auth/me")
		var r struct{ User store.User }
		json.NewDecoder(me.Body).Decode(&r)
		if me.StatusCode != 200 || r.User.ID != u.ID {
			t.Fatalf("me: %d %+v", me.StatusCode, r.User)
		}
	})
	t.Run("next is sanitized", func(t *testing.T) {
		c := browser()
		res := get(t, c, e.srv.URL+"/api/oidc/login?next=//evil.example/x")
		res = get(t, c, m.authorize(t, res.Header.Get("Location"), map[string]any{"email": email}))
		if res.Header.Get("Location") != "/" {
			t.Fatalf("next not sanitized: %q", res.Header.Get("Location"))
		}
	})
	t.Run("wrong state", func(t *testing.T) {
		c := browser()
		res := get(t, c, e.srv.URL+"/api/oidc/login")
		cb := m.authorize(t, res.Header.Get("Location"), map[string]any{"email": email})
		cb = regexp.MustCompile(`state=[^&]+`).ReplaceAllString(cb, "state=forged")
		wantOIDCError(t, get(t, c, cb), "bad_state")
	})
	t.Run("state from another browser", func(t *testing.T) {
		res := get(t, browser(), e.srv.URL+"/api/oidc/login")
		cb := m.authorize(t, res.Header.Get("Location"), map[string]any{"email": email})
		wantOIDCError(t, get(t, browser(), cb), "bad_state")
	})
	t.Run("state is single use", func(t *testing.T) {
		c := browser()
		res := get(t, c, e.srv.URL+"/api/oidc/login")
		cb := m.authorize(t, res.Header.Get("Location"), map[string]any{"email": email})
		if r := get(t, c, cb); r.Header.Get("Location") != "/" {
			t.Fatalf("first use failed: %q", r.Header.Get("Location"))
		}
		wantOIDCError(t, get(t, c, cb), "bad_state")
	})
	t.Run("wrong nonce", func(t *testing.T) {
		wantOIDCError(t, e.browserLogin(m, browser(), map[string]any{"email": email, "nonce": "other"}), "bad_token")
	})
	t.Run("wrong audience", func(t *testing.T) {
		wantOIDCError(t, e.browserLogin(m, browser(), map[string]any{"email": email, "aud": "someone-else"}), "bad_token")
	})
	t.Run("expired token", func(t *testing.T) {
		wantOIDCError(t, e.browserLogin(m, browser(), map[string]any{"email": email, "exp": time.Now().Add(-time.Hour).Unix()}), "bad_token")
	})
	t.Run("unverified email", func(t *testing.T) {
		wantOIDCError(t, e.browserLogin(m, browser(), map[string]any{"email": email, "email_verified": false}), "email_unverified")
	})
	t.Run("disallowed domain", func(t *testing.T) {
		wantOIDCError(t, e.browserLogin(m, browser(), map[string]any{"email": uniqueEmail("x", "evil.test")}), "domain_not_allowed")
	})
	t.Run("auto-create off", func(t *testing.T) {
		wantOIDCError(t, e.browserLogin(m, browser(), map[string]any{"email": uniqueEmail("new", "example.com")}), "no_account")
	})
	t.Run("disabled account", func(t *testing.T) {
		d := e.createUser(uniqueEmail("off", "example.com"), "user", true)
		wantOIDCError(t, e.browserLogin(m, browser(), map[string]any{"email": d.Email}), "disabled_account")
	})
	t.Run("auto-create on", func(t *testing.T) {
		e.configureOIDC(m, []string{"example.com"}, true)
		newEmail := uniqueEmail("new", "example.com")
		res := e.browserLogin(m, browser(), map[string]any{"email": newEmail, "name": "Somsri"})
		if res.Header.Get("Location") != "/devices" {
			t.Fatalf("auto-create: %q", res.Header.Get("Location"))
		}
		nu, err := e.st.GetUserByEmail(context.Background(), newEmail)
		if err != nil || nu.Role != "user" || nu.Name != "Somsri" {
			t.Fatalf("created user: %+v %v", nu, err)
		}
	})
}

func TestOIDCAppFlow(t *testing.T) {
	e := newTestEnv(t)
	m := newMockIdP(t)
	e.configureOIDC(m, nil, false)
	u := e.createUser(uniqueEmail("app", "example.org"), "user", false)
	api := &http.Client{}

	post := func() (code, link string) {
		req, _ := http.NewRequest("POST", e.srv.URL+"/api/oidc/auth", strings.NewReader(`{"device":"PC-01"}`))
		req.Header.Set("X-ReDesk", "1")
		res, err := api.Do(req)
		if err != nil {
			t.Fatal(err)
		}
		defer res.Body.Close()
		var r struct{ Code, URL string }
		json.NewDecoder(res.Body).Decode(&r)
		if res.StatusCode != 200 || r.Code == "" || !strings.HasPrefix(r.URL, "/api/oidc/login?app=") || strings.Contains(r.URL, r.Code) {
			t.Fatalf("auth: %d %+v", res.StatusCode, r)
		}
		return r.Code, r.URL
	}
	poll := func(code string) (int, map[string]any) {
		res := get(t, api, e.srv.URL+"/api/oidc/auth-query?code="+url.QueryEscape(code))
		var r map[string]any
		json.NewDecoder(res.Body).Decode(&r)
		return res.StatusCode, r
	}
	// confirm opens the browser link and follows the confirmation page to the IdP.
	confirm := func(c *http.Client, link string) string {
		res := get(t, c, e.srv.URL+link)
		body, _ := io.ReadAll(res.Body)
		if res.StatusCode != 200 || !bytes.Contains(body, []byte("PC-01")) {
			t.Fatalf("confirm page: %d %s", res.StatusCode, body)
		}
		mm := regexp.MustCompile(`href="([^"]+)"`).FindSubmatch(body)
		if mm == nil {
			t.Fatal("no continue link")
		}
		res = get(t, c, e.srv.URL+html.UnescapeString(string(mm[1])))
		if res.StatusCode != http.StatusSeeOther {
			t.Fatalf("continue: %d", res.StatusCode)
		}
		return res.Header.Get("Location")
	}

	code, link := post()
	if st, r := poll(code); st != 200 || r["pending"] != true {
		t.Fatalf("pending: %d %v", st, r)
	}
	// The link must not skip the confirmation page.
	if res := get(t, browser(), e.srv.URL+link+"&confirm=guess"); res.StatusCode != 200 || res.Header.Get("Location") != "" {
		t.Fatalf("confirm bypass: %d", res.StatusCode)
	}
	c := browser()
	cb := m.authorize(t, confirm(c, link), map[string]any{"email": u.Email})
	if res := get(t, c, cb); res.StatusCode != 200 {
		t.Fatalf("app callback: %d", res.StatusCode)
	}
	st, r := poll(code)
	if st != 200 || r["token"] == nil || r["server_key"] == nil || r["enroll_token"] == nil || r["user"].(map[string]any)["email"] != u.Email {
		t.Fatalf("token: %d %v", st, r)
	}
	req, _ := http.NewRequest("GET", e.srv.URL+"/api/auth/me", nil)
	req.Header.Set("Authorization", "Bearer "+r["token"].(string))
	if res, _ := api.Do(req); res == nil || res.StatusCode != 200 {
		t.Fatal("app token rejected")
	}
	if st, _ := poll(code); st != http.StatusNotFound {
		t.Fatalf("code reused: %d", st)
	}

	// A failed browser sign-in is reported to the app.
	code, link = post()
	c = browser()
	cb = m.authorize(t, confirm(c, link), map[string]any{"email": uniqueEmail("nobody", "example.org")})
	if res := get(t, c, cb); res.StatusCode != http.StatusForbidden {
		t.Fatalf("app failure page: %d", res.StatusCode)
	}
	if st, r := poll(code); st != http.StatusForbidden || !strings.Contains(r["error"].(string), "no_account") {
		t.Fatalf("app failure poll: %d %v", st, r)
	}
}

func TestOIDCSettingsAPI(t *testing.T) {
	e := newTestEnv(t)
	m := newMockIdP(t)
	admin := e.createUser(uniqueEmail("admin", "example.com"), "admin", false)
	tok := e.bearer(admin)
	put := func(body string) int {
		req, _ := http.NewRequest("PUT", e.srv.URL+"/api/oidc/settings", strings.NewReader(body))
		req.Header.Set("X-ReDesk", "1")
		req.Header.Set("Authorization", "Bearer "+tok)
		res, err := http.DefaultClient.Do(req)
		if err != nil {
			t.Fatal(err)
		}
		res.Body.Close()
		return res.StatusCode
	}
	if s := put(`{"name":"M365","issuer":"http://idp.example.com","client_id":"x","enabled":true}`); s != 400 {
		t.Fatalf("plain-http issuer accepted: %d", s)
	}
	if s := put(`{"name":"M365","issuer":"` + m.srv.URL + `","client_id":"x","allowed_domains":["bad domain"],"enabled":false}`); s != 400 {
		t.Fatalf("bad domain accepted: %d", s)
	}
	if s := put(`{"name":"M365","issuer":"` + m.srv.URL + `","client_id":"x","client_secret":"top","allowed_domains":["@Example.COM"],"enabled":true}`); s != 200 {
		t.Fatalf("save: %d", s)
	}
	o, _ := e.st.GetOIDCSettings(context.Background())
	if plain, _ := e.v.Open(o.ClientSecret); plain != "top" || bytes.Contains(o.ClientSecret, []byte("top")) || o.AllowedDomains[0] != "example.com" {
		t.Fatalf("stored: %+v", o)
	}
	// Secret is write-only and kept when omitted.
	if s := put(`{"name":"M365","issuer":"` + m.srv.URL + `","client_id":"y","enabled":true}`); s != 200 {
		t.Fatalf("save 2: %d", s)
	}
	req, _ := http.NewRequest("GET", e.srv.URL+"/api/oidc/settings", nil)
	req.Header.Set("Authorization", "Bearer "+tok)
	res, _ := http.DefaultClient.Do(req)
	b, _ := io.ReadAll(res.Body)
	res.Body.Close()
	if bytes.Contains(b, []byte("top")) || !bytes.Contains(b, []byte(`"has_secret":true`)) || !bytes.Contains(b, []byte("/api/oidc/callback")) {
		t.Fatalf("get settings: %s", b)
	}
}

// --- updates ---

func TestSemver(t *testing.T) {
	order := []string{"0.9.9", "1.0.0-alpha", "1.0.0-alpha.1", "1.0.0-alpha.beta", "1.0.0-beta", "1.0.0-beta.2", "1.0.0-beta.11", "1.0.0-rc.1", "1.0.0", "1.0.1", "1.2.0", "1.10.0", "2.0.0"}
	for i := range order {
		for j := range order {
			a, ok1 := parseSemver(order[i])
			b, ok2 := parseSemver(order[j])
			if !ok1 || !ok2 {
				t.Fatalf("parse %s %s", order[i], order[j])
			}
			want := 0
			if i < j {
				want = -1
			} else if i > j {
				want = 1
			}
			if got := a.cmp(b); got != want {
				t.Errorf("cmp(%s, %s) = %d, want %d", order[i], order[j], got, want)
			}
		}
	}
	if a, _ := parseSemver("v1.2.3+build.7"); a.cmp(semver{1, 2, 3, nil}) != 0 {
		t.Error("build metadata must be ignored")
	}
	for _, bad := range []string{"", "1", "1.2", "1.2.3.4", "01.2.3", "1.2.3-", "1.2.3-01", "1.2.3-a..b", "a.b.c", "1.2.3-ä", "1.2.3+", " 1.2.3"} {
		if _, ok := parseSemver(bad); ok {
			t.Errorf("accepted %q", bad)
		}
	}
}

func TestReleaseFilename(t *testing.T) {
	cases := map[string]string{
		"redesk-0.4.0-x64.msi":        "redesk-0.4.0-x64.msi",
		`C:\Users\x\redesk setup.exe`: "redesk_setup.exe",
		"../../etc/passwd":            "passwd",
		"..":                          "redesk-1.0.0-linux-x86_64",
		"\"; rm -rf.exe":              "___rm_-rf.exe",
	}
	for in, want := range cases {
		if got := releaseFilename(in, "1.0.0", "linux", "x86_64"); got != want {
			t.Errorf("releaseFilename(%q) = %q, want %q", in, got, want)
		}
	}
}

func (e *testEnv) upload(tok, version, osName, arch string, data []byte) (int, map[string]any) {
	var buf bytes.Buffer
	mw := multipart.NewWriter(&buf)
	mw.WriteField("version", version)
	mw.WriteField("os", osName)
	mw.WriteField("arch", arch)
	mw.WriteField("notes", "แก้บั๊ก")
	fw, _ := mw.CreateFormFile("file", "redesk-"+version+".exe")
	fw.Write(data)
	mw.Close()
	req, _ := http.NewRequest("POST", e.srv.URL+"/api/agent/releases", &buf)
	req.Header.Set("Content-Type", mw.FormDataContentType())
	req.Header.Set("X-ReDesk", "1")
	req.Header.Set("Authorization", "Bearer "+tok)
	res, err := http.DefaultClient.Do(req)
	if err != nil {
		e.t.Fatal(err)
	}
	defer res.Body.Close()
	var r map[string]any
	json.NewDecoder(res.Body).Decode(&r)
	return res.StatusCode, r
}

func TestAgentUpdates(t *testing.T) {
	e := newTestEnv(t)
	admin := e.createUser(uniqueEmail("admin", "example.com"), "admin", false)
	tok := e.bearer(admin)
	userTok := e.bearer(e.createUser(uniqueEmail("user", "example.com"), "user", false))

	if s, _ := e.upload(userTok, "0.5.0", "windows", "x86_64", []byte("x")); s != http.StatusForbidden {
		t.Fatalf("non-admin upload: %d", s)
	}
	if s, _ := e.upload(tok, "latest", "windows", "x86_64", []byte("x")); s != http.StatusBadRequest {
		t.Fatalf("bad version: %d", s)
	}
	if s, _ := e.upload(tok, "0.5.0", "dos", "x86_64", []byte("x")); s != http.StatusBadRequest {
		t.Fatalf("bad os: %d", s)
	}
	ids := map[string]int64{}
	payloads := map[string][]byte{}
	for _, v := range []struct{ ver, os, arch string }{{"0.4.0", "windows", "x86_64"}, {"0.5.0", "windows", "universal"}, {"0.5.0", "windows", "x86_64"}, {"0.6.0-beta.1", "windows", "aarch64"}, {"0.9.0", "linux", "x86_64"}} {
		data := []byte("binary " + v.ver + v.os + v.arch)
		s, r := e.upload(tok, v.ver, v.os, v.arch, data)
		if s != http.StatusCreated {
			t.Fatalf("upload %v: %d %v", v, s, r)
		}
		rel := r["release"].(map[string]any)
		sum := sha256.Sum256(data)
		if rel["sha256"] != hex.EncodeToString(sum[:]) || rel["enabled"] != false {
			t.Fatalf("release: %v", rel)
		}
		key := v.ver + "/" + v.os + "/" + v.arch
		ids[key], payloads[key] = int64(rel["id"].(float64)), data
	}
	if s, _ := e.upload(tok, "0.5.0", "windows", "x86_64", []byte("dup")); s != http.StatusConflict {
		t.Fatalf("duplicate: %d", s)
	}

	check := func(q string) map[string]any {
		res := get(t, http.DefaultClient, e.srv.URL+"/api/agent/update?"+q)
		var r map[string]any
		json.NewDecoder(res.Body).Decode(&r)
		if res.StatusCode != 200 {
			t.Fatalf("update %s: %d %v", q, res.StatusCode, r)
		}
		return r
	}
	if r := check("os=windows&arch=x86_64&version=0.3.0"); r["available"] != false {
		t.Fatalf("disabled releases offered: %v", r)
	}
	// Disabled releases are not downloadable without an admin session.
	id := ids["0.5.0/windows/x86_64"]
	if res := get(t, http.DefaultClient, e.srv.URL+"/api/agent/releases/"+itoa(id)+"/download"); res.StatusCode != 404 {
		t.Fatalf("disabled download: %d", res.StatusCode)
	}
	setEnabled := func(key string, on bool) {
		req, _ := http.NewRequest("PATCH", e.srv.URL+"/api/agent/releases/"+itoa(ids[key]), strings.NewReader(`{"enabled":`+map[bool]string{true: "true", false: "false"}[on]+`}`))
		req.Header.Set("X-ReDesk", "1")
		req.Header.Set("Authorization", "Bearer "+tok)
		res, err := http.DefaultClient.Do(req)
		if err != nil || res.StatusCode != 200 {
			t.Fatalf("enable %s: %v %v", key, err, res)
		}
		res.Body.Close()
	}
	for k := range ids {
		setEnabled(k, true)
	}
	r := check("os=windows&arch=x86_64&version=0.4.0")
	if r["available"] != true || r["version"] != "0.5.0" || r["url"] != "/api/agent/releases/"+itoa(id)+"/download" {
		t.Fatalf("x86_64 update: %v", r)
	}
	if r := check("os=windows&arch=aarch64&version=0.4.0"); r["version"] != "0.6.0-beta.1" {
		t.Fatalf("aarch64 update: %v", r)
	}
	if r := check("os=windows&arch=aarch64&version=0.6.0"); r["available"] != false {
		t.Fatalf("pre-release must be older than release: %v", r)
	}
	if r := check("os=windows&arch=x86_64&version=0.10.0"); r["available"] != false {
		t.Fatalf("numeric compare: %v", r)
	}
	if r := check("os=macos&arch=x86_64&version=0.1.0"); r["available"] != false {
		t.Fatalf("other OS: %v", r)
	}
	setEnabled("0.5.0/windows/x86_64", false)
	if r := check("os=windows&arch=x86_64&version=0.4.0"); r["id"] != float64(ids["0.5.0/windows/universal"]) {
		t.Fatalf("universal fallback: %v", r)
	}
	setEnabled("0.5.0/windows/x86_64", true)
	if res := get(t, http.DefaultClient, e.srv.URL+"/api/agent/update?os=windows&arch=x86_64&version=latest"); res.StatusCode != 400 {
		t.Fatalf("bad version query: %d", res.StatusCode)
	}

	res := get(t, http.DefaultClient, e.srv.URL+"/api/agent/releases/"+itoa(id)+"/download")
	body, _ := io.ReadAll(res.Body)
	if res.StatusCode != 200 || !bytes.Equal(body, payloads["0.5.0/windows/x86_64"]) || res.Header.Get("Content-Type") != "application/octet-stream" ||
		res.Header.Get("Content-Disposition") != `attachment; filename=redesk-0.5.0.exe` || res.Header.Get("Content-Length") != itoa(int64(len(body))) {
		t.Fatalf("download: %d %v %q", res.StatusCode, res.Header, body)
	}

	// Delete removes the stored file.
	sum := sha256.Sum256(payloads["0.9.0/linux/x86_64"])
	path := e.api.releaseDir() + "/" + hex.EncodeToString(sum[:])
	if _, err := os.Stat(path); err != nil {
		t.Fatal(err)
	}
	req, _ := http.NewRequest("DELETE", e.srv.URL+"/api/agent/releases/"+itoa(ids["0.9.0/linux/x86_64"]), nil)
	req.Header.Set("X-ReDesk", "1")
	req.Header.Set("Authorization", "Bearer "+tok)
	if res, err := http.DefaultClient.Do(req); err != nil || res.StatusCode != 200 {
		t.Fatalf("delete: %v", err)
	}
	if _, err := os.Stat(path); !os.IsNotExist(err) {
		t.Fatal("file not removed")
	}
	var n int
	e.st.DB.QueryRow(context.Background(), `SELECT count(*) FROM audit_log WHERE action LIKE 'release.%' AND actor_id=$1`, admin.ID).Scan(&n)
	if n < 8 {
		t.Fatalf("audit entries: %d", n)
	}
}

func itoa(n int64) string { return strconv.FormatInt(n, 10) }
