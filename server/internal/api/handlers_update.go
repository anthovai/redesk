package api

import (
	"crypto/sha256"
	"encoding/hex"
	"errors"
	"io"
	"mime"
	"net/http"
	"os"
	"path/filepath"
	"strconv"
	"strings"
	"time"

	"github.com/jackc/pgx/v5/pgconn"

	"aegiron/server/internal/store"
)

// Agent update distribution: admins upload vetted builds, apps ask for the newest enabled
// one and download it from this server. Nothing is installed automatically.

const maxReleaseSize = 300 << 20

// --- semantic versions (semver.org 2.0) ---

type semver struct {
	major, minor, patch uint64
	pre                 []string
}

func numericID(s string) (uint64, bool) {
	if s == "" || (len(s) > 1 && s[0] == '0') {
		return 0, false
	}
	n, err := strconv.ParseUint(s, 10, 64)
	return n, err == nil
}

func validIdent(s string) bool {
	if s == "" {
		return false
	}
	for _, c := range s {
		if !(c >= '0' && c <= '9' || c >= 'a' && c <= 'z' || c >= 'A' && c <= 'Z' || c == '-') {
			return false
		}
	}
	return true
}

// parseSemver accepts "1.2.3", "v1.2.3", "1.2.3-beta.1+build.5".
func parseSemver(s string) (semver, bool) {
	var v semver
	s = strings.TrimPrefix(s, "v")
	if len(s) > 64 {
		return v, false
	}
	if i := strings.IndexByte(s, '+'); i >= 0 {
		for _, id := range strings.Split(s[i+1:], ".") {
			if !validIdent(id) {
				return v, false
			}
		}
		s = s[:i]
	}
	core, pre, hasPre := strings.Cut(s, "-")
	parts := strings.Split(core, ".")
	if len(parts) != 3 {
		return v, false
	}
	nums := [3]*uint64{&v.major, &v.minor, &v.patch}
	for i, p := range parts {
		n, ok := numericID(p)
		if !ok {
			return v, false
		}
		*nums[i] = n
	}
	if hasPre {
		for _, id := range strings.Split(pre, ".") {
			if !validIdent(id) {
				return v, false
			}
			if _, err := strconv.ParseUint(id, 10, 64); err == nil && len(id) > 1 && id[0] == '0' {
				return v, false
			}
			v.pre = append(v.pre, id)
		}
	}
	return v, true
}

func cmpU(a, b uint64) int {
	switch {
	case a < b:
		return -1
	case a > b:
		return 1
	}
	return 0
}

// cmp orders versions by semver precedence (build metadata ignored).
func (a semver) cmp(b semver) int {
	if c := cmpU(a.major, b.major); c != 0 {
		return c
	}
	if c := cmpU(a.minor, b.minor); c != 0 {
		return c
	}
	if c := cmpU(a.patch, b.patch); c != 0 {
		return c
	}
	switch {
	case len(a.pre) == 0 && len(b.pre) == 0:
		return 0
	case len(a.pre) == 0:
		return 1
	case len(b.pre) == 0:
		return -1
	}
	for i := 0; i < len(a.pre) && i < len(b.pre); i++ {
		x, y := a.pre[i], b.pre[i]
		xn, xerr := strconv.ParseUint(x, 10, 64)
		yn, yerr := strconv.ParseUint(y, 10, 64)
		switch {
		case xerr == nil && yerr == nil:
			if c := cmpU(xn, yn); c != 0 {
				return c
			}
		case xerr == nil:
			return -1 // numeric identifiers sort before alphanumeric
		case yerr == nil:
			return 1
		default:
			if c := strings.Compare(x, y); c != 0 {
				return c
			}
		}
	}
	return cmpU(uint64(len(a.pre)), uint64(len(b.pre)))
}

func validOS(s string) bool   { return s == "windows" || s == "macos" || s == "linux" }
func validArch(s string) bool { return s == "x86_64" || s == "aarch64" }

// releaseFilename keeps a safe download name.
func releaseFilename(name, version, osName, arch string) string {
	name = filepath.Base(strings.ReplaceAll(name, "\\", "/"))
	var b strings.Builder
	for _, c := range name {
		if c >= '0' && c <= '9' || c >= 'a' && c <= 'z' || c >= 'A' && c <= 'Z' || c == '.' || c == '-' || c == '_' || c == '+' {
			b.WriteRune(c)
		} else {
			b.WriteByte('_')
		}
	}
	out := strings.TrimLeft(b.String(), ".")
	if len(out) > 128 {
		out = out[len(out)-128:]
	}
	if out == "" || out == "_" {
		out = "aegiron-" + version + "-" + osName + "-" + arch
	}
	return out
}

func (a *API) releaseDir() string { return filepath.Join(a.cfg.DataDir, "releases") }

// --- admin ---

func (a *API) listReleases(w http.ResponseWriter, r *http.Request) {
	rs, err := a.store.ListReleases(r.Context())
	if err != nil {
		a.internal(w, err)
		return
	}
	writeJSON(w, http.StatusOK, map[string]any{"releases": rs})
}

// uploadRelease: POST /api/agent/releases (multipart: version, os, arch, notes, file).
// The file is streamed to disk while hashing, then stored content-addressed by sha256.
func (a *API) uploadRelease(w http.ResponseWriter, r *http.Request) {
	r.Body = http.MaxBytesReader(w, r.Body, maxReleaseSize+1<<20)
	mr, err := r.MultipartReader()
	if err != nil {
		writeErr(w, http.StatusBadRequest, "expected multipart/form-data")
		return
	}
	dir := a.releaseDir()
	if err := os.MkdirAll(dir, 0o700); err != nil {
		a.internal(w, err)
		return
	}
	fields := map[string]string{}
	var tmp, sum, origName string
	var size int64
	defer func() {
		if tmp != "" {
			os.Remove(tmp)
		}
	}()
	for {
		part, err := mr.NextPart()
		if errors.Is(err, io.EOF) {
			break
		}
		if err != nil {
			writeErr(w, http.StatusBadRequest, "invalid upload: "+err.Error())
			return
		}
		switch name := part.FormName(); name {
		case "file":
			if tmp != "" {
				writeErr(w, http.StatusBadRequest, "only one file per release")
				return
			}
			f, err := os.CreateTemp(dir, ".upload-*")
			if err != nil {
				a.internal(w, err)
				return
			}
			tmp = f.Name()
			h := sha256.New()
			size, err = io.Copy(io.MultiWriter(f, h), io.LimitReader(part, maxReleaseSize+1))
			if cerr := f.Close(); err == nil {
				err = cerr
			}
			if err != nil {
				var mbe *http.MaxBytesError
				if errors.As(err, &mbe) {
					writeErr(w, http.StatusRequestEntityTooLarge, "file too large (max 300 MB)")
					return
				}
				writeErr(w, http.StatusBadRequest, "upload failed: "+err.Error())
				return
			}
			if size > maxReleaseSize {
				writeErr(w, http.StatusRequestEntityTooLarge, "file too large (max 300 MB)")
				return
			}
			sum, origName = hex.EncodeToString(h.Sum(nil)), part.FileName()
		case "version", "os", "arch", "notes":
			b, err := io.ReadAll(io.LimitReader(part, 16<<10+1))
			if err != nil || len(b) > 16<<10 {
				writeErr(w, http.StatusBadRequest, "field too long: "+name)
				return
			}
			fields[name] = string(b)
		default:
			_, _ = io.Copy(io.Discard, io.LimitReader(part, 16<<10))
		}
	}

	version := strings.TrimPrefix(strings.TrimSpace(fields["version"]), "v")
	osName, arch := strings.TrimSpace(fields["os"]), strings.TrimSpace(fields["arch"])
	if _, ok := parseSemver(version); !ok {
		writeErr(w, http.StatusBadRequest, "version must be semantic (e.g. 0.4.0)")
		return
	}
	if !validOS(osName) || !(validArch(arch) || arch == "universal") {
		writeErr(w, http.StatusBadRequest, "invalid os or arch")
		return
	}
	if tmp == "" || size == 0 {
		writeErr(w, http.StatusBadRequest, "file is required")
		return
	}
	notes := strings.TrimSpace(fields["notes"])
	if len(notes) > 4000 {
		writeErr(w, http.StatusBadRequest, "notes too long")
		return
	}
	final := filepath.Join(dir, sum)
	if _, err := os.Stat(final); errors.Is(err, os.ErrNotExist) {
		if err := os.Rename(tmp, final); err != nil {
			a.internal(w, err)
			return
		}
		tmp = ""
	}
	u := userFrom(r)
	rel, err := a.store.CreateRelease(r.Context(), &store.Release{
		Version: version, OS: osName, Arch: arch, Filename: releaseFilename(origName, version, osName, arch),
		Size: size, SHA256: sum, Notes: notes, CreatedBy: &u.ID,
	})
	if err != nil {
		a.removeReleaseFile(r, sum)
		var pe *pgconn.PgError
		if errors.As(err, &pe) && pe.Code == "23505" {
			writeErr(w, http.StatusConflict, "this version already exists for that OS and architecture")
			return
		}
		a.internal(w, err)
		return
	}
	a.audit(r, "release.uploaded", strconv.FormatInt(rel.ID, 10), map[string]any{
		"version": rel.Version, "os": rel.OS, "arch": rel.Arch, "size": rel.Size, "sha256": rel.SHA256, "filename": rel.Filename,
	})
	writeJSON(w, http.StatusCreated, map[string]any{"release": rel})
}

// removeReleaseFile deletes a stored file once no release references it.
func (a *API) removeReleaseFile(r *http.Request, sha string) {
	if used, err := a.store.ReleaseFileInUse(r.Context(), sha); err == nil && !used && len(sha) == 64 {
		if err := os.Remove(filepath.Join(a.releaseDir(), sha)); err != nil && !errors.Is(err, os.ErrNotExist) {
			a.log.Warn("remove release file", "sha256", sha, "err", err)
		}
	}
}

func (a *API) updateRelease(w http.ResponseWriter, r *http.Request) {
	id, err := strconv.ParseInt(r.PathValue("id"), 10, 64)
	if err != nil {
		writeErr(w, http.StatusBadRequest, "bad id")
		return
	}
	var req struct {
		Enabled bool `json:"enabled"`
	}
	if !readJSON(w, r, &req) {
		return
	}
	if err := a.store.SetReleaseEnabled(r.Context(), id, req.Enabled); err != nil {
		storeErr(a, w, err)
		return
	}
	action := "release.disabled"
	if req.Enabled {
		action = "release.enabled"
	}
	a.audit(r, action, strconv.FormatInt(id, 10), nil)
	writeJSON(w, http.StatusOK, map[string]bool{"ok": true})
}

func (a *API) deleteRelease(w http.ResponseWriter, r *http.Request) {
	id, err := strconv.ParseInt(r.PathValue("id"), 10, 64)
	if err != nil {
		writeErr(w, http.StatusBadRequest, "bad id")
		return
	}
	rel, err := a.store.DeleteRelease(r.Context(), id)
	if err != nil {
		storeErr(a, w, err)
		return
	}
	a.removeReleaseFile(r, rel.SHA256)
	a.audit(r, "release.deleted", strconv.FormatInt(id, 10), map[string]any{
		"version": rel.Version, "os": rel.OS, "arch": rel.Arch, "sha256": rel.SHA256,
	})
	writeJSON(w, http.StatusOK, map[string]bool{"ok": true})
}

// --- public: update check and download ---

// agentUpdate: GET /api/agent/update?os=&arch=&version= -> newest enabled release, if newer.
func (a *API) agentUpdate(w http.ResponseWriter, r *http.Request) {
	if !a.limiter.allow("update:"+a.clientIP(r), 60, 10*time.Minute) {
		writeErr(w, http.StatusTooManyRequests, "too many requests")
		return
	}
	q := r.URL.Query()
	osName, arch := q.Get("os"), q.Get("arch")
	cur, ok := parseSemver(q.Get("version"))
	if !validOS(osName) || !validArch(arch) || !ok {
		writeErr(w, http.StatusBadRequest, "os, arch and a semantic version are required")
		return
	}
	rs, err := a.store.EnabledReleases(r.Context(), osName, arch)
	if err != nil {
		a.internal(w, err)
		return
	}
	var best *store.Release
	var bestV semver
	for _, x := range rs {
		v, ok := parseSemver(x.Version)
		if !ok || v.cmp(cur) <= 0 {
			continue
		}
		// Prefer the exact architecture over universal for the same version.
		if best == nil || v.cmp(bestV) > 0 || (v.cmp(bestV) == 0 && best.Arch == "universal" && x.Arch == arch) {
			best, bestV = x, v
		}
	}
	if best == nil {
		writeJSON(w, http.StatusOK, map[string]any{"available": false})
		return
	}
	writeJSON(w, http.StatusOK, map[string]any{
		"available": true,
		"id":        best.ID,
		"version":   best.Version,
		"notes":     best.Notes,
		"size":      best.Size,
		"sha256":    best.SHA256,
		"filename":  best.Filename,
		"url":       "/api/agent/releases/" + strconv.FormatInt(best.ID, 10) + "/download",
	})
}

// downloadRelease: GET /api/agent/releases/{id}/download (enabled releases; admins: any).
func (a *API) downloadRelease(w http.ResponseWriter, r *http.Request) {
	if !a.limiter.allow("download:"+a.clientIP(r), 100, 10*time.Minute) {
		writeErr(w, http.StatusTooManyRequests, "too many requests")
		return
	}
	id, err := strconv.ParseInt(r.PathValue("id"), 10, 64)
	if err != nil {
		writeErr(w, http.StatusBadRequest, "bad id")
		return
	}
	rel, err := a.store.GetRelease(r.Context(), id)
	if err != nil {
		storeErr(a, w, err)
		return
	}
	if !rel.Enabled {
		if u := a.sessionUser(r); u == nil || u.Role != "admin" {
			writeErr(w, http.StatusNotFound, "not found")
			return
		}
	}
	f, err := os.Open(filepath.Join(a.releaseDir(), rel.SHA256))
	if err != nil {
		a.log.Error("release file missing", "id", rel.ID, "sha256", rel.SHA256, "err", err)
		writeErr(w, http.StatusNotFound, "file missing")
		return
	}
	defer f.Close()
	h := w.Header()
	h.Set("Content-Type", "application/octet-stream")
	h.Set("Content-Disposition", mime.FormatMediaType("attachment", map[string]string{"filename": rel.Filename}))
	h.Set("Cache-Control", "private, no-cache")
	h.Set("ETag", `"`+rel.SHA256+`"`)
	h.Set("X-Checksum-SHA256", rel.SHA256)
	http.ServeContent(w, r, "", rel.CreatedAt, f)
}
