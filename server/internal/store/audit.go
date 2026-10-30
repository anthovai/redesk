package store

import (
	"context"
	"encoding/json"
	"time"
)

// --- audit log ---

type AuditEntry struct {
	ID         int64           `json:"id"`
	At         time.Time       `json:"at"`
	ActorID    *int64          `json:"actor_id"`
	ActorEmail *string         `json:"actor_email"`
	ActorIP    string          `json:"actor_ip"`
	Action     string          `json:"action"`
	Target     string          `json:"target"`
	Detail     json.RawMessage `json:"detail"`
}

func (s *Store) AddAudit(ctx context.Context, actorID *int64, ip, action, target string, detail map[string]any) error {
	if detail == nil {
		detail = map[string]any{}
	}
	b, err := json.Marshal(detail)
	if err != nil {
		return err
	}
	_, err = s.DB.Exec(ctx, `INSERT INTO audit_log(actor_id, actor_ip, action, target, detail) VALUES ($1,$2,$3,$4,$5)`,
		actorID, ip, action, target, b)
	return err
}

func (s *Store) ListAudit(ctx context.Context, action string, limit, offset int) ([]*AuditEntry, error) {
	rows, err := s.DB.Query(ctx, `SELECT a.id, a.at, a.actor_id, u.email, a.actor_ip, a.action, a.target, a.detail
		FROM audit_log a LEFT JOIN users u ON u.id=a.actor_id
		WHERE ($1='' OR a.action LIKE $1 || '%')
		ORDER BY a.id DESC LIMIT $2 OFFSET $3`, action, limit, offset)
	if err != nil {
		return nil, err
	}
	defer rows.Close()
	out := []*AuditEntry{}
	for rows.Next() {
		var e AuditEntry
		if err := rows.Scan(&e.ID, &e.At, &e.ActorID, &e.ActorEmail, &e.ActorIP, &e.Action, &e.Target, &e.Detail); err != nil {
			return nil, err
		}
		out = append(out, &e)
	}
	return out, rows.Err()
}

// --- remote sessions ---

type RemoteSession struct {
	ID            string     `json:"id"`
	DeviceID      string     `json:"device_id"`
	UserID        *int64     `json:"user_id"`
	UserEmail     *string    `json:"user_email"`
	ViewerIP      string     `json:"viewer_ip"`
	CreatedAt     time.Time  `json:"created_at"`
	StartedAt     *time.Time `json:"started_at"`
	EndedAt       *time.Time `json:"ended_at"`
	BytesToViewer int64      `json:"bytes_to_viewer"`
	BytesToAgent  int64      `json:"bytes_to_agent"`
	EndReason     string     `json:"end_reason"`
	EndpointID    *int64     `json:"endpoint_id"`
	EndpointName  *string    `json:"endpoint_name"`
	Provider      *string    `json:"provider"`
	// Kind is "screen", "tunnel" or "terminal"; Target is a tunnel's host:port.
	Kind   string `json:"type"`
	Target string `json:"target"`
}

func (s *Store) CreateRemoteSession(ctx context.Context, id, deviceID string, userID int64, viewerIP string) error {
	return s.CreateRemoteSessionOf(ctx, id, deviceID, userID, viewerIP, "screen", "")
}

// CreateRemoteSessionOf records a new session of any type (tunnel target in `target`).
func (s *Store) CreateRemoteSessionOf(ctx context.Context, id, deviceID string, userID int64, viewerIP, kind, target string) error {
	_, err := s.DB.Exec(ctx, `INSERT INTO remote_sessions(id, device_id, user_id, viewer_ip, kind, target) VALUES ($1,$2,$3,$4,$5,$6)`,
		id, deviceID, userID, viewerIP, kind, target)
	return err
}

func (s *Store) MarkSessionStarted(ctx context.Context, id string) error {
	_, err := s.DB.Exec(ctx, `UPDATE remote_sessions SET started_at=now() WHERE id=$1`, id)
	return err
}

func (s *Store) MarkSessionEnded(ctx context.Context, id string, toViewer, toAgent int64, reason string) error {
	_, err := s.DB.Exec(ctx, `UPDATE remote_sessions SET ended_at=now(), bytes_to_viewer=$2, bytes_to_agent=$3, end_reason=$4 WHERE id=$1`,
		id, toViewer, toAgent, reason)
	return err
}

func (s *Store) ListRemoteSessions(ctx context.Context, limit int) ([]*RemoteSession, error) {
	rows, err := s.DB.Query(ctx, `SELECT r.id, COALESCE(r.device_id, ''), r.user_id, u.email, r.viewer_ip, r.created_at, r.started_at, r.ended_at,
		r.bytes_to_viewer, r.bytes_to_agent, r.end_reason, r.endpoint_id, e.name, e.provider, r.kind, r.target
		FROM remote_sessions r LEFT JOIN users u ON u.id=r.user_id LEFT JOIN endpoints e ON e.id=r.endpoint_id
		ORDER BY r.created_at DESC LIMIT $1`, limit)
	if err != nil {
		return nil, err
	}
	defer rows.Close()
	out := []*RemoteSession{}
	for rows.Next() {
		var r RemoteSession
		if err := rows.Scan(&r.ID, &r.DeviceID, &r.UserID, &r.UserEmail, &r.ViewerIP, &r.CreatedAt, &r.StartedAt, &r.EndedAt,
			&r.BytesToViewer, &r.BytesToAgent, &r.EndReason, &r.EndpointID, &r.EndpointName, &r.Provider, &r.Kind, &r.Target); err != nil {
			return nil, err
		}
		out = append(out, &r)
	}
	return out, rows.Err()
}

// CloseDanglingSessions marks sessions left open by a previous server process as ended.
func (s *Store) CloseDanglingSessions(ctx context.Context) error {
	_, err := s.DB.Exec(ctx, `UPDATE remote_sessions SET ended_at=now(), end_reason='server_restart' WHERE ended_at IS NULL`)
	return err
}

// --- enrollment tokens ---

type EnrollToken struct {
	ID        int64      `json:"id"`
	Note      string     `json:"note"`
	CreatedAt time.Time  `json:"created_at"`
	ExpiresAt *time.Time `json:"expires_at"`
	MaxUses   int        `json:"max_uses"`
	Uses      int        `json:"uses"`
	GroupID   *int64     `json:"group_id"`
}

func (s *Store) CreateEnrollToken(ctx context.Context, hash []byte, note string, createdBy int64, expires *time.Time, maxUses int, groupID *int64) (*EnrollToken, error) {
	var t EnrollToken
	err := s.DB.QueryRow(ctx, `INSERT INTO enroll_tokens(token_hash, note, created_by, expires_at, max_uses, group_id) VALUES ($1,$2,$3,$4,$5,$6)
		RETURNING id, note, created_at, expires_at, max_uses, uses, group_id`, hash, note, createdBy, expires, maxUses, groupID).
		Scan(&t.ID, &t.Note, &t.CreatedAt, &t.ExpiresAt, &t.MaxUses, &t.Uses, &t.GroupID)
	return &t, err
}

func (s *Store) ListEnrollTokens(ctx context.Context) ([]*EnrollToken, error) {
	rows, err := s.DB.Query(ctx, `SELECT id, note, created_at, expires_at, max_uses, uses, group_id FROM enroll_tokens WHERE owner_id IS NULL ORDER BY id DESC`)
	if err != nil {
		return nil, err
	}
	defer rows.Close()
	out := []*EnrollToken{}
	for rows.Next() {
		var t EnrollToken
		if err := rows.Scan(&t.ID, &t.Note, &t.CreatedAt, &t.ExpiresAt, &t.MaxUses, &t.Uses, &t.GroupID); err != nil {
			return nil, err
		}
		out = append(out, &t)
	}
	return out, rows.Err()
}

func (s *Store) DeleteEnrollToken(ctx context.Context, id int64) error {
	_, err := s.DB.Exec(ctx, `DELETE FROM enroll_tokens WHERE id=$1`, id)
	return err
}

// EnrollGrant is what a consumed enrollment token gives the new device.
type EnrollGrant struct {
	GroupID *int64
	OwnerID *int64
}

// ConsumeEnrollToken atomically uses one slot of a valid token.
func (s *Store) ConsumeEnrollToken(ctx context.Context, hash []byte) (ok bool, g EnrollGrant, err error) {
	err = s.DB.QueryRow(ctx, `UPDATE enroll_tokens SET uses=uses+1
		WHERE token_hash=$1 AND (expires_at IS NULL OR expires_at > now()) AND (max_uses=0 OR uses < max_uses)
		RETURNING group_id, owner_id`, hash).Scan(&g.GroupID, &g.OwnerID)
	if notFound(err) == ErrNotFound {
		return false, g, nil
	}
	return err == nil, g, err
}

// CreateOwnedEnrollToken issues a single-use token whose device will belong to ownerID
// (used when someone signs in from the desktop app).
func (s *Store) CreateOwnedEnrollToken(ctx context.Context, hash []byte, note string, ownerID int64, expires *time.Time) (int64, error) {
	var id int64
	err := s.DB.QueryRow(ctx, `INSERT INTO enroll_tokens(token_hash, note, created_by, owner_id, expires_at, max_uses)
		VALUES ($1,$2,$3,$3,$4,1) RETURNING id`, hash, note, ownerID, expires).Scan(&id)
	return id, err
}

// --- settings ---

type Settings struct {
	RequireEnroll bool `json:"require_enroll"` // new devices need an enrollment token
	SessionHours  int  `json:"session_hours"`  // console login lifetime
	// AccessPolicy: "open" = any console user who knows a device id (and its password) may connect;
	// "groups" = non-admins only reach devices in groups they belong to.
	AccessPolicy string `json:"access_policy"`
}

func DefaultSettings() Settings {
	return Settings{RequireEnroll: false, SessionHours: 12, AccessPolicy: "open"}
}

func (s *Store) GetSettings(ctx context.Context) (Settings, error) {
	st := DefaultSettings()
	var raw []byte
	err := s.DB.QueryRow(ctx, `SELECT value FROM settings WHERE key='server'`).Scan(&raw)
	if err != nil {
		if notFound(err) == ErrNotFound {
			return st, nil
		}
		return st, err
	}
	err = json.Unmarshal(raw, &st)
	return st, err
}

func (s *Store) SaveSettings(ctx context.Context, st Settings) error {
	b, err := json.Marshal(st)
	if err != nil {
		return err
	}
	_, err = s.DB.Exec(ctx, `INSERT INTO settings(key, value) VALUES ('server', $1) ON CONFLICT (key) DO UPDATE SET value=EXCLUDED.value`, b)
	return err
}
