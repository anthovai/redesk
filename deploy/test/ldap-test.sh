#!/usr/bin/env bash
# LDAP sign-in tests against a real directory server (glauth) over LDAPS and StartTLS.
#
#   deploy/test/ldap-test.sh          # up + go test + down
#   deploy/test/ldap-test.sh up       # start the directory only (prints the env to export)
#   deploy/test/ldap-test.sh down     # stop and remove it
#
# The directory runs in Docker as "aegiron-admintest-ldap" on 127.0.0.1:13893 (ldap://, StartTLS)
# and 127.0.0.1:13894 (ldaps://) with a throwaway CA made here. Its users are in
# deploy/test/ldap/glauth.cfg. Set AEGIRON_TEST_DATABASE_URL to a scratch database to also run
# the API tests (web and desktop-app sign-in through LDAP).
#
# Env: LDAPTEST_DIR (scratch dir, default $TMPDIR/aegiron-ldaptest).
set -euo pipefail

ROOT=$(cd "$(dirname "$0")/../.." && pwd)
TMP=${TMPDIR:-/tmp}
DIR=${LDAPTEST_DIR:-${TMP%/}/aegiron-ldaptest}
DIR=${DIR%/}
NAME=aegiron-admintest-ldap
IMAGE=glauth/glauth:v2.4.0

certs() {
	mkdir -p "$DIR"
	[ -f "$DIR/ca.pem" ] && return
	openssl req -x509 -newkey ec -pkeyopt ec_paramgen_curve:P-256 -nodes -days 2 -subj "/CN=Aegiron LDAP test CA" \
		-keyout "$DIR/ca.key" -out "$DIR/ca.pem" 2>/dev/null
	openssl req -newkey ec -pkeyopt ec_paramgen_curve:P-256 -nodes -subj "/CN=localhost" \
		-keyout "$DIR/server.key" -out "$DIR/server.csr" 2>/dev/null
	printf 'subjectAltName=DNS:localhost,IP:127.0.0.1\nextendedKeyUsage=serverAuth\n' >"$DIR/ext.cnf"
	openssl x509 -req -in "$DIR/server.csr" -CA "$DIR/ca.pem" -CAkey "$DIR/ca.key" -CAcreateserial -days 2 \
		-extfile "$DIR/ext.cnf" -out "$DIR/server.pem" 2>/dev/null
	chmod 644 "$DIR/server.key"
}

up() {
	certs
	cp "$ROOT/deploy/test/ldap/glauth.cfg" "$DIR/config.cfg"
	docker rm -f "$NAME" >/dev/null 2>&1 || true
	# Copied in rather than bind-mounted: Docker VMs (Colima, Docker Desktop) often do not share $TMPDIR.
	docker create --name "$NAME" -p 127.0.0.1:13893:3893 -p 127.0.0.1:13894:3894 "$IMAGE" >/dev/null
	mkdir -p "$DIR/config" && cp "$DIR/config.cfg" "$DIR/server.pem" "$DIR/server.key" "$DIR/config/"
	docker cp -q "$DIR/config" "$NAME:/app/config"
	docker start "$NAME" >/dev/null
	for _ in $(seq 1 50); do
		if nc -z 127.0.0.1 13894 2>/dev/null && nc -z 127.0.0.1 13893 2>/dev/null; then break; fi
		sleep 0.2
	done
	cat <<EOF
export AEGIRON_TEST_LDAPS_URL=ldaps://localhost:13894
export AEGIRON_TEST_LDAP_URL=ldap://localhost:13893
export AEGIRON_TEST_LDAP_CA=$DIR/ca.pem
EOF
}

down() {
	docker rm -f "$NAME" >/dev/null 2>&1 || true
	rm -rf "$DIR"
}

run() {
	export AEGIRON_TEST_LDAPS_URL=ldaps://localhost:13894
	export AEGIRON_TEST_LDAP_URL=ldap://localhost:13893
	export AEGIRON_TEST_LDAP_CA=$DIR/ca.pem
	(cd "$ROOT/server" && go test -count=1 -run 'LDAP' -v ./internal/ldapauth/ ./internal/api/)
}

case "${1:-all}" in
up) up ;;
down) down ;;
run) run ;;
all)
	trap down EXIT
	up >/dev/null
	run
	;;
*) echo "usage: $0 [up|run|down]" >&2; exit 2 ;;
esac
