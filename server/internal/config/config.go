// Package config loads server settings from environment variables.
package config

import (
	"os"
	"strconv"
	"strings"
)

type Config struct {
	Listen         string   // REDESK_LISTEN
	DatabaseURL    string   // REDESK_DATABASE_URL
	DataDir        string   // REDESK_DATA_DIR: holds the server identity key
	WebDir         string   // REDESK_WEB_DIR: built SvelteKit app (empty = API only)
	TLSCert        string   // REDESK_TLS_CERT
	TLSKey         string   // REDESK_TLS_KEY
	CookieSecure   bool     // REDESK_COOKIE_SECURE (defaults to true when TLS is on)
	AllowedOrigins []string // REDESK_ALLOWED_ORIGINS: extra browser origins allowed on WebSockets
	TrustProxy     bool     // REDESK_TRUST_PROXY: read client IP from X-Forwarded-For
	AdminEmail     string   // REDESK_ADMIN_EMAIL: bootstrap admin (only when no users exist)
	AdminPassword  string   // REDESK_ADMIN_PASSWORD
	VaultKey       string   // REDESK_VAULT_KEY: base64 32-byte key for stored credentials (else <data>/vault.key)
	GuacdAddr      string   // REDESK_GUACD_ADDR: host:port of guacd; empty disables the RDP/VNC/SSH gateway
	// REDESK_GATEWAY_RECORDING_PATH: directory *inside the guacd container* for session recordings (empty = off)
	GatewayRecordingPath string
	Timezone             string // REDESK_TIMEZONE: sent to remote sessions (default Asia/Bangkok)
	TeamViewerAPI        string // REDESK_TEAMVIEWER_API: Web API base URL (override for testing)
	SyncInterval         int    // REDESK_SYNC_MINUTES: vendor API sync period (default 15)
	// REDESK_PUBLIC_URL: external base URL, e.g. https://redesk.example.com (OIDC redirect URI)
	PublicURL string
}

func Load() Config {
	c := Config{
		Listen:        env("REDESK_LISTEN", ":21114"),
		DatabaseURL:   env("REDESK_DATABASE_URL", "postgres://redesk:redesk@localhost:5432/redesk?sslmode=disable"),
		DataDir:       env("REDESK_DATA_DIR", "./data"),
		WebDir:        env("REDESK_WEB_DIR", "../web/build"),
		TLSCert:       os.Getenv("REDESK_TLS_CERT"),
		TLSKey:        os.Getenv("REDESK_TLS_KEY"),
		TrustProxy:    envBool("REDESK_TRUST_PROXY", false),
		AdminEmail:    env("REDESK_ADMIN_EMAIL", "admin@redesk.local"),
		AdminPassword: os.Getenv("REDESK_ADMIN_PASSWORD"),
		VaultKey:      os.Getenv("REDESK_VAULT_KEY"),
		GuacdAddr:     os.Getenv("REDESK_GUACD_ADDR"),
		Timezone:      env("REDESK_TIMEZONE", "Asia/Bangkok"),

		GatewayRecordingPath: os.Getenv("REDESK_GATEWAY_RECORDING_PATH"),
		TeamViewerAPI:        env("REDESK_TEAMVIEWER_API", "https://webapi.teamviewer.com"),
		PublicURL:            strings.TrimRight(strings.TrimSpace(os.Getenv("REDESK_PUBLIC_URL")), "/"),
	}
	c.SyncInterval, _ = strconv.Atoi(env("REDESK_SYNC_MINUTES", "15"))
	if c.SyncInterval < 1 {
		c.SyncInterval = 15
	}
	c.CookieSecure = envBool("REDESK_COOKIE_SECURE", c.TLSCert != "")
	for _, o := range strings.Split(os.Getenv("REDESK_ALLOWED_ORIGINS"), ",") {
		if o = strings.TrimSpace(o); o != "" {
			c.AllowedOrigins = append(c.AllowedOrigins, o)
		}
	}
	return c
}

func env(k, def string) string {
	if v := os.Getenv(k); v != "" {
		return v
	}
	return def
}

func envBool(k string, def bool) bool {
	if v, err := strconv.ParseBool(os.Getenv(k)); err == nil {
		return v
	}
	return def
}
