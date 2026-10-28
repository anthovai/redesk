#!/bin/bash
# Builds the Windows agent installer on macOS or Linux (cross-compiled, no Windows machine needed).
#
#   brew install mingw-w64 makensis && rustup target add x86_64-pc-windows-gnu    # once
#   deploy/windows/build-installer.sh
#
# Pre-configured installer for your organisation (users just click Next; also used by /S):
#   SERVER_URL=https://aegiron.example.com SERVER_KEY=<key from Server settings> deploy/windows/build-installer.sh
#
# Optional Authenticode signing with osslsigncode (brew install osslsigncode). Signs
# aegiron.exe and aegiron-agent.exe before packaging, the uninstaller while NSIS builds it, and
# the installer last. Set ONE of:
#   AEGIRON_WIN_SIGN_PKCS12=org.p12                         PKCS#12 with certificate chain + key
#   AEGIRON_WIN_SIGN_CERT=org.crt AEGIRON_WIN_SIGN_KEY=org.key   PEM/DER (or .spc) certificate + key
# and, if the key is encrypted, AEGIRON_WIN_SIGN_PASS_FILE=<file with the password> (preferred)
# or AEGIRON_WIN_SIGN_PASS=<password> (never passed on a command line; written to a 0600 temp file).
#   AEGIRON_WIN_SIGN_TIMESTAMP   RFC 3161 server (default http://timestamp.digicert.com; "none" =
#                                no timestamp: the signature then expires with the certificate)
#   AEGIRON_WIN_SIGN_CAFILE      CA bundle used to verify the result (default: the system's)
#
# Output: dist/Aegiron-Agent-Setup-<version>[-<host>].exe and its SHA-256.
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
VERSION="$(tr -d '[:space:]' < "$ROOT/VERSION")"
TARGET=x86_64-pc-windows-gnu
SERVER_URL="${SERVER_URL:-}"
SERVER_KEY="${SERVER_KEY:-}"
SIGN_P12="${AEGIRON_WIN_SIGN_PKCS12:-}"
SIGN_CERT="${AEGIRON_WIN_SIGN_CERT:-}"
SIGN_KEY="${AEGIRON_WIN_SIGN_KEY:-}"
SIGN_PASS="${AEGIRON_WIN_SIGN_PASS:-}"
SIGN_PASS_FILE="${AEGIRON_WIN_SIGN_PASS_FILE:-}"
SIGN_TS="${AEGIRON_WIN_SIGN_TIMESTAMP:-http://timestamp.digicert.com}"
SIGN_CAFILE="${AEGIRON_WIN_SIGN_CAFILE:-}"

die() { echo "error: $*" >&2; exit 1; }

command -v makensis >/dev/null || die "makensis not found (brew install makensis)"
command -v x86_64-w64-mingw32-gcc >/dev/null || die "mingw-w64 not found (brew install mingw-w64)"

if [ -n "$SERVER_URL" ]; then
	case "$SERVER_URL" in https://*) ;; *) die "SERVER_URL must start with https://" ;; esac
	echo "$SERVER_KEY" | grep -Eq '^[A-Za-z0-9+/]{43}=$' || die "SERVER_KEY must be the 44-character base64 key from Server settings"
	host="${SERVER_URL#https://}"; host="${host%%/*}"; host="${host%%:*}"
	OUT="Aegiron-Agent-Setup-$VERSION-$host.exe"
else
	OUT="Aegiron-Agent-Setup-$VERSION.exe"
fi

# --- signing configuration: checked before the (slow) build, so a mistake fails fast ---
SIGN=""
if [ -n "$SIGN_P12$SIGN_CERT$SIGN_KEY$SIGN_PASS$SIGN_PASS_FILE" ]; then
	SIGN=1
	command -v osslsigncode >/dev/null || die "Authenticode signing requested but osslsigncode is not installed (brew install osslsigncode)"
	if [ -n "$SIGN_P12" ]; then
		[ -z "$SIGN_CERT$SIGN_KEY" ] || die "set either AEGIRON_WIN_SIGN_PKCS12 or AEGIRON_WIN_SIGN_CERT + AEGIRON_WIN_SIGN_KEY, not both"
		[ -f "$SIGN_P12" ] && [ -r "$SIGN_P12" ] || die "AEGIRON_WIN_SIGN_PKCS12: cannot read $SIGN_P12"
	else
		[ -n "$SIGN_CERT" ] && [ -n "$SIGN_KEY" ] || die "signing needs AEGIRON_WIN_SIGN_PKCS12, or both AEGIRON_WIN_SIGN_CERT and AEGIRON_WIN_SIGN_KEY"
		[ -f "$SIGN_CERT" ] && [ -r "$SIGN_CERT" ] || die "AEGIRON_WIN_SIGN_CERT: cannot read $SIGN_CERT"
		[ -f "$SIGN_KEY" ] && [ -r "$SIGN_KEY" ] || die "AEGIRON_WIN_SIGN_KEY: cannot read $SIGN_KEY"
	fi
	[ -z "$SIGN_PASS_FILE" ] || [ -r "$SIGN_PASS_FILE" ] || die "AEGIRON_WIN_SIGN_PASS_FILE: cannot read $SIGN_PASS_FILE"
	[ -z "$SIGN_PASS" ] || [ -z "$SIGN_PASS_FILE" ] || die "set AEGIRON_WIN_SIGN_PASS or AEGIRON_WIN_SIGN_PASS_FILE, not both"
	case "$SIGN_TS" in none | http://* | https://*) ;; *) die "AEGIRON_WIN_SIGN_TIMESTAMP must be an http(s) URL or \"none\"" ;; esac
	[ -z "$SIGN_CAFILE" ] || [ -r "$SIGN_CAFILE" ] || die "AEGIRON_WIN_SIGN_CAFILE: cannot read $SIGN_CAFILE"
fi

WORK="$(mktemp -d "${TMPDIR:-/tmp}/aegiron-win.XXXXXX")"
trap 'rm -rf "$WORK"' EXIT

if [ -n "$SIGN" ]; then
	chmod 700 "$WORK"
	args=()
	if [ -n "$SIGN_P12" ]; then args+=(-pkcs12 "$SIGN_P12"); else args+=(-certs "$SIGN_CERT" -key "$SIGN_KEY"); fi
	if [ -n "$SIGN_PASS" ]; then
		(umask 077 && printf '%s' "$SIGN_PASS" > "$WORK/pass")
		SIGN_PASS_FILE="$WORK/pass"
	fi
	[ -z "$SIGN_PASS_FILE" ] || args+=(-readpass "$SIGN_PASS_FILE")
	args+=(-h sha256 -n Aegiron -i https://github.com/anthovai/redesk)
	[ "$SIGN_TS" = none ] || args+=(-ts "$SIGN_TS")
	verify=(osslsigncode verify)
	[ -z "$SIGN_CAFILE" ] || verify+=(-CAfile "$SIGN_CAFILE")
	[ -z "$SIGN_CAFILE" ] || [ "$SIGN_TS" = none ] || verify+=(-TSA-CAfile "$SIGN_CAFILE")
	# One helper used for the programs, the uninstaller (from inside makensis) and the installer.
	{
		echo '#!/bin/bash'
		echo 'set -euo pipefail'
		printf 'f="$1"; tmp="$f.signing"; rm -f "$tmp"\n'
		printf 'if ! osslsigncode sign'; printf ' %q' "${args[@]}"; printf ' -in "$f" -out "$tmp" >"$tmp.log" 2>&1; then\n'
		echo '  echo "error: osslsigncode could not sign $(basename "$f"):" >&2; sed "s/^/   /" "$tmp.log" >&2; rm -f "$tmp" "$tmp.log"; exit 1'
		echo 'fi'
		echo 'rm -f "$tmp.log"; mv "$tmp" "$f"'
		echo 'echo "   signed $(basename "$f")"'
	} > "$WORK/sign-one.sh"
	chmod 700 "$WORK/sign-one.sh"
	[ "$SIGN_TS" = none ] && echo "warning: no timestamp; signatures stop being valid when the certificate expires" >&2
fi

echo "==> Building Windows binaries ($TARGET)"
(cd "$ROOT/agent" && cargo build --release --quiet --target "$TARGET")
SRC="$ROOT/agent/target/$TARGET/release"

nsis_sign=()
if [ -n "$SIGN" ]; then
	echo "==> Signing programs (Authenticode)"
	mkdir -p "$WORK/bin"
	cp "$SRC/aegiron.exe" "$SRC/aegiron-agent.exe" "$WORK/bin/"
	"$WORK/sign-one.sh" "$WORK/bin/aegiron.exe"
	"$WORK/sign-one.sh" "$WORK/bin/aegiron-agent.exe"
	SRC="$WORK/bin"
	nsis_sign=(-DSIGN_CMD="$WORK/sign-one.sh")
fi

echo "==> Building installer"
mkdir -p "$ROOT/dist"
makensis -V2 -INPUTCHARSET UTF8 \
	-DVERSION="$VERSION" \
	-DSRC="$SRC" \
	-DASSETS="$ROOT/agent/assets" \
	-DSERVER_URL="$SERVER_URL" \
	-DSERVER_KEY="$SERVER_KEY" \
	-DOUTFILE="$ROOT/dist/$OUT" \
	${nsis_sign[@]+"${nsis_sign[@]}"} \
	"$ROOT/deploy/windows/aegiron-agent.nsi"

if [ -n "$SIGN" ]; then
	echo "==> Signing installer"
	"$WORK/sign-one.sh" "$ROOT/dist/$OUT"
	if ! "${verify[@]}" -in "$ROOT/dist/$OUT" > "$WORK/verify.log" 2>&1; then
		sed 's/^/   /' "$WORK/verify.log" >&2
		die "signature on $OUT does not verify (for a private CA, set AEGIRON_WIN_SIGN_CAFILE)"
	fi
	grep -E '^(Signer|Subject|Signature verification|Timestamp)' "$WORK/verify.log" | sed 's/^/   /' || true
fi

(cd "$ROOT/dist" && shasum -a 256 "$OUT" | tee "$OUT.sha256")
echo "==> dist/$OUT"
if [ -z "$SIGN" ]; then
	echo "    Unsigned: Windows SmartScreen will warn until it is signed with your organisation's"
	echo "    code-signing certificate (see deploy/windows/README.md, AEGIRON_WIN_SIGN_*)."
fi
