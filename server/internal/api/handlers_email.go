package api

// Outgoing mail: login verification codes (second factor by email) and admin alerts
// (new device registered, sign-ins locked after repeated failures).

import (
	"context"
	"crypto/rand"
	"crypto/sha256"
	"crypto/subtle"
	"errors"
	"fmt"
	"math/big"
	"net/http"
	"strconv"
	"strings"
	"time"

	"aegiron/server/internal/auth"
	"aegiron/server/internal/ldapauth"
	"aegiron/server/internal/mail"
	"aegiron/server/internal/store"
)

const (
	emailCodeTTL      = 10 * time.Minute
	emailCodeResend   = time.Minute // a new code is mailed at most this often
	emailCodeAttempts = 5
)

// errMailUnavailable: email is needed (e.g. a code) but mail is not configured or failed.
var errMailUnavailable = errors.New("mail unavailable")

// mailConfig returns the SMTP configuration when mail is set up and switched on.
func (a *API) mailConfig(ctx context.Context) (*mail.Config, *store.SMTPSettings, error) {
	m, err := a.store.GetSMTPSettings(ctx)
	if errors.Is(err, store.ErrNotFound) {
		return nil, nil, nil
	} else if err != nil {
		return nil, nil, err
	}
	if !m.Enabled || m.Host == "" {
		return nil, m, nil
	}
	pw, err := a.vault.Open(m.Password)
	if err != nil {
		return nil, nil, err
	}
	return &mail.Config{Host: m.Host, Port: m.Port, Security: m.Security, CACert: m.CACert, Username: m.Username, Password: pw, From: m.From}, m, nil
}

func (a *API) sendMail(ctx context.Context, msg mail.Message) error {
	cfg, _, err := a.mailConfig(ctx)
	if err != nil {
		return err
	}
	if cfg == nil {
		return errMailUnavailable
	}
	ctx, cancel := context.WithTimeout(ctx, 30*time.Second)
	defer cancel()
	return mail.Send(ctx, *cfg, msg)
}

// --- second factor by email ---

func emailCodeHash(userID int64, code string) []byte {
	h := sha256.Sum256([]byte("aegiron-email-code-v1\n" + strconv.FormatInt(userID, 10) + "\n" + code))
	return h[:]
}

// issueEmailCode mails a fresh 6-digit code unless one was sent less than a minute ago.
func (a *API) issueEmailCode(ctx context.Context, u *store.User, ip string) error {
	if c, err := a.store.GetEmailCode(ctx, u.ID); err == nil && time.Since(c.SentAt) < emailCodeResend && time.Now().Before(c.ExpiresAt) {
		return nil
	}
	n, err := rand.Int(rand.Reader, big.NewInt(1_000_000))
	if err != nil {
		return err
	}
	code := fmt.Sprintf("%06d", n.Int64())
	if err := a.store.PutEmailCode(ctx, u.ID, emailCodeHash(u.ID, code), time.Now().Add(emailCodeTTL)); err != nil {
		return err
	}
	body := fmt.Sprintf("รหัสยืนยันการเข้าสู่ระบบ Aegiron ของคุณคือ\n\n    %s\n\nรหัสนี้ใช้ได้ %d นาที มีคนใส่รหัสผ่านของบัญชี %s ถูกต้องจาก IP %s\nถ้าไม่ใช่คุณ ให้เปลี่ยนรหัสผ่านทันทีและแจ้งผู้ดูแลระบบ\n\nYour Aegiron sign-in code is %s (valid for %d minutes).\n",
		code, int(emailCodeTTL/time.Minute), u.Email, ip, code, int(emailCodeTTL/time.Minute))
	if err := a.sendMail(ctx, mail.Message{To: []string{u.Email}, Subject: "รหัสเข้าสู่ระบบ Aegiron: " + code, Body: body}); err != nil {
		_ = a.store.DeleteEmailCode(ctx, u.ID)
		a.log.Warn("cannot send sign-in code", "user", u.Email, "err", err)
		return errMailUnavailable
	}
	_ = a.store.AddAudit(ctx, &u.ID, ip, "auth.email_code_sent", u.Email, nil)
	return nil
}

// checkEmailCode verifies (and consumes) the user's pending code.
func (a *API) checkEmailCode(ctx context.Context, u *store.User, code string) (bool, error) {
	c, err := a.store.GetEmailCode(ctx, u.ID)
	if errors.Is(err, store.ErrNotFound) {
		return false, nil
	} else if err != nil {
		return false, err
	}
	if time.Now().After(c.ExpiresAt) || c.Attempts >= emailCodeAttempts {
		_ = a.store.DeleteEmailCode(ctx, u.ID)
		return false, nil
	}
	if subtle.ConstantTimeCompare(emailCodeHash(u.ID, strings.TrimSpace(code)), c.CodeHash) == 1 {
		return true, a.store.DeleteEmailCode(ctx, u.ID)
	}
	if n, err := a.store.FailEmailCode(ctx, u.ID); err == nil && n >= emailCodeAttempts {
		_ = a.store.DeleteEmailCode(ctx, u.ID) // the next password entry mails a new code
	}
	return false, nil
}

// verifyOwnPassword checks the signed-in user's password (local hash or the directory).
func (a *API) verifyOwnPassword(ctx context.Context, u *store.User, pw string) bool {
	if u.AuthSource != "ldap" {
		return auth.VerifyPassword(u.PasswordHash, pw)
	}
	l, err := a.ldapEnabled(ctx)
	if err != nil || l == nil || u.ExternalDN == "" {
		return false
	}
	cfg, err := a.ldapConfig(l)
	if err != nil {
		return false
	}
	return ldapauth.CheckPassword(cfg, u.ExternalDN, pw) == nil
}

// setEmailOTP: POST /api/auth/email-otp {enabled, password} — the user's own second factor.
func (a *API) setEmailOTP(w http.ResponseWriter, r *http.Request) {
	var req struct {
		Enabled  bool   `json:"enabled"`
		Password string `json:"password"`
	}
	if !readJSON(w, r, &req) {
		return
	}
	u := userFrom(r)
	if !a.limiter.allow("email-otp:"+u.Email, 10, 10*time.Minute) {
		writeErr(w, http.StatusTooManyRequests, "too many attempts, try again later")
		return
	}
	if !a.verifyOwnPassword(r.Context(), u, req.Password) {
		writeErr(w, http.StatusUnauthorized, "password is incorrect")
		return
	}
	if req.Enabled {
		cfg, _, err := a.mailConfig(r.Context())
		if err != nil {
			a.internal(w, err)
			return
		}
		if cfg == nil {
			writeErr(w, http.StatusConflict, "ผู้ดูแลระบบยังไม่ได้ตั้งค่าอีเมลขาออก (SMTP)")
			return
		}
	}
	if err := a.store.SetEmailOTP(r.Context(), u.ID, req.Enabled); err != nil {
		a.internal(w, err)
		return
	}
	_ = a.store.DeleteEmailCode(r.Context(), u.ID)
	if req.Enabled {
		a.audit(r, "user.email_otp_enabled", u.Email, nil)
		a.revokeTrustedDevices(r, u, "email_otp_enabled")
	} else {
		a.audit(r, "user.email_otp_disabled", u.Email, nil)
		a.revokeTrustedDevices(r, u, "email_otp_disabled")
	}
	writeJSON(w, http.StatusOK, map[string]bool{"ok": true})
}

// --- admin alerts ---

// alert mails the admins in the background when that alert is switched on.
func (a *API) alert(kind, subject, body string) {
	go func() {
		ctx, cancel := context.WithTimeout(context.Background(), 45*time.Second)
		defer cancel()
		cfg, m, err := a.mailConfig(ctx)
		if err != nil || cfg == nil {
			return
		}
		if (kind == "new_device" && !m.AlertNewDevice) || (kind == "lockout" && !m.AlertLockout) {
			return
		}
		to := m.AlertRecipients
		if len(to) == 0 {
			if to, err = a.store.AdminEmails(ctx); err != nil || len(to) == 0 {
				return
			}
		}
		if err := mail.Send(ctx, *cfg, mail.Message{To: to, Subject: "[Aegiron] " + subject, Body: body}); err != nil {
			a.log.Warn("admin alert not sent", "kind", kind, "err", err)
		}
	}()
}

// deviceRegistered is the hub hook for the "new device" alert.
func (a *API) deviceRegistered(d *store.Device, ip string) {
	a.alert("new_device", "เครื่องใหม่ลงทะเบียน: "+d.Hostname,
		fmt.Sprintf("มีเครื่องใหม่ลงทะเบียนกับเซิร์ฟเวอร์ Aegiron\n\nID: %s\nชื่อเครื่อง: %s\nระบบ: %s\nIP: %s\nเวลา: %s\n\nถ้าไม่ได้ตั้งใจให้เครื่องนี้เข้าระบบ ปิดใช้งานได้ที่หน้า อุปกรณ์ทั้งหมด ในคอนโซล\n",
			d.ID, d.Hostname, d.OS, ip, time.Now().Format(time.RFC1123)))
}

// lockedOut records and announces (once per window) that an account hit the sign-in limit.
func (a *API) lockedOut(ctx context.Context, email, ip string) {
	if !a.limiter.allow("lockout-alert:"+email, 1, 10*time.Minute) {
		return
	}
	_ = a.store.AddAudit(ctx, nil, ip, "auth.lockout", email, map[string]any{"minutes": 10})
	a.alert("lockout", "บัญชีถูกล็อกชั่วคราว: "+email,
		fmt.Sprintf("มีการใส่รหัสผ่านผิดหลายครั้งติดกันสำหรับบัญชี %s\nระบบระงับการเข้าสู่ระบบของบัญชีนี้ 10 นาที\n\nIP ล่าสุด: %s\nเวลา: %s\n", email, ip, time.Now().Format(time.RFC1123)))
}

// --- admin settings ---

func (a *API) getSMTPSettings(w http.ResponseWriter, r *http.Request) {
	m, err := a.store.GetSMTPSettings(r.Context())
	if errors.Is(err, store.ErrNotFound) {
		m, err = &store.SMTPSettings{Port: 587, Security: "starttls", AlertRecipients: []string{}}, nil
	}
	if err != nil {
		a.internal(w, err)
		return
	}
	writeJSON(w, http.StatusOK, map[string]any{"settings": m})
}

func (a *API) putSMTPSettings(w http.ResponseWriter, r *http.Request) {
	var req struct {
		Host            string   `json:"host"`
		Port            int      `json:"port"`
		Security        string   `json:"security"`
		CACert          string   `json:"ca_cert"`
		Username        string   `json:"username"`
		Password        *string  `json:"password"` // nil = keep
		From            string   `json:"from"`
		AlertRecipients []string `json:"alert_recipients"`
		AlertNewDevice  bool     `json:"alert_new_device"`
		AlertLockout    bool     `json:"alert_lockout"`
		Enabled         bool     `json:"enabled"`
	}
	if !readJSON(w, r, &req) {
		return
	}
	m := &store.SMTPSettings{
		Host: strings.ToLower(trim(req.Host, 253)), Port: req.Port, Security: req.Security, CACert: strings.TrimSpace(req.CACert),
		Username: trim(req.Username, 256), From: trim(req.From, 254), AlertNewDevice: req.AlertNewDevice,
		AlertLockout: req.AlertLockout, Enabled: req.Enabled, AlertRecipients: []string{},
	}
	if m.Security == "" {
		m.Security = "starttls"
	}
	if m.Port == 0 {
		m.Port = 587
	}
	for _, to := range req.AlertRecipients {
		to = store.NormalizeEmail(to)
		if to == "" {
			continue
		}
		if !mail.ValidAddress(to) || len(m.AlertRecipients) >= 20 {
			writeErr(w, http.StatusBadRequest, "invalid recipient: "+to)
			return
		}
		m.AlertRecipients = append(m.AlertRecipients, to)
	}
	if len(m.CACert) > 64<<10 {
		writeErr(w, http.StatusBadRequest, "CA certificate too large")
		return
	}
	if m.Host != "" || m.Enabled {
		cfg := mail.Config{Host: m.Host, Port: m.Port, Security: m.Security, CACert: m.CACert, Username: m.Username, From: m.From}
		if err := cfg.Validate(); err != nil {
			writeErr(w, http.StatusBadRequest, err.Error())
			return
		}
	}
	var sealed []byte
	if req.Password != nil {
		if len(*req.Password) > 1024 || strings.ContainsAny(*req.Password, "\r\n") {
			writeErr(w, http.StatusBadRequest, "invalid password")
			return
		}
		sealed = []byte{}
		if *req.Password != "" {
			var err error
			if sealed, err = a.vault.Seal(*req.Password); err != nil {
				a.internal(w, err)
				return
			}
		}
	}
	if err := a.store.SaveSMTPSettings(r.Context(), m, sealed); err != nil {
		a.internal(w, err)
		return
	}
	a.audit(r, "smtp.settings_saved", m.Host, map[string]any{
		"enabled": m.Enabled, "port": m.Port, "security": m.Security, "username": m.Username, "from": m.From,
		"alert_recipients": m.AlertRecipients, "alert_new_device": m.AlertNewDevice, "alert_lockout": m.AlertLockout,
		"ca_cert": m.CACert != "", "password_changed": req.Password != nil,
	})
	writeJSON(w, http.StatusOK, map[string]bool{"ok": true})
}

// testSMTP: POST /api/smtp/test {to} sends a test message with the saved settings.
func (a *API) testSMTP(w http.ResponseWriter, r *http.Request) {
	var req struct {
		To string `json:"to"`
	}
	if !readJSON(w, r, &req) {
		return
	}
	to := store.NormalizeEmail(req.To)
	if to == "" {
		to = userFrom(r).Email
	}
	if !mail.ValidAddress(to) {
		writeErr(w, http.StatusBadRequest, "invalid recipient")
		return
	}
	if !a.limiter.allow("smtp-test:"+userFrom(r).Email, 10, 10*time.Minute) {
		writeErr(w, http.StatusTooManyRequests, "too many test messages")
		return
	}
	err := a.sendMail(r.Context(), mail.Message{To: []string{to}, Subject: "ทดสอบอีเมลจาก Aegiron",
		Body: "อีเมลนี้ส่งจากหน้า ตั้งค่าเซิร์ฟเวอร์ → อีเมล ของ Aegiron เพื่อทดสอบการตั้งค่า SMTP\n"})
	a.audit(r, "smtp.test", to, map[string]any{"ok": err == nil})
	if errors.Is(err, errMailUnavailable) {
		writeJSON(w, http.StatusOK, map[string]any{"ok": false, "error": "ยังไม่ได้เปิดใช้อีเมล"})
		return
	}
	if err != nil {
		writeJSON(w, http.StatusOK, map[string]any{"ok": false, "error": trim(err.Error(), 500)})
		return
	}
	writeJSON(w, http.StatusOK, map[string]any{"ok": true})
}
