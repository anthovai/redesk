package store

import (
	"context"
	"encoding/json"
	"time"
)

// Recording is one uploaded (or still expected) recording of an Aegiron remote session.
type Recording struct {
	ID         int64      `json:"id"`
	SID        string     `json:"sid"`
	DeviceID   string     `json:"device_id"`
	DeviceName string     `json:"device_name"`
	UserID     *int64     `json:"user_id"`
	UserEmail  *string    `json:"user_email"`
	ViewerIP   string     `json:"viewer_ip"`
	Status     string     `json:"status"` // pending | ready
	CreatedAt  time.Time  `json:"created_at"`
	UploadedAt *time.Time `json:"uploaded_at"`
	DurationMS int64      `json:"duration_ms"`
	SizeBytes  int64      `json:"size_bytes"`
	SHA256     string     `json:"sha256"`
	Codec      string     `json:"codec"`
	Frames     int        `json:"frames"`
	Audio      string     `json:"audio"` // "opus" or "" (no sound recorded)
}

// RecordingMeta is what the server learns from a validated upload.
type RecordingMeta struct {
	DurationMS int64
	SizeBytes  int64
	SHA256     string
	Codec      string
	Frames     int
	Audio      string
}

const recordingCols = `r.id, r.sid, r.device_id, COALESCE(NULLIF(d.alias, ''), d.hostname, ''), r.user_id, u.email, r.viewer_ip,
	r.status, r.created_at, r.uploaded_at, r.duration_ms, r.size_bytes, r.sha256, r.codec, r.frames, r.audio`

const recordingFrom = ` FROM session_recordings r LEFT JOIN users u ON u.id=r.user_id LEFT JOIN devices d ON d.id=r.device_id`

func scanRecording(row scanner) (*Recording, error) {
	var r Recording
	err := row.Scan(&r.ID, &r.SID, &r.DeviceID, &r.DeviceName, &r.UserID, &r.UserEmail, &r.ViewerIP,
		&r.Status, &r.CreatedAt, &r.UploadedAt, &r.DurationMS, &r.SizeBytes, &r.SHA256, &r.Codec, &r.Frames, &r.Audio)
	if err != nil {
		return nil, notFound(err)
	}
	return &r, nil
}

func (s *Store) queryRecordings(ctx context.Context, where string, args ...any) ([]*Recording, error) {
	rows, err := s.DB.Query(ctx, `SELECT `+recordingCols+recordingFrom+` `+where, args...)
	if err != nil {
		return nil, err
	}
	defer rows.Close()
	out := []*Recording{}
	for rows.Next() {
		r, err := scanRecording(rows)
		if err != nil {
			return nil, err
		}
		out = append(out, r)
	}
	return out, rows.Err()
}

// CreatePendingRecording registers a session that the device was asked to record.
func (s *Store) CreatePendingRecording(ctx context.Context, sid, deviceID string, userID int64, viewerIP string, tokenHash []byte) error {
	_, err := s.DB.Exec(ctx, `INSERT INTO session_recordings(sid, device_id, user_id, viewer_ip, token_hash) VALUES ($1,$2,$3,$4,$5)`,
		sid, deviceID, userID, viewerIP, tokenHash)
	return err
}

// PendingRecording finds the pending recording that may be uploaded with this token.
func (s *Store) PendingRecording(ctx context.Context, sid string, tokenHash []byte, maxAge time.Duration) (*Recording, error) {
	return scanRecording(s.DB.QueryRow(ctx, `SELECT `+recordingCols+recordingFrom+`
		WHERE r.sid=$1 AND r.token_hash=$2 AND r.status='pending' AND r.created_at > $3`, sid, tokenHash, time.Now().Add(-maxAge)))
}

// MarkRecordingStarted notes that the device began recording the session.
func (s *Store) MarkRecordingStarted(ctx context.Context, sid, deviceID string) error {
	_, err := s.DB.Exec(ctx, `UPDATE session_recordings SET recording_started=true WHERE sid=$1 AND device_id=$2`, sid, deviceID)
	return err
}

// CompleteRecording marks an upload as stored and burns its token.
func (s *Store) CompleteRecording(ctx context.Context, id int64, m RecordingMeta) error {
	tag, err := s.DB.Exec(ctx, `UPDATE session_recordings SET status='ready', token_hash=NULL, uploaded_at=now(),
		duration_ms=$2, size_bytes=$3, sha256=$4, codec=$5, frames=$6, audio=$7 WHERE id=$1 AND status='pending'`,
		id, m.DurationMS, m.SizeBytes, m.SHA256, m.Codec, m.Frames, m.Audio)
	if err == nil && tag.RowsAffected() == 0 {
		return ErrNotFound
	}
	return err
}

func (s *Store) GetRecording(ctx context.Context, id int64) (*Recording, error) {
	return scanRecording(s.DB.QueryRow(ctx, `SELECT `+recordingCols+recordingFrom+` WHERE r.id=$1`, id))
}

// ListRecordings returns the newest recordings, optionally for one device.
func (s *Store) ListRecordings(ctx context.Context, deviceID string, limit int) ([]*Recording, error) {
	return s.queryRecordings(ctx, `WHERE ($1='' OR r.device_id=$1) AND (r.status='ready' OR r.recording_started)
		ORDER BY r.created_at DESC LIMIT $2`, deviceID, limit)
}

func (s *Store) DeleteRecording(ctx context.Context, id int64) error {
	_, err := s.DB.Exec(ctx, `DELETE FROM session_recordings WHERE id=$1`, id)
	return err
}

// ExpiredRecordings lists uploaded recordings older than the retention period (0 = keep
// forever), pending ones whose upload window has passed, and requests for sessions that
// never started recording (e.g. the viewer never authenticated) after a day.
func (s *Store) ExpiredRecordings(ctx context.Context, retentionDays int, pendingMaxAge time.Duration) ([]*Recording, error) {
	var before *time.Time
	if retentionDays > 0 {
		t := time.Now().AddDate(0, 0, -retentionDays)
		before = &t
	}
	return s.queryRecordings(ctx, `WHERE (r.status='ready' AND $1::timestamptz IS NOT NULL AND r.created_at < $1::timestamptz)
		OR (r.status='pending' AND r.created_at < $2)
		OR (r.status='pending' AND NOT r.recording_started AND r.created_at < $3)`,
		before, time.Now().Add(-pendingMaxAge), time.Now().Add(-24*time.Hour))
}

// RecordingSettings is the organisation's recording policy.
type RecordingSettings struct {
	// Mode: "off", "all" (every Aegiron session) or "groups" (devices in GroupIDs).
	Mode          string  `json:"mode"`
	GroupIDs      []int64 `json:"group_ids"`
	RetentionDays int     `json:"retention_days"` // 0 = keep until deleted
}

func DefaultRecordingSettings() RecordingSettings {
	return RecordingSettings{Mode: "off", GroupIDs: []int64{}, RetentionDays: 90}
}

func (s *Store) GetRecordingSettings(ctx context.Context) (RecordingSettings, error) {
	st := DefaultRecordingSettings()
	var raw []byte
	err := s.DB.QueryRow(ctx, `SELECT value FROM settings WHERE key='recording'`).Scan(&raw)
	if err != nil {
		if notFound(err) == ErrNotFound {
			return st, nil
		}
		return st, err
	}
	err = json.Unmarshal(raw, &st)
	if st.GroupIDs == nil {
		st.GroupIDs = []int64{}
	}
	return st, err
}

func (s *Store) SaveRecordingSettings(ctx context.Context, st RecordingSettings) error {
	b, err := json.Marshal(st)
	if err != nil {
		return err
	}
	_, err = s.DB.Exec(ctx, `INSERT INTO settings(key, value) VALUES ('recording', $1) ON CONFLICT (key) DO UPDATE SET value=EXCLUDED.value`, b)
	return err
}
