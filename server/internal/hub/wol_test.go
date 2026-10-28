package hub

import (
	"reflect"
	"testing"

	"aegiron/server/internal/store"
)

func TestCleanMacs(t *testing.T) {
	in := []string{
		"00:11:22:AA:bb:cc", "00-11-22-aa-bb-cc", // duplicate after normalizing
		"02-00-00-00-00-01",
		"00:00:00:00:00:00", "ff:ff:ff:ff:ff:ff", "01:00:5e:00:00:01", // zero, broadcast, multicast
		"00:11:22:33:44", "00:11-22:33:44:55", "0011.2233.4455", "00:11:22:33:44:5g", "zz:11:22:33:44:55",
		"00:11:22:33:44:55:66", " 0:11:22:33:44:55",
	}
	got := CleanMacs(in)
	want := []string{"00:11:22:aa:bb:cc", "02:00:00:00:00:01"}
	if !reflect.DeepEqual(got, want) {
		t.Fatalf("got %v want %v", got, want)
	}
	many := []string{}
	for i := 0; i < 40; i++ {
		many = append(many, "02:00:00:00:00:"+string("0123456789abcdef"[i%16])+string("0123456789abcdef"[i/16]))
	}
	if n := len(CleanMacs(many)); n != maxMacs {
		t.Fatalf("cap: got %d", n)
	}
}

func TestCleanLAN(t *testing.T) {
	got := cleanLAN([]store.LanAddr{
		{IP: "192.168.1.5", Prefix: 24}, {IP: "127.0.0.1", Prefix: 8}, {IP: "::1", Prefix: 128},
		{IP: "10.0.0.1", Prefix: 33}, {IP: "nonsense", Prefix: 24}, {IP: "0.0.0.0", Prefix: 0},
	})
	if !reflect.DeepEqual(got, []store.LanAddr{{IP: "192.168.1.5", Prefix: 24}}) {
		t.Fatalf("got %v", got)
	}
}

func dev(id, ip string, lan ...store.LanAddr) *store.Device {
	return &store.Device{ID: id, LastIP: ip, LAN: lan, PublicKey: []byte{1}}
}

func TestPickWakeHelper(t *testing.T) {
	home := store.LanAddr{IP: "192.168.1.20", Prefix: 24}
	target := dev("100000000", "203.0.113.7", home)

	sameIPOnly := dev("200000000", "203.0.113.7", store.LanAddr{IP: "10.9.0.4", Prefix: 24})
	sameNetOnly := dev("300000000", "198.51.100.1", store.LanAddr{IP: "192.168.1.77", Prefix: 24})
	both := dev("400000000", "203.0.113.7", store.LanAddr{IP: "192.168.1.30", Prefix: 24})
	unrelated := dev("500000000", "198.51.100.9", store.LanAddr{IP: "172.16.0.2", Prefix: 16})
	linkLocal := dev("600000000", "198.51.100.9", store.LanAddr{IP: "169.254.3.3", Prefix: 16})
	disabled := dev("700000000", "203.0.113.7", home)
	disabled.Disabled = true
	keyReset := dev("800000000", "203.0.113.7", home)
	keyReset.PublicKey = nil

	cases := []struct {
		name string
		in   []*store.Device
		want string
	}{
		{"none", nil, ""},
		{"self is not a helper", []*store.Device{target}, ""},
		{"unrelated", []*store.Device{unrelated, linkLocal}, ""},
		{"disabled or reset skipped", []*store.Device{disabled, keyReset}, ""},
		{"subnet only", []*store.Device{unrelated, sameNetOnly}, "300000000"},
		{"public ip beats subnet", []*store.Device{sameNetOnly, sameIPOnly}, "200000000"},
		{"both best", []*store.Device{sameIPOnly, sameNetOnly, both}, "400000000"},
	}
	for _, c := range cases {
		got := PickWakeHelper(target, c.in)
		id := ""
		if got != nil {
			id = got.ID
		}
		if id != c.want {
			t.Errorf("%s: got %q want %q", c.name, id, c.want)
		}
	}

	// Link-local subnets prove nothing; empty last_ip never matches.
	t2 := dev("110000000", "", store.LanAddr{IP: "169.254.3.4", Prefix: 16})
	if PickWakeHelper(t2, []*store.Device{linkLocal, dev("120000000", "")}) != nil {
		t.Fatal("link-local / empty ip should not match")
	}
}
