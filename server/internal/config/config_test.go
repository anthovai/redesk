package config

import "testing"

func TestLegacyEnvNames(t *testing.T) {
	t.Setenv("REDESK_LISTEN", ":1111")
	t.Setenv("REDESK_TRUST_PROXY", "true")
	t.Setenv("AEGIRON_TIMEZONE", "UTC")
	t.Setenv("REDESK_TIMEZONE", "Asia/Tokyo")
	c := Load()
	if c.Listen != ":1111" || !c.TrustProxy {
		t.Fatalf("REDESK_* fallback not applied: listen=%q trust=%v", c.Listen, c.TrustProxy)
	}
	if c.Timezone != "UTC" {
		t.Fatalf("AEGIRON_* must win over REDESK_*, got %q", c.Timezone)
	}
}
