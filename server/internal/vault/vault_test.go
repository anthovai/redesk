package vault

import "testing"

func TestSealOpen(t *testing.T) {
	v, err := Load("", t.TempDir())
	if err != nil {
		t.Fatal(err)
	}
	s, err := v.Seal("hunter2")
	if err != nil {
		t.Fatal(err)
	}
	if got, err := v.Open(s); err != nil || got != "hunter2" {
		t.Fatalf("got %q, %v", got, err)
	}
	s[len(s)-1] ^= 1
	if _, err := v.Open(s); err == nil {
		t.Fatal("tampered ciphertext must not open")
	}
	if s, _ := v.Seal(""); s != nil {
		t.Fatal("empty secret should seal to nil")
	}
}
