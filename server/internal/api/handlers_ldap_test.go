package api

import (
	"context"
	"os"
	"strings"
	"testing"

	"aegiron/server/internal/store"
)

func TestLDAPSettingsValidation(t *testing.T) {
	e := newTestEnv(t)
	adm := e.admin()
	for name, body := range map[string]map[string]any{
		"plain http":       {"url": "http://dc.corp.local", "base_dn": "dc=corp", "enabled": true},
		"no base dn":       {"url": "ldaps://dc.corp.local", "enabled": true},
		"filter injection": {"url": "ldaps://dc.corp.local", "base_dn": "dc=corp", "user_filter": "(uid=admin)", "enabled": true},
		"bad attribute":    {"url": "ldaps://dc.corp.local", "base_dn": "dc=corp", "email_attr": "mail)(x"},
	} {
		if st, _ := e.post(nil, "PUT", "/api/ldap/settings", adm, body); st != 400 {
			t.Errorf("%s: %d", name, st)
		}
	}
	if st, _ := e.post(nil, "PUT", "/api/ldap/settings", adm, map[string]any{"url": "ldap://dc.corp.local", "base_dn": "dc=corp", "bind_password": "s3cret"}); st != 200 {
		t.Fatalf("ldap:// (StartTLS) refused: %d", st)
	}
	st, r := e.post(nil, "GET", "/api/ldap/settings", adm, nil)
	if s := r["settings"].(map[string]any); st != 200 || s["has_bind_password"] != true || s["bind_password"] != nil {
		t.Fatalf("get: %v", r)
	}
	var sealed []byte
	e.st.DB.QueryRow(context.Background(), `SELECT bind_password FROM ldap_settings`).Scan(&sealed)
	if strings.Contains(string(sealed), "s3cret") {
		t.Fatal("bind password stored in clear")
	}
	e.st.DB.Exec(context.Background(), `DELETE FROM ldap_settings`)
}

// TestLDAPLogin runs against the directory from deploy/test/ldap-test.sh.
func TestLDAPLogin(t *testing.T) {
	url := os.Getenv("AEGIRON_TEST_LDAPS_URL")
	if url == "" {
		t.Skip("AEGIRON_TEST_LDAPS_URL not set (run deploy/test/ldap-test.sh)")
	}
	ca, err := os.ReadFile(os.Getenv("AEGIRON_TEST_LDAP_CA"))
	if err != nil {
		t.Fatal(err)
	}
	e := newTestEnv(t)
	ctx := context.Background()
	e.st.DB.Exec(ctx, `DELETE FROM users WHERE email LIKE '%@aegiron.test'`)
	e.st.DB.Exec(ctx, `DELETE FROM audit_log WHERE target LIKE '%@aegiron.test' OR target IN ('somchai', 'malee', 'outsider')`)
	t.Cleanup(func() {
		e.st.DB.Exec(ctx, `DELETE FROM ldap_settings`)
		e.st.DB.Exec(ctx, `DELETE FROM users WHERE email LIKE '%@aegiron.test'`)
	})
	adm := e.admin()
	settings := map[string]any{
		"url": url, "ca_cert": string(ca), "base_dn": "dc=aegiron,dc=test",
		"bind_dn": "cn=svc,ou=svcaccts,ou=users,dc=aegiron,dc=test", "bind_password": "Svc-Pass-1!",
		"user_filter": "(&(objectClass=posixAccount)(|(cn={username})(mail={username})))",
		"email_attr":  "mail", "name_attr": "givenName",
		"admin_groups": []string{"aegiron-admins"}, "user_groups": []string{"staff"},
		"auto_create": true, "enabled": true,
	}
	if st, r := e.post(nil, "PUT", "/api/ldap/settings", adm, settings); st != 200 {
		t.Fatalf("settings: %d %v", st, r)
	}
	if st, r := e.post(nil, "POST", "/api/ldap/test", adm, map[string]any{"username": "somchai"}); st != 200 || r["ok"] != true || r["admin"] != true {
		t.Fatalf("test: %d %v", st, r)
	}
	var info map[string]any
	if _, info = e.post(nil, "GET", "/api/server-info", "", nil); info["ldap"] != true {
		t.Fatalf("server-info: %v", info)
	}

	login := func(name, pw string) (int, map[string]any) {
		return e.post(nil, "POST", "/api/auth/login", "", map[string]any{"email": name, "password": pw})
	}
	// Directory user name, admin through the group; account created on first sign-in.
	st, r := login("somchai", "Somchai-Pass-1!")
	if st != 200 {
		t.Fatalf("somchai: %d %v", st, r)
	}
	u := r["user"].(map[string]any)
	if u["email"] != "somchai@aegiron.test" || u["role"] != "admin" || u["auth_source"] != "ldap" || u["name"] != "Somchai" {
		t.Fatalf("somchai user: %v", u)
	}
	if e.auditCount("user.created", "somchai@aegiron.test") != 1 {
		t.Fatal("creation not audited")
	}
	// By email too; the account is reused.
	if st, r := login("somchai@aegiron.test", "Somchai-Pass-1!"); st != 200 || r["user"].(map[string]any)["id"] != u["id"] {
		t.Fatalf("by email: %d %v", st, r)
	}
	if st, _ := login("somchai", "wrong"); st != 401 {
		t.Fatalf("wrong password: %d", st)
	}
	// Plain member: role user.
	st, r = login("malee", "Malee-Pass-1!")
	if st != 200 || r["user"].(map[string]any)["role"] != "user" {
		t.Fatalf("malee: %d %v", st, r)
	}
	// Not in the allowed groups.
	if st, _ := login("outsider", "Malee-Pass-1!"); st != 401 {
		t.Fatalf("outsider: %d", st)
	}
	if e.auditCount("auth.login_failed", "outsider") != 1 {
		t.Fatal("refusal not audited")
	}
	// The desktop app signs in the same way.
	st, r = e.post(nil, "POST", "/api/agent/login", "", map[string]any{"email": "malee", "password": "Malee-Pass-1!", "device": "test-pc"})
	if st != 200 || r["token"] == nil || r["enroll_token"] == nil {
		t.Fatalf("app login: %d %v", st, r)
	}
	tok := r["token"].(string)
	// Directory accounts cannot set a local password.
	if st, _ := e.post(nil, "POST", "/api/auth/password", tok, map[string]any{"current": "Malee-Pass-1!", "new": "another-password-1"}); st != 409 {
		t.Fatalf("password change: %d", st)
	}
	// ...but can re-authenticate with the directory password (e.g. turning on email codes).
	if !e.api.verifyOwnPassword(ctx, mustUser(t, e, "malee@aegiron.test"), "Malee-Pass-1!") {
		t.Fatal("re-authentication against the directory failed")
	}

	// A local account with the same email is never taken over by the directory entry.
	e.st.DB.Exec(ctx, `UPDATE users SET auth_source='local' WHERE email='malee@aegiron.test'`)
	if st, _ := login("malee", "Malee-Pass-1!"); st != 401 {
		t.Fatalf("local account takeover: %d", st)
	}
	// Removing someone from the admin group demotes them at the next sign-in.
	settings["admin_groups"] = []string{"nobody"}
	settings["bind_password"] = nil
	e.post(nil, "PUT", "/api/ldap/settings", adm, settings)
	if st, r := login("somchai", "Somchai-Pass-1!"); st != 200 || r["user"].(map[string]any)["role"] != "user" {
		t.Fatalf("demotion: %d %v", st, r)
	}
	// LDAP off: directory accounts cannot sign in at all.
	settings["enabled"] = false
	e.post(nil, "PUT", "/api/ldap/settings", adm, settings)
	if st, _ := login("somchai@aegiron.test", "Somchai-Pass-1!"); st != 401 {
		t.Fatalf("ldap disabled: %d", st)
	}
}

func mustUser(t *testing.T, e *testEnv, email string) *store.User {
	u, err := e.st.GetUserByEmail(context.Background(), email)
	if err != nil {
		t.Fatal(err)
	}
	return u
}
