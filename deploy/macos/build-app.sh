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
# deployment with MDM/PPPC profiles, sign with a Developer ID and notarize instead (see README):
#
#   AEGIRON_SIGN_ID          codesign identity in the keychain, e.g.
#                            "Developer ID Application: Example Co., Ltd. (ABCDE12345)" or its SHA-1
#   AEGIRON_NOTARY_PROFILE   notarytool keychain profile (xcrun notarytool store-credentials <name>);
#                            when set, the app and the dmg are notarized and stapled
#   AEGIRON_NOTARY_KEYCHAIN  keychain holding that profile (optional, e.g. on a CI machine)
#
# With a Developer ID the build also writes dist/Aegiron-PPPC.mobileconfig (make-pppc.sh).
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
VERSION="$(tr -d '[:space:]' < "$ROOT/VERSION")"
DIST="$ROOT/dist"
APP="$DIST/Aegiron.app"
DMG="$DIST/Aegiron-$VERSION.dmg"
BUNDLE_ID="co.aegiron.app"
SIGN_ID="${AEGIRON_SIGN_ID:--}" # "-" = ad-hoc
NOTARY="${AEGIRON_NOTARY_PROFILE:-}"
NOTARY_KEYCHAIN="${AEGIRON_NOTARY_KEYCHAIN:-}"

step() { printf '\n\033[1m==> %s\033[0m\n' "$*"; }
die() { printf 'error: %s\n' "$*" >&2; exit 1; }

BINS=""
while [ $# -gt 0 ]; do
	case "$1" in
		--bins) BINS="$(cd "$2" && pwd)"; shift 2 ;;
		*) echo "unknown option: $1" >&2; exit 2 ;;
	esac
done

# Signing and notarization settings are checked before the (slow) build so mistakes fail fast.
if [ "$SIGN_ID" != - ]; then
	# Valid code-signing identities only: '  1) <SHA-1> "Developer ID Application: Name (TEAMID)"'.
	identities="$(security find-identity -v -p codesigning 2>/dev/null | grep -E '^ *[0-9]+\) [0-9A-F]{40} "' || true)"
	if ! printf '%s\n' "$identities" | grep -Fq -- "$SIGN_ID"; then
		printf '%s\n' "${identities:-(no valid code-signing identities in the keychain)}" | sed 's/^/   /' >&2
		die "signing identity \"$SIGN_ID\" is not in the keychain (list: security find-identity -v -p codesigning)"
	fi
	case "$SIGN_ID" in
		"Developer ID Application:"*) ;;
		*) printf '%s\n' "$identities" | grep -F -- "$SIGN_ID" | grep -q '"Developer ID Application:' \
			|| echo "warning: \"$SIGN_ID\" is not a Developer ID Application identity: Gatekeeper and notarization will reject it" >&2 ;;
	esac
fi
notary_args=()
if [ -n "$NOTARY" ]; then
	[ "$SIGN_ID" != - ] || die "AEGIRON_NOTARY_PROFILE is set but the build is ad-hoc signed: notarization needs AEGIRON_SIGN_ID=\"Developer ID Application: ...\""
	xcrun --find notarytool >/dev/null 2>&1 || die "xcrun notarytool not found (install Xcode or the Command Line Tools, Xcode 13+)"
	xcrun --find stapler >/dev/null 2>&1 || die "xcrun stapler not found (install Xcode or the Command Line Tools)"
	notary_args=(--keychain-profile "$NOTARY")
	if [ -n "$NOTARY_KEYCHAIN" ]; then
		[ -f "$NOTARY_KEYCHAIN" ] || die "AEGIRON_NOTARY_KEYCHAIN: no such keychain file: $NOTARY_KEYCHAIN"
		notary_args+=(--keychain "$NOTARY_KEYCHAIN")
	fi
	# Fails fast on a missing/misspelled profile or revoked credentials (contacts Apple).
	if ! out="$(xcrun notarytool history "${notary_args[@]}" --output-format json 2>&1)"; then
		printf '%s\n' "$out" | sed 's/^/   /' >&2
		die "notarytool cannot use keychain profile \"$NOTARY\" (create it with: xcrun notarytool store-credentials \"$NOTARY\" --apple-id <id> --team-id <TEAMID>)"
	fi
fi

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
if [ "$SIGN_ID" != - ]; then
	echo "   $(codesign -d -r- "$APP" 2>&1 | sed -n 's/^designated => //p')"
fi

# notarize FILE: submit to Apple's notary service and wait; prints the log and fails if rejected.
notarize() {
	local file="$1" json id status
	json="$WORK/notary-$(basename "$file").json"
	echo "   submitting $(basename "$file") (this usually takes a few minutes)"
	xcrun notarytool submit "$file" "${notary_args[@]}" --wait --timeout 1h --output-format json > "$json" || true
	id="$(plutil -extract id raw -o - "$json" 2>/dev/null || true)"
	status="$(plutil -extract status raw -o - "$json" 2>/dev/null || true)"
	if [ "$status" != Accepted ]; then
		sed 's/^/   /' "$json" >&2
		[ -z "$id" ] || xcrun notarytool log "$id" "${notary_args[@]}" >&2 || true
		die "notarization of $(basename "$file") failed (status: ${status:-unknown})"
	fi
	echo "   accepted ($id)"
}

if [ -n "$NOTARY" ]; then
	step "Notarize Aegiron.app"
	ditto -c -k --keepParent "$APP" "$WORK/Aegiron.zip"
	notarize "$WORK/Aegiron.zip"
	xcrun stapler staple -q "$APP"
	xcrun stapler validate -q "$APP"
	spctl --assess --type execute -vv "$APP" 2>&1 | sed 's/^/   /'
fi

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

if [ -n "$NOTARY" ]; then
	step "Notarize Aegiron-$VERSION.dmg"
	notarize "$DMG"
	xcrun stapler staple -q "$DMG"
	xcrun stapler validate -q "$DMG"
	spctl --assess --type open --context context:primary-signature -vv "$DMG" 2>&1 | sed 's/^/   /'
fi

if [ "$SIGN_ID" != - ]; then
	step "PPPC profile for MDM"
	"$ROOT/deploy/macos/make-pppc.sh" --app "$APP" --out "$DIST/Aegiron-PPPC.mobileconfig"
elif [ -z "$NOTARY" ]; then
	echo "   ad-hoc signed: Gatekeeper warns on first launch; no PPPC profile (needs a Developer ID)"
fi
