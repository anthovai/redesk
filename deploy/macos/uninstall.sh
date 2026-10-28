#!/bin/bash
# Removes the ReDesk server from this Mac.
#   sudo ./uninstall.sh            stop services, remove program files (keeps data, config, database, backups)
#   sudo ./uninstall.sh --purge    also delete the database, keys, backups, logs, guacd and the _redesk account
set -euo pipefail
PURGE=0
[ "${1:-}" = "--purge" ] && PURGE=1
[ "$(id -u)" = 0 ] || { echo "run with sudo"; exit 1; }
LABEL=co.redesk
BREW_USER="${SUDO_USER:-$(stat -f %Su /dev/console)}"
for b in /opt/homebrew/bin/brew /usr/local/bin/brew; do [ -x "$b" ] && BREW_PREFIX="$("$b" --prefix)" && break; done
USER_HOME="$(dscl . -read "/Users/$BREW_USER" NFSHomeDirectory | awk '{print $2}')"

if [ "$PURGE" = 1 ]; then
	read -r -p "Delete ALL ReDesk data including the database and keys? Type 'delete' to confirm: " ok
	[ "$ok" = delete ] || { echo "cancelled"; exit 1; }
fi

for svc in server caddy backup; do
	launchctl bootout "system/$LABEL.$svc" 2>/dev/null || true
	rm -f "/Library/LaunchDaemons/$LABEL.$svc.plist"
done
rm -rf /usr/local/redesk /etc/newsyslog.d/redesk.conf
echo "Removed ReDesk server and Caddy services."

if [ "$PURGE" = 1 ]; then
	if [ -n "${BREW_PREFIX:-}" ] && [ -S "$USER_HOME/.colima/default/docker.sock" ]; then
		sudo -u "$BREW_USER" -H env DOCKER_HOST="unix://$USER_HOME/.colima/default/docker.sock" \
			"$BREW_PREFIX/bin/docker" rm -f redesk-guacd >/dev/null 2>&1 || true
		sudo -u "$BREW_USER" -H env DOCKER_HOST="unix://$USER_HOME/.colima/default/docker.sock" \
			"$BREW_PREFIX/bin/docker" volume rm redesk-recordings >/dev/null 2>&1 || true
	fi
	if [ -n "${BREW_PREFIX:-}" ] && [ -x "$BREW_PREFIX/opt/postgresql@16/bin/psql" ]; then
		sudo -u "$BREW_USER" -H "$BREW_PREFIX/opt/postgresql@16/bin/psql" -h 127.0.0.1 -d postgres -qc "DROP DATABASE IF EXISTS redesk" || true
		sudo -u "$BREW_USER" -H "$BREW_PREFIX/opt/postgresql@16/bin/psql" -h 127.0.0.1 -d postgres -qc "DROP ROLE IF EXISTS redesk" || true
	fi
	for svc in colima postgresql; do
		launchctl bootout "system/$LABEL.$svc" 2>/dev/null || true
		rm -f "/Library/LaunchDaemons/$LABEL.$svc.plist"
	done
	rm -rf /usr/local/etc/redesk /usr/local/var/redesk /usr/local/var/log/redesk
	dscl . -delete /Users/_redesk 2>/dev/null || true
	dscl . -delete /Groups/_redesk 2>/dev/null || true
	echo "Purged data, database, guacd and the _redesk account."
	echo "Homebrew packages (postgresql@16, caddy, colima, docker) were left installed."
else
	echo "Kept: /usr/local/etc/redesk, /usr/local/var/redesk (keys, backups), the database, PostgreSQL and Colima daemons."
fi
