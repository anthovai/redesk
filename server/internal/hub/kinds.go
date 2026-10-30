package hub

// Session types besides screen control (docs/PROTOCOL.md §11): TCP tunnels and remote
// terminals. The server only relays them like any session; it checks that the agent supports
// the type, tells the agent which type and tunnel target the viewer asked for, and writes the
// type and target into every audit record of the session.

import (
	"errors"
	"net"
	"regexp"
	"strconv"
	"strings"
)

const (
	KindScreen   = "screen"
	KindTunnel   = "tunnel"
	KindTerminal = "terminal"

	// Advertised in register.features by agents that run these session types.
	FeatureTunnel   = "tunnel1"
	FeatureTerminal = "terminal1"
)

var (
	// ErrUnsupported: the agent did not advertise the requested session type (older agent).
	ErrUnsupported = errors.New("the device's Aegiron version does not support this session type")
	ErrBadKind     = errors.New("unknown session type")
	ErrBadTarget   = errors.New("invalid tunnel target (use host:port)")

	hostPattern = regexp.MustCompile(`^[A-Za-z0-9._-]{1,253}$`)
)

// SessionOptions selects the session type. The zero value is a screen session.
type SessionOptions struct {
	Kind   string // "", "screen", "tunnel" or "terminal"
	Target string // tunnels: host:port as seen from the device
}

// Normalize validates the options and returns them in canonical form.
func (o SessionOptions) Normalize() (SessionOptions, error) {
	switch o.Kind {
	case "", KindScreen:
		return SessionOptions{Kind: KindScreen}, nil
	case KindTerminal:
		return SessionOptions{Kind: KindTerminal}, nil
	case KindTunnel:
		t, err := NormalizeTarget(o.Target)
		if err != nil {
			return o, err
		}
		return SessionOptions{Kind: KindTunnel, Target: t}, nil
	}
	return o, ErrBadKind
}

// NormalizeTarget checks a tunnel target "host:port" ("[v6]:port") and lower-cases the host.
// Whether the device lets a tunnel reach it is the device's decision (allow_tunnel_lan).
func NormalizeTarget(s string) (string, error) {
	host, port, err := net.SplitHostPort(strings.TrimSpace(s))
	if err != nil {
		return "", ErrBadTarget
	}
	p, err := strconv.Atoi(port)
	if err != nil || p < 1 || p > 65535 {
		return "", ErrBadTarget
	}
	if ip := net.ParseIP(host); ip == nil && !hostPattern.MatchString(host) {
		return "", ErrBadTarget
	}
	return net.JoinHostPort(strings.ToLower(host), strconv.Itoa(p)), nil
}

// supports reports whether an agent advertised the session type.
func (a *agentConn) supports(kind string) bool {
	switch kind {
	case KindTunnel:
		return a.features[FeatureTunnel]
	case KindTerminal:
		return a.features[FeatureTerminal]
	}
	return true
}

// tagged adds the session type (and tunnel target) to an audit detail.
func (rl *relay) tagged(d map[string]any) map[string]any {
	if rl.kind != "" && rl.kind != KindScreen {
		d["type"] = rl.kind
		if rl.target != "" {
			d["target"] = rl.target
		}
	}
	return d
}
