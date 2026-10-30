package ldapauth

import (
	"errors"
	"net"
	"os"
	"strings"
	"sync"
	"testing"

	ber "github.com/go-asn1-ber/asn1-ber"
	"github.com/go-ldap/ldap/v3"
)

func TestInGroups(t *testing.T) {
	groups := []string{"cn=Aegiron-Admins,ou=groups,dc=example,dc=com", "CN=Staff,OU=Groups,DC=corp,DC=local"}
	for _, w := range []string{"aegiron-admins", "cn=aegiron-admins,ou=groups,dc=example,dc=com", "staff"} {
		if !InGroups(groups, []string{w}) {
			t.Errorf("%q should match", w)
		}
	}
	for _, w := range []string{"admins", "groups", "dc=example,dc=com", ""} {
		if InGroups(groups, []string{w}) {
			t.Errorf("%q should not match", w)
		}
	}
}

func TestValidate(t *testing.T) {
	ok := Config{URL: "ldaps://dc.corp.local", BaseDN: "dc=corp,dc=local"}
	if err := ok.Validate(); err != nil {
		t.Fatal(err)
	}
	for name, c := range map[string]Config{
		"http":             {URL: "http://dc.corp.local", BaseDN: "dc=x"},
		"no base":          {URL: "ldaps://dc.corp.local"},
		"no placeholder":   {URL: "ldaps://dc", BaseDN: "dc=x", UserFilter: "(uid=admin)"},
		"broken filter":    {URL: "ldaps://dc", BaseDN: "dc=x", UserFilter: "(uid={username}"},
		"group without dn": {URL: "ldaps://dc", BaseDN: "dc=x", GroupFilter: "(member=x)"},
		"bad ca":           {URL: "ldaps://dc", BaseDN: "dc=x", CACert: "nope"},
		"credentials":      {URL: "ldaps://u:p@dc", BaseDN: "dc=x"},
	} {
		if err := c.Validate(); err == nil {
			t.Errorf("%s: accepted", name)
		}
	}
}

func TestFilterEscaping(t *testing.T) {
	c := Config{UserFilter: "(uid={username})"}
	if f := c.userFilter("*)(uid=admin"); f != `(uid=\2a\29\28uid=admin)` {
		t.Fatalf("filter injection not escaped: %s", f)
	}
}

func TestEmptyPasswordNeverBinds(t *testing.T) {
	// No server at all: an empty password must be refused before connecting.
	c := Config{URL: "ldaps://127.0.0.1:1", BaseDN: "dc=x"}
	if _, err := Authenticate(c, "somchai", ""); !errors.Is(err, ErrInvalidCredentials) {
		t.Fatalf("got %v", err)
	}
}

// A server that answers StartTLS with an error: the client must give up without sending
// anything else (no plaintext bind).
func TestStartTLSRefusedNoFallback(t *testing.T) {
	ln, err := net.Listen("tcp", "127.0.0.1:0")
	if err != nil {
		t.Fatal(err)
	}
	defer ln.Close()
	var mu sync.Mutex
	var tags []ber.Tag
	done := make(chan struct{})
	go func() {
		defer close(done)
		c, err := ln.Accept()
		if err != nil {
			return
		}
		defer c.Close()
		for {
			p, err := ber.ReadPacket(c)
			if err != nil || len(p.Children) < 2 {
				return
			}
			mu.Lock()
			tags = append(tags, p.Children[1].Tag)
			mu.Unlock()
			resp := ber.Encode(ber.ClassUniversal, ber.TypeConstructed, ber.TagSequence, nil, "LDAP Response")
			resp.AppendChild(ber.NewInteger(ber.ClassUniversal, ber.TypePrimitive, ber.TagInteger, p.Children[0].Value, "MessageID"))
			ext := ber.Encode(ber.ClassApplication, ber.TypeConstructed, ldap.ApplicationExtendedResponse, nil, "Extended Response")
			ext.AppendChild(ber.NewInteger(ber.ClassUniversal, ber.TypePrimitive, ber.TagEnumerated, int64(ldap.LDAPResultProtocolError), "resultCode"))
			ext.AppendChild(ber.NewString(ber.ClassUniversal, ber.TypePrimitive, ber.TagOctetString, "", "matchedDN"))
			ext.AppendChild(ber.NewString(ber.ClassUniversal, ber.TypePrimitive, ber.TagOctetString, "StartTLS not supported", "errorMessage"))
			resp.AppendChild(ext)
			c.Write(resp.Bytes())
		}
	}()
	c := Config{URL: "ldap://" + ln.Addr().String(), BaseDN: "dc=x", BindDN: "cn=svc", BindPassword: "secret"}
	_, err = Authenticate(c, "somchai", "pw")
	if !errors.Is(err, ErrUnavailable) || !strings.Contains(err.Error(), "StartTLS") {
		t.Fatalf("want StartTLS failure, got %v", err)
	}
	ln.Close()
	<-done
	mu.Lock()
	defer mu.Unlock()
	if len(tags) != 1 || tags[0] != ldap.ApplicationExtendedRequest {
		t.Fatalf("client sent %v; want only the StartTLS extended request", tags)
	}
}

// --- against a real directory (deploy/test/ldap-test.sh) ---

func liveConfig(t *testing.T, url string) Config {
	if url == "" {
		t.Skip("AEGIRON_TEST_LDAPS_URL / AEGIRON_TEST_LDAP_URL not set (run deploy/test/ldap-test.sh)")
	}
	ca, err := os.ReadFile(os.Getenv("AEGIRON_TEST_LDAP_CA"))
	if err != nil {
		t.Fatal(err)
	}
	return Config{
		URL: url, CACert: string(ca), BaseDN: "dc=aegiron,dc=test",
		BindDN: "cn=svc,ou=svcaccts,ou=users,dc=aegiron,dc=test", BindPassword: "Svc-Pass-1!",
		UserFilter: "(&(objectClass=posixAccount)(|(cn={username})(mail={username})))",
		EmailAttr:  "mail", NameAttr: "givenName",
	}
}

func TestLDAPLive(t *testing.T) {
	for _, url := range []string{os.Getenv("AEGIRON_TEST_LDAPS_URL"), os.Getenv("AEGIRON_TEST_LDAP_URL")} {
		t.Run(url, func(t *testing.T) {
			c := liveConfig(t, url)
			u, err := Authenticate(c, "somchai", "Somchai-Pass-1!")
			if err != nil {
				t.Fatal(err)
			}
			if u.Email != "somchai@aegiron.test" || u.Name != "Somchai" || !strings.HasPrefix(strings.ToLower(u.DN), "cn=somchai,") {
				t.Fatalf("user: %+v", u)
			}
			if !InGroups(u.Groups, []string{"aegiron-admins"}) || !InGroups(u.Groups, []string{"staff"}) {
				t.Fatalf("groups: %v", u.Groups)
			}
			if u, err := Authenticate(c, "somchai@aegiron.test", "Somchai-Pass-1!"); err != nil || u.Email != "somchai@aegiron.test" {
				t.Fatalf("by mail: %v %+v", err, u)
			}
			if _, err := Authenticate(c, "somchai", "wrong-password"); !errors.Is(err, ErrInvalidCredentials) {
				t.Fatalf("wrong password: %v", err)
			}
			if _, err := Authenticate(c, "nobody", "Somchai-Pass-1!"); !errors.Is(err, ErrInvalidCredentials) {
				t.Fatalf("unknown user: %v", err)
			}
			if _, err := Authenticate(c, "*", "Somchai-Pass-1!"); !errors.Is(err, ErrInvalidCredentials) {
				t.Fatalf("wildcard user: %v", err)
			}
			if u, err := Authenticate(c, "malee", "Malee-Pass-1!"); err != nil || InGroups(u.Groups, []string{"aegiron-admins"}) {
				t.Fatalf("malee: %v %+v", err, u)
			}
			bad := c
			bad.BindPassword = "nope"
			if _, err := Authenticate(bad, "somchai", "Somchai-Pass-1!"); !errors.Is(err, ErrUnavailable) {
				t.Fatalf("bad service password: %v", err)
			}
			if u, err := Lookup(c, "malee"); err != nil || u == nil || u.Email != "malee@aegiron.test" {
				t.Fatalf("lookup: %v %+v", err, u)
			}
		})
	}
}

func TestLDAPLiveRejectsUntrustedCertificate(t *testing.T) {
	c := liveConfig(t, os.Getenv("AEGIRON_TEST_LDAPS_URL"))
	c.CACert = ""
	if _, err := Authenticate(c, "somchai", "Somchai-Pass-1!"); !errors.Is(err, ErrUnavailable) || !strings.Contains(err.Error(), "certificate") {
		t.Fatalf("ldaps with unknown CA: %v", err)
	}
	c = liveConfig(t, os.Getenv("AEGIRON_TEST_LDAP_URL"))
	c.CACert = ""
	if _, err := Authenticate(c, "somchai", "Somchai-Pass-1!"); !errors.Is(err, ErrUnavailable) || !strings.Contains(err.Error(), "certificate") {
		t.Fatalf("StartTLS with unknown CA: %v", err)
	}
}

func TestLDAPLiveGroupFilter(t *testing.T) {
	c := liveConfig(t, os.Getenv("AEGIRON_TEST_LDAPS_URL"))
	c.GroupFilter = "(&(objectClass=posixGroup)(uniqueMember={dn}))"
	u, err := Authenticate(c, "somchai", "Somchai-Pass-1!")
	if err != nil {
		t.Fatal(err)
	}
	// memberOf names ou=...,ou=groups; the group search finds ou=...,ou=users entries.
	found := false
	for _, g := range u.Groups {
		found = found || strings.EqualFold(g, "ou=aegiron-admins,ou=users,dc=aegiron,dc=test")
	}
	if !found {
		t.Fatalf("group search: %v", u.Groups)
	}
}
