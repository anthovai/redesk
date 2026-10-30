package store

import (
	"context"
	"strings"
	"time"
)

type User struct {
	ID           int64      `json:"id"`
	Email        string     `json:"email"`
	Name         string     `json:"name"`
	Role         string     `json:"role"`
	PasswordHash string     `json:"-"`
	TOTPSecret   *string    `json:"-"`
	TOTPEnabled  bool       `json:"totp_enabled"`
	Disabled     bool       `json:"disabled"`
	CreatedAt    time.Time  `json:"created_at"`
	LastLoginAt  *time.Time `json:"last_login_at"`
	// AuthSource is where the password is checked: "local" or "ldap".
	AuthSource string `json:"auth_source"`
	ExternalDN string `json:"external_dn,omitempty"`
	// EmailOTP: the second factor is a code sent by email (when TOTP is not enabled).
	EmailOTP bool `json:"email_otp"`
}

const userCols = `id, email, name, role, password_hash, totp_secret, totp_enabled, disabled, created_at, last_login_at, auth_source, external_dn, email_otp`

type scanner interface{ Scan(dest ...any) error }

func scanUser(r scanner) (*User, error) {
	var u User
	err := r.Scan(&u.ID, &u.Email, &u.Name, &u.Role, &u.PasswordHash, &u.TOTPSecret, &u.TOTPEnabled, &u.Disabled, &u.CreatedAt, &u.LastLoginAt, &u.AuthSource, &u.ExternalDN, &u.EmailOTP)
	if err != nil {
		return nil, notFound(err)
	}
	return &u, nil
}

func NormalizeEmail(e string) string { return strings.ToLower(strings.TrimSpace(e)) }

func (s *Store) CountUsers(ctx context.Context) (int, error) {
	var n int
	err := s.DB.QueryRow(ctx, `SELECT count(*) FROM users`).Scan(&n)
	return n, err
}

func (s *Store) CreateUser(ctx context.Context, email, name, role, passwordHash string) (*User, error) {
	return scanUser(s.DB.QueryRow(ctx,
		`INSERT INTO users(email, name, role, password_hash) VALUES ($1,$2,$3,$4) RETURNING `+userCols,
		NormalizeEmail(email), name, role, passwordHash))
}

func (s *Store) GetUserByEmail(ctx context.Context, email string) (*User, error) {
	return scanUser(s.DB.QueryRow(ctx, `SELECT `+userCols+` FROM users WHERE email=$1`, NormalizeEmail(email)))
}

func (s *Store) GetUser(ctx context.Context, id int64) (*User, error) {
	return scanUser(s.DB.QueryRow(ctx, `SELECT `+userCols+` FROM users WHERE id=$1`, id))
}

func (s *Store) ListUsers(ctx context.Context) ([]*User, error) {
	rows, err := s.DB.Query(ctx, `SELECT `+userCols+` FROM users ORDER BY id`)
	if err != nil {
		return nil, err
	}
	defer rows.Close()
	out := []*User{}
	for rows.Next() {
		u, err := scanUser(rows)
		if err != nil {
			return nil, err
		}
		out = append(out, u)
	}
	return out, rows.Err()
}

func (s *Store) UpdateUser(ctx context.Context, id int64, name, role string, disabled bool) error {
	tag, err := s.DB.Exec(ctx, `UPDATE users SET name=$2, role=$3, disabled=$4 WHERE id=$1`, id, name, role, disabled)
	if err == nil && tag.RowsAffected() == 0 {
		return ErrNotFound
	}
	return err
}

func (s *Store) SetUserPassword(ctx context.Context, id int64, hash string) error {
	_, err := s.DB.Exec(ctx, `UPDATE users SET password_hash=$2 WHERE id=$1`, id, hash)
	return err
}

func (s *Store) SetTOTP(ctx context.Context, id int64, secret *string, enabled bool) error {
	_, err := s.DB.Exec(ctx, `UPDATE users SET totp_secret=$2, totp_enabled=$3 WHERE id=$1`, id, secret, enabled)
	return err
}

// CreateLDAPUser creates an account whose password lives in the directory. passwordHash is an
// unusable random hash.
func (s *Store) CreateLDAPUser(ctx context.Context, email, name, role, passwordHash, dn string) (*User, error) {
	return scanUser(s.DB.QueryRow(ctx,
		`INSERT INTO users(email, name, role, password_hash, auth_source, external_dn) VALUES ($1,$2,$3,$4,'ldap',$5) RETURNING `+userCols,
		NormalizeEmail(email), name, role, passwordHash, dn))
}

// SyncLDAPUser refreshes the directory facts of an LDAP account after a successful bind.
// An empty name keeps the stored one; role "" keeps the stored role.
func (s *Store) SyncLDAPUser(ctx context.Context, id int64, name, role, dn string) error {
	_, err := s.DB.Exec(ctx, `UPDATE users SET name=CASE WHEN $2='' THEN name ELSE $2 END,
		role=CASE WHEN $3='' THEN role ELSE $3 END, external_dn=$4 WHERE id=$1 AND auth_source='ldap'`, id, name, role, dn)
	return err
}

func (s *Store) SetEmailOTP(ctx context.Context, id int64, on bool) error {
	_, err := s.DB.Exec(ctx, `UPDATE users SET email_otp=$2 WHERE id=$1`, id, on)
	return err
}

// AdminEmails lists the addresses of enabled admins (alert recipients by default).
func (s *Store) AdminEmails(ctx context.Context) ([]string, error) {
	rows, err := s.DB.Query(ctx, `SELECT email FROM users WHERE role='admin' AND NOT disabled ORDER BY id`)
	if err != nil {
		return nil, err
	}
	defer rows.Close()
	out := []string{}
	for rows.Next() {
		var e string
		if err := rows.Scan(&e); err != nil {
			return nil, err
		}
		out = append(out, e)
	}
	return out, rows.Err()
}

func (s *Store) TouchLogin(ctx context.Context, id int64) error {
	_, err := s.DB.Exec(ctx, `UPDATE users SET last_login_at=now() WHERE id=$1`, id)
	return err
}

// --- login sessions ---

func (s *Store) CreateAuthSession(ctx context.Context, tokenHash []byte, userID int64, ip, ua string, expires time.Time) error {
	_, err := s.DB.Exec(ctx, `INSERT INTO auth_sessions(token_hash, user_id, ip, user_agent, expires_at) VALUES ($1,$2,$3,$4,$5)`,
		tokenHash, userID, ip, ua, expires)
	return err
}

// UserForSession returns the active, non-disabled user owning the session token.
func (s *Store) UserForSession(ctx context.Context, tokenHash []byte) (*User, error) {
	return scanUser(s.DB.QueryRow(ctx, `SELECT `+prefixed("u.", userCols)+` FROM auth_sessions a JOIN users u ON u.id=a.user_id
		WHERE a.token_hash=$1 AND a.expires_at > now() AND NOT u.disabled`, tokenHash))
}

func (s *Store) DeleteAuthSession(ctx context.Context, tokenHash []byte) error {
	_, err := s.DB.Exec(ctx, `DELETE FROM auth_sessions WHERE token_hash=$1`, tokenHash)
	return err
}

func (s *Store) DeleteUserSessions(ctx context.Context, userID int64) error {
	_, err := s.DB.Exec(ctx, `DELETE FROM auth_sessions WHERE user_id=$1`, userID)
	return err
}

func (s *Store) PurgeExpiredSessions(ctx context.Context) error {
	_, err := s.DB.Exec(ctx, `DELETE FROM auth_sessions WHERE expires_at <= now()`)
	return err
}

func prefixed(p, cols string) string {
	parts := strings.Split(cols, ", ")
	for i := range parts {
		parts[i] = p + parts[i]
	}
	return strings.Join(parts, ", ")
}
