package api

// Sign-in with an LDAP directory or Active Directory, for the web console and the desktop app
// (both go through checkCredentials). Accounts that came from the directory have
// auth_source = "ldap" and never use a local password; local accounts never use the directory.

import (
	"context"
	"errors"
	"net/http"
	"net/mail"
	"strings"

	"aegiron/server/internal/auth"
	"aegiron/server/internal/ldapauth"
	"aegiron/server/internal/store"
)

// ldapEnabled returns the settings when LDAP sign-in is configured and switched on.
func (a *API) ldapEnabled(ctx context.Context) (*store.LDAPSettings, error) {
	l, err := a.store.GetLDAPSettings(ctx)
	if errors.Is(err, store.ErrNotFound) {
		return nil, nil
	} else if err != nil {
		return nil, err
	}
	if !l.Enabled || l.URL == "" {
		return nil, nil
	}
	return l, nil
}

func (a *API) ldapConfig(l *store.LDAPSettings) (ldapauth.Config, error) {
	pw, err := a.vault.Open(l.BindPassword)
	if err != nil {
		return ldapauth.Config{}, err
	}
	return ldapauth.Config{
		URL: l.URL, CACert: l.CACert, BindDN: l.BindDN, BindPassword: pw, BaseDN: l.BaseDN,
		UserFilter: l.UserFilter, EmailAttr: l.EmailAttr, NameAttr: l.NameAttr, GroupFilter: l.GroupFilter,
	}, nil
}

// ldapLogin authenticates against the directory and maps the entry to an Aegiron account,
// creating it when allowed. reason != "" means the sign-in is refused (for the audit log).
func (a *API) ldapLogin(ctx context.Context, l *store.LDAPSettings, username, password string) (u *store.User, created bool, reason string, err error) {
	cfg, err := a.ldapConfig(l)
	if err != nil {
		return nil, false, "", err
	}
	du, err := ldapauth.Authenticate(cfg, username, password)
	switch {
	case errors.Is(err, ldapauth.ErrInvalidCredentials):
		return nil, false, "bad_credentials", nil
	case err != nil:
		a.log.Warn("ldap sign-in", "err", err)
		return nil, false, "ldap_unavailable", nil
	}
	admin := len(l.AdminGroups) > 0 && ldapauth.InGroups(du.Groups, l.AdminGroups)
	if len(l.UserGroups) > 0 && !admin && !ldapauth.InGroups(du.Groups, l.UserGroups) {
		return nil, false, "ldap_not_in_group", nil
	}
	email := store.NormalizeEmail(du.Email)
	if addr, err := mail.ParseAddress(email); err != nil || addr.Address != email || len(email) > 254 {
		return nil, false, "no_email", nil
	}
	// Role follows the directory only when admin groups are configured.
	role := ""
	if len(l.AdminGroups) > 0 {
		role = "user"
		if admin {
			role = "admin"
		}
	}
	u, err = a.store.GetUserByEmail(ctx, email)
	switch {
	case errors.Is(err, store.ErrNotFound):
		if !l.AutoCreate {
			return nil, false, "no_account", nil
		}
		hash, err := auth.HashPassword(auth.RandomToken(32)) // unusable: the directory checks passwords
		if err != nil {
			return nil, false, "", err
		}
		if role == "" {
			role = "user"
		}
		if u, err = a.store.CreateLDAPUser(ctx, email, trim(du.Name, 100), role, hash, du.DN); err != nil {
			if u, err = a.store.GetUserByEmail(ctx, email); err != nil { // lost a race
				return nil, false, "", err
			}
		} else {
			created = true
		}
	case err != nil:
		return nil, false, "", err
	}
	if u.AuthSource != "ldap" {
		// A local account with the same email is never taken over by a directory entry.
		return nil, false, "local_account", nil
	}
	if !created {
		if err := a.store.SyncLDAPUser(ctx, u.ID, trim(du.Name, 100), role, du.DN); err != nil {
			return nil, false, "", err
		}
		if u, err = a.store.GetUser(ctx, u.ID); err != nil {
			return nil, false, "", err
		}
	}
	return u, created, "", nil
}

// --- admin settings ---

func (a *API) getLDAPSettings(w http.ResponseWriter, r *http.Request) {
	l, err := a.store.GetLDAPSettings(r.Context())
	if errors.Is(err, store.ErrNotFound) {
		l, err = &store.LDAPSettings{EmailAttr: "mail", NameAttr: "displayName", AutoCreate: true, AdminGroups: []string{}, UserGroups: []string{}}, nil
	}
	if err != nil {
		a.internal(w, err)
		return
	}
	writeJSON(w, http.StatusOK, map[string]any{"settings": l, "default_user_filter": ldapauth.DefaultUserFilter})
}

type ldapReq struct {
	URL          string   `json:"url"`
	CACert       string   `json:"ca_cert"`
	BindDN       string   `json:"bind_dn"`
	BindPassword *string  `json:"bind_password"` // nil = keep
	BaseDN       string   `json:"base_dn"`
	UserFilter   string   `json:"user_filter"`
	EmailAttr    string   `json:"email_attr"`
	NameAttr     string   `json:"name_attr"`
	GroupFilter  string   `json:"group_filter"`
	AdminGroups  []string `json:"admin_groups"`
	UserGroups   []string `json:"user_groups"`
	AutoCreate   bool     `json:"auto_create"`
	Enabled      bool     `json:"enabled"`
}

func validLDAPAttr(s string) bool {
	if s == "" || len(s) > 64 {
		return false
	}
	for _, c := range s {
		if !(c >= 'a' && c <= 'z' || c >= 'A' && c <= 'Z' || c >= '0' && c <= '9' || c == '-') {
			return false
		}
	}
	return true
}

func cleanList(in []string, max int) []string {
	out := []string{}
	for _, s := range in {
		if s = trim(s, 256); s != "" && !strings.ContainsAny(s, "\r\n") && len(out) < max {
			out = append(out, s)
		}
	}
	return out
}

func (req *ldapReq) settings() (*store.LDAPSettings, string) {
	l := &store.LDAPSettings{
		URL: strings.TrimRight(strings.TrimSpace(req.URL), "/"), CACert: strings.TrimSpace(req.CACert),
		BindDN: trim(req.BindDN, 512), BaseDN: trim(req.BaseDN, 512), UserFilter: trim(req.UserFilter, 1024),
		EmailAttr: trim(req.EmailAttr, 64), NameAttr: trim(req.NameAttr, 64), GroupFilter: trim(req.GroupFilter, 1024),
		AdminGroups: cleanList(req.AdminGroups, 50), UserGroups: cleanList(req.UserGroups, 50),
		AutoCreate: req.AutoCreate, Enabled: req.Enabled,
	}
	if l.EmailAttr == "" {
		l.EmailAttr = "mail"
	}
	if !validLDAPAttr(l.EmailAttr) || (l.NameAttr != "" && !validLDAPAttr(l.NameAttr)) {
		return nil, "invalid attribute name"
	}
	if len(l.CACert) > 64<<10 || strings.ContainsAny(l.BindDN+l.BaseDN+l.UserFilter+l.GroupFilter, "\r\n") {
		return nil, "invalid characters"
	}
	if l.URL != "" || l.Enabled {
		cfg := ldapauth.Config{URL: l.URL, CACert: l.CACert, BaseDN: l.BaseDN, UserFilter: l.UserFilter, GroupFilter: l.GroupFilter}
		if err := cfg.Validate(); err != nil {
			return nil, err.Error()
		}
	}
	return l, ""
}

func (a *API) putLDAPSettings(w http.ResponseWriter, r *http.Request) {
	var req ldapReq
	if !readJSON(w, r, &req) {
		return
	}
	l, msg := req.settings()
	if msg != "" {
		writeErr(w, http.StatusBadRequest, msg)
		return
	}
	var sealed []byte
	if req.BindPassword != nil {
		if len(*req.BindPassword) > 1024 {
			writeErr(w, http.StatusBadRequest, "password too long")
			return
		}
		sealed = []byte{} // empty = clear
		if *req.BindPassword != "" {
			var err error
			if sealed, err = a.vault.Seal(*req.BindPassword); err != nil {
				a.internal(w, err)
				return
			}
		}
	}
	if err := a.store.SaveLDAPSettings(r.Context(), l, sealed); err != nil {
		a.internal(w, err)
		return
	}
	a.audit(r, "ldap.settings_saved", l.URL, map[string]any{
		"enabled": l.Enabled, "bind_dn": l.BindDN, "base_dn": l.BaseDN, "user_filter": l.UserFilter, "group_filter": l.GroupFilter,
		"admin_groups": l.AdminGroups, "user_groups": l.UserGroups, "auto_create": l.AutoCreate,
		"ca_cert": l.CACert != "", "password_changed": req.BindPassword != nil,
	})
	writeJSON(w, http.StatusOK, map[string]bool{"ok": true})
}

// testLDAP: POST /api/ldap/test {username?} — connects with the saved settings, binds the
// service account and, given a username, shows what the directory returns for it (no password).
func (a *API) testLDAP(w http.ResponseWriter, r *http.Request) {
	var req struct {
		Username string `json:"username"`
	}
	if !readJSON(w, r, &req) {
		return
	}
	l, err := a.store.GetLDAPSettings(r.Context())
	if errors.Is(err, store.ErrNotFound) {
		writeErr(w, http.StatusBadRequest, "LDAP is not configured")
		return
	} else if err != nil {
		a.internal(w, err)
		return
	}
	cfg, err := a.ldapConfig(l)
	if err != nil {
		a.internal(w, err)
		return
	}
	u, err := ldapauth.Lookup(cfg, trim(req.Username, 256))
	if err != nil {
		writeJSON(w, http.StatusOK, map[string]any{"ok": false, "error": trim(err.Error(), 500)})
		return
	}
	out := map[string]any{"ok": true}
	if u != nil {
		out["user"] = u
		out["admin"] = len(l.AdminGroups) > 0 && ldapauth.InGroups(u.Groups, l.AdminGroups)
		out["allowed"] = len(l.UserGroups) == 0 || ldapauth.InGroups(u.Groups, l.UserGroups) || out["admin"] == true
	} else if req.Username != "" {
		out["user"] = nil
	}
	writeJSON(w, http.StatusOK, out)
}
