package store

import (
	"context"
	"time"

	"github.com/jackc/pgx/v5"
)

type Group struct {
	ID          int64     `json:"id"`
	Name        string    `json:"name"`
	Description string    `json:"description"`
	CreatedAt   time.Time `json:"created_at"`
	MemberIDs   []int64   `json:"member_ids"`
	DeviceCount int       `json:"device_count"`
	StrategyID  *int64    `json:"strategy_id"`
}

func (s *Store) ListGroups(ctx context.Context) ([]*Group, error) {
	rows, err := s.DB.Query(ctx, `SELECT g.id, g.name, g.description, g.created_at,
			COALESCE((SELECT array_agg(user_id ORDER BY user_id) FROM group_members m WHERE m.group_id=g.id), '{}'),
			(SELECT count(*) FROM devices d WHERE d.group_id=g.id), g.strategy_id
		FROM device_groups g ORDER BY g.name`)
	if err != nil {
		return nil, err
	}
	defer rows.Close()
	out := []*Group{}
	for rows.Next() {
		var g Group
		if err := rows.Scan(&g.ID, &g.Name, &g.Description, &g.CreatedAt, &g.MemberIDs, &g.DeviceCount, &g.StrategyID); err != nil {
			return nil, err
		}
		out = append(out, &g)
	}
	return out, rows.Err()
}

func (s *Store) CreateGroup(ctx context.Context, name, description string) (int64, error) {
	var id int64
	err := s.DB.QueryRow(ctx, `INSERT INTO device_groups(name, description) VALUES ($1,$2) RETURNING id`, name, description).Scan(&id)
	return id, err
}

func (s *Store) UpdateGroup(ctx context.Context, id int64, name, description string) error {
	tag, err := s.DB.Exec(ctx, `UPDATE device_groups SET name=$2, description=$3 WHERE id=$1`, id, name, description)
	if err == nil && tag.RowsAffected() == 0 {
		return ErrNotFound
	}
	return err
}

func (s *Store) DeleteGroup(ctx context.Context, id int64) error {
	_, err := s.DB.Exec(ctx, `DELETE FROM device_groups WHERE id=$1`, id)
	return err
}

// SetGroupMembers replaces the member list of a group.
func (s *Store) SetGroupMembers(ctx context.Context, groupID int64, userIDs []int64) error {
	return pgx.BeginFunc(ctx, s.DB, func(tx pgx.Tx) error {
		if _, err := tx.Exec(ctx, `DELETE FROM group_members WHERE group_id=$1`, groupID); err != nil {
			return err
		}
		_, err := tx.Exec(ctx, `INSERT INTO group_members(group_id, user_id) SELECT $1, unnest($2::bigint[]) ON CONFLICT DO NOTHING`, groupID, userIDs)
		return err
	})
}

// UserInDeviceGroup reports whether the user is a member of the device's group.
func (s *Store) UserInDeviceGroup(ctx context.Context, userID int64, deviceID string) (bool, error) {
	var ok bool
	err := s.DB.QueryRow(ctx, `SELECT EXISTS(SELECT 1 FROM devices d JOIN group_members m ON m.group_id=d.group_id
		WHERE d.id=$1 AND m.user_id=$2)`, deviceID, userID).Scan(&ok)
	return ok, err
}

// DevicesForUser lists devices shared with the user through group membership.
func (s *Store) DevicesForUser(ctx context.Context, userID int64) ([]*Device, error) {
	return s.queryDevices(ctx, `SELECT `+prefixed("d.", deviceCols)+` FROM devices d
		JOIN group_members m ON m.group_id=d.group_id WHERE m.user_id=$1 AND NOT d.disabled
		ORDER BY d.last_seen_at DESC NULLS LAST, d.id`, userID)
}
