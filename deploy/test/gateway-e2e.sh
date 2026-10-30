#!/usr/bin/env bash
# End-to-end test of the RDP / VNC / SSH browser gateway against real protocol servers.
#
#   deploy/test/gateway-e2e.sh            # up + run + down
#   deploy/test/gateway-e2e.sh up         # build/start targets, database and server only
#   deploy/test/gateway-e2e.sh run        # run the browser test against a running setup
#   deploy/test/gateway-e2e.sh down       # stop everything, drop the test database
#
# Isolated from any other instance: database "aegiron_gw" on the local Postgres, server on
# :21300 with its own data dir, guacd + targets in Docker (containers "aegiron-gwtest-*",
# guacd published on 127.0.0.1:14822). Needs Docker, Go, bun, Postgres 16 and Google Chrome.
#
# Env: GWTEST_DIR (scratch dir, default $TMPDIR/aegiron-gwtest), GWTEST_PORT (21300),
#      PSQL (psql binary), CHROME (browser binary, default: installed Google Chrome).
set -euo pipefail

ROOT=$(cd "$(dirname "$0")/../.." && pwd)
COMPOSE=(docker compose -f "$ROOT/deploy/test/gateway/compose.yml")
DIR=${GWTEST_DIR:-${TMPDIR:-/tmp}/aegiron-gwtest}
DIR=${DIR%/}
PORT=${GWTEST_PORT:-21300}
DB=aegiron_gw
PSQL=${PSQL:-$(command -v psql || echo /opt/homebrew/opt/postgresql@16/bin/psql)}

export GWTEST_GUACD_PORT=${GWTEST_GUACD_PORT:-14822}
export GWTEST_RDP_PASSWORD='Rdp-Pass-1!'
export GWTEST_VNC_PASSWORD='vncpw123'
export GWTEST_SSH_PASSWORD='Ssh-Pass-1!'
export AEGIRON_URL="http://127.0.0.1:$PORT"
export AEGIRON_EMAIL=admin@gwtest.local
export AEGIRON_PASSWORD='Gw-Admin-Pass-1!'
export GWTEST_DIR=$DIR

keys() {
	mkdir -p "$DIR"
	[ -f "$DIR/id_ed25519" ] || ssh-keygen -q -t ed25519 -N '' -C gwtest-ed25519 -f "$DIR/id_ed25519"
	[ -f "$DIR/id_rsa" ] || ssh-keygen -q -t rsa -b 3072 -m PEM -N '' -C gwtest-rsa -f "$DIR/id_rsa"
	GWTEST_SSH_PUBKEYS="$(cat "$DIR/id_ed25519.pub" "$DIR/id_rsa.pub")"
	export GWTEST_SSH_PUBKEYS
}

up() {
	keys
	"${COMPOSE[@]}" up -d --build --wait 2>&1 | tail -n 20
	"$PSQL" -d postgres -tc "select 1 from pg_roles where rolname='aegiron'" | grep -q 1 ||
		"$PSQL" -d postgres -c "create role aegiron login password 'aegiron'"
	"$PSQL" -d postgres -tc "select 1 from pg_database where datname='$DB'" | grep -q 1 ||
		"$PSQL" -d postgres -c "create database $DB owner aegiron"

	(cd "$ROOT/web" && bun install --frozen-lockfile >/dev/null && bun run build >/dev/null)
	(cd "$ROOT/server" && go build -o "$DIR/aegiron-server" ./cmd/aegiron-server)
	mkdir -p "$DIR/data"
	if [ -f "$DIR/server.pid" ] && kill -0 "$(cat "$DIR/server.pid")" 2>/dev/null; then kill "$(cat "$DIR/server.pid")"; sleep 1; fi
	AEGIRON_LISTEN="127.0.0.1:$PORT" \
	AEGIRON_DATABASE_URL="postgres://aegiron:aegiron@localhost:5432/$DB?sslmode=disable" \
	AEGIRON_DATA_DIR="$DIR/data" \
	AEGIRON_WEB_DIR="$ROOT/web/build" \
	AEGIRON_ADMIN_EMAIL="$AEGIRON_EMAIL" AEGIRON_ADMIN_PASSWORD="$AEGIRON_PASSWORD" \
	AEGIRON_GUACD_ADDR="127.0.0.1:$GWTEST_GUACD_PORT" \
	AEGIRON_GATEWAY_RECORDING_PATH=/recordings \
	AEGIRON_UDP_PORT=0 AEGIRON_SYNC_MINUTES=1440 \
		nohup "$DIR/aegiron-server" >"$DIR/server.log" 2>&1 &
	echo $! >"$DIR/server.pid"
	for _ in $(seq 100); do
		curl -fs -o /dev/null "$AEGIRON_URL/healthz" && break
		sleep 0.2
	done
	curl -fs -o /dev/null "$AEGIRON_URL/healthz" || { tail -n 20 "$DIR/server.log"; exit 1; }
	echo "server: $AEGIRON_URL (log $DIR/server.log)"
}

run() {
	keys
	(cd "$ROOT/web" && bun scripts/e2e-gateway.ts)
}

down() {
	if [ -f "$DIR/server.pid" ]; then kill "$(cat "$DIR/server.pid")" 2>/dev/null || true; rm -f "$DIR/server.pid"; fi
	keys
	"${COMPOSE[@]}" down -v --remove-orphans 2>&1 | tail -n 5
	"$PSQL" -d postgres -c "drop database if exists $DB" >/dev/null
	echo "stopped; database $DB dropped (scratch files kept in $DIR)"
}

case "${1:-all}" in
up) up ;;
run) run ;;
down) down ;;
all)
	up
	trap down EXIT
	run
	;;
*) echo "usage: $0 [up|run|down]" >&2; exit 2 ;;
esac
