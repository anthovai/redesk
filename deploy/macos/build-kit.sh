#!/bin/bash
# Builds the macOS deploy kit on a development Mac.
#
#   deploy/macos/build-kit.sh
#
# Output (dist/):
#   aegiron-server-macos-<version>.tar.gz   server kit: universal binary, web console, installer
#   aegiron-agent-macos-<version>.tar.gz    agent for controlled Macs (universal if the x86_64 target is installed)
#   Aegiron-<version>.dmg                   Aegiron.app for controlled Macs (see build-app.sh)
#   SHA256SUMS
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
VERSION="$(tr -d '[:space:]' < "$ROOT/VERSION")"
DIST="$ROOT/dist"
KIT="$DIST/aegiron-server-macos-$VERSION"
AGENT_KIT="$DIST/aegiron-agent-macos-$VERSION"

step() { printf '\n\033[1m==> %s\033[0m\n' "$*"; }

rm -rf "$KIT" "$AGENT_KIT"
mkdir -p "$KIT/bin" "$AGENT_KIT"

step "Web console"
(cd "$ROOT/web" && bun install --frozen-lockfile >/dev/null && bun run build >/dev/null)
cp -R "$ROOT/web/build" "$KIT/web"

step "aegiron-server $VERSION (arm64 + amd64)"
for arch in arm64 amd64; do
	(cd "$ROOT/server" && GOOS=darwin GOARCH=$arch CGO_ENABLED=0 go build -trimpath \
		-ldflags "-s -w -X aegiron/server/internal/api.Version=$VERSION" \
		-o "$DIST/.aegiron-server-$arch" ./cmd/aegiron-server)
done
lipo -create -output "$KIT/bin/aegiron-server" "$DIST/.aegiron-server-arm64" "$DIST/.aegiron-server-amd64"
rm -f "$DIST"/.aegiron-server-*

step "aegiron-agent $VERSION"
targets=()
for target in aarch64-apple-darwin x86_64-apple-darwin; do
	if rustup target list --installed | grep -qx "$target"; then
		(cd "$ROOT/agent" && cargo build --release --quiet --target "$target")
		targets+=("$target")
	else
		echo "   skip $target (install with: rustup target add $target)"
	fi
done
# aegiron-agent = CLI, aegiron = desktop app (window, tray, remote-control windows)
for bin in aegiron-agent aegiron; do
	slices=()
	for t in "${targets[@]}"; do slices+=("$ROOT/agent/target/$t/release/$bin"); done
	lipo -create -output "$AGENT_KIT/$bin" "${slices[@]}"
done

# Ad-hoc signatures: required to run on Apple Silicon. For fleet-wide TCC permissions via MDM
# (PPPC profiles), sign the agent with your organisation's Developer ID instead.
codesign --force --sign - "$KIT/bin/aegiron-server" "$AGENT_KIT/aegiron-agent" "$AGENT_KIT/aegiron" 2>/dev/null

cp "$ROOT/deploy/macos/install.sh" "$ROOT/deploy/macos/uninstall.sh" "$ROOT/deploy/macos/aegiron-backup" "$KIT/"
cp "$ROOT/deploy/macos/README.md" "$KIT/README.md"
cp "$ROOT/VERSION" "$KIT/VERSION"
chmod 755 "$KIT"/*.sh "$KIT/aegiron-backup" "$KIT/bin/aegiron-server" "$AGENT_KIT/aegiron-agent" "$AGENT_KIT/aegiron"
cat > "$AGENT_KIT/README.txt" <<EOF
Aegiron agent $VERSION for macOS ($(lipo -archs "$AGENT_KIT/aegiron-agent"))

  sudo mkdir -p /usr/local/bin && sudo cp aegiron-agent aegiron /usr/local/bin/
  aegiron                      # desktop app: sign in, ID/password, control other machines
  xattr -d com.apple.quarantine /usr/local/bin/aegiron-agent 2>/dev/null
  aegiron-agent init --server https://<your-domain> --key <server public key from Settings>
  aegiron-agent install        # start at login (per user)
  aegiron-agent show-password  # current one-time password

Then grant Screen Recording and Accessibility to /usr/local/bin/aegiron-agent
in System Settings > Privacy & Security.
EOF

"$ROOT/deploy/macos/build-app.sh" --bins "$AGENT_KIT"

step "Archives"
(cd "$DIST" && tar -czf "$(basename "$KIT").tar.gz" "$(basename "$KIT")" \
	&& tar -czf "$(basename "$AGENT_KIT").tar.gz" "$(basename "$AGENT_KIT")" \
	&& shasum -a 256 ./*-"$VERSION".tar.gz ./Aegiron-"$VERSION".dmg > SHA256SUMS)
lipo -archs "$KIT/bin/aegiron-server" | sed 's/^/   server: /'
lipo -archs "$AGENT_KIT/aegiron-agent" | sed 's/^/   agent:  /'
cat "$DIST/SHA256SUMS"
