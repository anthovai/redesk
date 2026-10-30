package store

import (
	"context"
	"time"
)

// LDAPSettings configures sign-in against a directory. BindPassword is vault-sealed.
type LDAPSettings struct {
	URL             string    `json:"url"`
	CACert          string    `json:"ca_cert"`
	BindDN          string    `json:"bind_dn"`
	BindPassword    []byte    `json:"-"`
	HasBindPassword bool      `json:"has_bind_password"`
	BaseDN          string    `json:"base_dn"`
	UserFilter      string    `json:"user_filter"`
	EmailAttr       string    `json:"email_attr"`
	NameAttr        string    `json:"name_attr"`
	GroupFilter     string    `json:"group_filter"`
	AdminGroups     []string  `json:"admin_groups"`
	UserGroups      []string  `json:"user_groups"`
	AutoCreate      bool      `json:"auto_create"`
	Enabled         bool      `json:"enabled"`
	UpdatedAt       time.Time `json:"updated_at"`
}

// GetLDAPSettings returns ErrNotFound when LDAP was never configured.
func (s *Store) GetLDAPSettings(ctx context.Context) (*LDAPSettings, error) {
	var l LDAPSettings
	err := s.DB.QueryRow(ctx, `SELECT url, ca_cert, bind_dn, bind_password, base_dn, user_filter, email_attr, name_attr,
		group_filter, admin_groups, user_groups, auto_create, enabled, updated_at FROM ldap_settings WHERE id=1`).
		Scan(&l.URL, &l.CACert, &l.BindDN, &l.BindPassword, &l.BaseDN, &l.UserFilter, &l.EmailAttr, &l.NameAttr,
			&l.GroupFilter, &l.AdminGroups, &l.UserGroups, &l.AutoCreate, &l.Enabled, &l.UpdatedAt)
	if err != nil {
		return nil, notFound(err)
	}
	l.HasBindPassword = len(l.BindPassword) > 0
	return &l, nil
}

// SaveLDAPSettings upserts the settings; a nil password keeps the stored one.
func (s *Store) SaveLDAPSettings(ctx context.Context, l *LDAPSettings, password []byte) error {
	_, err := s.DB.Exec(ctx, `INSERT INTO ldap_settings(id, url, ca_cert, bind_dn, bind_password, base_dn, user_filter, email_attr, name_attr,
			group_filter, admin_groups, user_groups, auto_create, enabled)
		VALUES (1, $1, $2, $3, COALESCE($4, ''::bytea), $5, $6, $7, $8, $9, $10, $11, $12, $13)
		ON CONFLICT (id) DO UPDATE SET url=$1, ca_cert=$2, bind_dn=$3, bind_password=COALESCE($4, ldap_settings.bind_password),
			base_dn=$5, user_filter=$6, email_attr=$7, name_attr=$8, group_filter=$9, admin_groups=$10, user_groups=$11,
			auto_create=$12, enabled=$13, updated_at=now()`,
		l.URL, l.CACert, l.BindDN, password, l.BaseDN, l.UserFilter, l.EmailAttr, l.NameAttr, l.GroupFilter,
		nonNil(l.AdminGroups), nonNil(l.UserGroups), l.AutoCreate, l.Enabled)
	return err
}

// SMTPSettings configures outgoing mail. Password is vault-sealed.
type SMTPSettings struct {
	Host            string    `json:"host"`
	Port            int       `json:"port"`
	Security        string    `json:"security"` // starttls | tls | none
	CACert          string    `json:"ca_cert"`
	Username        string    `json:"username"`
	Password        []byte    `json:"-"`
	HasPassword     bool      `json:"has_password"`
	From            string    `json:"from"`
	AlertRecipients []string  `json:"alert_recipients"`
	AlertNewDevice  bool      `json:"alert_new_device"`
	AlertLockout    bool      `json:"alert_lockout"`
	Enabled         bool      `json:"enabled"`
	UpdatedAt       time.Time `json:"updated_at"`
}

// GetSMTPSettings returns ErrNotFound when mail was never configured.
func (s *Store) GetSMTPSettings(ctx context.Context) (*SMTPSettings, error) {
	var m SMTPSettings
	err := s.DB.QueryRow(ctx, `SELECT host, port, security, ca_cert, username, password, from_addr, alert_recipients,
		alert_new_device, alert_lockout, enabled, updated_at FROM smtp_settings WHERE id=1`).
		Scan(&m.Host, &m.Port, &m.Security, &m.CACert, &m.Username, &m.Password, &m.From, &m.AlertRecipients,
			&m.AlertNewDevice, &m.AlertLockout, &m.Enabled, &m.UpdatedAt)
	if err != nil {
		return nil, notFound(err)
	}
	m.HasPassword = len(m.Password) > 0
	return &m, nil
}

// SaveSMTPSettings upserts the settings; a nil password keeps the stored one.
func (s *Store) SaveSMTPSettings(ctx context.Context, m *SMTPSettings, password []byte) error {
	_, err := s.DB.Exec(ctx, `INSERT INTO smtp_settings(id, host, port, security, ca_cert, username, password, from_addr,
			alert_recipients, alert_new_device, alert_lockout, enabled)
		VALUES (1, $1, $2, $3, $4, $5, COALESCE($6, ''::bytea), $7, $8, $9, $10, $11)
		ON CONFLICT (id) DO UPDATE SET host=$1, port=$2, security=$3, ca_cert=$4, username=$5,
			password=COALESCE($6, smtp_settings.password), from_addr=$7, alert_recipients=$8, alert_new_device=$9,
			alert_lockout=$10, enabled=$11, updated_at=now()`,
		m.Host, m.Port, m.Security, m.CACert, m.Username, password, m.From, nonNil(m.AlertRecipients),
		m.AlertNewDevice, m.AlertLockout, m.Enabled)
	return err
}

func nonNil(v []string) []string {
	if v == nil {
		return []string{}
	}
	return v
}

// --- email verification codes ---

// EmailCode is the pending login code of a user.
type EmailCode struct {
	CodeHash  []byte
	Attempts  int
	SentAt    time.Time
	ExpiresAt time.Time
}

func (s *Store) GetEmailCode(ctx context.Context, userID int64) (*EmailCode, error) {
	var c EmailCode
	err := s.DB.QueryRow(ctx, `SELECT code_hash, attempts, sent_at, expires_at FROM email_codes WHERE user_id=$1`, userID).
		Scan(&c.CodeHash, &c.Attempts, &c.SentAt, &c.ExpiresAt)
	if err != nil {
		return nil, notFound(err)
	}
	return &c, nil
}

// PutEmailCode replaces the user's pending code.
func (s *Store) PutEmailCode(ctx context.Context, userID int64, hash []byte, expires time.Time) error {
	_, err := s.DB.Exec(ctx, `INSERT INTO email_codes(user_id, code_hash, expires_at) VALUES ($1,$2,$3)
		ON CONFLICT (user_id) DO UPDATE SET code_hash=$2, attempts=0, sent_at=now(), expires_at=$3`, userID, hash, expires)
	return err
}

// FailEmailCode counts a wrong guess and returns the attempts so far.
func (s *Store) FailEmailCode(ctx context.Context, userID int64) (int, error) {
	var n int
	err := s.DB.QueryRow(ctx, `UPDATE email_codes SET attempts=attempts+1 WHERE user_id=$1 RETURNING attempts`, userID).Scan(&n)
	return n, notFound(err)
}

func (s *Store) DeleteEmailCode(ctx context.Context, userID int64) error {
	_, err := s.DB.Exec(ctx, `DELETE FROM email_codes WHERE user_id=$1`, userID)
	return err
}
