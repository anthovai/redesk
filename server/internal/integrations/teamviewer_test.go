package integrations

import (
	"encoding/json"
	"net/http"
	"net/http/httptest"
	"testing"
)

// The fake mirrors the documented response shapes; it is not a substitute for testing
// against a real TeamViewer account.
func TestClientPagingAndAuth(t *testing.T) {
	var pages []string
	srv := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		if r.Header.Get("Authorization") != "Bearer tok" {
			w.WriteHeader(http.StatusUnauthorized)
			return
		}
		switch r.URL.Path {
		case "/api/v1/reports/connections":
			pages = append(pages, r.URL.Query().Get("offset_id"))
			if r.URL.Query().Get("offset_id") == "" {
				json.NewEncoder(w).Encode(map[string]any{"records": []map[string]any{{"id": "a"}}, "next_offset": "o2"})
			} else {
				json.NewEncoder(w).Encode(map[string]any{"records": []map[string]any{{"id": "b"}}})
			}
		}
	}))
	defer srv.Close()

	c := &tvClient{base: srv.URL, token: "tok", http: srv.Client()}
	var rep tvConnections
	if err := c.get(t.Context(), "/api/v1/reports/connections", nil, &rep); err != nil || rep.NextOffset != "o2" {
		t.Fatalf("%v %+v", err, rep)
	}
	bad := &tvClient{base: srv.URL, token: "wrong", http: srv.Client()}
	if err := bad.get(t.Context(), "/api/v1/devices", nil, &rep); err == nil {
		t.Fatal("expected 401 error")
	}
}
