// Package integrations mirrors device inventories and connection reports from other
// remote-desktop vendors into Aegiron.
//
// TeamViewer: Web API v1 with a script token (Bearer). /devices lists the account's
// devices; /reports/connections returns up to 1000 records per call and a next_offset
// to page with offset_id. AnyDesk's REST API does not publicly document session or client
// endpoints, so AnyDesk inventories come in through CSV import instead.
package integrations

import (
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"net/http"
	"net/url"
	"regexp"
	"strings"
	"time"

	"aegiron/server/internal/store"
	"aegiron/server/internal/vault"
)

const TeamViewer = "teamviewer"

var tvDigits = regexp.MustCompile(`^[0-9]{6,12}$`)

type Config struct {
	Token string `json:"token"`
}

type Result struct {
	Devices        int `json:"devices"`
	NewDevices     int `json:"new_devices"`
	Sessions       int `json:"sessions"`
	SkippedDevices int `json:"skipped_devices"`
}

type tvClient struct {
	base  string
	token string
	http  *http.Client
}

func (c *tvClient) get(ctx context.Context, path string, q url.Values, out any) error {
	u := strings.TrimRight(c.base, "/") + path
	if len(q) > 0 {
		u += "?" + q.Encode()
	}
	req, err := http.NewRequestWithContext(ctx, http.MethodGet, u, nil)
	if err != nil {
		return err
	}
	req.Header.Set("Authorization", "Bearer "+c.token)
	req.Header.Set("Accept", "application/json")
	res, err := c.http.Do(req)
	if err != nil {
		return err
	}
	defer res.Body.Close()
	body, err := io.ReadAll(io.LimitReader(res.Body, 32<<20))
	if err != nil {
		return err
	}
	if res.StatusCode != http.StatusOK {
		msg := strings.TrimSpace(string(body))
		if len(msg) > 200 {
			msg = msg[:200]
		}
		if res.StatusCode == http.StatusUnauthorized || res.StatusCode == http.StatusForbidden {
			return fmt.Errorf("TeamViewer %s: %d — check the script token and its permissions (%s)", path, res.StatusCode, msg)
		}
		return fmt.Errorf("TeamViewer %s: HTTP %d: %s", path, res.StatusCode, msg)
	}
	return json.Unmarshal(body, out)
}

type tvDevice struct {
	RemoteControlID string `json:"remotecontrol_id"` // "r123456789"
	DeviceID        string `json:"device_id"`        // "d1234"
	Alias           string `json:"alias"`
	OnlineState     string `json:"online_state"` // "Online" / "Offline"
}

type tvConnections struct {
	Records    []map[string]any `json:"records"`
	NextOffset string           `json:"next_offset"`
}

func str(m map[string]any, keys ...string) string {
	for _, k := range keys {
		if v, ok := m[k].(string); ok && v != "" {
			return v
		}
	}
	return ""
}

func parseTime(s string) *time.Time {
	if t, err := time.Parse(time.RFC3339, s); err == nil {
		return &t
	}
	return nil
}

// SyncTeamViewer imports devices as endpoints and connection reports as external sessions.
func SyncTeamViewer(ctx context.Context, st *store.Store, v *vault.Vault, baseURL string) (*Result, error) {
	in, err := st.GetIntegration(ctx, TeamViewer)
	if err != nil {
		return nil, err
	}
	raw, err := v.Open(in.Config)
	if err != nil {
		return nil, err
	}
	var cfg Config
	if err := json.Unmarshal([]byte(raw), &cfg); err != nil || cfg.Token == "" {
		return nil, errors.New("TeamViewer token is not configured")
	}
	c := &tvClient{base: baseURL, token: cfg.Token, http: &http.Client{Timeout: 30 * time.Second}}
	res, cursor, err := syncTV(ctx, st, c, in)
	msg := ""
	if err != nil {
		msg = err.Error()
	}
	_ = st.RecordSync(ctx, TeamViewer, cursor, msg)
	return res, err
}

func syncTV(ctx context.Context, st *store.Store, c *tvClient, in *store.Integration) (*Result, string, error) {
	res := &Result{}
	var devs struct {
		Devices []tvDevice `json:"devices"`
	}
	if err := c.get(ctx, "/api/v1/devices", nil, &devs); err != nil {
		return res, "", err
	}
	for _, d := range devs.Devices {
		id := strings.TrimPrefix(d.RemoteControlID, "r")
		if !tvDigits.MatchString(id) || d.DeviceID == "" {
			res.SkippedDevices++ // e.g. non-remote-control entries; ids go into URL handlers, so only digits pass
			continue
		}
		name := strings.TrimSpace(d.Alias)
		if name == "" {
			name = "TeamViewer " + id
		}
		online := strings.EqualFold(d.OnlineState, "online")
		created, err := st.UpsertSyncedEndpoint(ctx, "teamviewer_api", d.DeviceID, "teamviewer", name, id, &online, in.GroupID)
		if err != nil {
			return res, "", err
		}
		res.Devices++
		if created {
			res.NewDevices++
		}
	}

	// Connection reports since the last imported start date (first sync: last 30 days).
	from := time.Now().AddDate(0, 0, -30)
	if t := parseTime(in.Cursor); t != nil {
		from = t.Add(-time.Minute) // overlap; rows are upserted by report id
	}
	cursor := in.Cursor
	q := url.Values{"from_date": {from.UTC().Format("2006-01-02T15:04:05Z")}}
	for page := 0; page < 100; page++ {
		var rep tvConnections
		if err := c.get(ctx, "/api/v1/reports/connections", q, &rep); err != nil {
			return res, cursor, err
		}
		for _, r := range rep.Records {
			id := str(r, "id")
			if id == "" {
				continue
			}
			rawJSON, _ := json.Marshal(r)
			x := &store.ExternalSession{
				Provider:   TeamViewer,
				ExternalID: id,
				UserName:   str(r, "username", "userid"),
				TargetName: str(r, "devicename", "targetname"),
				TargetID:   str(r, "deviceid", "targetid"),
				StartedAt:  parseTime(str(r, "start_date")),
				EndedAt:    parseTime(str(r, "end_date")),
				Raw:        rawJSON,
			}
			if err := st.UpsertExternalSession(ctx, x); err != nil {
				return res, cursor, err
			}
			res.Sessions++
			if x.StartedAt != nil {
				if c := parseTime(cursor); c == nil || x.StartedAt.After(*c) {
					cursor = x.StartedAt.UTC().Format(time.RFC3339)
				}
			}
		}
		if rep.NextOffset == "" {
			break
		}
		q.Set("offset_id", rep.NextOffset)
	}
	return res, cursor, nil
}
