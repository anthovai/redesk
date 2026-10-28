#!/bin/bash
# Builds Aegiron.app (menu-bar desktop app + aegiron-agent CLI) and a drag-to-install disk image.
#
#   deploy/macos/build-app.sh                # build the agent for every installed Apple target
#   deploy/macos/build-app.sh --bins DIR     # use already built (universal) aegiron + aegiron-agent from DIR
#
# Output (dist/):
#   Aegiron.app
#   Aegiron-<version>.dmg
#
# The app is signed ad-hoc with the hardened runtime. macOS ties privacy permissions (Screen
# Recording, Accessibility) to the signature, so every rebuild asks for them again. For fleet
# deployment with MDM/PPPC profiles, sign with a Developer ID and notarize instead (see README).
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
VERSION="$(tr -d '[:space:]' < "$ROOT/VERSION")"
DIST="$ROOT/dist"
APP="$DIST/Aegiron.app"
DMG="$DIST/Aegiron-$VERSION.dmg"
BUNDLE_ID="co.aegiron.app"
SIGN_ID="${AEGIRON_SIGN_ID:--}" # "-" = ad-hoc

step() { printf '\n\033[1m==> %s\033[0m\n' "$*"; }

BINS=""
while [ $# -gt 0 ]; do
	case "$1" in
		--bins) BINS="$(cd "$2" && pwd)"; shift 2 ;;
		*) echo "unknown option: $1" >&2; exit 2 ;;
	esac
done

WORK="$(mktemp -d "${TMPDIR:-/tmp}/aegiron-app.XXXXXX")"
trap 'rm -rf "$WORK"' EXIT

if [ -z "$BINS" ]; then
	step "aegiron + aegiron-agent $VERSION"
	targets=()
	for target in aarch64-apple-darwin x86_64-apple-darwin; do
		if rustup target list --installed | grep -qx "$target"; then
			(cd "$ROOT/agent" && cargo build --release --quiet --target "$target")
			targets+=("$target")
		else
			echo "   skip $target (install with: rustup target add $target)"
		fi
	done
	BINS="$WORK/bin"
	mkdir -p "$BINS"
	for bin in aegiron aegiron-agent; do
		slices=()
		for t in "${targets[@]}"; do slices+=("$ROOT/agent/target/$t/release/$bin"); done
		lipo -create -output "$BINS/$bin" "${slices[@]}"
	done
fi
for bin in aegiron aegiron-agent; do
	[ -x "$BINS/$bin" ] || { echo "missing $BINS/$bin" >&2; exit 1; }
done

step "Aegiron.app"
rm -rf "$APP"
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources"
cp "$BINS/aegiron" "$BINS/aegiron-agent" "$APP/Contents/MacOS/"
chmod 755 "$APP/Contents/MacOS/"*

(cd "$ROOT/agent" && cargo run --quiet --example make_icon -- --iconset "$WORK/Aegiron.iconset" >/dev/null)
iconutil -c icns -o "$APP/Contents/Resources/Aegiron.icns" "$WORK/Aegiron.iconset"

cat > "$APP/Contents/Info.plist" <<EOF
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
	<key>CFBundleDevelopmentRegion</key>
	<string>th</string>
	<key>CFBundleExecutable</key>
	<string>aegiron</string>
	<key>CFBundleIdentifier</key>
	<string>$BUNDLE_ID</string>
	<key>CFBundleInfoDictionaryVersion</key>
	<string>6.0</string>
	<key>CFBundleName</key>
	<string>Aegiron</string>
	<key>CFBundleDisplayName</key>
	<string>Aegiron</string>
	<key>CFBundlePackageType</key>
	<string>APPL</string>
	<key>CFBundleShortVersionString</key>
	<string>$VERSION</string>
	<key>CFBundleVersion</key>
	<string>$VERSION</string>
	<key>CFBundleIconFile</key>
	<string>Aegiron</string>
	<key>CFBundleURLTypes</key>
	<array>
		<dict>
			<key>CFBundleURLName</key>
			<string>$BUNDLE_ID</string>
			<key>CFBundleURLSchemes</key>
			<array>
				<string>aegiron</string>
				<string>redesk</string>
			</array>
		</dict>
	</array>
	<key>LSMinimumSystemVersion</key>
	<string>11.0</string>
	<key>LSUIElement</key>
	<true/>
	<key>LSApplicationCategoryType</key>
	<string>public.app-category.utilities</string>
	<key>NSHighResolutionCapable</key>
	<true/>
	<key>NSMicrophoneUsageDescription</key>
	<string>Aegiron ใช้ไมโครโฟนเพื่อส่งเสียงระหว่างการควบคุมระยะไกล / Aegiron uses the microphone to send audio during remote sessions.</string>
	<key>NSHumanReadableCopyright</key>
	<string>Aegiron</string>
</dict>
</plist>
EOF
printf 'APPL????' > "$APP/Contents/PkgInfo"
plutil -lint "$APP/Contents/Info.plist" >/dev/null

# Not sandboxed (remote control needs the whole session); outgoing connections and audio input.
cat > "$WORK/entitlements.plist" <<'EOF'
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
	<key>com.apple.security.app-sandbox</key>
	<false/>
	<key>com.apple.security.network.client</key>
	<true/>
	<key>com.apple.security.device.audio-input</key>
	<true/>
</dict>
</plist>
EOF
plutil -lint "$WORK/entitlements.plist" >/dev/null

step "Sign ($([ "$SIGN_ID" = - ] && echo ad-hoc || echo "$SIGN_ID"))"
TS=--timestamp # secure timestamp: required for notarization
[ "$SIGN_ID" = - ] && TS=--timestamp=none
sign() { codesign --force "$TS" --options runtime --entitlements "$WORK/entitlements.plist" --sign "$SIGN_ID" "$@"; }
# Nested code first, then the bundle (which seals Contents/).
sign --identifier co.aegiron.agent "$APP/Contents/MacOS/aegiron-agent"
sign "$APP"
codesign --verify --deep --strict "$APP"
echo "   $(lipo -archs "$APP/Contents/MacOS/aegiron")  $BUNDLE_ID $VERSION"

step "Aegiron-$VERSION.dmg"
STAGE="$WORK/dmg"
mkdir -p "$STAGE"
cp -R "$APP" "$STAGE/"
ln -s /Applications "$STAGE/Applications"
rm -f "$DMG"
hdiutil create -quiet -volname "Aegiron $VERSION" -srcfolder "$STAGE" -fs HFS+ -format UDZO -ov "$DMG"
hdiutil verify -quiet "$DMG"
if [ "$SIGN_ID" != - ]; then codesign --sign "$SIGN_ID" --timestamp "$DMG"; fi
echo "   $DMG"
