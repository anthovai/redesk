package hub

import (
	"context"
	"errors"
	"log/slog"
	"testing"
)

func TestSessionOptions(t *testing.T) {
	for in, want := range map[SessionOptions]SessionOptions{
		{}:                                       {Kind: KindScreen},
		{Kind: "screen", Target: "ignored:1"}:    {Kind: KindScreen},
		{Kind: "terminal"}:                       {Kind: KindTerminal},
		{Kind: "tunnel", Target: "LocalHost:22"}: {Kind: KindTunnel, Target: "localhost:22"},
		{Kind: "tunnel", Target: "[::1]:3389"}:   {Kind: KindTunnel, Target: "[::1]:3389"},
		{Kind: "tunnel", Target: "10.0.0.5:80"}:  {Kind: KindTunnel, Target: "10.0.0.5:80"},
	} {
		got, err := in.Normalize()
		if err != nil || got != want {
			t.Errorf("%+v: got %+v, %v; want %+v", in, got, err, want)
		}
	}
	for _, bad := range []SessionOptions{
		{Kind: "vpn"},
		{Kind: "tunnel"},
		{Kind: "tunnel", Target: "host"},
		{Kind: "tunnel", Target: "host:0"},
		{Kind: "tunnel", Target: "host:65536"},
		{Kind: "tunnel", Target: "a b:80"},
		{Kind: "tunnel", Target: "::1:22"},
		{Kind: "tunnel", Target: "x/y:80"},
	} {
		if _, err := bad.Normalize(); err == nil {
			t.Errorf("%+v accepted", bad)
		}
	}
}

func TestStartSessionChecksAgentFeatures(t *testing.T) {
	h := New(nil, nil, slog.New(slog.DiscardHandler), nil)
	h.agents["111111111"] = &agentConn{id: "111111111", out: make(chan []byte, 1), features: map[string]bool{}}
	h.agents["222222222"] = &agentConn{id: "222222222", out: make(chan []byte, 1), features: map[string]bool{FeatureTunnel: true}}
	ctx := context.Background()
	v := ViewerInfo{Email: "a@b.c"}

	// An agent that predates tunnels and terminals is never asked for one.
	for _, k := range []SessionOptions{{Kind: KindTerminal}, {Kind: KindTunnel, Target: "localhost:22"}} {
		if _, err := h.StartSessionOf(ctx, "111111111", 1, v, false, k); !errors.Is(err, ErrUnsupported) {
			t.Errorf("old agent, %s: %v", k.Kind, err)
		}
	}
	if _, err := h.StartSessionOf(ctx, "222222222", 1, v, false, SessionOptions{Kind: KindTerminal}); !errors.Is(err, ErrUnsupported) {
		t.Errorf("tunnel-only agent asked for a terminal: %v", err)
	}
	if _, err := h.StartSessionOf(ctx, "222222222", 1, v, false, SessionOptions{Kind: KindTunnel, Target: "nope"}); !errors.Is(err, ErrBadTarget) {
		t.Errorf("bad target: %v", err)
	}
	if _, err := h.StartSessionOf(ctx, "333333333", 1, v, false, SessionOptions{Kind: KindTerminal}); !errors.Is(err, ErrOffline) {
		t.Errorf("offline: %v", err)
	}
	if !h.agents["222222222"].supports(KindTunnel) || !h.agents["111111111"].supports(KindScreen) {
		t.Error("supports")
	}
}

func TestTaggedAuditDetail(t *testing.T) {
	rl := &relay{kind: KindTunnel, target: "localhost:5432"}
	d := rl.tagged(map[string]any{"sid": "x"})
	if d["type"] != "tunnel" || d["target"] != "localhost:5432" {
		t.Fatalf("tunnel detail %v", d)
	}
	if d := (&relay{kind: KindScreen}).tagged(map[string]any{}); len(d) != 0 {
		t.Fatalf("screen sessions keep their detail as before: %v", d)
	}
	if d := (&relay{kind: KindTerminal}).tagged(map[string]any{}); d["type"] != "terminal" || d["target"] != nil {
		t.Fatalf("terminal detail %v", d)
	}
}
