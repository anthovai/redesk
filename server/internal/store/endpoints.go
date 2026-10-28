package store

import (
	"context"
	"encoding/json"
	"time"
)

// Endpoint is a connection target served by another product or an open protocol.
type Endpoint struct {
	ID         int64           `json:"id"`
	Name       string          `json:"name"`
	Provider   string          `json:"provider"`
	Address    string          `json:"address"`
	Port       *int            `json:"port"`
	Username   string          `json:"username"`
	Secret     []byte          `json:"-"`
	HasSecret  bool            `json:"has_secret"`
	Options    json.RawMessage `json:"options"`
	GroupID    *int64          `json:"group_id"`
	Tags       []string        `json:"tags"`
	Note       string          `json:"note"`
	Source     string          `json:"source"`
	ExternalID *string         `json:"external_id"`
	Online     *bool           `json:"online"`
	SyncedAt   *time.Time      `json:"synced_at"`
	CreatedBy  *int64          `json:"created_by"`
	CreatedAt  time.Time       `json:"created_at"`
	UpdatedAt  time.Time       `json:"updated_at"`
}

const endpointCols = `id, name, provider, address, port, username, secret, options, group_id, tags, note, source, external_id, online, synced_at, created_by, created_at, updated_at`

func scanEndpoint(r scanner) (*Endpoint, error) {
	var e Endpoint
	err := r.Scan(&e.ID, &e.Name, &e.Provider, &e.Address, &e.Port, &e.Username, &e.Secret, &e.Options, &e.GroupID, &e.Tags,
		&e.Note, &e.Source, &e.ExternalID, &e.Online, &e.SyncedAt, &e.CreatedBy, &e.CreatedAt, &e.UpdatedAt)
	if err != nil {
		return nil, notFound(err)
	}
	e.HasSecret = len(e.Secret) > 0
	return &e, nil
}

func (s *Store) queryEndpoints(ctx context.Context, sql string, args ...any) ([]*Endpoint, error) {
	rows, err := s.DB.Query(ctx, sql, args...)
	if err != nil {
		return nil, err
	}
	defer rows.Close()
	out := []*Endpoint{}
	for rows.Next() {
		e, err := scanEndpoint(rows)
		if err != nil {
			return nil, err
		}
		out = append(out, e)
	}
	return out, rows.Err()
}

// ListEndpoints returns all endpoints (userID == 0) or those a user may use:
// endpoints they created plus endpoints in groups they belong to.
func (s *Store) ListEndpoints(ctx context.Context, userID int64) ([]*Endpoint, error) {
	if userID == 0 {
		return s.queryEndpoints(ctx, `SELECT `+endpointCols+` FROM endpoints ORDER BY name, id`)
	}
	return s.queryEndpoints(ctx, `SELECT `+endpointCols+` FROM endpoints e
		WHERE e.created_by=$1 OR e.group_id IN (SELECT group_id FROM group_members WHERE user_id=$1)
		ORDER BY name, id`, userID)
}

func (s *Store) GetEndpoint(ctx context.Context, id int64) (*Endpoint, error) {
	return scanEndpoint(s.DB.QueryRow(ctx, `SELECT `+endpointCols+` FROM endpoints WHERE id=$1`, id))
}

// UserCanUseEndpoint mirrors ListEndpoints for a single endpoint.
func (s *Store) UserCanUseEndpoint(ctx context.Context, userID int64, e *Endpoint) (bool, error) {
	if e.CreatedBy != nil && *e.CreatedBy == userID {
		return true, nil
	}
	if e.GroupID == nil {
		return false, nil
	}
	var ok bool
	err := s.DB.QueryRow(ctx, `SELECT EXISTS(SELECT 1 FROM group_members WHERE group_id=$1 AND user_id=$2)`, *e.GroupID, userID).Scan(&ok)
	return ok, err
}

func (s *Store) CreateEndpoint(ctx context.Context, e *Endpoint) (int64, error) {
	if e.Tags == nil {
		e.Tags = []string{}
	}
	if e.Options == nil {
		e.Options = json.RawMessage(`{}`)
	}
	var id int64
	err := s.DB.QueryRow(ctx, `INSERT INTO endpoints(name, provider, address, port, username, secret, options, group_id, tags, note, source, external_id, created_by)
		VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13) RETURNING id`,
		e.Name, e.Provider, e.Address, e.Port, e.Username, e.Secret, e.Options, e.GroupID, e.Tags, e.Note, e.Source, e.ExternalID, e.CreatedBy).Scan(&id)
	return id, err
}

// UpdateEndpoint saves editable fields. keepSecret leaves the stored password untouched.
func (s *Store) UpdateEndpoint(ctx context.Context, e *Endpoint, keepSecret bool) error {
	if e.Tags == nil {
		e.Tags = []string{}
	}
	tag, err := s.DB.Exec(ctx, `UPDATE endpoints SET name=$2, provider=$3, address=$4, port=$5, username=$6,
		secret=CASE WHEN $7 THEN secret ELSE $8 END, options=$9, group_id=$10, tags=$11, note=$12, updated_at=now() WHERE id=$1`,
		e.ID, e.Name, e.Provider, e.Address, e.Port, e.Username, keepSecret, e.Secret, e.Options, e.GroupID, e.Tags, e.Note)
	if err == nil && tag.RowsAffected() == 0 {
		return ErrNotFound
	}
	return err
}

func (s *Store) DeleteEndpoint(ctx context.Context, id int64) error {
	_, err := s.DB.Exec(ctx, `DELETE FROM endpoints WHERE id=$1`, id)
	return err
}

// UpsertSyncedEndpoint creates or refreshes an endpoint mirrored from a vendor API.
// Name, address and online state follow the vendor; local edits (group, password, tags) are kept.
func (s *Store) UpsertSyncedEndpoint(ctx context.Context, source, externalID, provider, name, address string, online *bool, groupID *int64) (created bool, err error) {
	err = s.DB.QueryRow(ctx, `INSERT INTO endpoints(name, provider, address, source, external_id, online, synced_at, group_id)
		VALUES ($1,$2,$3,$4,$5,$6,now(),$7)
		ON CONFLICT (source, external_id) DO UPDATE SET name=EXCLUDED.name, address=EXCLUDED.address,
			online=EXCLUDED.online, synced_at=now(), updated_at=now()
		RETURNING (xmax = 0)`, name, provider, address, source, externalID, online, groupID).Scan(&created)
	return created, err
}

// --- integrations ---

type Integration struct {
	Provider   string     `json:"provider"`
	Config     []byte     `json:"-"`
	Enabled    bool       `json:"enabled"`
	GroupID    *int64     `json:"group_id"`
	LastSyncAt *time.Time `json:"last_sync_at"`
	LastError  string     `json:"last_error"`
	Cursor     string     `json:"-"`
}

func (s *Store) GetIntegration(ctx context.Context, provider string) (*Integration, error) {
	var i Integration
	err := s.DB.QueryRow(ctx, `SELECT provider, config, enabled, group_id, last_sync_at, last_error, cursor FROM integrations WHERE provider=$1`, provider).
		Scan(&i.Provider, &i.Config, &i.Enabled, &i.GroupID, &i.LastSyncAt, &i.LastError, &i.Cursor)
	if err != nil {
		return nil, notFound(err)
	}
	return &i, nil
}

// SaveIntegration stores settings; a nil config keeps the existing sealed config.
func (s *Store) SaveIntegration(ctx context.Context, provider string, config []byte, enabled bool, groupID *int64) error {
	_, err := s.DB.Exec(ctx, `INSERT INTO integrations(provider, config, enabled, group_id) VALUES ($1, COALESCE($2, ''::bytea), $3, $4)
		ON CONFLICT (provider) DO UPDATE SET config=COALESCE($2, integrations.config), enabled=$3, group_id=$4`,
		provider, config, enabled, groupID)
	return err
}

func (s *Store) DeleteIntegration(ctx context.Context, provider string) error {
	_, err := s.DB.Exec(ctx, `DELETE FROM integrations WHERE provider=$1`, provider)
	return err
}

func (s *Store) RecordSync(ctx context.Context, provider, cursor, errMsg string) error {
	_, err := s.DB.Exec(ctx, `UPDATE integrations SET last_sync_at=now(), last_error=$2, cursor=CASE WHEN $3='' THEN cursor ELSE $3 END WHERE provider=$1`,
		provider, errMsg, cursor)
	return err
}

// --- external sessions ---

type ExternalSession struct {
	Provider   string          `json:"provider"`
	ExternalID string          `json:"external_id"`
	UserName   string          `json:"user_name"`
	TargetName string          `json:"target_name"`
	TargetID   string          `json:"target_id"`
	StartedAt  *time.Time      `json:"started_at"`
	EndedAt    *time.Time      `json:"ended_at"`
	Raw        json.RawMessage `json:"raw"`
}

func (s *Store) UpsertExternalSession(ctx context.Context, x *ExternalSession) error {
	_, err := s.DB.Exec(ctx, `INSERT INTO external_sessions(provider, external_id, user_name, target_name, target_id, started_at, ended_at, raw)
		VALUES ($1,$2,$3,$4,$5,$6,$7,$8)
		ON CONFLICT (provider, external_id) DO UPDATE SET ended_at=EXCLUDED.ended_at, raw=EXCLUDED.raw`,
		x.Provider, x.ExternalID, x.UserName, x.TargetName, x.TargetID, x.StartedAt, x.EndedAt, x.Raw)
	return err
}

func (s *Store) ListExternalSessions(ctx context.Context, limit int) ([]*ExternalSession, error) {
	rows, err := s.DB.Query(ctx, `SELECT provider, external_id, user_name, target_name, target_id, started_at, ended_at, raw
		FROM external_sessions ORDER BY started_at DESC NULLS LAST LIMIT $1`, limit)
	if err != nil {
		return nil, err
	}
	defer rows.Close()
	out := []*ExternalSession{}
	for rows.Next() {
		var x ExternalSession
		if err := rows.Scan(&x.Provider, &x.ExternalID, &x.UserName, &x.TargetName, &x.TargetID, &x.StartedAt, &x.EndedAt, &x.Raw); err != nil {
			return nil, err
		}
		out = append(out, &x)
	}
	return out, rows.Err()
}

// --- gateway sessions (stored alongside ReDesk sessions) ---

func (s *Store) CreateGatewaySession(ctx context.Context, id string, endpointID, userID int64, viewerIP string) error {
	_, err := s.DB.Exec(ctx, `INSERT INTO remote_sessions(id, endpoint_id, user_id, viewer_ip, started_at) VALUES ($1,$2,$3,$4,now())`,
		id, endpointID, userID, viewerIP)
	return err
}
