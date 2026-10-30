package api

import (
	"crypto/ed25519"
	"crypto/rand"
	"crypto/rsa"
	"crypto/x509"
	"encoding/json"
	"encoding/pem"
	"strings"
	"testing"

	"golang.org/x/crypto/ssh"

	"aegiron/server/internal/config"
	"aegiron/server/internal/store"
)

func TestNormalizePrivateKey(t *testing.T) {
	_, edKey, _ := ed25519.GenerateKey(rand.Reader)
	openssh, err := ssh.MarshalPrivateKey(edKey, "test")
	if err != nil {
		t.Fatal(err)
	}
	locked, err := ssh.MarshalPrivateKeyWithPassphrase(edKey, "test", []byte("pass"))
	if err != nil {
		t.Fatal(err)
	}
	rsaKey, _ := rsa.GenerateKey(rand.Reader, 2048)
	pkcs1 := &pem.Block{Type: "RSA PRIVATE KEY", Bytes: x509.MarshalPKCS1PrivateKey(rsaKey)}
	// Deprecated API, but legacy encrypted PEM is exactly what must be rejected.
	legacy, _ := x509.EncryptPEMBlock(rand.Reader, "RSA PRIVATE KEY", pkcs1.Bytes, []byte("pass"), x509.PEMCipherAES256)

	for name, k := range map[string]string{
		"openssh ed25519":    string(pem.EncodeToMemory(openssh)),
		"pem rsa":            string(pem.EncodeToMemory(pkcs1)),
		"crlf + blank lines": "\r\n" + strings.ReplaceAll(string(pem.EncodeToMemory(openssh)), "\n", "\r\n") + "\r\n",
	} {
		got, err := normalizePrivateKey(k)
		if err != nil {
			t.Errorf("%s rejected: %v", name, err)
			continue
		}
		if strings.Contains(got, "\r") || !strings.HasPrefix(got, "-----BEGIN") || !strings.HasSuffix(got, "-----\n") {
			t.Errorf("%s not normalized: %q", name, got)
		}
	}
	for name, k := range map[string]string{
		"password":          "hunter2",
		"public key":        "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIOMqqnkVzrm0SdG6UOoqKLsabgH5C9okWi0dh2l9GKJl test",
		"certificate":       "-----BEGIN CERTIFICATE-----\nAAAA\n-----END CERTIFICATE-----\n",
		"openssh encrypted": string(pem.EncodeToMemory(locked)),
		"pem encrypted":     string(pem.EncodeToMemory(legacy)),
		"truncated openssh": "-----BEGIN OPENSSH PRIVATE KEY-----\nb3BlbnNzaC1rZXktdjEAAAAA/w==\n-----END OPENSSH PRIVATE KEY-----\n",
	} {
		if _, err := normalizePrivateKey(k); err == nil {
			t.Errorf("%s accepted", name)
		}
	}
}

func TestNormalizeEndpointSSHOptions(t *testing.T) {
	e, err := normalizeEndpoint(&endpointReq{Name: "a", Provider: "ssh", Address: "h", Options: json.RawMessage(`{"auth":"key"}`)})
	if err != nil || !sshKeyAuth(e) {
		t.Fatalf("key auth: %v %s", err, e.Options)
	}
	e, err = normalizeEndpoint(&endpointReq{Name: "a", Provider: "ssh", Address: "h", Options: json.RawMessage(`{"auth":"password"}`)})
	if err != nil || sshKeyAuth(e) || string(e.Options) != "{}" {
		t.Fatalf("password auth: %v %s", err, e.Options)
	}
	for _, o := range []string{`{"auth":"gssapi"}`, `{"private_key":"x"}`} {
		if _, err := normalizeEndpoint(&endpointReq{Name: "a", Provider: "ssh", Address: "h", Options: json.RawMessage(o)}); err == nil {
			t.Errorf("accepted %s", o)
		}
	}
	// Options of another provider never turn into SSH key auth.
	e, _ = normalizeEndpoint(&endpointReq{Name: "a", Provider: "vnc", Address: "h", Options: json.RawMessage(`{"auth":"key"}`)})
	if sshKeyAuth(e) {
		t.Fatal("vnc with auth=key")
	}
}

func TestGatewayParams(t *testing.T) {
	port := 22
	a := &API{cfg: config.Config{GatewayRecordingPath: "/recordings"}}
	pw := &store.Endpoint{Provider: "ssh", Address: "h", Port: &port, Username: "u", Options: json.RawMessage(`{}`)}
	p := a.gatewayParams(pw, "secret", "SID")
	if p["password"] != "secret" || p["private-key"] != "" {
		t.Fatalf("password auth params: %v", p)
	}
	// Recording and typescript must not share a name, or guacd writes the typescript to "SID.1".
	if p["recording-name"] != "SID" || p["typescript-name"] == p["recording-name"] || p["typescript-path"] != "/recordings" {
		t.Fatalf("recording names: %v", p)
	}
	key := &store.Endpoint{Provider: "ssh", Address: "h", Port: &port, Username: "u", Options: json.RawMessage(`{"auth":"key"}`)}
	p = a.gatewayParams(key, "-----BEGIN OPENSSH PRIVATE KEY-----\n", "SID")
	if p["private-key"] == "" || p["password"] != "" {
		t.Fatalf("key auth params: %v", p)
	}
	vnc := &store.Endpoint{Provider: "vnc", Address: "h", Port: &port, Options: json.RawMessage(`{}`)}
	if p = a.gatewayParams(vnc, "pw", "SID"); p["typescript-path"] != "" || p["recording-name"] != "SID" {
		t.Fatalf("vnc params: %v", p)
	}
	a.cfg.GatewayRecordingPath = ""
	if p = a.gatewayParams(pw, "pw", "SID"); p["recording-path"] != "" || p["typescript-path"] != "" {
		t.Fatalf("recording off: %v", p)
	}
}

func TestMayReleaseSecret(t *testing.T) {
	creator, member := int64(1), int64(2)
	admin := &store.User{ID: 3, Role: "admin"}
	owner := &store.User{ID: creator, Role: "user"}
	shared := &store.User{ID: member, Role: "user"}
	rdp := &store.Endpoint{Provider: "rdp", CreatedBy: &creator, Options: json.RawMessage(`{}`)}
	anydesk := &store.Endpoint{Provider: "anydesk", CreatedBy: &creator, Options: json.RawMessage(`{}`)}
	key := &store.Endpoint{Provider: "ssh", CreatedBy: &creator, Options: json.RawMessage(`{"auth":"key"}`)}
	cases := []struct {
		u       *store.User
		e       *store.Endpoint
		gateway bool
		want    bool
	}{
		{shared, rdp, true, false}, // group member: use the gateway instead
		{owner, rdp, true, true},
		{admin, rdp, true, true},
		{shared, rdp, false, true}, // no gateway: the native client is the only way in
		{shared, anydesk, true, true},
		{admin, key, true, false}, // private keys never leave the server
		{owner, key, false, false},
	}
	for i, c := range cases {
		if got := mayReleaseSecret(c.u, c.e, c.gateway); got != c.want {
			t.Errorf("case %d: got %v", i, got)
		}
	}
}
