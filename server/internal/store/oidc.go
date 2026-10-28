package store

import (
	"context"
	"time"
)

// OIDCSettings configures single sign-on. ClientSecret is vault-sealed.
type OIDCSettings struct {
	Name           string    `json:"name"`
	Issuer         string    `json:"issuer"`
	ClientID       string    `json:"client_id"`
	ClientSecret   []byte    `json:"-"`
	HasSecret      bool      `json:"has_secret"`
	AllowedDomains []string  `json:"allowed_domains"`
	AutoCreate     bool      `json:"auto_create"`
	Enabled        bool      `json:"enabled"`
	UpdatedAt      time.Time `json:"updated_at"`
}

// GetOIDCSettings returns ErrNotFound when SSO was never configured.
func (s *Store) GetOIDCSettings(ctx context.Context) (*OIDCSettings, error) {
	var o OIDCSettings
	err := s.DB.QueryRow(ctx, `SELECT name, issuer, client_id, client_secret, allowed_domains, auto_create, enabled, updated_at
		FROM oidc_settings WHERE id=1`).
		Scan(&o.Name, &o.Issuer, &o.ClientID, &o.ClientSecret, &o.AllowedDomains, &o.AutoCreate, &o.Enabled, &o.UpdatedAt)
	if err != nil {
		return nil, notFound(err)
	}
	o.HasSecret = len(o.ClientSecret) > 0
	return &o, nil
}

// SaveOIDCSettings upserts the settings; a nil secret keeps the stored one.
func (s *Store) SaveOIDCSettings(ctx context.Context, o *OIDCSettings, secret []byte) error {
	domains := o.AllowedDomains
	if domains == nil {
		domains = []string{}
	}
	_, err := s.DB.Exec(ctx, `INSERT INTO oidc_settings(id, name, issuer, client_id, client_secret, allowed_domains, auto_create, enabled)
		VALUES (1, $1, $2, $3, COALESCE($4, ''::bytea), $5, $6, $7)
		ON CONFLICT (id) DO UPDATE SET name=$1, issuer=$2, client_id=$3, client_secret=COALESCE($4, oidc_settings.client_secret),
			allowed_domains=$5, auto_create=$6, enabled=$7, updated_at=now()`,
		o.Name, o.Issuer, o.ClientID, secret, domains, o.AutoCreate, o.Enabled)
	return err
}
