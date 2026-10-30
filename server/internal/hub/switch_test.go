package hub

import (
	"errors"
	"log/slog"
	"testing"
	"time"
)

func TestSwitchGrantIsOneTimeAndBound(t *testing.T) {
	var g switchGrants
	g.put("tok", &switchGrant{from: "111111111", to: "222222222", expires: time.Now().Add(time.Minute)})
	if _, err := g.take("tok", "333333333"); !errors.Is(err, ErrBadGrant) {
		t.Fatalf("another device redeemed the grant: %v", err)
	}
	sg, err := g.take("tok", "111111111")
	if err != nil || sg.to != "222222222" {
		t.Fatalf("owner could not redeem: %v", err)
	}
	if _, err := g.take("tok", "111111111"); !errors.Is(err, ErrBadGrant) {
		t.Fatal("grant redeemed twice")
	}
	g.put("old", &switchGrant{from: "111111111", expires: time.Now().Add(-time.Second)})
	if _, err := g.take("old", "111111111"); !errors.Is(err, ErrBadGrant) {
		t.Fatal("expired grant redeemed")
	}
	if _, err := g.take("", "111111111"); err == nil {
		t.Fatal("empty grant accepted")
	}
}

func TestOfferSwitchNeedsTheViewersActiveSession(t *testing.T) {
	h := New(nil, nil, slog.New(slog.DiscardHandler), nil)
	rl := &relay{sid: "s1", deviceID: "111111111", userID: 7, kind: KindScreen, paired: make(chan struct{}), done: make(chan struct{})}
	h.relays["s1"] = rl
	h.agents["222222222"] = &agentConn{id: "222222222", out: make(chan []byte, 1)}

	if _, err := h.OfferSwitch("s1", 7, "222222222", "pc", nil); !errors.Is(err, ErrNoSession) {
		t.Fatalf("offered before the session was paired: %v", err)
	}
	close(rl.paired)
	h.relays["t1"] = &relay{sid: "t1", deviceID: "111111111", userID: 7, kind: KindTerminal, paired: rl.paired, done: rl.done}
	if _, err := h.OfferSwitch("t1", 7, "222222222", "pc", nil); !errors.Is(err, ErrNoSession) {
		t.Fatalf("offered from a terminal session: %v", err)
	}
	if _, err := h.OfferSwitch("s1", 8, "222222222", "pc", nil); !errors.Is(err, ErrNoSession) {
		t.Fatalf("another user offered: %v", err)
	}
	if _, err := h.OfferSwitch("s1", 7, "111111111", "pc", nil); !errors.Is(err, ErrSameDevice) {
		t.Fatalf("offered the controlled device itself: %v", err)
	}
	if _, err := h.OfferSwitch("s1", 7, "333333333", "pc", nil); !errors.Is(err, ErrOffline) {
		t.Fatalf("offered an offline device: %v", err)
	}
	tok, err := h.OfferSwitch("s1", 7, "222222222", "pc", nil)
	if err != nil || tok == "" {
		t.Fatalf("offer failed: %v", err)
	}
	sg, err := h.grants.take(tok, "111111111")
	if err != nil || sg.to != "222222222" || sg.userID != 7 || sg.origSID != "s1" {
		t.Fatalf("grant %+v, %v", sg, err)
	}
}
