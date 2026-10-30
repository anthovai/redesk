#!/usr/bin/env bash
# End-to-end test on this machine (docs/TESTING.md): builds everything, starts an isolated server
# (own database, port and data folder) and an agent with a scratch config, then runs the protocol,
# native-viewer, browser media and sign-in tests against them. Cleans up on exit.
#
#   scripts/e2e.sh            # or: make e2e
#
# Env: E2E_PORT (21200), E2E_DB (aegiron_e2e, dropped and recreated), PG_BIN (folder with psql,
# createdb, dropdb), CHROME_PATH, E2E_NO_BUILD=1 (reuse existing builds), E2E_KEEP=1 (leave the
# server and agent running and print how to reach them), E2E_REAL_AUDIO=1 (capture system audio
# instead of the test tone; needs Screen Recording permission and something playing),
# E2E_STEPS (space-separated subset of: protocol native media auth remote; default all).
set -euo pipefail

ROOT=$(cd "$(dirname "$0")/.." && pwd)
PORT=${E2E_PORT:-21200}
DB=${E2E_DB:-aegiron_e2e}
PG_BIN=${PG_BIN:-$(dirname "$(command -v psql 2>/dev/null || echo /opt/homebrew/opt/postgresql@16/bin/psql)")}
URL="http://127.0.0.1:$PORT"
ADMIN_PW="e2e-admin-$(openssl rand -hex 6)"
WORK=$(mktemp -d "${TMPDIR:-/tmp}/aegiron-e2e.XXXXXX")
REL="$ROOT/agent/target/release"
PIDS=()

cleanup() {
	local code=$?
	if [ -n "${E2E_KEEP:-}" ]; then
		echo "kept running: server $URL (admin@aegiron.local / $ADMIN_PW), work dir $WORK, pids ${PIDS[*]}"
		exit $code
	fi
	for p in "${PIDS[@]}"; do kill "$p" 2>/dev/null || true; done
	wait 2>/dev/null || true
	"$PG_BIN/dropdb" --if-exists "$DB" >/dev/null 2>&1 || true
	if [ $code -ne 0 ]; then
		echo "--- server log (tail)"; tail -20 "$WORK/server.log" 2>/dev/null || true
		echo "--- agent log (tail)"; tail -20 "$WORK/agent.log" 2>/dev/null || true
	fi
	rm -rf "$WORK"
	exit $code
}
trap cleanup EXIT

if [ -z "${E2E_NO_BUILD:-}" ]; then
	echo "=== build"
	(cd "$ROOT/web" && bun install --frozen-lockfile >/dev/null && bun run build >/dev/null)
	(cd "$ROOT/server" && go build -o "$WORK/aegiron-server" ./cmd/aegiron-server)
	(cd "$ROOT/agent" && cargo build --release --bin aegiron-agent --example media_probe --example make_recording)
else
	(cd "$ROOT/server" && go build -o "$WORK/aegiron-server" ./cmd/aegiron-server)
fi

echo "=== server on $URL (database $DB)"
"$PG_BIN/dropdb" --if-exists "$DB" >/dev/null
"$PG_BIN/createdb" "$DB"
PGHOST=$("$PG_BIN/psql" -d postgres -Atc 'show unix_socket_directories' | cut -d, -f1)
DB_URL="postgres:///$DB?host=$PGHOST"
mkdir -p "$WORK/data" "$WORK/agent"
(cd "$ROOT/server" && AEGIRON_LISTEN="127.0.0.1:$PORT" AEGIRON_DATABASE_URL="$DB_URL&sslmode=disable" \
	AEGIRON_DATA_DIR="$WORK/data" AEGIRON_WEB_DIR="$ROOT/web/build" AEGIRON_ADMIN_PASSWORD="$ADMIN_PW" \
	AEGIRON_P2P=false exec "$WORK/aegiron-server" >"$WORK/server.log" 2>&1) &
PIDS+=($!)
for _ in $(seq 50); do curl -fs "$URL/healthz" >/dev/null 2>&1 && break; sleep 0.2; done
KEY=$(sed -n 's/.*server identity" public_key="\([^"]*\)".*/\1/p' "$WORK/server.log")
[ -n "$KEY" ] || { echo "server did not start"; exit 1; }

echo "=== agent (scratch config, no input, no LAN/direct; tunnels to this machine and terminals allowed)"
AGENT=("$REL/aegiron-agent" --config "$WORK/agent/agent.toml")
"${AGENT[@]}" init --server "$URL" --key "$KEY" >/dev/null
"${AGENT[@]}" set --allow-audio true --allow-input false --allow-clipboard false --direct false \
	--allow-tunnel true --allow-tunnel-lan false --allow-terminal true >/dev/null
sed -i.bak 's/^lan_discovery = true/lan_discovery = false/' "$WORK/agent/agent.toml"
TONE=tone
[ -n "${E2E_REAL_AUDIO:-}" ] && TONE=
AEGIRON_TEST_AUDIO=$TONE RUST_LOG=info "${AGENT[@]}" run >"$WORK/agent.log" 2>&1 &
PIDS+=($!)
for _ in $(seq 50); do grep -q 'registered with server' "$WORK/agent.log" && break; sleep 0.2; done
ID=$(sed -n 's/^id = "\(.*\)"/\1/p' "$WORK/agent/agent.toml")
echo "device $ID"
# The temporary password changes after every successful session: read it before each step.
pw() { "${AGENT[@]}" show-password; }

export AEGIRON_URL="$URL" AEGIRON_PASSWORD="$ADMIN_PW" ROOT ID REL
export PW=
FAILED=0
# step <key> <title> <command...>; E2E_STEPS="remote auth" runs only those.
step() {
	local key=$1
	shift
	if [ -n "${E2E_STEPS:-}" ] && [[ " $E2E_STEPS " != *" $key "* ]]; then return; fi
	echo; echo "=== $1"; shift; PW=$(pw); "$@" || FAILED=$((FAILED + 1))
}

step protocol "protocol: codecs, audio, mute (web/scripts/e2e-viewer.ts)" \
	bash -c 'cd "$ROOT/web" && bun scripts/e2e-viewer.ts "$ID" "$PW"'
step native "native viewer asks for AV1 (examples/media_probe)" bash -c '
	out=$(AEGIRON_CODEC=av1 AEGIRON_AUDIO_OUT=0 "$REL/examples/media_probe" "$ID" "$PW" 5 2>/dev/null | grep -E "^t=|SUMMARY")
	echo "$out" | tail -3
	echo "$out" | grep -q "codec=h264" && echo "$out" | grep -qE "audio_seconds_nonsilent=[1-9]"'
step media "browser: live viewer, recordings, AV1 playback (web/scripts/e2e-media.ts)" \
	env AEGIRON_AGENT_CONFIG="$WORK/agent/agent.toml" AEGIRON_E2E_DB="$DB_URL" PSQL="$PG_BIN/psql" \
	AEGIRON_MAKE_RECORDING="$REL/examples/make_recording" bash -c 'cd "$ROOT/web" && bun scripts/e2e-media.ts "$ID" "$PW"'
step auth "browser + CLI: remembered 2FA devices (web/scripts/e2e-auth.ts)" \
	env AEGIRON_AGENT_BIN="$REL/aegiron-agent" bash -c 'cd "$ROOT/web" && bun scripts/e2e-auth.ts'
step remote "CLI + browser: TCP tunnels and terminals (web/scripts/e2e-remote.ts)" \
	env AEGIRON_AGENT_BIN="$REL/aegiron-agent" AEGIRON_AGENT_CONFIG="$WORK/agent/agent.toml" \
	bash -c 'cd "$ROOT/web" && bun scripts/e2e-remote.ts "$ID"'

echo
if [ $FAILED -ne 0 ]; then
	echo "e2e: $FAILED step(s) failed"
	exit 1
fi
echo "e2e: all steps passed"
