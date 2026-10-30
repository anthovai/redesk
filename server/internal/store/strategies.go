package store

import (
	"context"
	"encoding/json"
	"time"

	"github.com/jackc/pgx/v5"
)

// StrategySettings are the agent settings a strategy enforces. A nil field is not managed:
// the device keeps its own value and the user may change it.
type StrategySettings struct {
	// ApproveMode: "password", "click" or "both".
	ApproveMode *string `json:"approve_mode,omitempty"`
	// AllowPermanentPassword false forces one-time passwords only.
	AllowPermanentPassword *bool `json:"allow_permanent_password,omitempty"`
	AllowInput             *bool `json:"allow_input,omitempty"`
	AllowClipboard         *bool `json:"allow_clipboard,omitempty"`
	AllowFileTransfer      *bool `json:"allow_file_transfer,omitempty"`
	AllowAudio             *bool `json:"allow_audio,omitempty"`
	AllowWhiteboard        *bool `json:"allow_whiteboard,omitempty"`
	AllowResolution        *bool `json:"allow_resolution,omitempty"`
	// Session controls a viewer may use (block input, privacy mode, lock, restart).
	AllowBlockInput  *bool `json:"allow_block_input,omitempty"`
	AllowPrivacyMode *bool `json:"allow_privacy_mode,omitempty"`
	AllowLock        *bool `json:"allow_lock,omitempty"`
	AllowRestart     *bool `json:"allow_restart,omitempty"`
	// Tunnels and the remote terminal (allow_tunnel_lan: tunnels may reach other LAN hosts).
	AllowTunnel    *bool `json:"allow_tunnel,omitempty"`
	AllowTunnelLAN *bool `json:"allow_tunnel_lan,omitempty"`
	AllowTerminal  *bool `json:"allow_terminal,omitempty"`
	// DirectEnabled false keeps every session on the relay (no LAN/P2P).
	DirectEnabled *bool `json:"direct_enabled,omitempty"`
	LANDiscovery  *bool `json:"lan_discovery,omitempty"`
	// LockAfterSession locks the controlled machine's screen when a session ends.
	LockAfterSession *bool `json:"lock_after_session,omitempty"`
	// RecordSessions true records every session to these devices, whatever the server-wide
	// recording mode says (enforced by the server, not the agent).
	RecordSessions *bool `json:"record_sessions,omitempty"`
}

type Strategy struct {
	ID          int64            `json:"id"`
	Name        string           `json:"name"`
	Description string           `json:"description"`
	Settings    StrategySettings `json:"settings"`
	CreatedAt   time.Time        `json:"created_at"`
	UpdatedAt   time.Time        `json:"updated_at"`
	GroupIDs    []int64          `json:"group_ids"`
	DeviceIDs   []string         `json:"device_ids"`
}

const strategyCols = `s.id, s.name, s.description, s.settings, s.created_at, s.updated_at,
	COALESCE((SELECT array_agg(g.id ORDER BY g.id) FROM device_groups g WHERE g.strategy_id=s.id), '{}'),
	COALESCE((SELECT array_agg(d.id ORDER BY d.id) FROM devices d WHERE d.strategy_id=s.id), '{}')`

func scanStrategy(r scanner) (*Strategy, error) {
	var st Strategy
	var raw []byte
	if err := r.Scan(&st.ID, &st.Name, &st.Description, &raw, &st.CreatedAt, &st.UpdatedAt, &st.GroupIDs, &st.DeviceIDs); err != nil {
		return nil, notFound(err)
	}
	if err := json.Unmarshal(raw, &st.Settings); err != nil {
		return nil, err
	}
	return &st, nil
}

func (s *Store) ListStrategies(ctx context.Context) ([]*Strategy, error) {
	rows, err := s.DB.Query(ctx, `SELECT `+strategyCols+` FROM strategies s ORDER BY s.name`)
	if err != nil {
		return nil, err
	}
	defer rows.Close()
	out := []*Strategy{}
	for rows.Next() {
		st, err := scanStrategy(rows)
		if err != nil {
			return nil, err
		}
		out = append(out, st)
	}
	return out, rows.Err()
}

func (s *Store) GetStrategy(ctx context.Context, id int64) (*Strategy, error) {
	return scanStrategy(s.DB.QueryRow(ctx, `SELECT `+strategyCols+` FROM strategies s WHERE s.id=$1`, id))
}

// SaveStrategy creates (id 0) or updates a strategy and returns its id.
func (s *Store) SaveStrategy(ctx context.Context, id int64, name, description string, settings StrategySettings) (int64, error) {
	b, err := json.Marshal(settings)
	if err != nil {
		return 0, err
	}
	if id == 0 {
		err = s.DB.QueryRow(ctx, `INSERT INTO strategies(name, description, settings) VALUES ($1,$2,$3) RETURNING id`, name, description, b).Scan(&id)
		return id, err
	}
	tag, err := s.DB.Exec(ctx, `UPDATE strategies SET name=$2, description=$3, settings=$4, updated_at=now() WHERE id=$1`, id, name, description, b)
	if err == nil && tag.RowsAffected() == 0 {
		return 0, ErrNotFound
	}
	return id, err
}

func (s *Store) DeleteStrategy(ctx context.Context, id int64) error {
	tag, err := s.DB.Exec(ctx, `DELETE FROM strategies WHERE id=$1`, id)
	if err == nil && tag.RowsAffected() == 0 {
		return ErrNotFound
	}
	return err
}

// SetStrategyGroups makes exactly these groups use the strategy (a group has at most one
// strategy, so listed groups move away from any other strategy).
func (s *Store) SetStrategyGroups(ctx context.Context, id int64, groupIDs []int64) error {
	if groupIDs == nil {
		groupIDs = []int64{}
	}
	return pgx.BeginFunc(ctx, s.DB, func(tx pgx.Tx) error {
		if _, err := tx.Exec(ctx, `UPDATE device_groups SET strategy_id=NULL WHERE strategy_id=$1 AND NOT (id = ANY($2))`, id, groupIDs); err != nil {
			return err
		}
		_, err := tx.Exec(ctx, `UPDATE device_groups SET strategy_id=$1 WHERE id = ANY($2)`, id, groupIDs)
		return err
	})
}

// SetDeviceStrategy assigns a strategy to one device (nil = follow its group).
func (s *Store) SetDeviceStrategy(ctx context.Context, deviceID string, strategyID *int64) error {
	tag, err := s.DB.Exec(ctx, `UPDATE devices SET strategy_id=$2 WHERE id=$1`, deviceID, strategyID)
	if err == nil && tag.RowsAffected() == 0 {
		return ErrNotFound
	}
	return err
}

// EffectiveStrategies returns the strategy in force for each of the given devices: the
// device's own one, else its group's. Devices without one are absent from the map.
func (s *Store) EffectiveStrategies(ctx context.Context, deviceIDs []string) (map[string]*Strategy, error) {
	rows, err := s.DB.Query(ctx, `SELECT d.id, `+strategyCols+` FROM devices d
		LEFT JOIN device_groups dg ON dg.id=d.group_id
		JOIN strategies s ON s.id=COALESCE(d.strategy_id, dg.strategy_id)
		WHERE d.id = ANY($1)`, deviceIDs)
	if err != nil {
		return nil, err
	}
	defer rows.Close()
	out := map[string]*Strategy{}
	for rows.Next() {
		var id string
		var st Strategy
		var raw []byte
		if err := rows.Scan(&id, &st.ID, &st.Name, &st.Description, &raw, &st.CreatedAt, &st.UpdatedAt, &st.GroupIDs, &st.DeviceIDs); err != nil {
			return nil, err
		}
		if err := json.Unmarshal(raw, &st.Settings); err != nil {
			return nil, err
		}
		out[id] = &st
	}
	return out, rows.Err()
}

// EffectiveStrategy is EffectiveStrategies for one device (nil when it has none).
func (s *Store) EffectiveStrategy(ctx context.Context, deviceID string) (*Strategy, error) {
	m, err := s.EffectiveStrategies(ctx, []string{deviceID})
	if err != nil {
		return nil, err
	}
	return m[deviceID], nil
}
