package mail

import (
	"context"
	"strings"
	"testing"

	"aegiron/server/internal/mail/mailtest"
)

func TestSendSTARTTLS(t *testing.T) {
	srv := mailtest.New(t, true)
	cfg := Config{Host: "127.0.0.1", Port: srv.Port(), Security: "starttls", CACert: srv.CAPEM, Username: "u", Password: "p", From: "aegiron@example.com"}
	err := Send(context.Background(), cfg, Message{To: []string{"admin@example.com"}, Subject: "รหัสยืนยัน\r\nBcc: x@evil", Body: "รหัสของคุณคือ 123456\n"})
	if err != nil {
		t.Fatal(err)
	}
	m := srv.Wait(t, 0)
	if !m.TLS || m.Auth != "\x00u\x00p" || len(m.To) != 1 || m.To[0] != "admin@example.com" {
		t.Fatalf("message: %+v", m)
	}
	if strings.Contains(m.Data, "\r\nBcc:") || !strings.Contains(m.Data, "Subject: =?utf-8?q?") {
		t.Fatalf("headers: %s", m.Data)
	}
	if !strings.Contains(m.Body(), "123456") {
		t.Fatalf("body: %q", m.Body())
	}
}

func TestSendRefusesWithoutSTARTTLS(t *testing.T) {
	srv := mailtest.New(t, false)
	cfg := Config{Host: "127.0.0.1", Port: srv.Port(), Security: "starttls", CACert: srv.CAPEM, Username: "u", Password: "p", From: "a@example.com"}
	err := Send(context.Background(), cfg, Message{To: []string{"b@example.com"}, Subject: "x", Body: "y"})
	if err == nil || !strings.Contains(err.Error(), "STARTTLS") {
		t.Fatalf("want STARTTLS refusal, got %v", err)
	}
	if len(srv.Messages()) != 0 {
		t.Fatal("message sent without TLS")
	}
}

func TestSendVerifiesCertificate(t *testing.T) {
	srv := mailtest.New(t, true)
	cfg := Config{Host: "127.0.0.1", Port: srv.Port(), Security: "starttls", Username: "u", Password: "p", From: "a@example.com"}
	err := Send(context.Background(), cfg, Message{To: []string{"b@example.com"}, Subject: "x", Body: "y"})
	if err == nil || !strings.Contains(err.Error(), "certificate") {
		t.Fatalf("want certificate error, got %v", err)
	}
	if len(srv.Messages()) != 0 {
		t.Fatal("message sent to an unverified server")
	}
}

func TestValidate(t *testing.T) {
	ok := Config{Host: "smtp.example.com", Port: 587, Security: "starttls", From: "a@example.com"}
	if err := ok.Validate(); err != nil {
		t.Fatal(err)
	}
	for name, c := range map[string]Config{
		"plain remote":  {Host: "smtp.example.com", Port: 25, Security: "none", From: "a@example.com"},
		"bad security":  {Host: "smtp.example.com", Port: 25, Security: "ssl", From: "a@example.com"},
		"bad from":      {Host: "smtp.example.com", Port: 25, Security: "tls", From: "A <a@example.com>"},
		"header inject": {Host: "smtp.example.com", Port: 25, Security: "tls", From: "a@example.com\r\nBcc: b@x"},
		"bad port":      {Host: "smtp.example.com", Port: 0, Security: "tls", From: "a@example.com"},
		"bad ca":        {Host: "smtp.example.com", Port: 25, Security: "tls", From: "a@example.com", CACert: "nope"},
	} {
		if err := c.Validate(); err == nil {
			t.Errorf("%s: accepted", name)
		}
	}
	local := Config{Host: "127.0.0.1", Port: 25, Security: "none", From: "a@example.com"}
	if err := local.Validate(); err != nil {
		t.Fatalf("loopback plain: %v", err)
	}
}
