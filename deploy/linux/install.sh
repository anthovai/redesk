#!/bin/sh
# Installs Aegiron from the .tar.gz for the current user (no root):
#
#   ./install.sh                 into ~/.local (binaries in ~/.local/bin)
#   ./install.sh --prefix DIR    elsewhere (e.g. sudo ./install.sh --prefix /usr/local)
#   ./install.sh --uninstall     removes it again (settings in ~/.config/Aegiron stay)
#
# Registers the app menu entry, the icon and the aegiron:// link handler. "Start at login"
# is turned on from the app's settings (or `aegiron-agent install` for the CLI agent).
set -eu

HERE="$(cd "$(dirname "$0")" && pwd)"
PREFIX="$HOME/.local"
MODE=install
while [ $# -gt 0 ]; do
	case "$1" in
		--prefix) PREFIX="$2"; shift ;;
		--uninstall) MODE=uninstall ;;
		*) echo "usage: $0 [--prefix DIR] [--uninstall]" >&2; exit 2 ;;
	esac
	shift
done
BIN="$PREFIX/bin"
APPS="$PREFIX/share/applications"
ICONS="$PREFIX/share/icons/hicolor/256x256/apps"

refresh() {
	command -v update-desktop-database >/dev/null 2>&1 && update-desktop-database -q "$APPS" 2>/dev/null || true
	command -v gtk-update-icon-cache >/dev/null 2>&1 && gtk-update-icon-cache -q -t "$PREFIX/share/icons/hicolor" 2>/dev/null || true
}

if [ "$MODE" = uninstall ]; then
	"$BIN/aegiron-agent" uninstall >/dev/null 2>&1 || true
	rm -f "$BIN/aegiron" "$BIN/aegiron-agent" "$APPS/aegiron.desktop" "$ICONS/aegiron.png"
	rm -f "${XDG_CONFIG_HOME:-$HOME/.config}/autostart/aegiron.desktop"
	refresh
	echo "Aegiron removed from $PREFIX"
	exit 0
fi

mkdir -p "$BIN" "$APPS" "$ICONS"
install -m755 "$HERE/aegiron" "$HERE/aegiron-agent" "$BIN/"
install -m644 "$HERE/aegiron.png" "$ICONS/aegiron.png"
# Absolute Exec: ~/.local/bin is not on every PATH.
sed "s|^Exec=aegiron |Exec=$BIN/aegiron |" "$HERE/aegiron.desktop" > "$APPS/aegiron.desktop"
chmod 644 "$APPS/aegiron.desktop"
refresh
if command -v xdg-mime >/dev/null 2>&1; then
	xdg-mime default aegiron.desktop x-scheme-handler/aegiron 2>/dev/null || true
	xdg-mime default aegiron.desktop x-scheme-handler/redesk 2>/dev/null || true
fi
echo "Aegiron installed in $PREFIX. Start it from the app menu or with: $BIN/aegiron"
