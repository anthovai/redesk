#!/bin/bash
# Runs the agent's tests on a headless X server with a sound server, as CI does:
#
#   deploy/linux/test-x11.sh [extra cargo test args]
#
# Xvfb (1280x720) for capture, input and clipboard (agent/tests/linux_x11.rs), PulseAudio with
# a null sink for audio capture of the default monitor and playback through ALSA's pulse plugin
# (the ignored audio device tests in agent/src/audio.rs). Needs the packages in the Dockerfile.
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
# Our own server on an unusual display number; -noreset keeps the root window (painted by the
# tests with xsetroot) when the last client disconnects.
export DISPLAY="${TEST_DISPLAY:-:87}"
Xvfb "$DISPLAY" -screen 0 1280x720x24 -nolisten tcp -noreset >/tmp/aegiron-xvfb.log 2>&1 &
XVFB=$!
trap 'kill $XVFB 2>/dev/null || true; pulseaudio --kill 2>/dev/null || true' EXIT
for _ in $(seq 50); do xdpyinfo >/dev/null 2>&1 && break; sleep 0.1; done
kill -0 $XVFB 2>/dev/null || { cat /tmp/aegiron-xvfb.log; echo "Xvfb did not start" >&2; exit 1; }
xdpyinfo | grep -E 'dimensions|depths'

# PulseAudio for the current user (as root in a container it only warns).
export XDG_RUNTIME_DIR="${XDG_RUNTIME_DIR:-$(mktemp -d)}"
chmod 700 "$XDG_RUNTIME_DIR"
pulseaudio --start --exit-idle-time=-1 -n --load=module-native-protocol-unix \
	--load="module-null-sink sink_name=aegiron_test" --load=module-always-sink
pactl set-default-sink aegiron_test || true
pactl info | grep -E 'Server Name|Default Sink'
# ALSA's default device through PulseAudio (cpal plays through ALSA).
if [[ ! -e "$HOME/.asoundrc" && ! -e /etc/asound.conf ]]; then
	printf 'pcm.!default pulse\nctl.!default pulse\n' > "$HOME/.asoundrc"
fi

cd "$ROOT/agent"
AEGIRON_REQUIRE_X11=1 AEGIRON_REQUIRE_AUDIO=1 cargo test --locked "$@" -- --include-ignored --test-threads=1 --nocapture
