// Package config loads server settings from environment variables.
package config

import (
	"os"
	"strconv"
	"strings"
)

type Config struct {
	Listen         string   // AEGIRON_LISTEN
	DatabaseURL    string   // AEGIRON_DATABASE_URL
	DataDir        string   // AEGIRON_DATA_DIR: holds the server identity key
	WebDir         string   // AEGIRON_WEB_DIR: built SvelteKit app (empty = API only)
	TLSCert        string   // AEGIRON_TLS_CERT
	TLSKey         string   // AEGIRON_TLS_KEY
	CookieSecure   bool     // AEGIRON_COOKIE_SECURE (defaults to true when TLS is on)
	AllowedOrigins []string // AEGIRON_ALLOWED_ORIGINS: extra browser origins allowed on WebSockets
	TrustProxy     bool     // AEGIRON_TRUST_PROXY: read client IP from X-Forwarded-For
	AdminEmail     string   // AEGIRON_ADMIN_EMAIL: bootstrap admin (only when no users exist)
	AdminPassword  string   // AEGIRON_ADMIN_PASSWORD
	VaultKey       string   // AEGIRON_VAULT_KEY: base64 32-byte key for stored credentials (else <data>/vault.key)
	GuacdAddr      string   // AEGIRON_GUACD_ADDR: host:port of guacd; empty disables the RDP/VNC/SSH gateway
	// AEGIRON_GATEWAY_RECORDING_PATH: directory *inside the guacd container* for session recordings (empty = off)
	GatewayRecordingPath string
	Timezone             string // AEGIRON_TIMEZONE: sent to remote sessions (default Asia/Bangkok)
	TeamViewerAPI        string // AEGIRON_TEAMVIEWER_API: Web API base URL (override for testing)
	SyncInterval         int    // AEGIRON_SYNC_MINUTES: vendor API sync period (default 15)
	// AEGIRON_PUBLIC_URL: external base URL, e.g. https://aegiron.example.com (OIDC redirect URI)
	PublicURL string
}

func Load() Config {
	c := Config{
		Listen:        env("AEGIRON_LISTEN", ":21114"),
		DatabaseURL:   env("AEGIRON_DATABASE_URL", "postgres://aegiron:aegiron@localhost:5432/aegiron?sslmode=disable"),
		DataDir:       env("AEGIRON_DATA_DIR", "./data"),
		WebDir:        env("AEGIRON_WEB_DIR", "../web/build"),
		TLSCert:       getenv("AEGIRON_TLS_CERT"),
		TLSKey:        getenv("AEGIRON_TLS_KEY"),
		TrustProxy:    envBool("AEGIRON_TRUST_PROXY", false),
		AdminEmail:    env("AEGIRON_ADMIN_EMAIL", "admin@aegiron.local"),
		AdminPassword: getenv("AEGIRON_ADMIN_PASSWORD"),
		VaultKey:      getenv("AEGIRON_VAULT_KEY"),
		GuacdAddr:     getenv("AEGIRON_GUACD_ADDR"),
		Timezone:      env("AEGIRON_TIMEZONE", "Asia/Bangkok"),

		GatewayRecordingPath: getenv("AEGIRON_GATEWAY_RECORDING_PATH"),
		TeamViewerAPI:        env("AEGIRON_TEAMVIEWER_API", "https://webapi.teamviewer.com"),
		PublicURL:            strings.TrimRight(strings.TrimSpace(getenv("AEGIRON_PUBLIC_URL")), "/"),
	}
	c.SyncInterval, _ = strconv.Atoi(env("AEGIRON_SYNC_MINUTES", "15"))
	if c.SyncInterval < 1 {
		c.SyncInterval = 15
	}
	c.CookieSecure = envBool("AEGIRON_COOKIE_SECURE", c.TLSCert != "")
	for _, o := range strings.Split(getenv("AEGIRON_ALLOWED_ORIGINS"), ",") {
		if o = strings.TrimSpace(o); o != "" {
			c.AllowedOrigins = append(c.AllowedOrigins, o)
		}
	}
	return c
}

// getenv reads AEGIRON_X, falling back to REDESK_X so servers set up before the rename
// (env files, launchd plists, compose files) keep working unchanged.
func getenv(k string) string {
	if v := os.Getenv(k); v != "" {
		return v
	}
	return os.Getenv("REDESK_" + strings.TrimPrefix(k, "AEGIRON_"))
}

func env(k, def string) string {
	if v := getenv(k); v != "" {
		return v
	}
	return def
}

func envBool(k string, def bool) bool {
	if v, err := strconv.ParseBool(getenv(k)); err == nil {
		return v
	}
	return def
}
