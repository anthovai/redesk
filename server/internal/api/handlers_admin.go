package api

import (
	"net/http"
	"strconv"
	"strings"
	"time"

	"aegiron/server/internal/auth"
	"aegiron/server/internal/store"
)

func (a *API) listAudit(w http.ResponseWriter, r *http.Request) {
	q := r.URL.Query()
	limit, _ := strconv.Atoi(q.Get("limit"))
	if limit <= 0 || limit > 500 {
		limit = 100
	}
	offset, _ := strconv.Atoi(q.Get("offset"))
	entries, err := a.store.ListAudit(r.Context(), q.Get("action"), limit, max(offset, 0))
	if err != nil {
		a.internal(w, err)
		return
	}
	writeJSON(w, http.StatusOK, map[string]any{"entries": entries})
}

// --- users ---

func (a *API) listUsers(w http.ResponseWriter, r *http.Request) {
	users, err := a.store.ListUsers(r.Context())
	if err != nil {
		a.internal(w, err)
		return
	}
	writeJSON(w, http.StatusOK, map[string]any{"users": users})
}

func validRole(role string) bool { return role == "admin" || role == "user" }

func (a *API) createUser(w http.ResponseWriter, r *http.Request) {
	var req struct {
		Email    string `json:"email"`
		Name     string `json:"name"`
		Role     string `json:"role"`
		Password string `json:"password"`
	}
	if !readJSON(w, r, &req) {
		return
	}
	email := store.NormalizeEmail(req.Email)
	if !strings.Contains(email, "@") || len(email) > 254 {
		writeErr(w, http.StatusBadRequest, "invalid email")
		return
	}
	if !validRole(req.Role) {
		writeErr(w, http.StatusBadRequest, "role must be admin or user")
		return
	}
	if err := auth.ValidatePassword(req.Password); err != nil {
		writeErr(w, http.StatusBadRequest, err.Error())
		return
	}
	hash, err := auth.HashPassword(req.Password)
	if err != nil {
		a.internal(w, err)
		return
	}
	u, err := a.store.CreateUser(r.Context(), email, trim(req.Name, 100), req.Role, hash)
	if err != nil {
		if strings.Contains(err.Error(), "duplicate key") {
			writeErr(w, http.StatusConflict, "email already exists")
			return
		}
		a.internal(w, err)
		return
	}
	a.audit(r, "user.created", u.Email, map[string]any{"role": u.Role})
	writeJSON(w, http.StatusCreated, map[string]any{"user": u})
}

func (a *API) userParam(w http.ResponseWriter, r *http.Request) (*store.User, bool) {
	id, err := strconv.ParseInt(r.PathValue("id"), 10, 64)
	if err != nil {
		writeErr(w, http.StatusBadRequest, "invalid user id")
		return nil, false
	}
	u, err := a.store.GetUser(r.Context(), id)
	if err != nil {
		storeErr(a, w, err)
		return nil, false
	}
	return u, true
}

func (a *API) updateUser(w http.ResponseWriter, r *http.Request) {
	target, ok := a.userParam(w, r)
	if !ok {
		return
	}
	var req struct {
		Name     string `json:"name"`
		Role     string `json:"role"`
		Disabled bool   `json:"disabled"`
	}
	if !readJSON(w, r, &req) {
		return
	}
	if !validRole(req.Role) {
		writeErr(w, http.StatusBadRequest, "role must be admin or user")
		return
	}
	if target.ID == userFrom(r).ID && (req.Disabled || req.Role != "admin") {
		writeErr(w, http.StatusBadRequest, "you cannot disable or demote yourself")
		return
	}
	if err := a.store.UpdateUser(r.Context(), target.ID, trim(req.Name, 100), req.Role, req.Disabled); err != nil {
		storeErr(a, w, err)
		return
	}
	a.audit(r, "user.updated", target.Email, map[string]any{"role": req.Role, "disabled": req.Disabled})
	if req.Disabled {
		_ = a.store.DeleteUserSessions(r.Context(), target.ID)
		a.revokeTrustedDevices(r, target, "account_disabled")
	}
	writeJSON(w, http.StatusOK, map[string]bool{"ok": true})
}

func (a *API) adminSetPassword(w http.ResponseWriter, r *http.Request) {
	target, ok := a.userParam(w, r)
	if !ok {
		return
	}
	var req struct {
		Password string `json:"password"`
	}
	if !readJSON(w, r, &req) {
		return
	}
	if err := auth.ValidatePassword(req.Password); err != nil {
		writeErr(w, http.StatusBadRequest, err.Error())
		return
	}
	hash, err := auth.HashPassword(req.Password)
	if err != nil {
		a.internal(w, err)
		return
	}
	if err := a.store.SetUserPassword(r.Context(), target.ID, hash); err != nil {
		a.internal(w, err)
		return
	}
	_ = a.store.DeleteUserSessions(r.Context(), target.ID)
	a.audit(r, "user.password_reset", target.Email, nil)
	a.revokeTrustedDevices(r, target, "password_reset")
	writeJSON(w, http.StatusOK, map[string]bool{"ok": true})
}

func (a *API) adminResetTOTP(w http.ResponseWriter, r *http.Request) {
	target, ok := a.userParam(w, r)
	if !ok {
		return
	}
	if err := a.store.SetTOTP(r.Context(), target.ID, nil, false); err != nil {
		a.internal(w, err)
		return
	}
	a.audit(r, "user.totp_reset", target.Email, nil)
	a.revokeTrustedDevices(r, target, "totp_reset")
	writeJSON(w, http.StatusOK, map[string]bool{"ok": true})
}

// --- enrollment tokens ---

func (a *API) listEnrollTokens(w http.ResponseWriter, r *http.Request) {
	t, err := a.store.ListEnrollTokens(r.Context())
	if err != nil {
		a.internal(w, err)
		return
	}
	writeJSON(w, http.StatusOK, map[string]any{"tokens": t})
}

func (a *API) createEnrollToken(w http.ResponseWriter, r *http.Request) {
	var req struct {
		Note      string `json:"note"`
		ValidDays int    `json:"valid_days"` // 0 = never expires
		MaxUses   int    `json:"max_uses"`   // 0 = unlimited
		GroupID   *int64 `json:"group_id"`   // devices enrolled with it join this group
	}
	if !readJSON(w, r, &req) {
		return
	}
	var exp *time.Time
	if req.ValidDays > 0 {
		t := time.Now().AddDate(0, 0, req.ValidDays)
		exp = &t
	}
	secret := "rdk_" + auth.RandomToken(24)
	t, err := a.store.CreateEnrollToken(r.Context(), auth.HashToken(secret), trim(req.Note, 200), userFrom(r).ID, exp, max(req.MaxUses, 0), req.GroupID)
	if err != nil {
		a.internal(w, err)
		return
	}
	a.audit(r, "enroll_token.created", strconv.FormatInt(t.ID, 10), map[string]any{"note": t.Note})
	// The plaintext token is only shown once.
	writeJSON(w, http.StatusCreated, map[string]any{"token": t, "secret": secret})
}

func (a *API) deleteEnrollToken(w http.ResponseWriter, r *http.Request) {
	id, err := strconv.ParseInt(r.PathValue("id"), 10, 64)
	if err != nil {
		writeErr(w, http.StatusBadRequest, "invalid id")
		return
	}
	if err := a.store.DeleteEnrollToken(r.Context(), id); err != nil {
		a.internal(w, err)
		return
	}
	a.audit(r, "enroll_token.deleted", r.PathValue("id"), nil)
	writeJSON(w, http.StatusOK, map[string]bool{"ok": true})
}

// --- settings ---

func (a *API) getSettings(w http.ResponseWriter, r *http.Request) {
	st, err := a.store.GetSettings(r.Context())
	if err != nil {
		a.internal(w, err)
		return
	}
	writeJSON(w, http.StatusOK, map[string]any{"settings": st})
}

func (a *API) putSettings(w http.ResponseWriter, r *http.Request) {
	var st store.Settings
	if !readJSON(w, r, &st) {
		return
	}
	if st.AccessPolicy != "open" && st.AccessPolicy != "groups" {
		writeErr(w, http.StatusBadRequest, "access_policy must be open or groups")
		return
	}
	if st.SessionHours < 1 || st.SessionHours > 24*30 {
		writeErr(w, http.StatusBadRequest, "session_hours must be between 1 and 720")
		return
	}
	if err := a.store.SaveSettings(r.Context(), st); err != nil {
		a.internal(w, err)
		return
	}
	a.audit(r, "settings.updated", "", map[string]any{"require_enroll": st.RequireEnroll, "session_hours": st.SessionHours, "access_policy": st.AccessPolicy})
	writeJSON(w, http.StatusOK, map[string]any{"settings": st})
}

// --- device groups ---

func (a *API) listGroups(w http.ResponseWriter, r *http.Request) {
	g, err := a.store.ListGroups(r.Context())
	if err != nil {
		a.internal(w, err)
		return
	}
	writeJSON(w, http.StatusOK, map[string]any{"groups": g})
}

type groupReq struct {
	Name        string  `json:"name"`
	Description string  `json:"description"`
	MemberIDs   []int64 `json:"member_ids"`
}

func (a *API) saveGroup(w http.ResponseWriter, r *http.Request, id int64) {
	var req groupReq
	if !readJSON(w, r, &req) {
		return
	}
	name := trim(req.Name, 64)
	if name == "" {
		writeErr(w, http.StatusBadRequest, "name is required")
		return
	}
	var err error
	if id == 0 {
		id, err = a.store.CreateGroup(r.Context(), name, trim(req.Description, 500))
	} else {
		err = a.store.UpdateGroup(r.Context(), id, name, trim(req.Description, 500))
	}
	if err != nil {
		if strings.Contains(err.Error(), "duplicate key") {
			writeErr(w, http.StatusConflict, "group name already exists")
			return
		}
		storeErr(a, w, err)
		return
	}
	if req.MemberIDs == nil {
		req.MemberIDs = []int64{}
	}
	if err := a.store.SetGroupMembers(r.Context(), id, req.MemberIDs); err != nil {
		a.internal(w, err)
		return
	}
	a.audit(r, "group.saved", strconv.FormatInt(id, 10), map[string]any{"name": name, "members": req.MemberIDs})
	writeJSON(w, http.StatusOK, map[string]any{"id": id})
}

func (a *API) createGroup(w http.ResponseWriter, r *http.Request) { a.saveGroup(w, r, 0) }

func (a *API) updateGroup(w http.ResponseWriter, r *http.Request) {
	id, err := strconv.ParseInt(r.PathValue("id"), 10, 64)
	if err != nil || id <= 0 {
		writeErr(w, http.StatusBadRequest, "invalid id")
		return
	}
	a.saveGroup(w, r, id)
}

func (a *API) deleteGroup(w http.ResponseWriter, r *http.Request) {
	id, err := strconv.ParseInt(r.PathValue("id"), 10, 64)
	if err != nil {
		writeErr(w, http.StatusBadRequest, "invalid id")
		return
	}
	if err := a.store.DeleteGroup(r.Context(), id); err != nil {
		a.internal(w, err)
		return
	}
	a.audit(r, "group.deleted", r.PathValue("id"), nil)
	writeJSON(w, http.StatusOK, map[string]bool{"ok": true})
}
