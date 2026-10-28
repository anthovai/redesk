package api

import (
	"context"
	"encoding/json"
	"io"
	"net/http"
	"strconv"
	"strings"
	"sync/atomic"
	"time"

	"github.com/coder/websocket"

	"aegiron/server/internal/auth"
	"aegiron/server/internal/gateway"
	"aegiron/server/internal/store"
)

// Guacamole status codes, sent as the WebSocket close reason (the JS client parses them).
const (
	guacServerError     = "512"
	guacUpstreamTimeout = "516"
	guacUpstreamError   = "514"
	guacUnauthorized    = "769"
	guacForbidden       = "771"
)

// gatewayParams maps an endpoint to guacd connection parameters (credentials included).
func (a *API) gatewayParams(e *store.Endpoint, secret, sid string) map[string]string {
	p := map[string]string{
		"hostname": e.Address,
		"port":     strconv.Itoa(*e.Port),
		"username": e.Username,
		"password": secret,
	}
	switch e.Provider {
	case "rdp":
		var o rdpOptions
		_ = json.Unmarshal(e.Options, &o)
		p["domain"] = o.Domain
		p["security"] = o.Security
		if o.IgnoreCert {
			p["ignore-cert"] = "true"
		}
		p["resize-method"] = "display-update"
		p["enable-font-smoothing"] = "true"
	case "ssh":
		p["font-size"] = "12"
		p["color-scheme"] = "white-black"
	}
	if a.cfg.GatewayRecordingPath != "" {
		// guacd writes a session recording (playable with guacamole-common-js) per session.
		p["recording-path"] = a.cfg.GatewayRecordingPath
		p["recording-name"] = sid
		p["create-recording-path"] = "true"
		if e.Provider == "ssh" {
			p["typescript-path"] = a.cfg.GatewayRecordingPath
			p["typescript-name"] = sid
			p["create-typescript-path"] = "true"
		}
	}
	return p
}

// handleGateway: GET /ws/gateway?endpoint=ID&width=&height=&dpi=  (guacamole-common-js WebSocketTunnel)
func (a *API) handleGateway(w http.ResponseWriter, r *http.Request) {
	if a.cfg.GuacdAddr == "" {
		http.Error(w, "gateway disabled", http.StatusNotFound)
		return
	}
	accept := func() *websocket.Conn {
		c, err := websocket.Accept(w, r, &websocket.AcceptOptions{Subprotocols: []string{"guacamole"}, OriginPatterns: a.cfg.AllowedOrigins})
		if err != nil {
			return nil
		}
		return c
	}
	u := a.sessionUser(r)
	if u == nil {
		if c := accept(); c != nil {
			c.Close(websocket.StatusPolicyViolation, guacUnauthorized)
		}
		return
	}
	r = r.WithContext(context.WithValue(r.Context(), ctxKey{}, u))
	q := r.URL.Query()
	id, _ := strconv.ParseInt(q.Get("endpoint"), 10, 64)
	e, err := a.store.GetEndpoint(r.Context(), id)
	allowed := err == nil && gatewayProviders[e.Provider]
	if allowed && u.Role != "admin" {
		allowed, _ = a.store.UserCanUseEndpoint(r.Context(), u.ID, e)
	}
	c := accept()
	if c == nil {
		return
	}
	if !allowed {
		c.Close(websocket.StatusPolicyViolation, guacForbidden)
		return
	}
	c.SetReadLimit(1 << 20)

	secret, err := a.vault.Open(e.Secret)
	if err != nil {
		a.log.Error("vault open failed", "endpoint", e.ID, "err", err)
		c.Close(websocket.StatusInternalError, guacServerError)
		return
	}
	dim := func(k string, def, lo, hi int) int {
		v, err := strconv.Atoi(q.Get(k))
		if err != nil {
			return def
		}
		return min(max(v, lo), hi)
	}
	disp := gateway.Display{Width: dim("width", 1280, 320, 7680), Height: dim("height", 720, 240, 4320), DPI: dim("dpi", 96, 48, 300)}

	sid := auth.RandomToken(18)
	ip := a.clientIP(r)
	ctx := r.Context()
	cctx, cancel := context.WithTimeout(ctx, 20*time.Second)
	gc, err := gateway.Connect(cctx, a.cfg.GuacdAddr, e.Provider, a.gatewayParams(e, secret, sid), disp, a.cfg.Timezone)
	cancel()
	if err != nil {
		a.log.Warn("gateway connect failed", "endpoint", e.ID, "err", err)
		a.audit(r, "gateway.failed", strconv.FormatInt(e.ID, 10), map[string]any{"name": e.Name, "provider": e.Provider, "error": err.Error()})
		c.Close(websocket.StatusTryAgainLater, guacUpstreamError)
		return
	}
	defer gc.Close()

	_ = a.store.CreateGatewaySession(ctx, sid, e.ID, u.ID, ip)
	a.audit(r, "gateway.start", strconv.FormatInt(e.ID, 10), map[string]any{
		"sid": sid, "name": e.Name, "provider": e.Provider, "address": e.Address, "recorded": a.cfg.GatewayRecordingPath != "",
	})
	started := time.Now()
	var toBrowser, toTarget atomic.Int64

	// The first message tells the JS tunnel its UUID and marks it open.
	if err := c.Write(ctx, websocket.MessageText, []byte(gateway.Encode("", sid))); err != nil {
		return
	}

	ctx, stop := context.WithCancel(ctx)
	defer stop()
	reason := make(chan string, 2)

	// guacd -> browser: forward whole instructions, batching what is already buffered.
	go func() {
		var buf []byte
		for {
			_, raw, err := gc.Reader.Read()
			if err != nil {
				if err == io.EOF {
					reason <- "target_closed"
				} else {
					reason <- "guacd_error"
				}
				return
			}
			buf = append(buf, raw...)
			if gc.Reader.Buffered() > 0 && len(buf) < 256<<10 {
				continue
			}
			if err := c.Write(ctx, websocket.MessageText, buf); err != nil {
				reason <- "browser_write_failed"
				return
			}
			toBrowser.Add(int64(len(buf)))
			buf = buf[:0]
		}
	}()

	// browser -> guacd: drop the tunnel's internal instructions (answering pings), pass the rest.
	go func() {
		for {
			_, msg, err := c.Read(ctx)
			if err != nil {
				reason <- "browser_closed"
				return
			}
			rd := gateway.NewReader(strings.NewReader(string(msg)))
			for {
				elems, raw, err := rd.Read()
				if err != nil {
					break
				}
				if elems[0] == "" {
					if len(elems) > 1 && elems[1] == "ping" {
						_ = c.Write(ctx, websocket.MessageText, []byte(gateway.Encode(elems...)))
					}
					continue
				}
				if _, err := gc.Write(raw); err != nil {
					reason <- "guacd_write_failed"
					return
				}
				toTarget.Add(int64(len(raw)))
			}
		}
	}()

	why := <-reason
	stop()
	gc.Close()
	c.Close(websocket.StatusNormalClosure, "0")

	ectx, cancel2 := context.WithTimeout(context.Background(), 5*time.Second)
	defer cancel2()
	_ = a.store.MarkSessionEnded(ectx, sid, toBrowser.Load(), toTarget.Load(), why)
	_ = a.store.AddAudit(ectx, &u.ID, ip, "gateway.end", strconv.FormatInt(e.ID, 10), map[string]any{
		"sid": sid, "name": e.Name, "reason": why, "seconds": int(time.Since(started).Seconds()),
		"bytes_to_viewer": toBrowser.Load(), "bytes_to_target": toTarget.Load(),
	})
}
