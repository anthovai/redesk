#!/bin/bash
# ReDesk server installer for macOS (Apple Silicon or Intel).
#
#   sudo ./install.sh --domain redesk.example.com --email it@example.com
#
# Installs (or upgrades, when run again from a newer kit):
#   redesk-server     native binary, launchd daemon, runs as hidden user _redesk on 127.0.0.1:21114
#   Caddy             HTTPS reverse proxy on :80/:443 with automatic Let's Encrypt certificates
#   PostgreSQL 16     Homebrew, launchd daemon (starts at boot without anyone logged in)
#   guacd             RDP/VNC/SSH gateway engine in Colima (Docker) — skip with --no-gateway
#   daily backups     02:30, /usr/local/var/redesk/backups, kept 14 days
#
# Options:
#   --domain NAME        public DNS name pointing at this Mac (required)
#   --email ADDR         Let's Encrypt contact and first admin login (required on first install)
#   --no-gateway         do not install Colima/guacd (no in-browser RDP/VNC/SSH)
#   --database-url URL   use an existing PostgreSQL instead of installing one
#   --server-power       set power settings for a 24/7 server (no sleep, restart after power loss)
#   --dry-run            print what would be done without changing anything
set -euo pipefail

PREFIX=/usr/local/redesk
ETC=/usr/local/etc/redesk
VAR=/usr/local/var/redesk
LOG=/usr/local/var/log/redesk
SVC_USER=_redesk
LABEL=co.redesk
KIT="$(cd "$(dirname "$0")" && pwd)"

DOMAIN="" EMAIL="" GATEWAY=1 DB_URL="" POWER=0 DRY=0
while [ $# -gt 0 ]; do
	case "$1" in
		--domain) DOMAIN="${2:-}"; shift 2 ;;
		--email) EMAIL="${2:-}"; shift 2 ;;
		--no-gateway) GATEWAY=0; shift ;;
		--database-url) DB_URL="${2:-}"; shift 2 ;;
		--server-power) POWER=1; shift ;;
		--dry-run) DRY=1; shift ;;
		-h|--help) sed -n '2,22p' "$0"; exit 0 ;;
		*) echo "unknown option: $1" >&2; exit 2 ;;
	esac
done

# --- helpers ---------------------------------------------------------------

bold() { printf '\033[1m%s\033[0m\n' "$*"; }
step() { printf '\n\033[1;36m==> %s\033[0m\n' "$*"; }
warn() { printf '\033[33m!! %s\033[0m\n' "$*" >&2; }
die() { printf '\033[31mxx %s\033[0m\n' "$*" >&2; exit 1; }

run() {
	if [ "$DRY" = 1 ]; then printf '   + %s\n' "$*"; else "$@"; fi
}

# Runs a command as the Homebrew owner (Homebrew refuses to run as root).
# `brew_user` is for read-only checks (also used in --dry-run); `as_user` changes things.
brew_user() {
	if [ "$(id -un)" = "$BREW_USER" ]; then "$@"; else sudo -u "$BREW_USER" -H "$@"; fi
}
as_user() {
	run sudo -u "$BREW_USER" -H env PATH="$BREW_PREFIX/bin:$BREW_PREFIX/sbin:/usr/bin:/bin:/usr/sbin:/sbin" "$@"
}

# write_file PATH MODE OWNER:GROUP  (content on stdin)
write_file() {
	local path="$1" mode="$2" owner="$3" tmp
	if [ "$DRY" = 1 ]; then
		printf '   + write %s (%s %s)\n' "$path" "$mode" "$owner"
		sed "s/^/       | /"
		return
	fi
	tmp="$(mktemp "$path.XXXXXX")"
	cat > "$tmp"
	chmod "$mode" "$tmp"
	chown "$owner" "$tmp"
	mv -f "$tmp" "$path"
}

# load_daemon LABEL: (re)starts /Library/LaunchDaemons/LABEL.plist
load_daemon() {
	run launchctl bootout "system/$1" 2>/dev/null || true
	run launchctl bootstrap system "/Library/LaunchDaemons/$1.plist"
	run launchctl enable "system/$1"
}

# daemon_plist LABEL USER LOGFILE ARG... -> plist on stdout (KeepAlive, RunAtLoad)
daemon_plist() {
	local label="$1" user="$2" log="$3"; shift 3
	local args="" a
	for a in "$@"; do
		a="${a//&/&amp;}"; a="${a//</&lt;}"; a="${a//>/&gt;}"
		args+="    <string>$a</string>"$'\n'
	done
	cat <<EOF
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>Label</key><string>$label</string>
  <key>UserName</key><string>$user</string>
  <key>ProgramArguments</key>
  <array>
$args  </array>
  <key>EnvironmentVariables</key>
  <dict>
    <key>PATH</key><string>$BREW_PREFIX/bin:$BREW_PREFIX/sbin:/usr/bin:/bin:/usr/sbin:/sbin</string>
    <key>HOME</key><string>${DAEMON_HOME:-$VAR}</string>
    <key>XDG_DATA_HOME</key><string>$VAR/caddy</string>
    <key>XDG_CONFIG_HOME</key><string>$VAR/caddy</string>
  </dict>
  <key>RunAtLoad</key><true/>
  <key>KeepAlive</key><true/>
  <key>ThrottleInterval</key><integer>10</integer>
  <key>SoftResourceLimits</key><dict><key>NumberOfFiles</key><integer>65536</integer></dict>
  <key>StandardOutPath</key><string>$log</string>
  <key>StandardErrorPath</key><string>$log</string>
</dict>
</plist>
EOF
}

wait_for() { # wait_for SECONDS DESCRIPTION COMMAND...
	local secs="$1" what="$2"; shift 2
	[ "$DRY" = 1 ] && { printf '   + wait for %s\n' "$what"; return 0; }
	local i
	for ((i = 0; i < secs; i++)); do
		if "$@" >/dev/null 2>&1; then return 0; fi
		sleep 1
	done
	return 1
}

random_secret() { openssl rand -hex "${1:-24}"; }

# --- preflight -------------------------------------------------------------

step "Checking this Mac"
[ "$(uname -s)" = Darwin ] || die "this installer is for macOS"
if [ "$DRY" = 0 ] && [ "$(id -u)" != 0 ]; then die "run with sudo"; fi
BREW_USER="${SUDO_USER:-$(stat -f %Su /dev/console)}"
[ -n "$BREW_USER" ] && [ "$BREW_USER" != root ] || die "run with sudo from the admin account that owns Homebrew"
for b in /opt/homebrew/bin/brew /usr/local/bin/brew; do [ -x "$b" ] && BREW="$b" && break; done
[ -n "${BREW:-}" ] || die "Homebrew not found. Install it first (as $BREW_USER): https://brew.sh"
BREW_PREFIX="$("$BREW" --prefix)"
USER_HOME="$(dscl . -read "/Users/$BREW_USER" NFSHomeDirectory | awk '{print $2}')"
[ -f "$KIT/bin/redesk-server" ] && [ -d "$KIT/web" ] || die "run install.sh from inside the extracted kit"
VERSION="$(cat "$KIT/VERSION" 2>/dev/null || echo unknown)"

UPGRADE=0
[ -f "$ETC/redesk.env" ] && UPGRADE=1
if [ -z "$DOMAIN" ] && [ -f "$ETC/domain" ]; then DOMAIN="$(cat "$ETC/domain")"; fi
[ -n "$DOMAIN" ] || die "--domain is required"
echo "$DOMAIN" | grep -Eq '^[A-Za-z0-9]([A-Za-z0-9-]*[A-Za-z0-9])?(\.[A-Za-z0-9]([A-Za-z0-9-]*[A-Za-z0-9])?)+$' || die "invalid domain: $DOMAIN"
if [ "$UPGRADE" = 0 ] && [ -z "$EMAIL" ]; then die "--email is required on first install"; fi
[ -z "$EMAIL" ] || echo "$EMAIL" | grep -Eq '^[^@ ]+@[^@ ]+\.[^@ ]+$' || die "invalid email: $EMAIL"

echo "   macOS $(sw_vers -productVersion) ($(uname -m)), Homebrew at $BREW_PREFIX owned by $BREW_USER"
echo "   ReDesk $VERSION, domain $DOMAIN, $([ "$UPGRADE" = 1 ] && echo upgrade || echo new install)$([ "$DRY" = 1 ] && echo ', DRY RUN')"

if [ "$DRY" = 0 ]; then
	xattr -dr com.apple.quarantine "$KIT" 2>/dev/null || true
fi

# --- service account ------------------------------------------------------

step "Service account $SVC_USER"
if id "$SVC_USER" >/dev/null 2>&1; then
	echo "   exists"
else
	# Hidden role account (like the system's _www, _postgres): no shell, no password, no login.
	SVC_ID=""
	for id in $(seq 420 499); do
		if ! dscl . -list /Users UniqueID | awk '{print $2}' | grep -qx "$id" &&
			! dscl . -list /Groups PrimaryGroupID | awk '{print $2}' | grep -qx "$id"; then SVC_ID=$id; break; fi
	done
	[ -n "$SVC_ID" ] || die "no free uid/gid in 420-499"
	run dscl . -create "/Groups/$SVC_USER"
	run dscl . -create "/Groups/$SVC_USER" PrimaryGroupID "$SVC_ID"
	run dscl . -create "/Groups/$SVC_USER" RealName "ReDesk service"
	run dscl . -create "/Users/$SVC_USER"
	run dscl . -create "/Users/$SVC_USER" UniqueID "$SVC_ID"
	run dscl . -create "/Users/$SVC_USER" PrimaryGroupID "$SVC_ID"
	run dscl . -create "/Users/$SVC_USER" UserShell /usr/bin/false
	run dscl . -create "/Users/$SVC_USER" NFSHomeDirectory "$VAR"
	run dscl . -create "/Users/$SVC_USER" RealName "ReDesk service"
	run dscl . -create "/Users/$SVC_USER" Password '*'
	run dscl . -create "/Users/$SVC_USER" IsHidden 1
	echo "   created uid/gid $SVC_ID"
fi

run mkdir -p "$PREFIX/bin" "$ETC" "$VAR/data" "$VAR/caddy" "$VAR/backups" "$LOG"
run chown -R "$SVC_USER:$SVC_USER" "$VAR/data" "$VAR/caddy" "$LOG"
run chmod 700 "$VAR/data" "$VAR/backups"
run chmod 750 "$ETC"
run chown "root:$SVC_USER" "$ETC"

# --- packages --------------------------------------------------------------

step "Homebrew packages"
PKGS=(caddy)
[ -z "$DB_URL" ] && PKGS+=(postgresql@16)
[ "$GATEWAY" = 1 ] && PKGS+=(colima docker)
for p in "${PKGS[@]}"; do
	if brew_user "$BREW" list --versions "$p" >/dev/null 2>&1; then
		echo "   $p already installed"
	else
		as_user "$BREW" install --quiet "$p"
	fi
done
PG_BIN="$BREW_PREFIX/opt/postgresql@16/bin"

# --- PostgreSQL ------------------------------------------------------------

if [ -z "$DB_URL" ]; then
	step "PostgreSQL 16"
	PGDATA="$BREW_PREFIX/var/postgresql@16"
	if [ ! -f "$PGDATA/PG_VERSION" ]; then
		as_user "$PG_BIN/initdb" --locale=C -E UTF-8 -U "$BREW_USER" "$PGDATA"
	fi
	# A per-user "brew services" agent only runs while someone is logged in; replace it with a boot daemon.
	if brew_user "$BREW" services list 2>/dev/null | grep -Eq '^postgresql@16 +started'; then
		as_user "$BREW" services stop postgresql@16
	fi
	if [ "$DRY" = 0 ] && "$PG_BIN/pg_isready" -q -h 127.0.0.1 -p 5432 &&
		! launchctl print "system/$LABEL.postgresql" >/dev/null 2>&1; then
		die "another PostgreSQL is already listening on 5432 — stop it, or pass --database-url"
	fi
	DAEMON_HOME="$USER_HOME" daemon_plist "$LABEL.postgresql" "$BREW_USER" "$LOG/postgresql.log" \
		"$PG_BIN/postgres" -D "$PGDATA" -c listen_addresses=127.0.0.1 |
		write_file "/Library/LaunchDaemons/$LABEL.postgresql.plist" 644 root:wheel
	run touch "$LOG/postgresql.log"
	run chown "$BREW_USER" "$LOG/postgresql.log"
	load_daemon "$LABEL.postgresql"
	wait_for 60 "PostgreSQL" "$PG_BIN/pg_isready" -q -h 127.0.0.1 -p 5432 || die "PostgreSQL did not start; see $LOG/postgresql.log"

	if [ "$UPGRADE" = 0 ]; then
		DB_PASS="$(random_secret 24)"
		PSQL=(sudo -u "$BREW_USER" -H "$PG_BIN/psql" -h 127.0.0.1 -d postgres -v ON_ERROR_STOP=1 -qtA)
		if [ "$DRY" = 1 ]; then
			echo "   + create role/database redesk"
		else
			if [ "$("${PSQL[@]}" -c "SELECT 1 FROM pg_roles WHERE rolname='redesk'")" = 1 ]; then
				"${PSQL[@]}" -c "ALTER ROLE redesk LOGIN PASSWORD '$DB_PASS'"
			else
				"${PSQL[@]}" -c "CREATE ROLE redesk LOGIN PASSWORD '$DB_PASS'"
			fi
			if [ "$("${PSQL[@]}" -c "SELECT 1 FROM pg_database WHERE datname='redesk'")" != 1 ]; then
				"${PSQL[@]}" -c "CREATE DATABASE redesk OWNER redesk"
			fi
		fi
		DB_URL="postgres://redesk:$DB_PASS@127.0.0.1:5432/redesk?sslmode=disable"
	fi
fi

# --- configuration -----------------------------------------------------------

step "Configuration ($ETC)"
run sh -c "echo '$DOMAIN' > '$ETC/domain'"
ADMIN_PASS=""
if [ "$UPGRADE" = 1 ]; then
	echo "   keeping existing $ETC/redesk.env"
else
	ADMIN_PASS="$(random_secret 9)"
	write_file "$ETC/redesk.env" 640 "root:$SVC_USER" <<EOF
# ReDesk server settings (see README). Restart after editing:
#   sudo launchctl kickstart -k system/$LABEL.server
REDESK_LISTEN=127.0.0.1:21114
REDESK_DATABASE_URL=$DB_URL
REDESK_DATA_DIR=$VAR/data
REDESK_WEB_DIR=$PREFIX/web
REDESK_TRUST_PROXY=true
REDESK_COOKIE_SECURE=true
REDESK_TIMEZONE=Asia/Bangkok
REDESK_ADMIN_EMAIL=$EMAIL
REDESK_ADMIN_PASSWORD=$ADMIN_PASS
$([ "$GATEWAY" = 1 ] && printf 'REDESK_GUACD_ADDR=127.0.0.1:4822\nREDESK_GATEWAY_RECORDING_PATH=/recordings' || echo '# REDESK_GUACD_ADDR=127.0.0.1:4822')
EOF
fi

# --- binaries ----------------------------------------------------------------

step "Installing ReDesk $VERSION to $PREFIX"
run launchctl bootout "system/$LABEL.server" 2>/dev/null || true
run rm -rf "$PREFIX/web.new"
run cp -R "$KIT/web" "$PREFIX/web.new"
run rm -rf "$PREFIX/web"
run mv "$PREFIX/web.new" "$PREFIX/web"
run install -m 755 -o root -g wheel "$KIT/bin/redesk-server" "$PREFIX/bin/redesk-server"
run install -m 755 -o root -g wheel "$KIT/redesk-backup" "$PREFIX/bin/redesk-backup"
run install -m 755 -o root -g wheel "$KIT/uninstall.sh" "$PREFIX/uninstall.sh"
write_file "$PREFIX/bin/redesk-server-run" 755 root:wheel <<EOF
#!/bin/sh
# launchd entry point: load settings, then run the server.
set -a
. "$ETC/redesk.env"
set +a
exec "$PREFIX/bin/redesk-server"
EOF
run sh -c "echo '$VERSION' > '$PREFIX/VERSION'"

# --- gateway (guacd in Colima) ---------------------------------------------

if [ "$GATEWAY" = 1 ]; then
	step "Gateway: Colima + guacd"
	# Colima as a boot daemon (not a login item), running as the Homebrew owner who owns the VM.
	DAEMON_HOME="$USER_HOME" daemon_plist "$LABEL.colima" "$BREW_USER" "$LOG/colima.log" \
		"$BREW_PREFIX/bin/colima" start --foreground --cpu 2 --memory 2 --disk 20 --mount none |
		write_file "/Library/LaunchDaemons/$LABEL.colima.plist" 644 root:wheel
	run touch "$LOG/colima.log"
	run chown "$BREW_USER" "$LOG/colima.log"
	load_daemon "$LABEL.colima"
	DOCKER=(env DOCKER_HOST="unix://$USER_HOME/.colima/default/docker.sock" "$BREW_PREFIX/bin/docker")
	echo "   waiting for the Colima VM (first start downloads an image, can take a few minutes)…"
	wait_for 600 "Docker in Colima" brew_user "${DOCKER[@]}" info ||
		die "Colima did not start; see $LOG/colima.log"
	as_user "${DOCKER[@]}" pull -q guacamole/guacd:1.5.5
	as_user "${DOCKER[@]}" rm -f redesk-guacd >/dev/null 2>&1 || true
	as_user "${DOCKER[@]}" run -d --name redesk-guacd --restart unless-stopped \
		-p 127.0.0.1:4822:4822 -v redesk-recordings:/recordings guacamole/guacd:1.5.5
	wait_for 60 "guacd on 127.0.0.1:4822" nc -z 127.0.0.1 4822 || warn "guacd is not reachable on 127.0.0.1:4822 yet"
fi

# --- server daemon -----------------------------------------------------------

step "ReDesk server daemon"
daemon_plist "$LABEL.server" "$SVC_USER" "$LOG/server.log" "$PREFIX/bin/redesk-server-run" |
	write_file "/Library/LaunchDaemons/$LABEL.server.plist" 644 root:wheel
load_daemon "$LABEL.server"
wait_for 60 "redesk-server" curl -fsS http://127.0.0.1:21114/healthz || die "server did not start; see $LOG/server.log"

if [ -n "$ADMIN_PASS" ] && [ "$DRY" = 0 ]; then
	# The bootstrap password is only needed for the very first start.
	sed -i '' '/^REDESK_ADMIN_PASSWORD=/d' "$ETC/redesk.env"
fi

# --- Caddy (HTTPS) -----------------------------------------------------------

step "Caddy (HTTPS for $DOMAIN)"
CADDY_EMAIL="$EMAIL"
if [ -z "$CADDY_EMAIL" ] && [ -f "$ETC/Caddyfile" ]; then
	CADDY_EMAIL="$(awk '/^[[:space:]]*email /{print $2; exit}' "$ETC/Caddyfile")"
fi
write_file "$ETC/Caddyfile" 644 root:wheel <<EOF
{
	email $CADDY_EMAIL
	admin off
}

$DOMAIN {
	encode zstd gzip
	header Strict-Transport-Security "max-age=31536000"
	reverse_proxy 127.0.0.1:21114
	log {
		output file $LOG/caddy-access.log {
			roll_size 20MiB
			roll_keep 10
		}
	}
}
EOF
run "$BREW_PREFIX/bin/caddy" validate --config "$ETC/Caddyfile" --adapter caddyfile
# macOS lets unprivileged processes bind ports 80/443, so Caddy runs as the service account.
daemon_plist "$LABEL.caddy" "$SVC_USER" "$LOG/caddy.log" \
	"$BREW_PREFIX/bin/caddy" run --config "$ETC/Caddyfile" --adapter caddyfile |
	write_file "/Library/LaunchDaemons/$LABEL.caddy.plist" 644 root:wheel
load_daemon "$LABEL.caddy"

FW=/usr/libexec/ApplicationFirewall/socketfilterfw
if [ -x "$FW" ] && "$FW" --getglobalstate 2>/dev/null | grep -q enabled; then
	run "$FW" --add "$BREW_PREFIX/opt/caddy/bin/caddy" >/dev/null
	run "$FW" --unblockapp "$BREW_PREFIX/opt/caddy/bin/caddy" >/dev/null
fi

# --- backups, log rotation, power ------------------------------------------

step "Daily backup and log rotation"
cat <<EOF | write_file "/Library/LaunchDaemons/$LABEL.backup.plist" 644 root:wheel
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>Label</key><string>$LABEL.backup</string>
  <key>ProgramArguments</key><array><string>$PREFIX/bin/redesk-backup</string></array>
  <key>EnvironmentVariables</key><dict><key>PG_BIN</key><string>$PG_BIN</string></dict>
  <key>StartCalendarInterval</key><dict><key>Hour</key><integer>2</integer><key>Minute</key><integer>30</integer></dict>
  <key>StandardOutPath</key><string>$LOG/backup.log</string>
  <key>StandardErrorPath</key><string>$LOG/backup.log</string>
</dict>
</plist>
EOF
run launchctl bootout "system/$LABEL.backup" 2>/dev/null || true
run launchctl bootstrap system "/Library/LaunchDaemons/$LABEL.backup.plist"
write_file /etc/newsyslog.d/redesk.conf 644 root:wheel <<EOF
# logfilename                                 [owner:group]     mode count size  when flags
$LOG/server.log      $SVC_USER:$SVC_USER  640  10    20480 *    J
$LOG/caddy.log       $SVC_USER:$SVC_USER  640  10    20480 *    J
$LOG/postgresql.log  $BREW_USER:staff     640  10    20480 *    J
$LOG/colima.log      $BREW_USER:staff     640  5     20480 *    J
$LOG/backup.log      root:wheel           640  5     1024  *    J
EOF

if [ "$POWER" = 1 ]; then
	step "Power settings for a 24/7 server"
	run pmset -a sleep 0 disksleep 0 displaysleep 10 womp 1 autorestart 1
	run systemsetup -setrestartfreeze on >/dev/null 2>&1 || true
fi

# --- verify & summary --------------------------------------------------------

step "Checking https://$DOMAIN"
if wait_for 90 "https://$DOMAIN" curl -fsS "https://$DOMAIN/healthz"; then
	HTTPS_OK=1
else
	HTTPS_OK=0
	warn "https://$DOMAIN is not reachable yet. Check that DNS points to this Mac's public IP and that"
	warn "ports 80 and 443 are forwarded/open (Let's Encrypt validation). Caddy log: $LOG/caddy.log"
fi

PUBKEY=""
if [ "$DRY" = 0 ]; then
	PUBKEY="$(curl -fsS http://127.0.0.1:21114/api/server-info | sed -n 's/.*"public_key":"\([^"]*\)".*/\1/p')"
fi

echo
bold "ReDesk $VERSION is installed."
echo "  Console:        https://$DOMAIN $([ "${HTTPS_OK:-0}" = 1 ] && echo '(OK)' || echo '(waiting for DNS/ports)')"
if [ -n "$ADMIN_PASS" ]; then
	echo "  First login:    $EMAIL / $ADMIN_PASS   ← shown only now; change it and enable 2FA"
fi
[ -n "$PUBKEY" ] && echo "  Server key:     $PUBKEY"
echo "  Agent setup:    redesk-agent init --server https://$DOMAIN --key <server key>"
echo "  Gateway:        $([ "$GATEWAY" = 1 ] && echo 'RDP/VNC/SSH enabled (guacd in Colima)' || echo disabled)"
echo "  Logs:           $LOG"
echo "  Backups:        $VAR/backups (daily 02:30, 14 days)"
echo
bold "Back up these off this machine (separately from database backups):"
echo "  $VAR/data/server_ed25519.key   agents trust this key — losing it means re-enrolling every agent"
echo "  $VAR/data/vault.key            decrypts stored passwords/tokens"
