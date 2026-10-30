package api

// Shared address books (RustDesk Pro style), besides each user's personal address book.
// Any user can create one and becomes its owner. Access is shared with users or groups as
// read (see entries), rw (also add, edit, remove entries) or full (also rename, delete and
// share). Owners and admins always have full access. Seeing a device in a book does not grant
// the right to connect: the access policy and the device password still apply.

import (
	"errors"
	"net/http"
	"strconv"
	"strings"
	"time"

	"aegiron/server/internal/hub"
	"aegiron/server/internal/store"
)

// bookParam loads the book in the path and checks that the user has at least perm.
func (a *API) bookParam(w http.ResponseWriter, r *http.Request, perm string) (*store.AddressBook, bool) {
	id, err := strconv.ParseInt(r.PathValue("book"), 10, 64)
	if err != nil {
		writeErr(w, http.StatusBadRequest, "invalid address book id")
		return nil, false
	}
	u := userFrom(r)
	b, err := a.store.GetAddressBook(r.Context(), id, u.ID, u.Role == "admin")
	if errors.Is(err, store.ErrNotFound) || (err == nil && b.Permission == "") {
		writeErr(w, http.StatusNotFound, "not found")
		return nil, false
	} else if err != nil {
		a.internal(w, err)
		return nil, false
	}
	if store.BookPermRank(b.Permission) < store.BookPermRank(perm) {
		writeErr(w, http.StatusForbidden, "you do not have "+perm+" access to this address book")
		return nil, false
	}
	return b, true
}

func (a *API) listAddressBooks(w http.ResponseWriter, r *http.Request) {
	u := userFrom(r)
	books, err := a.store.ListAddressBooks(r.Context(), u.ID, u.Role == "admin")
	if err != nil {
		a.internal(w, err)
		return
	}
	writeJSON(w, http.StatusOK, map[string]any{"books": books})
}

type bookReq struct {
	Name string `json:"name"`
	Note string `json:"note"`
}

func (a *API) createAddressBook(w http.ResponseWriter, r *http.Request) {
	var req bookReq
	if !readJSON(w, r, &req) {
		return
	}
	name := trim(req.Name, 64)
	if name == "" {
		writeErr(w, http.StatusBadRequest, "name is required")
		return
	}
	u := userFrom(r)
	if !a.limiter.allow("book-create:"+u.Email, 30, 10*time.Minute) {
		writeErr(w, http.StatusTooManyRequests, "too many address books created, try again later")
		return
	}
	id, err := a.store.CreateAddressBook(r.Context(), name, trim(req.Note, 500), u.ID)
	if err != nil {
		a.internal(w, err)
		return
	}
	a.audit(r, "address_book.created", strconv.FormatInt(id, 10), map[string]any{"name": name})
	writeJSON(w, http.StatusCreated, map[string]any{"id": id})
}

func (a *API) updateAddressBook(w http.ResponseWriter, r *http.Request) {
	b, ok := a.bookParam(w, r, store.BookFull)
	if !ok {
		return
	}
	var req bookReq
	if !readJSON(w, r, &req) {
		return
	}
	name := trim(req.Name, 64)
	if name == "" {
		writeErr(w, http.StatusBadRequest, "name is required")
		return
	}
	if err := a.store.UpdateAddressBook(r.Context(), b.ID, name, trim(req.Note, 500)); err != nil {
		storeErr(a, w, err)
		return
	}
	a.audit(r, "address_book.updated", strconv.FormatInt(b.ID, 10), map[string]any{"name": name})
	writeJSON(w, http.StatusOK, map[string]bool{"ok": true})
}

func (a *API) deleteAddressBook(w http.ResponseWriter, r *http.Request) {
	b, ok := a.bookParam(w, r, store.BookFull)
	if !ok {
		return
	}
	if err := a.store.DeleteAddressBook(r.Context(), b.ID); err != nil {
		storeErr(a, w, err)
		return
	}
	a.audit(r, "address_book.deleted", strconv.FormatInt(b.ID, 10), map[string]any{"name": b.Name})
	writeJSON(w, http.StatusOK, map[string]bool{"ok": true})
}

func (a *API) listBookEntries(w http.ResponseWriter, r *http.Request) {
	b, ok := a.bookParam(w, r, store.BookRead)
	if !ok {
		return
	}
	entries, err := a.store.ListBookEntries(r.Context(), b.ID)
	if err != nil {
		a.internal(w, err)
		return
	}
	vc := a.viewCtx(r)
	type entryView struct {
		*store.BookEntry
		Device *deviceView `json:"device"`
	}
	out := make([]entryView, len(entries))
	for i, e := range entries {
		out[i] = entryView{BookEntry: e, Device: vc.view(e.Device)}
	}
	writeJSON(w, http.StatusOK, map[string]any{"book": b, "entries": out})
}

func (a *API) putBookEntry(w http.ResponseWriter, r *http.Request) {
	b, ok := a.bookParam(w, r, store.BookRW)
	if !ok {
		return
	}
	var req struct {
		Alias string   `json:"alias"`
		Tags  []string `json:"tags"`
		Note  string   `json:"note"`
	}
	if !readJSON(w, r, &req) {
		return
	}
	id := normalizeDeviceID(r.PathValue("id"))
	if !hub.ValidDeviceID(id) {
		writeErr(w, http.StatusBadRequest, "invalid device id")
		return
	}
	tags := []string{}
	for _, t := range req.Tags {
		if t = trim(t, 32); t != "" && len(tags) < 16 {
			tags = append(tags, t)
		}
	}
	if b.EntryCount >= 5000 {
		writeErr(w, http.StatusConflict, "address book is full")
		return
	}
	if err := a.store.UpsertBookEntry(r.Context(), b.ID, id, trim(req.Alias, 64), tags, trim(req.Note, 1000)); err != nil {
		a.internal(w, err)
		return
	}
	a.audit(r, "address_book.entry_saved", strconv.FormatInt(b.ID, 10), map[string]any{"device": id, "alias": trim(req.Alias, 64)})
	writeJSON(w, http.StatusOK, map[string]bool{"ok": true})
}

func (a *API) deleteBookEntry(w http.ResponseWriter, r *http.Request) {
	b, ok := a.bookParam(w, r, store.BookRW)
	if !ok {
		return
	}
	id := normalizeDeviceID(r.PathValue("id"))
	if err := a.store.DeleteBookEntry(r.Context(), b.ID, id); err != nil {
		storeErr(a, w, err)
		return
	}
	a.audit(r, "address_book.entry_removed", strconv.FormatInt(b.ID, 10), map[string]any{"device": id})
	writeJSON(w, http.StatusOK, map[string]bool{"ok": true})
}

func (a *API) getBookShares(w http.ResponseWriter, r *http.Request) {
	b, ok := a.bookParam(w, r, store.BookFull)
	if !ok {
		return
	}
	shares, err := a.store.ListBookShares(r.Context(), b.ID)
	if err != nil {
		a.internal(w, err)
		return
	}
	writeJSON(w, http.StatusOK, map[string]any{"shares": shares})
}

func (a *API) putBookShares(w http.ResponseWriter, r *http.Request) {
	b, ok := a.bookParam(w, r, store.BookFull)
	if !ok {
		return
	}
	var req struct {
		Shares []store.BookShare `json:"shares"`
	}
	if !readJSON(w, r, &req) {
		return
	}
	if len(req.Shares) > 500 {
		writeErr(w, http.StatusBadRequest, "too many shares")
		return
	}
	seen := map[string]bool{}
	out := []store.BookShare{}
	for _, sh := range req.Shares {
		if (sh.UserID == nil) == (sh.GroupID == nil) {
			writeErr(w, http.StatusBadRequest, "each share needs exactly one of user_id or group_id")
			return
		}
		if store.BookPermRank(sh.Permission) == 0 {
			writeErr(w, http.StatusBadRequest, "permission must be read, rw or full")
			return
		}
		key := "g"
		if sh.UserID != nil {
			key = "u" + strconv.FormatInt(*sh.UserID, 10)
			if b.OwnerID != nil && *sh.UserID == *b.OwnerID {
				continue // the owner always has full access
			}
		} else {
			key += strconv.FormatInt(*sh.GroupID, 10)
		}
		if !seen[key] {
			seen[key] = true
			out = append(out, sh)
		}
	}
	if err := a.store.SetBookShares(r.Context(), b.ID, out); err != nil {
		if strings.Contains(err.Error(), "foreign key") {
			writeErr(w, http.StatusBadRequest, "unknown user or group")
			return
		}
		a.internal(w, err)
		return
	}
	a.audit(r, "address_book.shared", strconv.FormatInt(b.ID, 10), map[string]any{"name": b.Name, "shares": out})
	writeJSON(w, http.StatusOK, map[string]bool{"ok": true})
}

// shareTargets: GET /api/share-targets — users and groups an address book can be shared with
// (names only, for the share dialog).
func (a *API) shareTargets(w http.ResponseWriter, r *http.Request) {
	users, err := a.store.ListUsers(r.Context())
	if err != nil {
		a.internal(w, err)
		return
	}
	groups, err := a.store.ListGroups(r.Context())
	if err != nil {
		a.internal(w, err)
		return
	}
	type target struct {
		ID    int64  `json:"id"`
		Name  string `json:"name"`
		Email string `json:"email,omitempty"`
	}
	us, gs := []target{}, []target{}
	for _, u := range users {
		if !u.Disabled {
			us = append(us, target{ID: u.ID, Name: u.Name, Email: u.Email})
		}
	}
	for _, g := range groups {
		gs = append(gs, target{ID: g.ID, Name: g.Name})
	}
	writeJSON(w, http.StatusOK, map[string]any{"users": us, "groups": gs})
}
