package api

import (
	"encoding/json"
	"strings"
	"testing"
)

func TestNormalizeEndpointRejectsInjection(t *testing.T) {
	bad := []endpointReq{
		{Name: "x", Provider: "teamviewer", Address: "123456789 --play http://evil"}, // CVE-2020-13699 style
		{Name: "x", Provider: "teamviewer", Address: "123456789&authorization=x"},
		{Name: "x", Provider: "anydesk", Address: "1234567 -x"},
		{Name: "x", Provider: "rustdesk", Address: "../../etc"},
		{Name: "x", Provider: "rdp", Address: "host\r\nalternate shell:s:cmd.exe"},
		{Name: "x", Provider: "rdp", Address: "host", Username: "a\r\nalternate shell:s:cmd.exe"},
		{Name: "x", Provider: "rdp", Address: "host", Options: json.RawMessage(`{"domain":"d\nx"}`)},
		{Name: "x", Provider: "rdp", Address: "host", Options: json.RawMessage(`{"unknown":1}`)},
		{Name: "x", Provider: "ssh", Address: "-oProxyCommand=evil"},
		{Name: "x", Provider: "vnc", Address: "host", Port: new(int)},
		{Name: "x", Provider: "telnet", Address: "host"},
		{Name: "", Provider: "ssh", Address: "host"},
	}
	for _, r := range bad {
		if _, err := normalizeEndpoint(&r); err == nil {
			t.Errorf("accepted %+v", r)
		}
	}
	good := []endpointReq{
		{Name: "a", Provider: "teamviewer", Address: "123 456 789"},
		{Name: "a", Provider: "anydesk", Address: "office-pc@ad"},
		{Name: "a", Provider: "rdp", Address: "srv01.corp.local", Options: json.RawMessage(`{"security":"nla","domain":"CORP"}`)},
		{Name: "a", Provider: "ssh", Address: "[2001:db8::1]"},
	}
	for _, r := range good {
		if _, err := normalizeEndpoint(&r); err != nil {
			t.Errorf("rejected %+v: %v", r, err)
		}
	}
	e, _ := normalizeEndpoint(&good[2])
	if *e.Port != 3389 {
		t.Fatal("default RDP port")
	}
	e.Username = "somchai"
	f := rdpFile(e)
	if !strings.Contains(f, "full address:s:srv01.corp.local:3389\r\n") || !strings.Contains(f, "domain:s:CORP") || strings.Count(f, "\r\n") != 5 {
		t.Fatalf("rdp file: %q", f)
	}
}
