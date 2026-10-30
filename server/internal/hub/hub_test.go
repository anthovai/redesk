package hub

import "testing"

func TestAgentEventsIncludeSessionControls(t *testing.T) {
	// What agents report when a viewer uses the session controls; each ends up in the audit
	// log as agent.<kind>.
	for _, k := range []string{"view_only", "block_input", "privacy_mode", "lock_screen", "lock_after_session", "restart", "restart_failed"} {
		if !agentEvents[k] {
			t.Errorf("%s is not audited", k)
		}
	}
	for _, k := range []string{"", "shell", "session.start", "Restart"} {
		if agentEvents[k] {
			t.Errorf("%q must not be accepted from agents", k)
		}
	}
}
