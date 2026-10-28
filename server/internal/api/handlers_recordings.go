package api

// Recordings of Aegiron's own remote-control sessions.
//
// Sessions are end-to-end encrypted, so the server cannot record them. When the policy says a
// session must be recorded, the server hands the controlled device a one-time upload token
// with the session request; the device records what it streams and uploads the file here
// afterwards, signed with its registered Ed25519 key. Only admins can list, play or delete
// recordings, and every access is audited.

import (
	"context"
	"crypto/ed25519"
	"crypto/sha256"
	"encoding/base64"
	"encoding/hex"
	"errors"
	"io"
	"net/http"
	"os"
	"path/filepath"
	"regexp"
	"slices"
	"strconv"
	"time"

	"aegiron/server/internal/auth"
	"aegiron/server/internal/recording"
	"aegiron/server/internal/store"
)

// How long a device may take to upload a recording (it retries after restarts and outages).
const recordingUploadWindow = 7 * 24 * time.Hour

var sidPattern = regexp.MustCompile(`^[A-Za-z0-9_-]{8,64}$`)

func (a *API) recordingDir() string { return filepath.Join(a.cfg.DataDir, "recordings") }

func (a *API) recordingPath(sid string) string { return filepath.Join(a.recordingDir(), sid+".aerec") }

// recordingSignedMessage is what the device signs for an upload.
func recordingSignedMessage(deviceID, sid, sha string) []byte {
	return []byte("aegiron-recording-v1\n" + deviceID + "\n" + sid + "\n" + sha)
}

// recordingToken is the hub hook: decides by policy whether to record a new session.
func (a *API) recordingToken(ctx context.Context, sid, deviceID string, userID int64, viewerIP string) string {
	st, err := a.store.GetRecordingSettings(ctx)
	if err != nil {
		a.log.Error("recording settings unavailable", "err", err)
		return ""
	}
	switch st.Mode {
	case "all":
	case "groups":
		d, err := a.store.GetDevice(ctx, deviceID)
		if err != nil || d.GroupID == nil || !slices.Contains(st.GroupIDs, *d.GroupID) {
			return ""
		}
	default:
		return ""
	}
	tok := auth.RandomToken(24)
	if err := a.store.CreatePendingRecording(ctx, sid, deviceID, userID, viewerIP, auth.HashToken(tok)); err != nil {
		a.log.Error("cannot register recording", "sid", sid, "err", err)
		return ""
	}
	return tok
}

// uploadRecording: POST /api/agent/recordings/{sid}
//
//	Authorization: Bearer <upload token from the session request>
//	X-Aegiron-Signature: base64 Ed25519(device key, "aegiron-recording-v1\n" id "\n" sid "\n" hex(sha256(body)))
func (a *API) uploadRecording(w http.ResponseWriter, r *http.Request) {
	sid := r.PathValue("sid")
	ip := a.clientIP(r)
	if !a.limiter.allow("recording-upload:"+ip, 30, time.Minute) {
		writeErr(w, http.StatusTooManyRequests, "too many uploads")
		return
	}
	tok := bearerToken(r)
	if !sidPattern.MatchString(sid) || tok == "" {
		writeErr(w, http.StatusUnauthorized, "invalid upload credentials")
		return
	}
	rec, err := a.store.PendingRecording(r.Context(), sid, auth.HashToken(tok), recordingUploadWindow)
	if errors.Is(err, store.ErrNotFound) {
		writeErr(w, http.StatusUnauthorized, "invalid upload credentials")
		return
	} else if err != nil {
		a.internal(w, err)
		return
	}
	dev, err := a.store.GetDevice(r.Context(), rec.DeviceID)
	if err != nil || dev.PublicKey == nil || len(dev.PublicKey) != ed25519.PublicKeySize {
		writeErr(w, http.StatusConflict, "device key unavailable")
		return
	}
	sig, err := base64.StdEncoding.DecodeString(r.Header.Get("X-Aegiron-Signature"))
	if err != nil || len(sig) != ed25519.SignatureSize {
		writeErr(w, http.StatusUnauthorized, "missing device signature")
		return
	}

	if err := os.MkdirAll(a.recordingDir(), 0o700); err != nil {
		a.internal(w, err)
		return
	}
	tmp, err := os.CreateTemp(a.recordingDir(), sid+".*.upload")
	if err != nil {
		a.internal(w, err)
		return
	}
	defer os.Remove(tmp.Name()) // no-op after the rename
	defer tmp.Close()

	limit := a.cfg.RecordingMaxMB << 20
	body := http.MaxBytesReader(w, r.Body, limit)
	h := sha256.New()
	size, err := io.Copy(io.MultiWriter(tmp, h), body)
	if err != nil {
		var tooBig *http.MaxBytesError
		if errors.As(err, &tooBig) {
			writeErr(w, http.StatusRequestEntityTooLarge, "recording too large")
			return
		}
		writeErr(w, http.StatusBadRequest, "upload interrupted")
		return
	}
	sum := hex.EncodeToString(h.Sum(nil))
	if !ed25519.Verify(dev.PublicKey, recordingSignedMessage(rec.DeviceID, sid, sum), sig) {
		_ = a.store.AddAudit(r.Context(), nil, ip, "recording.rejected", rec.DeviceID, map[string]any{"sid": sid, "reason": "bad_signature"})
		writeErr(w, http.StatusUnauthorized, "device signature invalid")
		return
	}
	if _, err := tmp.Seek(0, io.SeekStart); err != nil {
		a.internal(w, err)
		return
	}
	info, err := recording.Scan(tmp)
	if err == nil && (info.Header.SID != sid || info.Header.DeviceID != rec.DeviceID) {
		err = errors.New("recording belongs to another session")
	}
	if err != nil {
		_ = a.store.AddAudit(r.Context(), nil, ip, "recording.rejected", rec.DeviceID, map[string]any{"sid": sid, "reason": err.Error()})
		writeErr(w, http.StatusBadRequest, "invalid recording: "+err.Error())
		return
	}
	if err := tmp.Sync(); err != nil {
		a.internal(w, err)
		return
	}
	if err := os.Rename(tmp.Name(), a.recordingPath(sid)); err != nil {
		a.internal(w, err)
		return
	}
	meta := store.RecordingMeta{DurationMS: info.DurationMS, SizeBytes: size, SHA256: sum, Codec: info.Codec, Frames: info.Frames, Audio: info.Audio}
	if err := a.store.CompleteRecording(r.Context(), rec.ID, meta); err != nil {
		os.Remove(a.recordingPath(sid))
		storeErr(a, w, err)
		return
	}
	_ = a.store.AddAudit(r.Context(), rec.UserID, ip, "recording.uploaded", rec.DeviceID, map[string]any{
		"sid": sid, "recording_id": rec.ID, "bytes": size, "duration_ms": info.DurationMS, "codec": info.Codec, "audio": info.Audio, "sha256": sum,
	})
	a.log.Info("session recording stored", "sid", sid, "device", rec.DeviceID, "bytes", size)
	writeJSON(w, http.StatusOK, map[string]any{"ok": true, "id": rec.ID})
}

func (a *API) listRecordings(w http.ResponseWriter, r *http.Request) {
	limit, _ := strconv.Atoi(r.URL.Query().Get("limit"))
	if limit <= 0 || limit > 500 {
		limit = 200
	}
	recs, err := a.store.ListRecordings(r.Context(), r.URL.Query().Get("device"), limit)
	if err != nil {
		a.internal(w, err)
		return
	}
	writeJSON(w, http.StatusOK, map[string]any{"recordings": recs})
}

func (a *API) recordingParam(w http.ResponseWriter, r *http.Request) (*store.Recording, bool) {
	id, err := strconv.ParseInt(r.PathValue("id"), 10, 64)
	if err != nil {
		writeErr(w, http.StatusBadRequest, "invalid id")
		return nil, false
	}
	rec, err := a.store.GetRecording(r.Context(), id)
	if err != nil {
		storeErr(a, w, err)
		return nil, false
	}
	return rec, true
}

func (a *API) getRecording(w http.ResponseWriter, r *http.Request) {
	if rec, ok := a.recordingParam(w, r); ok {
		writeJSON(w, http.StatusOK, map[string]any{"recording": rec})
	}
}

// recordingFile streams the recording to an admin (Range requests supported).
func (a *API) recordingFile(w http.ResponseWriter, r *http.Request) {
	rec, ok := a.recordingParam(w, r)
	if !ok {
		return
	}
	if rec.Status != "ready" {
		writeErr(w, http.StatusConflict, "recording has not been uploaded yet")
		return
	}
	f, err := os.Open(a.recordingPath(rec.SID))
	if err != nil {
		writeErr(w, http.StatusNotFound, "recording file is missing")
		return
	}
	defer f.Close()
	st, err := f.Stat()
	if err != nil {
		a.internal(w, err)
		return
	}
	// One audit entry per viewing, not per range request.
	if rg := r.Header.Get("Range"); rg == "" || rg == "bytes=0-" {
		a.audit(r, "recording.viewed", rec.DeviceID, map[string]any{"sid": rec.SID, "recording_id": rec.ID})
	}
	h := w.Header()
	h.Set("Content-Type", "application/octet-stream")
	h.Set("Cache-Control", "no-store")
	h.Set("Content-Disposition", `attachment; filename="aegiron-`+rec.DeviceID+`-`+rec.CreatedAt.UTC().Format("20060102-150405")+`.aerec"`)
	http.ServeContent(w, r, "", st.ModTime(), f)
}

func (a *API) deleteRecording(w http.ResponseWriter, r *http.Request) {
	rec, ok := a.recordingParam(w, r)
	if !ok {
		return
	}
	if err := os.Remove(a.recordingPath(rec.SID)); err != nil && !errors.Is(err, os.ErrNotExist) {
		a.internal(w, err)
		return
	}
	if err := a.store.DeleteRecording(r.Context(), rec.ID); err != nil {
		a.internal(w, err)
		return
	}
	a.audit(r, "recording.deleted", rec.DeviceID, map[string]any{"sid": rec.SID, "recording_id": rec.ID})
	writeJSON(w, http.StatusOK, map[string]bool{"ok": true})
}

func (a *API) getRecordingSettings(w http.ResponseWriter, r *http.Request) {
	st, err := a.store.GetRecordingSettings(r.Context())
	if err != nil {
		a.internal(w, err)
		return
	}
	writeJSON(w, http.StatusOK, map[string]any{"settings": st})
}

func (a *API) putRecordingSettings(w http.ResponseWriter, r *http.Request) {
	var st store.RecordingSettings
	if !readJSON(w, r, &st) {
		return
	}
	if st.Mode != "off" && st.Mode != "all" && st.Mode != "groups" {
		writeErr(w, http.StatusBadRequest, "mode must be off, all or groups")
		return
	}
	if st.RetentionDays < 0 || st.RetentionDays > 3650 {
		writeErr(w, http.StatusBadRequest, "retention_days must be between 0 and 3650")
		return
	}
	if st.GroupIDs == nil || st.Mode != "groups" {
		st.GroupIDs = []int64{}
	}
	if err := a.store.SaveRecordingSettings(r.Context(), st); err != nil {
		a.internal(w, err)
		return
	}
	a.audit(r, "recording.settings_updated", "", map[string]any{"mode": st.Mode, "group_ids": st.GroupIDs, "retention_days": st.RetentionDays})
	writeJSON(w, http.StatusOK, map[string]any{"settings": st})
}

// PurgeRecordings deletes recordings past the retention period and uploads that never came.
func (a *API) PurgeRecordings(ctx context.Context) {
	st, err := a.store.GetRecordingSettings(ctx)
	if err != nil {
		return
	}
	expired, err := a.store.ExpiredRecordings(ctx, st.RetentionDays, recordingUploadWindow)
	if err != nil {
		a.log.Error("recording retention query failed", "err", err)
		return
	}
	for _, rec := range expired {
		if err := os.Remove(a.recordingPath(rec.SID)); err != nil && !errors.Is(err, os.ErrNotExist) {
			a.log.Error("cannot delete expired recording", "sid", rec.SID, "err", err)
			continue
		}
		if err := a.store.DeleteRecording(ctx, rec.ID); err != nil {
			continue
		}
		reason := "retention"
		if rec.Status == "pending" {
			reason = "never_uploaded"
		}
		_ = a.store.AddAudit(ctx, nil, "", "recording.purged", rec.DeviceID, map[string]any{"sid": rec.SID, "recording_id": rec.ID, "reason": reason})
	}
}
