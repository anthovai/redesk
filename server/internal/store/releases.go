package store

import (
	"context"
	"time"
)

// Release is an agent build uploaded by an admin. The file lives at <data>/releases/<sha256>.
type Release struct {
	ID        int64     `json:"id"`
	Version   string    `json:"version"`
	OS        string    `json:"os"`
	Arch      string    `json:"arch"`
	Filename  string    `json:"filename"`
	Size      int64     `json:"size"`
	SHA256    string    `json:"sha256"`
	Notes     string    `json:"notes"`
	Enabled   bool      `json:"enabled"`
	CreatedBy *int64    `json:"created_by"`
	CreatedAt time.Time `json:"created_at"`
}

const releaseCols = `id, version, os, arch, filename, size, sha256, notes, enabled, created_by, created_at`

func scanRelease(r scanner) (*Release, error) {
	var x Release
	if err := r.Scan(&x.ID, &x.Version, &x.OS, &x.Arch, &x.Filename, &x.Size, &x.SHA256, &x.Notes, &x.Enabled, &x.CreatedBy, &x.CreatedAt); err != nil {
		return nil, notFound(err)
	}
	return &x, nil
}

func (s *Store) queryReleases(ctx context.Context, sql string, args ...any) ([]*Release, error) {
	rows, err := s.DB.Query(ctx, sql, args...)
	if err != nil {
		return nil, err
	}
	defer rows.Close()
	out := []*Release{}
	for rows.Next() {
		x, err := scanRelease(rows)
		if err != nil {
			return nil, err
		}
		out = append(out, x)
	}
	return out, rows.Err()
}

func (s *Store) CreateRelease(ctx context.Context, x *Release) (*Release, error) {
	return scanRelease(s.DB.QueryRow(ctx, `INSERT INTO agent_releases(version, os, arch, filename, size, sha256, notes, enabled, created_by)
		VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9) RETURNING `+releaseCols,
		x.Version, x.OS, x.Arch, x.Filename, x.Size, x.SHA256, x.Notes, x.Enabled, x.CreatedBy))
}

func (s *Store) ListReleases(ctx context.Context) ([]*Release, error) {
	return s.queryReleases(ctx, `SELECT `+releaseCols+` FROM agent_releases ORDER BY created_at DESC, id DESC`)
}

// EnabledReleases returns enabled builds for an OS and an arch (or "universal").
func (s *Store) EnabledReleases(ctx context.Context, os, arch string) ([]*Release, error) {
	return s.queryReleases(ctx, `SELECT `+releaseCols+` FROM agent_releases
		WHERE enabled AND os=$1 AND (arch=$2 OR arch='universal') ORDER BY id`, os, arch)
}

func (s *Store) GetRelease(ctx context.Context, id int64) (*Release, error) {
	return scanRelease(s.DB.QueryRow(ctx, `SELECT `+releaseCols+` FROM agent_releases WHERE id=$1`, id))
}

func (s *Store) SetReleaseEnabled(ctx context.Context, id int64, enabled bool) error {
	tag, err := s.DB.Exec(ctx, `UPDATE agent_releases SET enabled=$2 WHERE id=$1`, id, enabled)
	if err == nil && tag.RowsAffected() == 0 {
		return ErrNotFound
	}
	return err
}

func (s *Store) DeleteRelease(ctx context.Context, id int64) (*Release, error) {
	return scanRelease(s.DB.QueryRow(ctx, `DELETE FROM agent_releases WHERE id=$1 RETURNING `+releaseCols, id))
}

// ReleaseFileInUse reports whether any release still points at the stored file.
func (s *Store) ReleaseFileInUse(ctx context.Context, sha string) (bool, error) {
	var used bool
	err := s.DB.QueryRow(ctx, `SELECT EXISTS(SELECT 1 FROM agent_releases WHERE sha256=$1)`, sha).Scan(&used)
	return used, err
}
