// Package mailtest is an in-process SMTP server for tests: optional STARTTLS with a throwaway
// certificate for 127.0.0.1, AUTH PLAIN, and a record of every message.
package mailtest

import (
	"bufio"
	"crypto/ecdsa"
	"crypto/elliptic"
	"crypto/rand"
	"crypto/tls"
	"crypto/x509"
	"crypto/x509/pkix"
	"encoding/base64"
	"encoding/pem"
	"math/big"
	"net"
	"strings"
	"sync"
	"testing"
	"time"
)

// Message is one delivered message.
type Message struct {
	Auth string   // decoded AUTH PLAIN ("" if none)
	TLS  bool     // STARTTLS was used
	To   []string // RCPT TO
	Data string   // raw message
}

// Body returns the decoded base64 text body.
func (m Message) Body() string {
	i := strings.Index(m.Data, "\r\n\r\n")
	if i < 0 {
		return ""
	}
	b, _ := base64.StdEncoding.DecodeString(strings.ReplaceAll(m.Data[i+4:], "\r\n", ""))
	return string(b)
}

type Server struct {
	ln       net.Listener
	cert     tls.Certificate
	CAPEM    string // trust this to accept the server's certificate
	StartTLS bool
	mu       sync.Mutex
	msgs     []Message
	notify   chan struct{}
}

// New starts a server on 127.0.0.1; starttls false means the server never offers STARTTLS.
func New(t testing.TB, starttls bool) *Server {
	t.Helper()
	key, _ := ecdsa.GenerateKey(elliptic.P256(), rand.Reader)
	tmpl := &x509.Certificate{
		SerialNumber: big.NewInt(1), Subject: pkix.Name{CommonName: "localhost"},
		NotBefore: time.Now().Add(-time.Hour), NotAfter: time.Now().Add(time.Hour),
		DNSNames: []string{"localhost"}, IPAddresses: []net.IP{net.ParseIP("127.0.0.1")},
		IsCA: true, BasicConstraintsValid: true,
		KeyUsage: x509.KeyUsageDigitalSignature | x509.KeyUsageCertSign, ExtKeyUsage: []x509.ExtKeyUsage{x509.ExtKeyUsageServerAuth},
	}
	der, err := x509.CreateCertificate(rand.Reader, tmpl, tmpl, &key.PublicKey, key)
	if err != nil {
		t.Fatal(err)
	}
	ln, err := net.Listen("tcp", "127.0.0.1:0")
	if err != nil {
		t.Fatal(err)
	}
	s := &Server{
		ln: ln, cert: tls.Certificate{Certificate: [][]byte{der}, PrivateKey: key}, StartTLS: starttls,
		CAPEM:  string(pem.EncodeToMemory(&pem.Block{Type: "CERTIFICATE", Bytes: der})),
		notify: make(chan struct{}, 100),
	}
	t.Cleanup(func() { ln.Close() })
	go s.serve()
	return s
}

func (s *Server) Port() int { return s.ln.Addr().(*net.TCPAddr).Port }

// Messages returns what was delivered so far.
func (s *Server) Messages() []Message {
	s.mu.Lock()
	defer s.mu.Unlock()
	return append([]Message(nil), s.msgs...)
}

// Wait returns the n-th message (0-based), waiting up to 10 s for it.
func (s *Server) Wait(t testing.TB, n int) Message {
	t.Helper()
	deadline := time.After(10 * time.Second)
	for {
		if m := s.Messages(); len(m) > n {
			return m[n]
		}
		select {
		case <-s.notify:
		case <-deadline:
			t.Fatalf("no message #%d after 10s (got %d)", n+1, len(s.Messages()))
		}
	}
}

func (s *Server) serve() {
	for {
		c, err := s.ln.Accept()
		if err != nil {
			return
		}
		go s.handle(c)
	}
}

func (s *Server) handle(c net.Conn) {
	defer func() { c.Close() }()
	r, w := bufio.NewReader(c), net.Conn(c)
	say := func(line string) { w.Write([]byte(line + "\r\n")) }
	say("220 mailtest ESMTP")
	var cur Message
	for {
		line, err := r.ReadString('\n')
		if err != nil {
			return
		}
		cmd := strings.TrimRight(line, "\r\n")
		up := strings.ToUpper(cmd)
		switch {
		case strings.HasPrefix(up, "EHLO") || strings.HasPrefix(up, "HELO"):
			say("250-mailtest")
			if s.StartTLS && !cur.TLS {
				say("250 STARTTLS")
			} else {
				say("250 AUTH PLAIN")
			}
		case up == "STARTTLS" && s.StartTLS && !cur.TLS:
			say("220 go ahead")
			tc := tls.Server(c, &tls.Config{Certificates: []tls.Certificate{s.cert}})
			if tc.Handshake() != nil {
				return
			}
			c, r, w, cur.TLS = tc, bufio.NewReader(tc), tc, true
		case strings.HasPrefix(up, "AUTH PLAIN "):
			b, _ := base64.StdEncoding.DecodeString(cmd[len("AUTH PLAIN "):])
			cur.Auth = string(b)
			say("235 ok")
		case strings.HasPrefix(up, "MAIL FROM:"):
			say("250 ok")
		case strings.HasPrefix(up, "RCPT TO:"):
			cur.To = append(cur.To, strings.Trim(cmd[len("RCPT TO:"):], "<> "))
			say("250 ok")
		case up == "DATA":
			say("354 go")
			var b strings.Builder
			for {
				l, err := r.ReadString('\n')
				if err != nil {
					return
				}
				if l == ".\r\n" {
					break
				}
				b.WriteString(l)
			}
			cur.Data = b.String()
			s.mu.Lock()
			s.msgs = append(s.msgs, cur)
			s.mu.Unlock()
			select {
			case s.notify <- struct{}{}:
			default:
			}
			cur = Message{TLS: cur.TLS, Auth: cur.Auth}
			say("250 queued")
		case up == "QUIT":
			say("221 bye")
			return
		default:
			say("502 unsupported")
		}
	}
}
