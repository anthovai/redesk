package store

import (
	"context"
	"time"
)

// TrustedDevice is a browser or desktop app that may skip the TOTP step for one user
// ("remember this device for 30 days"). Only the token's hash is kept.
type TrustedDevice struct {
	ID         int64      `json:"id"`
	UserID     int64      `json:"user_id"`
	TokenHash  []byte     `json:"-"`
	Label      string     `json:"label"`
	UserAgent  string     `json:"user_agent"`
	IP         string     `json:"ip"`
	LastIP     string     `json:"last_ip"`
	CreatedAt  time.Time  `json:"created_at"`
	LastUsedAt *time.Time `json:"last_used_at"`
	ExpiresAt  time.Time  `json:"expires_at"`
}

// MaxTrustedDevices caps how many remembered devices one user keeps; the oldest go first.
const MaxTrustedDevices = 20

const trustedCols = `id, user_id, token_hash, label, user_agent, ip, last_ip, created_at, last_used_at, expires_at`

func scanTrusted(r scanner) (*TrustedDevice, error) {
	var d TrustedDevice
	if err := r.Scan(&d.ID, &d.UserID, &d.TokenHash, &d.Label, &d.UserAgent, &d.IP, &d.LastIP, &d.CreatedAt, &d.LastUsedAt, &d.ExpiresAt); err != nil {
		return nil, notFound(err)
	}
	return &d, nil
}

func (s *Store) CreateTrustedDevice(ctx context.Context, tokenHash []byte, userID int64, label, ua, ip string, expires time.Time) (*TrustedDevice, error) {
	d, err := scanTrusted(s.DB.QueryRow(ctx, `INSERT INTO trusted_devices(token_hash, user_id, label, user_agent, ip, last_ip, expires_at)
		VALUES ($1,$2,$3,$4,$5,$5,$6) RETURNING `+trustedCols, tokenHash, userID, label, ua, ip, expires))
	if err != nil {
		return nil, err
	}
	_, err = s.DB.Exec(ctx, `DELETE FROM trusted_devices WHERE user_id=$1 AND id NOT IN
		(SELECT id FROM trusted_devices WHERE user_id=$1 ORDER BY created_at DESC, id DESC LIMIT $2)`, userID, MaxTrustedDevices)
	return d, err
}

// UseTrustedDevice returns the unexpired device for this token and user, recording the use.
// A token of another user, an expired or a revoked token is ErrNotFound.
func (s *Store) UseTrustedDevice(ctx context.Context, tokenHash []byte, userID int64, ip string) (*TrustedDevice, error) {
	return scanTrusted(s.DB.QueryRow(ctx, `UPDATE trusted_devices SET last_used_at=now(), last_ip=$3
		WHERE token_hash=$1 AND user_id=$2 AND expires_at > now() RETURNING `+trustedCols, tokenHash, userID, ip))
}

func (s *Store) ListTrustedDevices(ctx context.Context, userID int64) ([]*TrustedDevice, error) {
	rows, err := s.DB.Query(ctx, `SELECT `+trustedCols+` FROM trusted_devices WHERE user_id=$1 AND expires_at > now()
		ORDER BY coalesce(last_used_at, created_at) DESC, id DESC`, userID)
	if err != nil {
		return nil, err
	}
	defer rows.Close()
	out := []*TrustedDevice{}
	for rows.Next() {
		d, err := scanTrusted(rows)
		if err != nil {
			return nil, err
		}
		out = append(out, d)
	}
	return out, rows.Err()
}

// DeleteTrustedDevice revokes one device of userID and returns it.
func (s *Store) DeleteTrustedDevice(ctx context.Context, userID, id int64) (*TrustedDevice, error) {
	return scanTrusted(s.DB.QueryRow(ctx, `DELETE FROM trusted_devices WHERE id=$1 AND user_id=$2 RETURNING `+trustedCols, id, userID))
}

// DeleteUserTrustedDevices revokes every remembered device of the user.
func (s *Store) DeleteUserTrustedDevices(ctx context.Context, userID int64) (int64, error) {
	tag, err := s.DB.Exec(ctx, `DELETE FROM trusted_devices WHERE user_id=$1`, userID)
	if err != nil {
		return 0, err
	}
	return tag.RowsAffected(), nil
}

func (s *Store) PurgeExpiredTrustedDevices(ctx context.Context) error {
	_, err := s.DB.Exec(ctx, `DELETE FROM trusted_devices WHERE expires_at <= now()`)
	return err
}
