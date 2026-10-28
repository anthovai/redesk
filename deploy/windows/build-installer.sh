#!/bin/bash
# Builds the Windows agent installer on macOS or Linux (cross-compiled, no Windows machine needed).
#
#   brew install mingw-w64 makensis && rustup target add x86_64-pc-windows-gnu    # once
#   deploy/windows/build-installer.sh
#
# Pre-configured installer for your organisation (users just click Next; also used by /S):
#   SERVER_URL=https://redesk.example.com SERVER_KEY=<key from Server settings> deploy/windows/build-installer.sh
#
# Output: dist/ReDesk-Agent-Setup-<version>[-<host>].exe and its SHA-256.
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
VERSION="$(tr -d '[:space:]' < "$ROOT/VERSION")"
TARGET=x86_64-pc-windows-gnu
SERVER_URL="${SERVER_URL:-}"
SERVER_KEY="${SERVER_KEY:-}"

command -v makensis >/dev/null || { echo "makensis not found (brew install makensis)"; exit 1; }
command -v x86_64-w64-mingw32-gcc >/dev/null || { echo "mingw-w64 not found (brew install mingw-w64)"; exit 1; }

if [ -n "$SERVER_URL" ]; then
	case "$SERVER_URL" in https://*) ;; *) echo "SERVER_URL must start with https://"; exit 1 ;; esac
	echo "$SERVER_KEY" | grep -Eq '^[A-Za-z0-9+/]{43}=$' || { echo "SERVER_KEY must be the 44-character base64 key from Server settings"; exit 1; }
	host="${SERVER_URL#https://}"; host="${host%%/*}"; host="${host%%:*}"
	OUT="ReDesk-Agent-Setup-$VERSION-$host.exe"
else
	OUT="ReDesk-Agent-Setup-$VERSION.exe"
fi

echo "==> Building Windows binaries ($TARGET)"
(cd "$ROOT/agent" && cargo build --release --quiet --target "$TARGET")

echo "==> Building installer"
mkdir -p "$ROOT/dist"
makensis -V2 -INPUTCHARSET UTF8 \
	-DVERSION="$VERSION" \
	-DSRC="$ROOT/agent/target/$TARGET/release" \
	-DASSETS="$ROOT/agent/assets" \
	-DSERVER_URL="$SERVER_URL" \
	-DSERVER_KEY="$SERVER_KEY" \
	-DOUTFILE="$ROOT/dist/$OUT" \
	"$ROOT/deploy/windows/redesk-agent.nsi"

(cd "$ROOT/dist" && shasum -a 256 "$OUT" | tee "$OUT.sha256")
echo "==> dist/$OUT"
echo "    Unsigned: Windows SmartScreen will warn until it is signed with your organisation's"
echo "    code-signing certificate (e.g. osslsigncode sign -pkcs12 cert.p12 ... on macOS)."
