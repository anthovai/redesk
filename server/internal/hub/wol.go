package hub

import (
	"context"
	"net/netip"
	"regexp"
	"sort"
	"strings"

	"redesk/server/internal/store"
)

// Wake-on-LAN: the server never sends packets itself. It asks an online agent
// on the target's LAN ("helper") to broadcast the magic packet.

const maxMacs = 16

var macPattern = regexp.MustCompile(`^[0-9a-f]{2}(:[0-9a-f]{2}){5}$`)

// CleanMacs normalizes agent-reported MACs to "aa:bb:cc:dd:ee:ff", dropping anything
// malformed, all-zero or multicast. At most 16 unique entries are kept.
func CleanMacs(in []string) []string {
	out := []string{}
	for _, m := range in {
		if len(out) >= maxMacs {
			break
		}
		if len(m) != 17 {
			continue
		}
		m = strings.ToLower(m)
		if m[2] == '-' {
			if strings.Count(m, "-") != 5 {
				continue
			}
			m = strings.ReplaceAll(m, "-", ":")
		}
		if !macPattern.MatchString(m) || m == "00:00:00:00:00:00" {
			continue
		}
		// Low bit of the first octet set = multicast/broadcast, not a NIC address.
		if strings.IndexByte("13579bdf", m[1]) >= 0 {
			continue
		}
		dup := false
		for _, o := range out {
			dup = dup || o == m
		}
		if !dup {
			out = append(out, m)
		}
	}
	return out
}

// cleanLAN keeps valid IPv4 interface addresses (max 16).
func cleanLAN(in []store.LanAddr) []store.LanAddr {
	out := []store.LanAddr{}
	for _, l := range in {
		if len(out) >= 16 {
			break
		}
		ip, err := netip.ParseAddr(l.IP)
		if err != nil || !ip.Is4() || ip.IsLoopback() || ip.IsUnspecified() || l.Prefix < 0 || l.Prefix > 32 {
			continue
		}
		out = append(out, store.LanAddr{IP: ip.String(), Prefix: l.Prefix})
	}
	return out
}

// lanPrefix returns the subnet of a LAN address, or false when it says nothing about
// locality (link-local, or wider than /8).
func lanPrefix(l store.LanAddr) (netip.Prefix, bool) {
	ip, err := netip.ParseAddr(l.IP)
	if err != nil || !ip.Is4() || ip.IsLinkLocalUnicast() || ip.IsLoopback() || l.Prefix < 8 || l.Prefix > 32 {
		return netip.Prefix{}, false
	}
	p, err := ip.Prefix(l.Prefix)
	return p, err == nil
}

func sharesSubnet(a, b []store.LanAddr) bool {
	for _, x := range a {
		px, ok := lanPrefix(x)
		if !ok {
			continue
		}
		for _, y := range b {
			if py, ok := lanPrefix(y); ok && px == py {
				return true
			}
		}
	}
	return false
}

// PickWakeHelper chooses the online device most likely on the target's LAN:
// same public address and subnet, then same public address, then same subnet.
// Returns nil when no candidate qualifies.
func PickWakeHelper(target *store.Device, online []*store.Device) *store.Device {
	type scored struct {
		d     *store.Device
		score int
	}
	var best []scored
	for _, c := range online {
		if c == nil || c.ID == target.ID || c.Disabled || c.PublicKey == nil {
			continue
		}
		score := 0
		if target.LastIP != "" && c.LastIP == target.LastIP {
			score += 2
		}
		if sharesSubnet(target.LAN, c.LAN) {
			score++
		}
		if score > 0 {
			best = append(best, scored{c, score})
		}
	}
	if len(best) == 0 {
		return nil
	}
	sort.SliceStable(best, func(i, j int) bool {
		if best[i].score != best[j].score {
			return best[i].score > best[j].score
		}
		return best[i].d.ID < best[j].d.ID
	})
	return best[0].d
}

// OnlineDevices returns the device records of all connected agents.
func (h *Hub) OnlineDevices(ctx context.Context) ([]*store.Device, error) {
	set := h.OnlineSet()
	ids := make([]string, 0, len(set))
	for id := range set {
		ids = append(ids, id)
	}
	if len(ids) == 0 {
		return nil, nil
	}
	return h.store.DevicesByIDs(ctx, ids)
}

// SendWol asks the helper agent to broadcast magic packets for macs.
func (h *Hub) SendWol(helperID string, macs []string, targetID string) error {
	macs = CleanMacs(macs)
	h.mu.Lock()
	a := h.agents[helperID]
	h.mu.Unlock()
	if a == nil {
		return ErrOffline
	}
	if !a.send(map[string]any{"t": "wol", "macs": macs, "target": targetID}) {
		return ErrBusy
	}
	return nil
}
