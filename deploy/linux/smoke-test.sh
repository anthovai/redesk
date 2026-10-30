#!/bin/sh
# Installs a built package on a clean system and checks it (CI runs it in fresh containers):
#
#   deploy/linux/smoke-test.sh deb|rpm|tar <dist dir>
#
# Dependencies resolve, every shared library loads, the CLI runs, the .desktop entry is valid
# and registered for aegiron:// links. Run it only on a throwaway system or container.
set -eu

KIND="$1"
DIST="$(cd "$2" && pwd)"
ARCH="$(uname -m)"
fail() { echo "FAIL: $*" >&2; exit 1; }
pass() { echo "PASS: $*"; }

case "$KIND" in
	deb)
		export DEBIAN_FRONTEND=noninteractive
		apt-get update -qq
		DEB="$(ls "$DIST"/aegiron_*_"$(dpkg --print-architecture)".deb)"
		apt-get install -y -qq "$DEB" desktop-file-utils xvfb xauth libgl1-mesa-dri >/dev/null
		dpkg -s aegiron | grep -E '^(Version|Depends)'
		BIN=/usr/bin APPS=/usr/share/applications
		;;
	rpm)
		RPM="$(ls "$DIST"/aegiron-*."$ARCH".rpm)"
		dnf install -y -q "$RPM" desktop-file-utils xorg-x11-server-Xvfb xauth mesa-dri-drivers
		rpm -q --requires aegiron | head -20
		BIN=/usr/bin APPS=/usr/share/applications
		;;
	tar)
		export DEBIAN_FRONTEND=noninteractive
		apt-get update -qq
		# What the binaries link against, plus the tools install.sh uses.
		apt-get install -y -qq --no-install-recommends libpipewire-0.3-0t64 libasound2t64 libxcb1 libxcb-randr0 libxcb-shm0 libxcb-xfixes0 \
			libegl1 libgbm1 libdbus-1-3 libgl1 libxkbcommon0 libxkbcommon-x11-0 libx11-6 libx11-xcb1 libxcursor1 libxi6 libxrandr2 \
			libwayland-client0 libwayland-cursor0 libwayland-egl1 xdg-utils desktop-file-utils xvfb xauth libgl1-mesa-dri >/dev/null
		TMP="$(mktemp -d)"
		tar -C "$TMP" -xzf "$(ls "$DIST"/aegiron-linux-"$ARCH"-*.tar.gz)"
		"$TMP"/aegiron-linux-*/install.sh
		BIN="$HOME/.local/bin" APPS="$HOME/.local/share/applications"
		grep -q "^Exec=$BIN/aegiron %u" "$APPS/aegiron.desktop" || fail "Exec not absolute"
		;;
	*) fail "usage: $0 deb|rpm|tar <dist>" ;;
esac

for b in aegiron aegiron-agent; do
	[ -x "$BIN/$b" ] || fail "$BIN/$b missing"
	if ldd "$BIN/$b" | grep 'not found'; then fail "$b: missing shared libraries"; fi
done
pass "binaries and libraries"

"$BIN/aegiron-agent" --help >/dev/null || fail "aegiron-agent --help"
"$BIN/aegiron-agent" --version 2>/dev/null || true
# Without a config the CLI explains instead of crashing.
"$BIN/aegiron-agent" --config /tmp/none/agent.toml info >/dev/null 2>&1 && fail "info without config succeeded"
pass "CLI runs"

desktop-file-validate "$APPS/aegiron.desktop" || fail "invalid aegiron.desktop"
grep -q 'x-scheme-handler/aegiron' "$APPS/mimeinfo.cache" 2>/dev/null || {
	update-desktop-database "$APPS"
	grep -q 'x-scheme-handler/aegiron' "$APPS/mimeinfo.cache" || fail "aegiron:// not in mimeinfo.cache"
}
pass "aegiron.desktop valid and registered for aegiron://"

# The desktop app opens its window on an X server and stays up; a second launch with a link
# hands it over through the instance socket and exits.
export XDG_RUNTIME_DIR="$(mktemp -d)"
CFG="$(mktemp -d)/agent.toml"
Xvfb :97 -screen 0 1280x720x24 -noreset >/dev/null 2>&1 &
XVFB=$!
sleep 2
DISPLAY=:97 "$BIN/aegiron" --config "$CFG" &
APP=$!
sleep 8
kill -0 "$APP" 2>/dev/null || { cat "$(dirname "$CFG")/aegiron.log"; fail "aegiron exited at startup"; }
DISPLAY=:97 "$BIN/aegiron" --config "$CFG" aegiron://connect/123456789 || fail "second launch failed"
sleep 1
grep -q 'request from another launch: Open("123456789")' "$(dirname "$CFG")/aegiron.log" || { cat "$(dirname "$CFG")/aegiron.log"; fail "link not handed over"; }
kill "$APP" "$XVFB" 2>/dev/null || true
pass "desktop app starts, second launch hands over aegiron://connect/123456789"

if [ "$KIND" = tar ]; then
	"$TMP"/aegiron-linux-*/install.sh --uninstall
	[ ! -e "$BIN/aegiron" ] || fail "uninstall left $BIN/aegiron"
	pass "uninstall"
fi
