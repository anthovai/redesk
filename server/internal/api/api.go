// Package api is the REST interface used by the web console.
package api

import (
	"context"
	"crypto/ed25519"
	"encoding/json"
	"errors"
	"log/slog"
	"net"
	"net/http"
	"os"
	"path/filepath"
	"strings"
	"sync"
	"time"

	"redesk/server/internal/auth"
	"redesk/server/internal/config"
	"redesk/server/internal/hub"
	"redesk/server/internal/store"
	"redesk/server/internal/vault"
)

const cookieName = "redesk_session"

// Version is set at build time: -ldflags "-X redesk/server/internal/api.Version=..."
var Version = "dev"

type API struct {
	cfg      config.Config
	store    *store.Store
	hub      *hub.Hub
	key      ed25519.PrivateKey
	log      *slog.Logger
	limiter  *rateLimiter
	vault    *vault.Vault
	handoffs *handoffs
}

func New(cfg config.Config, s *store.Store, h *hub.Hub, key ed25519.PrivateKey, v *vault.Vault, log *slog.Logger) *API {
	a := &API{cfg: cfg, store: s, hub: h, key: key, vault: v, log: log, limiter: newRateLimiter(), handoffs: newHandoffs()}
	h.ClientIP = a.clientIP
	h.ViewerUserID = func(r *http.Request) int64 {
		if u := a.sessionUser(r); u != nil {
			return u.ID
		}
		return 0
	}
	return a
}

type ctxKey struct{}

func userFrom(r *http.Request) *store.User { return r.Context().Value(ctxKey{}).(*store.User) }

func (a *API) Handler() http.Handler {
	mux := http.NewServeMux()
	pub := func(pattern string, h http.HandlerFunc) { mux.Handle(pattern, a.csrf(h)) }
	user := func(pattern string, h http.HandlerFunc) { mux.Handle(pattern, a.csrf(a.requireUser(h, false))) }
	admin := func(pattern string, h http.HandlerFunc) { mux.Handle(pattern, a.csrf(a.requireUser(h, true))) }

	pub("GET /api/server-info", a.serverInfo)
	pub("POST /api/auth/login", a.login)
	pub("POST /api/agent/login", a.agentLogin)
	user("POST /api/auth/handoff", a.createHandoff)
	pub("GET /api/auth/handoff/{code}", a.useHandoff)
	user("POST /api/auth/logout", a.logout)
	user("GET /api/auth/me", a.me)
	user("POST /api/auth/password", a.changePassword)
	user("POST /api/auth/totp/setup", a.totpSetup)
	user("POST /api/auth/totp/enable", a.totpEnable)
	user("POST /api/auth/totp/disable", a.totpDisable)

	user("GET /api/devices", a.listDevices)
	user("GET /api/devices/{id}", a.getDevice)
	admin("PATCH /api/devices/{id}", a.updateDevice)
	admin("DELETE /api/devices/{id}", a.deleteDevice)
	admin("POST /api/devices/{id}/reset-key", a.resetDeviceKey)
	user("POST /api/devices/{id}/wake", a.wakeDevice)

	user("GET /api/my-devices", a.myDevices)
	user("GET /api/address-book", a.listAddressBook)
	user("PUT /api/address-book/{id}", a.putAddressEntry)
	user("DELETE /api/address-book/{id}", a.deleteAddressEntry)

	user("POST /api/sessions", a.createSession)
	admin("GET /api/sessions", a.listSessions)
	admin("GET /api/sessions/active", a.activeSessions)

	admin("GET /api/audit", a.listAudit)
	admin("GET /api/users", a.listUsers)
	admin("POST /api/users", a.createUser)
	admin("PATCH /api/users/{id}", a.updateUser)
	admin("POST /api/users/{id}/password", a.adminSetPassword)
	admin("DELETE /api/users/{id}/totp", a.adminResetTOTP)

	user("GET /api/endpoints", a.listEndpoints)
	user("POST /api/endpoints", a.createEndpoint)
	admin("POST /api/endpoints/import", a.importEndpoints)
	user("PUT /api/endpoints/{id}", a.updateEndpoint)
	user("DELETE /api/endpoints/{id}", a.deleteEndpoint)
	user("POST /api/endpoints/{id}/launch", a.launchEndpoint)
	admin("GET /api/external-sessions", a.listExternalSessions)
	admin("GET /api/integrations/{provider}", a.getIntegration)
	admin("PUT /api/integrations/{provider}", a.putIntegration)
	admin("DELETE /api/integrations/{provider}", a.deleteIntegration)
	admin("POST /api/integrations/{provider}/sync", a.syncIntegration)

	admin("GET /api/groups", a.listGroups)
	admin("POST /api/groups", a.createGroup)
	admin("PUT /api/groups/{id}", a.updateGroup)
	admin("DELETE /api/groups/{id}", a.deleteGroup)

	admin("GET /api/enroll-tokens", a.listEnrollTokens)
	admin("POST /api/enroll-tokens", a.createEnrollToken)
	admin("DELETE /api/enroll-tokens/{id}", a.deleteEnrollToken)
	admin("GET /api/settings", a.getSettings)
	admin("PUT /api/settings", a.putSettings)
	mux.HandleFunc("/api/", func(w http.ResponseWriter, r *http.Request) { writeErr(w, http.StatusNotFound, "not found") })

	mux.HandleFunc("GET /ws/agent", a.hub.HandleAgent)
	mux.HandleFunc("GET /ws/relay", a.hub.HandleRelay)
	mux.HandleFunc("GET /ws/gateway", a.handleGateway)
	mux.HandleFunc("GET /healthz", func(w http.ResponseWriter, r *http.Request) { w.Write([]byte("ok")) })

	if a.cfg.WebDir != "" {
		mux.Handle("/", spaHandler(a.cfg.WebDir))
	}
	return securityHeaders(mux)
}

// --- middleware ---

func securityHeaders(next http.Handler) http.Handler {
	return http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		h := w.Header()
		h.Set("X-Content-Type-Options", "nosniff")
		h.Set("X-Frame-Options", "DENY")
		h.Set("Referrer-Policy", "same-origin")
		h.Set("Content-Security-Policy", "default-src 'self'; img-src 'self' data: blob:; style-src 'self' 'unsafe-inline'; script-src 'self' 'unsafe-inline'; connect-src 'self' ws: wss:; frame-ancestors 'none'")
		next.ServeHTTP(w, r)
	})
}

// csrf requires a custom header on state-changing requests. Browsers cannot send it
// cross-origin without a CORS preflight, which this server never grants.
func (a *API) csrf(next http.Handler) http.Handler {
	return http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		if r.Method != http.MethodGet && r.Method != http.MethodHead && r.Header.Get("X-ReDesk") != "1" {
			writeErr(w, http.StatusForbidden, "missing X-ReDesk header")
			return
		}
		next.ServeHTTP(w, r)
	})
}

// bearerToken returns the desktop app's session token ("Authorization: Bearer ..."), if any.
func bearerToken(r *http.Request) string {
	if v, ok := strings.CutPrefix(r.Header.Get("Authorization"), "Bearer "); ok {
		return strings.TrimSpace(v)
	}
	return ""
}

// sessionUser resolves the console cookie, or a bearer token from the desktop app.
func (a *API) sessionUser(r *http.Request) *store.User {
	tok := bearerToken(r)
	if tok == "" {
		c, err := r.Cookie(cookieName)
		if err != nil || c.Value == "" {
			return nil
		}
		tok = c.Value
	}
	u, err := a.store.UserForSession(r.Context(), auth.HashToken(tok))
	if err != nil {
		return nil
	}
	return u
}

func (a *API) requireUser(next http.HandlerFunc, adminOnly bool) http.Handler {
	return http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		u := a.sessionUser(r)
		if u == nil {
			writeErr(w, http.StatusUnauthorized, "not logged in")
			return
		}
		if adminOnly && u.Role != "admin" {
			writeErr(w, http.StatusForbidden, "admin only")
			return
		}
		next(w, r.WithContext(context.WithValue(r.Context(), ctxKey{}, u)))
	})
}

func (a *API) clientIP(r *http.Request) string {
	if a.cfg.TrustProxy {
		if xff := r.Header.Get("X-Forwarded-For"); xff != "" {
			return strings.TrimSpace(strings.Split(xff, ",")[0])
		}
	}
	host, _, err := net.SplitHostPort(r.RemoteAddr)
	if err != nil {
		return r.RemoteAddr
	}
	return host
}

func (a *API) audit(r *http.Request, action, target string, detail map[string]any) {
	var actor *int64
	if u, ok := r.Context().Value(ctxKey{}).(*store.User); ok {
		actor = &u.ID
	}
	if err := a.store.AddAudit(r.Context(), actor, a.clientIP(r), action, target, detail); err != nil {
		a.log.Error("audit write failed", "action", action, "err", err)
	}
}

// --- helpers ---

func writeJSON(w http.ResponseWriter, status int, v any) {
	w.Header().Set("Content-Type", "application/json")
	w.Header().Set("Cache-Control", "no-store")
	w.WriteHeader(status)
	json.NewEncoder(w).Encode(v)
}

func writeErr(w http.ResponseWriter, status int, msg string) {
	writeJSON(w, status, map[string]string{"error": msg})
}

func readJSON(w http.ResponseWriter, r *http.Request, v any) bool {
	r.Body = http.MaxBytesReader(w, r.Body, 1<<20)
	dec := json.NewDecoder(r.Body)
	dec.DisallowUnknownFields()
	if err := dec.Decode(v); err != nil {
		writeErr(w, http.StatusBadRequest, "invalid JSON: "+err.Error())
		return false
	}
	return true
}

func (a *API) internal(w http.ResponseWriter, err error) {
	a.log.Error("internal error", "err", err)
	writeErr(w, http.StatusInternalServerError, "internal error")
}

func storeErr(a *API, w http.ResponseWriter, err error) {
	if errors.Is(err, store.ErrNotFound) {
		writeErr(w, http.StatusNotFound, "not found")
		return
	}
	a.internal(w, err)
}

// spaHandler serves the built web console, falling back to index.html for client-side routes.
func spaHandler(dir string) http.Handler {
	fs := http.FileServer(http.Dir(dir))
	return http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		p := filepath.Join(dir, filepath.Clean("/"+r.URL.Path))
		if st, err := os.Stat(p); err != nil || st.IsDir() {
			if _, err := os.Stat(filepath.Join(p, "index.html")); err != nil {
				http.ServeFile(w, r, filepath.Join(dir, "index.html"))
				return
			}
		}
		if strings.HasPrefix(r.URL.Path, "/_app/immutable/") {
			w.Header().Set("Cache-Control", "public, max-age=31536000, immutable")
		}
		fs.ServeHTTP(w, r)
	})
}

// --- rate limiting (login brute force) ---

type rateLimiter struct {
	mu   sync.Mutex
	hits map[string][]time.Time
}

func newRateLimiter() *rateLimiter { return &rateLimiter{hits: map[string][]time.Time{}} }

// allow permits at most n events per window for key.
func (l *rateLimiter) allow(key string, n int, window time.Duration) bool {
	l.mu.Lock()
	defer l.mu.Unlock()
	now := time.Now()
	kept := l.hits[key][:0]
	for _, t := range l.hits[key] {
		if now.Sub(t) < window {
			kept = append(kept, t)
		}
	}
	if len(kept) >= n {
		l.hits[key] = kept
		return false
	}
	l.hits[key] = append(kept, now)
	return true
}
