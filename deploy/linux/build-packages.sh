#!/bin/bash
# Builds the Linux packages of the desktop app and agent for this machine's architecture.
#
#   deploy/linux/build-packages.sh            on Linux with the build dependencies (see Dockerfile)
#   deploy/linux/build-packages.sh --docker   anywhere with Docker (also the default off Linux)
#   PLATFORM=linux/amd64 deploy/linux/build-packages.sh --docker   another architecture (emulated)
#
# Output (dist/):
#   aegiron_<version>-1_<amd64|arm64>.deb
#   aegiron-<version>-1.<x86_64|aarch64>.rpm
#   aegiron-linux-<x86_64|aarch64>-<version>.tar.gz   binaries + install.sh (per-user, no root)
#   each with a .sha256 file
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
VERSION="$(tr -d '[:space:]' < "$ROOT/VERSION")"
DIST="$ROOT/dist"

step() { printf '\n\033[1m==> %s\033[0m\n' "$*"; }

if [[ "${1:-}" == "--docker" || "$(uname -s)" != "Linux" ]]; then
	IMAGE=aegiron-linux-build
	step "Build image ($IMAGE)"
	docker build ${PLATFORM:+--platform "$PLATFORM"} -t "$IMAGE" "$ROOT/deploy/linux"
	step "Build inside the container"
	# Cargo registry and target directory live in volumes, so the host's agent/target is untouched.
	docker run --rm ${PLATFORM:+--platform "$PLATFORM"} \
		-v "$ROOT:/src" \
		-v aegiron-linux-cargo:/usr/local/cargo/registry \
		-v "aegiron-linux-target${PLATFORM:+-${PLATFORM//\//-}}:/target" \
		-e CARGO_TARGET_DIR=/target -e HOST_UID="$(id -u)" -e HOST_GID="$(id -g)" \
		"$IMAGE" /src/deploy/linux/build-packages.sh --inside
	exit 0
fi

cd "$ROOT/agent"
TARGET_DIR="${CARGO_TARGET_DIR:-$ROOT/agent/target}"
ARCH="$(uname -m)"

step "aegiron + aegiron-agent $VERSION ($ARCH)"
cargo build --release --locked --bins

mkdir -p "$DIST"
step ".deb"
cargo deb --no-build --deb-version "$VERSION-1" --output "$DIST/"
step ".rpm"
cargo generate-rpm --target-dir "$TARGET_DIR" -s "version = \"$VERSION\"" -o "$DIST/"

step ".tar.gz"
NAME="aegiron-linux-$ARCH-$VERSION"
STAGE="$(mktemp -d)/$NAME"
mkdir -p "$STAGE"
install -m755 "$TARGET_DIR/release/aegiron" "$TARGET_DIR/release/aegiron-agent" "$ROOT/deploy/linux/install.sh" "$STAGE/"
install -m644 "$ROOT/deploy/linux/aegiron.desktop" "$ROOT/deploy/linux/aegiron-agent.service" "$ROOT/deploy/linux/README.md" "$STAGE/"
install -m644 "$ROOT/agent/assets/aegiron.png" "$STAGE/aegiron.png"
echo "$VERSION" > "$STAGE/VERSION"
tar -C "$(dirname "$STAGE")" -czf "$DIST/$NAME.tar.gz" "$NAME"
rm -rf "$(dirname "$STAGE")"

cd "$DIST"
for f in aegiron_"$VERSION"-1_*.deb aegiron-"$VERSION"-1.*.rpm "$NAME.tar.gz"; do
	[[ -f "$f" ]] && sha256sum "$f" > "$f.sha256"
done
if [[ -n "${HOST_UID:-}" ]]; then
	chown -R "$HOST_UID:${HOST_GID:-$HOST_UID}" "$DIST"
fi
step "Done"
ls -l "$DIST" | grep -E 'aegiron[_-]' || true
