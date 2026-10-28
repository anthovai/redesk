// Package gateway speaks the Guacamole protocol to guacd so RDP, VNC and SSH targets can be
// used from the browser. Credentials are injected here and never reach the browser.
//
// Wire format: an instruction is LENGTH.VALUE elements separated by ',' and ended by ';',
// where LENGTH counts Unicode code points, e.g. "4.size,4.1024,3.768;".
package gateway

import (
	"bufio"
	"context"
	"errors"
	"fmt"
	"io"
	"net"
	"strconv"
	"strings"
	"time"
	"unicode/utf8"
)

const maxElement = 8 << 20 // guacd image chunks are far smaller; guards against garbage input

// Encode builds one instruction.
func Encode(elems ...string) string {
	var b strings.Builder
	for i, e := range elems {
		if i > 0 {
			b.WriteByte(',')
		}
		b.WriteString(strconv.Itoa(utf8.RuneCountInString(e)))
		b.WriteByte('.')
		b.WriteString(e)
	}
	b.WriteByte(';')
	return b.String()
}

// Reader reads whole instructions, keeping their raw bytes for zero-copy forwarding.
type Reader struct {
	r   *bufio.Reader
	raw []byte
}

func NewReader(r io.Reader) *Reader { return &Reader{r: bufio.NewReaderSize(r, 64<<10)} }

// Buffered reports whether more input is already waiting (used to batch WebSocket frames).
func (r *Reader) Buffered() int { return r.r.Buffered() }

// Read returns the next instruction's elements and its raw encoding (valid until the next call).
func (r *Reader) Read() (elems []string, raw []byte, err error) {
	r.raw = r.raw[:0]
	for {
		n := 0
		digits := 0
		for {
			c, err := r.r.ReadByte()
			if err != nil {
				return nil, nil, err
			}
			r.raw = append(r.raw, c)
			if c == '.' {
				break
			}
			if c < '0' || c > '9' || digits > 9 {
				return nil, nil, errors.New("guacamole: malformed length")
			}
			n = n*10 + int(c-'0')
			digits++
		}
		if digits == 0 || n > maxElement {
			return nil, nil, errors.New("guacamole: bad element length")
		}
		start := len(r.raw)
		for i := 0; i < n; i++ {
			c, _, err := r.r.ReadRune()
			if err != nil {
				return nil, nil, err
			}
			r.raw = utf8.AppendRune(r.raw, c)
		}
		elems = append(elems, string(r.raw[start:]))
		t, err := r.r.ReadByte()
		if err != nil {
			return nil, nil, err
		}
		r.raw = append(r.raw, t)
		switch t {
		case ';':
			return elems, r.raw, nil
		case ',':
		default:
			return nil, nil, errors.New("guacamole: bad terminator")
		}
	}
}

// Conn is an established guacd connection, past the handshake.
type Conn struct {
	net.Conn
	Reader *Reader
	ID     string
}

type Display struct{ Width, Height, DPI int }

// Connect dials guacd and performs the handshake for protocol ("rdp", "vnc", "ssh").
// Parameters guacd asks for but which are absent from params are sent empty.
func Connect(ctx context.Context, addr, protocol string, params map[string]string, d Display, timezone string) (*Conn, error) {
	var dialer net.Dialer
	nc, err := dialer.DialContext(ctx, "tcp", addr)
	if err != nil {
		return nil, fmt.Errorf("guacd unreachable: %w", err)
	}
	_ = nc.SetDeadline(time.Now().Add(20 * time.Second))
	rd := NewReader(nc)
	fail := func(err error) (*Conn, error) {
		nc.Close()
		return nil, err
	}
	send := func(elems ...string) error {
		_, err := io.WriteString(nc, Encode(elems...))
		return err
	}

	if err := send("select", protocol); err != nil {
		return fail(err)
	}
	args, _, err := rd.Read()
	if err != nil {
		return fail(fmt.Errorf("guacd handshake: %w", err))
	}
	if len(args) == 0 || args[0] != "args" {
		return fail(fmt.Errorf("guacd: expected args, got %q", args))
	}

	steps := [][]string{
		{"size", strconv.Itoa(d.Width), strconv.Itoa(d.Height), strconv.Itoa(d.DPI)},
		{"audio"},
		{"video"},
		{"image", "image/png", "image/jpeg", "image/webp"},
	}
	if timezone != "" {
		steps = append(steps, []string{"timezone", timezone})
	}
	for _, s := range steps {
		if err := send(s...); err != nil {
			return fail(err)
		}
	}
	values := []string{"connect"}
	for _, name := range args[1:] {
		if strings.HasPrefix(name, "VERSION_") {
			values = append(values, "VERSION_1_5_0")
			continue
		}
		values = append(values, params[name])
	}
	if err := send(values...); err != nil {
		return fail(err)
	}
	ready, _, err := rd.Read()
	if err != nil {
		return fail(fmt.Errorf("guacd handshake: %w", err))
	}
	if ready[0] == "error" && len(ready) >= 3 {
		return fail(fmt.Errorf("guacd: %s (status %s)", ready[1], ready[2]))
	}
	if ready[0] != "ready" || len(ready) < 2 {
		return fail(fmt.Errorf("guacd: expected ready, got %q", ready))
	}
	_ = nc.SetDeadline(time.Time{})
	return &Conn{Conn: nc, Reader: rd, ID: ready[1]}, nil
}
