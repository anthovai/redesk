// Package mail sends plain-text email through an SMTP relay.
//
// Transport security is never downgraded: "starttls" (the default) fails when the server does
// not offer STARTTLS, "tls" is implicit TLS (port 465), and "none" is only accepted for a relay
// on this machine. Certificates are verified against the system roots plus an optional CA.
package mail

import (
	"context"
	"crypto/rand"
	"crypto/tls"
	"crypto/x509"
	"encoding/base64"
	"encoding/hex"
	"errors"
	"fmt"
	"mime"
	"net"
	"net/mail"
	"net/smtp"
	"strconv"
	"strings"
	"time"
)

type Config struct {
	Host     string
	Port     int
	Security string // starttls | tls | none
	CACert   string // extra trusted CA (PEM)
	Username string
	Password string
	From     string
}

// Message is one email; Body is plain UTF-8 text.
type Message struct {
	To      []string
	Subject string
	Body    string
}

// IsLoopback reports whether host is this machine (where plain SMTP is acceptable).
func IsLoopback(host string) bool {
	if host == "localhost" {
		return true
	}
	ip := net.ParseIP(host)
	return ip != nil && ip.IsLoopback()
}

// ValidAddress accepts a bare address (no display name), without control characters.
func ValidAddress(a string) bool {
	if strings.ContainsAny(a, "\r\n<>\"") || len(a) > 254 {
		return false
	}
	p, err := mail.ParseAddress(a)
	return err == nil && p.Address == a
}

// Validate checks a configuration before it is saved or used.
func (c *Config) Validate() error {
	if c.Host == "" || strings.ContainsAny(c.Host, " \r\n/") {
		return errors.New("invalid SMTP host")
	}
	if c.Port < 1 || c.Port > 65535 {
		return errors.New("invalid SMTP port")
	}
	switch c.Security {
	case "starttls", "tls":
	case "none":
		if !IsLoopback(c.Host) {
			return errors.New("unencrypted SMTP is only allowed to a relay on this machine (localhost)")
		}
	default:
		return errors.New("security must be starttls, tls or none")
	}
	if !ValidAddress(c.From) {
		return errors.New("invalid sender address")
	}
	if strings.ContainsAny(c.Username+c.Password, "\r\n") {
		return errors.New("invalid characters in credentials")
	}
	if c.CACert != "" {
		if _, err := certPool(c.CACert); err != nil {
			return err
		}
	}
	return nil
}

func certPool(pemCA string) (*x509.CertPool, error) {
	pool, err := x509.SystemCertPool()
	if err != nil || pool == nil {
		pool = x509.NewCertPool()
	}
	if pemCA != "" && !pool.AppendCertsFromPEM([]byte(pemCA)) {
		return nil, errors.New("CA certificate is not valid PEM")
	}
	return pool, nil
}

// Send delivers one message to all recipients.
func Send(ctx context.Context, c Config, m Message) error {
	if err := c.Validate(); err != nil {
		return err
	}
	if len(m.To) == 0 {
		return errors.New("no recipients")
	}
	for _, to := range m.To {
		if !ValidAddress(to) {
			return fmt.Errorf("invalid recipient %q", to)
		}
	}
	pool, err := certPool(c.CACert)
	if err != nil {
		return err
	}
	tlsCfg := &tls.Config{ServerName: c.Host, RootCAs: pool, MinVersion: tls.VersionTLS12}
	addr := net.JoinHostPort(c.Host, strconv.Itoa(c.Port))
	if _, ok := ctx.Deadline(); !ok {
		var cancel context.CancelFunc
		ctx, cancel = context.WithTimeout(ctx, 30*time.Second)
		defer cancel()
	}
	d := &net.Dialer{}
	var conn net.Conn
	if c.Security == "tls" {
		conn, err = (&tls.Dialer{NetDialer: d, Config: tlsCfg}).DialContext(ctx, "tcp", addr)
	} else {
		conn, err = d.DialContext(ctx, "tcp", addr)
	}
	if err != nil {
		return fmt.Errorf("connect %s: %w", addr, err)
	}
	if dl, ok := ctx.Deadline(); ok {
		conn.SetDeadline(dl)
	}
	cl, err := smtp.NewClient(conn, c.Host)
	if err != nil {
		conn.Close()
		return fmt.Errorf("smtp greeting: %w", err)
	}
	defer cl.Close()
	if err := cl.Hello("aegiron"); err != nil {
		return err
	}
	if c.Security == "starttls" {
		if ok, _ := cl.Extension("STARTTLS"); !ok {
			return errors.New("the SMTP server does not offer STARTTLS (refusing to send unencrypted)")
		}
		if err := cl.StartTLS(tlsCfg); err != nil {
			return fmt.Errorf("STARTTLS: %w", err)
		}
	}
	if c.Username != "" {
		// PlainAuth itself refuses to send credentials without TLS (except to localhost).
		if err := cl.Auth(smtp.PlainAuth("", c.Username, c.Password, c.Host)); err != nil {
			return fmt.Errorf("SMTP authentication: %w", err)
		}
	}
	if err := cl.Mail(c.From); err != nil {
		return err
	}
	for _, to := range m.To {
		if err := cl.Rcpt(to); err != nil {
			return fmt.Errorf("recipient %s: %w", to, err)
		}
	}
	w, err := cl.Data()
	if err != nil {
		return err
	}
	if _, err := w.Write(compose(c.From, m)); err != nil {
		return err
	}
	if err := w.Close(); err != nil {
		return err
	}
	return cl.Quit()
}

// compose builds the RFC 5322 message: UTF-8 subject (RFC 2047) and a base64 body.
func compose(from string, m Message) []byte {
	var b strings.Builder
	id := make([]byte, 12)
	rand.Read(id)
	_, domain, _ := strings.Cut(from, "@")
	subject := strings.NewReplacer("\r", " ", "\n", " ").Replace(m.Subject)
	fmt.Fprintf(&b, "From: %s\r\n", from)
	fmt.Fprintf(&b, "To: %s\r\n", strings.Join(m.To, ", "))
	fmt.Fprintf(&b, "Subject: %s\r\n", mime.QEncoding.Encode("utf-8", subject))
	fmt.Fprintf(&b, "Date: %s\r\n", time.Now().Format(time.RFC1123Z))
	fmt.Fprintf(&b, "Message-ID: <%s@%s>\r\n", hex.EncodeToString(id), domain)
	b.WriteString("MIME-Version: 1.0\r\nContent-Type: text/plain; charset=utf-8\r\nContent-Transfer-Encoding: base64\r\n\r\n")
	enc := base64.StdEncoding.EncodeToString([]byte(strings.ReplaceAll(m.Body, "\n", "\r\n")))
	for len(enc) > 76 {
		b.WriteString(enc[:76] + "\r\n")
		enc = enc[76:]
	}
	b.WriteString(enc + "\r\n")
	return []byte(b.String())
}
