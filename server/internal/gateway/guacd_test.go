package gateway

import (
	"context"
	"net"
	"strings"
	"testing"
)

func TestEncodeCountsRunes(t *testing.T) {
	if got := Encode("size", "สวัสดี", ""); got != "4.size,6.สวัสดี,0.;" {
		t.Fatal(got)
	}
}

func TestReaderRoundTrip(t *testing.T) {
	in := Encode("sync", "123") + Encode("", "ping", "1") + Encode("clipboard", "ไทย;,.")
	r := NewReader(strings.NewReader(in))
	var got []string
	for range 3 {
		elems, raw, err := r.Read()
		if err != nil {
			t.Fatal(err)
		}
		if Encode(elems...) != string(raw) {
			t.Fatalf("raw %q != re-encoded %q", raw, Encode(elems...))
		}
		got = append(got, strings.Join(elems, "|"))
	}
	if strings.Join(got, " ") != "sync|123 |ping|1 clipboard|ไทย;,." {
		t.Fatal(got)
	}
	if _, _, err := NewReader(strings.NewReader("4.size;x")).Read(); err != nil {
		t.Fatal(err)
	}
	if _, _, err := NewReader(strings.NewReader("x.size;")).Read(); err == nil {
		t.Fatal("expected malformed length error")
	}
}

// fakeGuacd answers a handshake the way guacd 1.5 does.
func TestConnectHandshake(t *testing.T) {
	ln, err := net.Listen("tcp", "127.0.0.1:0")
	if err != nil {
		t.Fatal(err)
	}
	defer ln.Close()
	gotConnect := make(chan []string, 1)
	go func() {
		c, err := ln.Accept()
		if err != nil {
			return
		}
		defer c.Close()
		r := NewReader(c)
		r.Read() // select
		c.Write([]byte(Encode("args", "VERSION_1_5_0", "hostname", "port", "password")))
		for {
			e, _, err := r.Read()
			if err != nil {
				return
			}
			if e[0] == "connect" {
				gotConnect <- e
				c.Write([]byte(Encode("ready", "$abc")))
				return
			}
		}
	}()
	conn, err := Connect(context.Background(), ln.Addr().String(), "vnc",
		map[string]string{"hostname": "10.0.0.5", "port": "5900", "password": "s3cret"}, Display{1024, 768, 96}, "Asia/Bangkok")
	if err != nil {
		t.Fatal(err)
	}
	conn.Close()
	if conn.ID != "$abc" {
		t.Fatal(conn.ID)
	}
	if got := strings.Join(<-gotConnect, "|"); got != "connect|VERSION_1_5_0|10.0.0.5|5900|s3cret" {
		t.Fatal(got)
	}
}
