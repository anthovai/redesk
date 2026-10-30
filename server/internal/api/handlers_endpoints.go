package api

import (
	"encoding/binary"
	"encoding/csv"
	"encoding/json"
	"encoding/pem"
	"errors"
	"fmt"
	"io"
	"net/http"
	"net/url"
	"regexp"
	"strconv"
	"strings"
	"time"

	"aegiron/server/internal/store"
)

// Vendor ids are digits only: they end up in custom URL handlers, which have a history of
// argument-injection bugs (e.g. TeamViewer CVE-2020-13699), so nothing else may pass.
var (
	vendorID     = regexp.MustCompile(`^[0-9]{6,12}$`)
	anydeskAlias = regexp.MustCompile(`^[A-Za-z0-9][A-Za-z0-9._-]{0,63}@[A-Za-z0-9._-]{1,32}$`)
	hostName     = regexp.MustCompile(`^[A-Za-z0-9]([A-Za-z0-9.-]{0,251}[A-Za-z0-9])?$`)
	ipv6Literal  = regexp.MustCompile(`^[0-9A-Fa-f:]{2,39}$`)
	safeText     = regexp.MustCompile(`^[^\x00-\x1f\x7f]*$`)
)

var defaultPorts = map[string]int{"rdp": 3389, "vnc": 5900, "ssh": 22}

// gatewayProviders can be opened in the browser through guacd.
var gatewayProviders = map[string]bool{"rdp": true, "vnc": true, "ssh": true}

type endpointReq struct {
	Name     string          `json:"name"`
	Provider string          `json:"provider"`
	Address  string          `json:"address"`
	Port     *int            `json:"port"`
	Username string          `json:"username"`
	Password *string         `json:"password"` // nil = keep, "" = clear
	Options  json.RawMessage `json:"options"`
	GroupID  *int64          `json:"group_id"`
	Tags     []string        `json:"tags"`
	Note     string          `json:"note"`
}

type rdpOptions struct {
	Security   string `json:"security,omitempty"` // any | nla | tls | rdp
	IgnoreCert bool   `json:"ignore_cert,omitempty"`
	Domain     string `json:"domain,omitempty"`
}

type sshOptions struct {
	// Auth "key": the vault secret is an unencrypted private key (OpenSSH or PEM) instead of a password.
	Auth string `json:"auth,omitempty"` // password (default) | key
}

func sshKeyAuth(e *store.Endpoint) bool {
	if e.Provider != "ssh" {
		return false
	}
	var o sshOptions
	_ = json.Unmarshal(e.Options, &o)
	return o.Auth == "key"
}

// normalizePrivateKey checks that s is an SSH private key guacd can use without a passphrase
// and returns it with Unix line endings (keys pasted from Windows editors carry CRLF).
func normalizePrivateKey(s string) (string, error) {
	s = strings.TrimSpace(strings.ReplaceAll(s, "\r\n", "\n")) + "\n"
	if len(s) > 16<<10 {
		return "", errors.New("private key is too large")
	}
	block, _ := pem.Decode([]byte(s))
	if block == nil || !strings.HasSuffix(block.Type, "PRIVATE KEY") {
		return "", errors.New("SSH key must be a private key in OpenSSH or PEM format (-----BEGIN ... PRIVATE KEY-----)")
	}
	encrypted := block.Headers["Proc-Type"] == "4,ENCRYPTED" || block.Type == "ENCRYPTED PRIVATE KEY"
	if block.Type == "OPENSSH PRIVATE KEY" {
		// openssh-key-v1\0, then string ciphername ("none" when not protected by a passphrase).
		const magic = "openssh-key-v1\x00"
		b := block.Bytes
		if len(b) < len(magic)+4 || string(b[:len(magic)]) != magic {
			return "", errors.New("invalid OpenSSH private key")
		}
		b = b[len(magic):]
		n := int(binary.BigEndian.Uint32(b))
		encrypted = n > len(b)-4 || string(b[4:4+n]) != "none"
	}
	if encrypted {
		return "", errors.New("passphrase-protected SSH keys are not supported; store an unencrypted key (ssh-keygen -p -N '')")
	}
	return s, nil
}

// sealSecret encrypts a password, or for SSH key authentication a validated private key.
func (a *API) sealSecret(e *store.Endpoint, secret string) ([]byte, error) {
	if secret != "" && sshKeyAuth(e) {
		k, err := normalizePrivateKey(secret)
		if err != nil {
			return nil, userError{err}
		}
		secret = k
	}
	return a.vault.Seal(secret)
}

// userError is a validation failure whose message is safe to show.
type userError struct{ error }

func (a *API) secretErr(w http.ResponseWriter, err error) {
	var ue userError
	if errors.As(err, &ue) {
		writeErr(w, http.StatusBadRequest, ue.Error())
		return
	}
	a.internal(w, err)
}

// normalize validates a request into an endpoint, returning a user-facing error.
func normalizeEndpoint(req *endpointReq) (*store.Endpoint, error) {
	e := &store.Endpoint{
		Name:     trim(req.Name, 100),
		Provider: req.Provider,
		Address:  strings.TrimSpace(req.Address),
		Username: strings.TrimSpace(req.Username),
		Note:     trim(req.Note, 2000),
		GroupID:  req.GroupID,
		Options:  json.RawMessage(`{}`),
	}
	if e.Name == "" {
		return nil, errors.New("name is required")
	}
	if !safeText.MatchString(e.Name) || !safeText.MatchString(e.Username) || len(e.Username) > 128 {
		return nil, errors.New("name and username must not contain control characters")
	}
	for _, t := range req.Tags {
		if t = trim(t, 32); t != "" && len(e.Tags) < 16 {
			e.Tags = append(e.Tags, t)
		}
	}
	switch e.Provider {
	case "anydesk":
		e.Address = strings.ReplaceAll(e.Address, " ", "")
		if !vendorID.MatchString(e.Address) && !anydeskAlias.MatchString(e.Address) {
			return nil, errors.New("AnyDesk address must be a numeric id or an alias like name@ad")
		}
	case "teamviewer", "rustdesk":
		e.Address = strings.ReplaceAll(e.Address, " ", "")
		if !vendorID.MatchString(e.Address) {
			return nil, fmt.Errorf("%s id must be 6-12 digits", e.Provider)
		}
	case "rdp", "vnc", "ssh":
		h := strings.TrimSuffix(strings.TrimPrefix(e.Address, "["), "]")
		if !hostName.MatchString(h) && !ipv6Literal.MatchString(h) {
			return nil, errors.New("invalid host name or IP address")
		}
		e.Address = h
		port := defaultPorts[e.Provider]
		if req.Port != nil {
			port = *req.Port
		}
		if port < 1 || port > 65535 {
			return nil, errors.New("port must be 1-65535")
		}
		e.Port = &port
		if e.Provider == "rdp" && len(req.Options) > 0 && string(req.Options) != "null" {
			var o rdpOptions
			dec := json.NewDecoder(strings.NewReader(string(req.Options)))
			dec.DisallowUnknownFields()
			if err := dec.Decode(&o); err != nil {
				return nil, errors.New("invalid RDP options")
			}
			switch o.Security {
			case "", "any", "nla", "tls", "rdp":
			default:
				return nil, errors.New("RDP security must be any, nla, tls or rdp")
			}
			if !safeText.MatchString(o.Domain) || len(o.Domain) > 64 {
				return nil, errors.New("invalid RDP domain")
			}
			e.Options, _ = json.Marshal(o)
		}
		if e.Provider == "ssh" && len(req.Options) > 0 && string(req.Options) != "null" {
			var o sshOptions
			dec := json.NewDecoder(strings.NewReader(string(req.Options)))
			dec.DisallowUnknownFields()
			if err := dec.Decode(&o); err != nil {
				return nil, errors.New("invalid SSH options")
			}
			switch o.Auth {
			case "", "password":
				o.Auth = ""
			case "key":
			default:
				return nil, errors.New("SSH auth must be password or key")
			}
			e.Options, _ = json.Marshal(o)
		}
	default:
		return nil, errors.New("provider must be anydesk, teamviewer, rustdesk, rdp, vnc or ssh")
	}
	return e, nil
}

func (a *API) endpointParam(w http.ResponseWriter, r *http.Request) (*store.Endpoint, bool) {
	id, err := strconv.ParseInt(r.PathValue("id"), 10, 64)
	if err != nil {
		writeErr(w, http.StatusBadRequest, "invalid id")
		return nil, false
	}
	e, err := a.store.GetEndpoint(r.Context(), id)
	if err != nil {
		storeErr(a, w, err)
		return nil, false
	}
	u := userFrom(r)
	if u.Role != "admin" {
		ok, err := a.store.UserCanUseEndpoint(r.Context(), u.ID, e)
		if err != nil {
			a.internal(w, err)
			return nil, false
		}
		if !ok {
			writeErr(w, http.StatusNotFound, "not found")
			return nil, false
		}
	}
	return e, true
}

// canEditEndpoint: admins edit everything; users edit the endpoints they created.
func canEditEndpoint(u *store.User, e *store.Endpoint) bool {
	return u.Role == "admin" || (e.CreatedBy != nil && *e.CreatedBy == u.ID && e.Source != "teamviewer_api")
}

func (a *API) listEndpoints(w http.ResponseWriter, r *http.Request) {
	u := userFrom(r)
	uid := u.ID
	if u.Role == "admin" {
		uid = 0
	}
	list, err := a.store.ListEndpoints(r.Context(), uid)
	if err != nil {
		a.internal(w, err)
		return
	}
	writeJSON(w, http.StatusOK, map[string]any{"endpoints": list, "gateway": a.cfg.GuacdAddr != ""})
}

func (a *API) createEndpoint(w http.ResponseWriter, r *http.Request) {
	var req endpointReq
	if !readJSON(w, r, &req) {
		return
	}
	u := userFrom(r)
	e, err := normalizeEndpoint(&req)
	if err != nil {
		writeErr(w, http.StatusBadRequest, err.Error())
		return
	}
	if u.Role != "admin" {
		e.GroupID = nil // sharing with a group is an admin decision
	}
	if req.Password != nil {
		if e.Secret, err = a.sealSecret(e, *req.Password); err != nil {
			a.secretErr(w, err)
			return
		}
	}
	e.Source, e.CreatedBy = "manual", &u.ID
	id, err := a.store.CreateEndpoint(r.Context(), e)
	if err != nil {
		a.internal(w, err)
		return
	}
	a.audit(r, "endpoint.created", strconv.FormatInt(id, 10), map[string]any{"name": e.Name, "provider": e.Provider, "address": e.Address})
	writeJSON(w, http.StatusCreated, map[string]any{"id": id})
}

func (a *API) updateEndpoint(w http.ResponseWriter, r *http.Request) {
	cur, ok := a.endpointParam(w, r)
	if !ok {
		return
	}
	u := userFrom(r)
	if !canEditEndpoint(u, cur) {
		writeErr(w, http.StatusForbidden, "you cannot edit this endpoint")
		return
	}
	var req endpointReq
	if !readJSON(w, r, &req) {
		return
	}
	e, err := normalizeEndpoint(&req)
	if err != nil {
		writeErr(w, http.StatusBadRequest, err.Error())
		return
	}
	e.ID = cur.ID
	if u.Role != "admin" {
		e.GroupID = cur.GroupID
	}
	if cur.Source == "teamviewer_api" { // identity follows the vendor
		e.Provider, e.Address = cur.Provider, cur.Address
	}
	keep := req.Password == nil
	if keep && sshKeyAuth(e) != sshKeyAuth(cur) && len(cur.Secret) > 0 {
		// A stored password is not a key (and a key must never be sent as a password).
		writeErr(w, http.StatusBadRequest, "enter the new password or private key when changing the SSH authentication method")
		return
	}
	if !keep {
		if e.Secret, err = a.sealSecret(e, *req.Password); err != nil {
			a.secretErr(w, err)
			return
		}
	}
	if err := a.store.UpdateEndpoint(r.Context(), e, keep); err != nil {
		storeErr(a, w, err)
		return
	}
	a.audit(r, "endpoint.updated", strconv.FormatInt(e.ID, 10), map[string]any{"name": e.Name, "password_changed": !keep})
	writeJSON(w, http.StatusOK, map[string]bool{"ok": true})
}

func (a *API) deleteEndpoint(w http.ResponseWriter, r *http.Request) {
	e, ok := a.endpointParam(w, r)
	if !ok {
		return
	}
	if !canEditEndpoint(userFrom(r), e) {
		writeErr(w, http.StatusForbidden, "you cannot delete this endpoint")
		return
	}
	if err := a.store.DeleteEndpoint(r.Context(), e.ID); err != nil {
		a.internal(w, err)
		return
	}
	a.audit(r, "endpoint.deleted", strconv.FormatInt(e.ID, 10), map[string]any{"name": e.Name, "provider": e.Provider})
	writeJSON(w, http.StatusOK, map[string]bool{"ok": true})
}

// launchEndpoint returns how to open an endpoint with the vendor's own client.
// Passwords never go into URLs (they would land in browser history and process lists);
// the console copies them to the clipboard instead.
func (a *API) launchEndpoint(w http.ResponseWriter, r *http.Request) {
	e, ok := a.endpointParam(w, r)
	if !ok {
		return
	}
	if !a.limiter.allow(fmt.Sprintf("launch:%d", userFrom(r).ID), 60, time.Minute) {
		writeErr(w, http.StatusTooManyRequests, "too many launches")
		return
	}
	secret, err := a.vault.Open(e.Secret)
	if err != nil {
		a.internal(w, err)
		return
	}
	if !mayReleaseSecret(userFrom(r), e, a.cfg.GuacdAddr != "") {
		secret = ""
	}
	resp := map[string]any{"provider": e.Provider}
	if secret != "" {
		resp["secret"] = secret
	}
	switch e.Provider {
	case "anydesk":
		resp["url"] = "anydesk:" + e.Address
	case "teamviewer":
		resp["url"] = "teamviewer10://control?device=" + e.Address
	case "rustdesk":
		resp["url"] = "rustdesk://connection/new/" + e.Address
	case "rdp":
		resp["file"] = map[string]string{"name": safeFileName(e.Name) + ".rdp", "content": rdpFile(e)}
	case "vnc":
		resp["url"] = (&url.URL{Scheme: "vnc", Host: hostPort(e)}).String()
	case "ssh":
		u := &url.URL{Scheme: "ssh", Host: hostPort(e)}
		if e.Username != "" {
			u.User = url.User(e.Username)
		}
		resp["url"] = u.String()
	}
	a.audit(r, "endpoint.launch", strconv.FormatInt(e.ID, 10), map[string]any{
		"name": e.Name, "provider": e.Provider, "address": e.Address, "mode": "native", "password_released": secret != "",
	})
	writeJSON(w, http.StatusOK, resp)
}

// mayReleaseSecret decides whether "open with the local program" hands the stored password
// to the browser. SSH private keys never leave the server. With the gateway on, RDP/VNC/SSH
// passwords go only to admins and the entry's creator: group members it was shared with use
// the gateway, where the server injects the password and the browser never sees it.
func mayReleaseSecret(u *store.User, e *store.Endpoint, gatewayOn bool) bool {
	if sshKeyAuth(e) {
		return false
	}
	if !gatewayOn || !gatewayProviders[e.Provider] || u.Role == "admin" {
		return true
	}
	return e.CreatedBy != nil && *e.CreatedBy == u.ID
}

func hostPort(e *store.Endpoint) string {
	h := e.Address
	if strings.Contains(h, ":") {
		h = "[" + h + "]"
	}
	if e.Port != nil {
		h += ":" + strconv.Itoa(*e.Port)
	}
	return h
}

// rdpFile builds a Microsoft .rdp file. Inputs are validated free of control characters,
// so a value cannot inject extra settings lines.
func rdpFile(e *store.Endpoint) string {
	var o rdpOptions
	_ = json.Unmarshal(e.Options, &o)
	lines := []string{"full address:s:" + hostPort(e), "prompt for credentials:i:1", "screen mode id:i:2"}
	if e.Username != "" {
		lines = append(lines, "username:s:"+e.Username)
	}
	if o.Domain != "" {
		lines = append(lines, "domain:s:"+o.Domain)
	}
	return strings.Join(lines, "\r\n") + "\r\n"
}

func safeFileName(s string) string {
	s = strings.Map(func(r rune) rune {
		if r < 32 || strings.ContainsRune(`/\:*?"<>|`, r) {
			return '_'
		}
		return r
	}, s)
	if s == "" {
		return "connection"
	}
	return s
}

// importEndpoints creates endpoints from CSV with header:
// name,provider,address,port,username,password,tags,note   (tags separated by ';')
func (a *API) importEndpoints(w http.ResponseWriter, r *http.Request) {
	var req struct {
		CSV     string `json:"csv"`
		GroupID *int64 `json:"group_id"`
	}
	if !readJSON(w, r, &req) {
		return
	}
	cr := csv.NewReader(strings.NewReader(strings.TrimPrefix(req.CSV, "\uFEFF")))
	cr.FieldsPerRecord = -1
	header, err := cr.Read()
	if err != nil {
		writeErr(w, http.StatusBadRequest, "empty CSV")
		return
	}
	col := map[string]int{}
	for i, h := range header {
		col[strings.ToLower(strings.TrimSpace(h))] = i
	}
	for _, need := range []string{"name", "provider", "address"} {
		if _, ok := col[need]; !ok {
			writeErr(w, http.StatusBadRequest, "CSV header must include name, provider, address")
			return
		}
	}
	get := func(rec []string, k string) string {
		if i, ok := col[k]; ok && i < len(rec) {
			return strings.TrimSpace(rec[i])
		}
		return ""
	}
	u := userFrom(r)
	created, line := 0, 1
	var problems []string
	for {
		rec, err := cr.Read()
		if err == io.EOF {
			break
		}
		line++
		if err != nil {
			problems = append(problems, fmt.Sprintf("line %d: %v", line, err))
			continue
		}
		er := endpointReq{Name: get(rec, "name"), Provider: strings.ToLower(get(rec, "provider")), Address: get(rec, "address"),
			Username: get(rec, "username"), Note: get(rec, "note"), GroupID: req.GroupID}
		if p := get(rec, "port"); p != "" {
			n, err := strconv.Atoi(p)
			if err != nil {
				problems = append(problems, fmt.Sprintf("line %d: invalid port", line))
				continue
			}
			er.Port = &n
		}
		if t := get(rec, "tags"); t != "" {
			er.Tags = strings.Split(t, ";")
		}
		e, err := normalizeEndpoint(&er)
		if err != nil {
			problems = append(problems, fmt.Sprintf("line %d: %v", line, err))
			continue
		}
		if e.Secret, err = a.vault.Seal(get(rec, "password")); err != nil {
			a.internal(w, err)
			return
		}
		e.Source, e.CreatedBy = "import", &u.ID
		if _, err := a.store.CreateEndpoint(r.Context(), e); err != nil {
			problems = append(problems, fmt.Sprintf("line %d: %v", line, err))
			continue
		}
		created++
		if line > 5001 {
			problems = append(problems, "stopped after 5000 rows")
			break
		}
	}
	a.audit(r, "endpoint.imported", "", map[string]any{"created": created, "errors": len(problems)})
	writeJSON(w, http.StatusOK, map[string]any{"created": created, "errors": problems})
}
