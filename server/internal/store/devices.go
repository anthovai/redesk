package store

import (
	"bytes"
	"context"
	"errors"
	"time"

	"github.com/jackc/pgx/v5"
)

var (
	ErrKeyMismatch    = errors.New("device id is bound to a different public key")
	ErrDeviceDisabled = errors.New("device is disabled")
)

type Device struct {
	ID           string     `json:"id"`
	PublicKey    []byte     `json:"public_key"` // JSON: base64
	Hostname     string     `json:"hostname"`
	OS           string     `json:"os"`
	AgentVersion string     `json:"agent_version"`
	LastIP       string     `json:"last_ip"`
	LastSeenAt   *time.Time `json:"last_seen_at"`
	Alias        string     `json:"alias"`
	Note         string     `json:"note"`
	Disabled     bool       `json:"disabled"`
	CreatedAt    time.Time  `json:"created_at"`
	GroupID      *int64     `json:"group_id"`
	OwnerID      *int64     `json:"owner_id"`
	// LAN facts from the agent (admin-only in API responses; cleared for other users).
	Macs []string  `json:"macs,omitempty"`
	LAN  []LanAddr `json:"lan,omitempty"`
}

// LanAddr is one IPv4 address of a device with its prefix length.
type LanAddr struct {
	IP     string `json:"ip"`
	Prefix int    `json:"prefix"`
}

const deviceCols = `id, public_key, hostname, os, agent_version, last_ip, last_seen_at, alias, note, disabled, created_at, group_id, owner_id, macs, lan`

func scanDevice(r scanner) (*Device, error) {
	var d Device
	err := r.Scan(&d.ID, &d.PublicKey, &d.Hostname, &d.OS, &d.AgentVersion, &d.LastIP, &d.LastSeenAt, &d.Alias, &d.Note, &d.Disabled, &d.CreatedAt, &d.GroupID, &d.OwnerID, &d.Macs, &d.LAN)
	if err != nil {
		return nil, notFound(err)
	}
	return &d, nil
}

func (s *Store) GetDevice(ctx context.Context, id string) (*Device, error) {
	return scanDevice(s.DB.QueryRow(ctx, `SELECT `+deviceCols+` FROM devices WHERE id=$1`, id))
}

func (s *Store) DeviceExists(ctx context.Context, id string) (bool, error) {
	var ok bool
	err := s.DB.QueryRow(ctx, `SELECT EXISTS(SELECT 1 FROM devices WHERE id=$1)`, id).Scan(&ok)
	return ok, err
}

// RegisterDevice binds id to publicKey on first contact (or after an admin key reset)
// and refreshes metadata. A different key for a bound id is rejected.
// Empty macs/lan (older agents) keep the stored values.
func (s *Store) RegisterDevice(ctx context.Context, id string, publicKey []byte, hostname, osName, version, ip string, macs []string, lan []LanAddr, grant EnrollGrant) (dev *Device, created bool, err error) {
	if macs == nil {
		macs = []string{}
	}
	if lan == nil {
		lan = []LanAddr{}
	}
	err = pgx.BeginFunc(ctx, s.DB, func(tx pgx.Tx) error {
		cur, err := scanDevice(tx.QueryRow(ctx, `SELECT `+deviceCols+` FROM devices WHERE id=$1 FOR UPDATE`, id))
		switch {
		case errors.Is(err, ErrNotFound):
			created = true
			dev, err = scanDevice(tx.QueryRow(ctx, `INSERT INTO devices(id, public_key, hostname, os, agent_version, last_ip, last_seen_at, group_id, owner_id, macs, lan)
				VALUES ($1,$2,$3,$4,$5,$6,now(),$7,$8,$9,$10) RETURNING `+deviceCols, id, publicKey, hostname, osName, version, ip, grant.GroupID, grant.OwnerID, macs, lan))
			return err
		case err != nil:
			return err
		}
		if cur.Disabled {
			return ErrDeviceDisabled
		}
		if cur.PublicKey != nil && !bytes.Equal(cur.PublicKey, publicKey) {
			return ErrKeyMismatch
		}
		dev, err = scanDevice(tx.QueryRow(ctx, `UPDATE devices SET public_key=$2, hostname=$3, os=$4, agent_version=$5, last_ip=$6, last_seen_at=now(),
			macs=CASE WHEN cardinality($7::text[]) > 0 THEN $7 ELSE macs END,
			lan=CASE WHEN jsonb_array_length($8::jsonb) > 0 THEN $8 ELSE lan END
			WHERE id=$1 RETURNING `+deviceCols, id, publicKey, hostname, osName, version, ip, macs, lan))
		return err
	})
	return dev, created, err
}

// DevicesByIDs returns the devices with the given ids (unknown ids are skipped).
func (s *Store) DevicesByIDs(ctx context.Context, ids []string) ([]*Device, error) {
	return s.queryDevices(ctx, `SELECT `+deviceCols+` FROM devices WHERE id = ANY($1)`, ids)
}

func (s *Store) TouchDevice(ctx context.Context, id string) error {
	_, err := s.DB.Exec(ctx, `UPDATE devices SET last_seen_at=now() WHERE id=$1`, id)
	return err
}

func (s *Store) ListDevices(ctx context.Context) ([]*Device, error) {
	return s.queryDevices(ctx, `SELECT `+deviceCols+` FROM devices ORDER BY last_seen_at DESC NULLS LAST, id`)
}

func (s *Store) queryDevices(ctx context.Context, sql string, args ...any) ([]*Device, error) {
	rows, err := s.DB.Query(ctx, sql, args...)
	if err != nil {
		return nil, err
	}
	defer rows.Close()
	out := []*Device{}
	for rows.Next() {
		d, err := scanDevice(rows)
		if err != nil {
			return nil, err
		}
		out = append(out, d)
	}
	return out, rows.Err()
}

func (s *Store) UpdateDevice(ctx context.Context, id, alias, note string, disabled bool, groupID *int64) error {
	tag, err := s.DB.Exec(ctx, `UPDATE devices SET alias=$2, note=$3, disabled=$4, group_id=$5 WHERE id=$1`, id, alias, note, disabled, groupID)
	if err == nil && tag.RowsAffected() == 0 {
		return ErrNotFound
	}
	return err
}

// ResetDeviceKey clears the bound key so the next registration for this id is accepted (re-enrollment).
func (s *Store) ResetDeviceKey(ctx context.Context, id string) error {
	tag, err := s.DB.Exec(ctx, `UPDATE devices SET public_key=NULL WHERE id=$1`, id)
	if err == nil && tag.RowsAffected() == 0 {
		return ErrNotFound
	}
	return err
}

func (s *Store) DeleteDevice(ctx context.Context, id string) error {
	tag, err := s.DB.Exec(ctx, `DELETE FROM devices WHERE id=$1`, id)
	if err == nil && tag.RowsAffected() == 0 {
		return ErrNotFound
	}
	return err
}

// --- address book ---

type AddressEntry struct {
	DeviceID  string    `json:"device_id"`
	Alias     string    `json:"alias"`
	Tags      []string  `json:"tags"`
	Favorite  bool      `json:"favorite"`
	CreatedAt time.Time `json:"created_at"`
	Device    *Device   `json:"device"` // nil when the id has never registered
}

func (s *Store) ListAddressBook(ctx context.Context, userID int64) ([]*AddressEntry, error) {
	rows, err := s.DB.Query(ctx, `SELECT device_id, alias, tags, favorite, created_at FROM address_book
		WHERE user_id=$1 ORDER BY favorite DESC, created_at`, userID)
	if err != nil {
		return nil, err
	}
	out := []*AddressEntry{}
	ids := []string{}
	for rows.Next() {
		var e AddressEntry
		if err := rows.Scan(&e.DeviceID, &e.Alias, &e.Tags, &e.Favorite, &e.CreatedAt); err != nil {
			rows.Close()
			return nil, err
		}
		out = append(out, &e)
		ids = append(ids, e.DeviceID)
	}
	rows.Close()
	if err := rows.Err(); err != nil {
		return nil, err
	}
	devs, err := s.queryDevices(ctx, `SELECT `+deviceCols+` FROM devices WHERE id = ANY($1)`, ids)
	if err != nil {
		return nil, err
	}
	byID := make(map[string]*Device, len(devs))
	for _, d := range devs {
		byID[d.ID] = d
	}
	for _, e := range out {
		e.Device = byID[e.DeviceID]
	}
	return out, nil
}

func (s *Store) UpsertAddressEntry(ctx context.Context, userID int64, deviceID, alias string, tags []string, favorite bool) error {
	if tags == nil {
		tags = []string{}
	}
	_, err := s.DB.Exec(ctx, `INSERT INTO address_book(user_id, device_id, alias, tags, favorite) VALUES ($1,$2,$3,$4,$5)
		ON CONFLICT (user_id, device_id) DO UPDATE SET alias=EXCLUDED.alias, tags=EXCLUDED.tags, favorite=EXCLUDED.favorite`,
		userID, deviceID, alias, tags, favorite)
	return err
}

// AddAddressEntryIfMissing puts a device in a user's address book without touching an existing entry.
func (s *Store) AddAddressEntryIfMissing(ctx context.Context, userID int64, deviceID, alias string) error {
	_, err := s.DB.Exec(ctx, `INSERT INTO address_book(user_id, device_id, alias) VALUES ($1,$2,$3) ON CONFLICT DO NOTHING`, userID, deviceID, alias)
	return err
}

func (s *Store) DeleteAddressEntry(ctx context.Context, userID int64, deviceID string) error {
	_, err := s.DB.Exec(ctx, `DELETE FROM address_book WHERE user_id=$1 AND device_id=$2`, userID, deviceID)
	return err
}
