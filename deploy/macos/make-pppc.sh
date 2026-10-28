#!/bin/bash
# Writes a PPPC (Privacy Preferences Policy Control) configuration profile for Aegiron.app, to
# be deployed by an MDM server (Jamf, Intune, Kandji, Mosyle, ...).
#
#   deploy/macos/make-pppc.sh --team-id ABCDE12345            # from your Apple Developer Team ID
#   deploy/macos/make-pppc.sh --app dist/Aegiron.app           # from a Developer ID signed build
#   deploy/macos/make-pppc.sh                                  # template with a __TEAM_ID__ placeholder
#   options: --out FILE (default dist/Aegiron-PPPC.mobileconfig)  --org "Example Co., Ltd."
#   AEGIRON_TEAM_ID may be used instead of --team-id.
#
# What it grants (bundle ID co.aegiron.app, pinned to your Team ID by a code requirement):
#   Accessibility  Allow                                 mouse and keyboard from the controller
#   PostEvent      Allow                                 sending synthetic input events
#   ScreenCapture  AllowStandardUserToSetSystemService   NOT a silent grant: macOS never lets MDM
#                  allow screen recording. This only lets a standard (non-admin) user switch it
#                  on in System Settings > Privacy & Security > Screen & System Audio Recording.
#
# macOS honours TCC payloads only from an MDM enrolled with user approval (or via ADE); a
# profile installed by double-clicking it is ignored for these settings. An ad-hoc signed app
# has no Team ID and cannot be targeted.
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
BUNDLE_ID="co.aegiron.app"
TEAM_ID="${AEGIRON_TEAM_ID:-}"
APP=""
OUT="$ROOT/dist/Aegiron-PPPC.mobileconfig"
ORG="Aegiron"

die() { printf 'error: %s\n' "$*" >&2; exit 1; }

while [ $# -gt 0 ]; do
	case "$1" in
		--team-id | --app | --out | --org) [ $# -ge 2 ] || die "$1 needs a value" ;;
	esac
	case "$1" in
		--team-id) TEAM_ID="${2:-}"; shift 2 ;;
		--app) APP="${2:-}"; shift 2 ;;
		--out) OUT="${2:-}"; shift 2 ;;
		--org) ORG="${2:-}"; shift 2 ;;
		-h | --help) sed -n '2,20p' "$0" | sed 's/^# \{0,1\}//'; exit 0 ;;
		*) die "unknown option: $1 (see --help)" ;;
	esac
done

xml_escape() { sed -e 's/&/\&amp;/g' -e 's/</\&lt;/g' -e 's/>/\&gt;/g' -e 's/"/\&quot;/g'; }

if [ -n "$APP" ]; then
	[ -d "$APP" ] || die "--app: no such app: $APP"
	id="$(/usr/libexec/PlistBuddy -c 'Print :CFBundleIdentifier' "$APP/Contents/Info.plist" 2>/dev/null || true)"
	[ "$id" = "$BUNDLE_ID" ] || die "--app: bundle identifier is \"$id\", expected $BUNDLE_ID"
	REQ="$(codesign -d -r- "$APP" 2>&1 | sed -n 's/^designated => //p')"
	team="$(codesign -dv "$APP" 2>&1 | sed -n 's/^TeamIdentifier=//p')"
	case "$team" in "" | "not set") die "--app: $APP is not signed with a Developer ID (no Team ID): PPPC cannot target it" ;; esac
	[ -z "$TEAM_ID" ] || [ "$TEAM_ID" = "$team" ] || die "--team-id $TEAM_ID does not match the app's Team ID $team"
	TEAM_ID="$team"
	case "$REQ" in *"anchor apple generic"*) ;; *) die "--app: unexpected designated requirement: $REQ" ;; esac
	TEMPLATE=""
elif [ -n "$TEAM_ID" ]; then
	TEMPLATE=""
else
	TEAM_ID="__TEAM_ID__"
	TEMPLATE=1
fi
if [ -z "$TEMPLATE" ]; then
	printf '%s' "$TEAM_ID" | grep -Eq '^[A-Z0-9]{10}$' || die "Team ID must be 10 characters A-Z/0-9 (got \"$TEAM_ID\")"
fi
# Default designated requirement of a Developer ID Application signature.
: "${REQ:=identifier \"$BUNDLE_ID\" and anchor apple generic and certificate 1[field.1.2.840.113635.100.6.2.6] /* exists */ and certificate leaf[field.1.2.840.113635.100.6.1.13] /* exists */ and certificate leaf[subject.OU] = \"$TEAM_ID\"}"

REQ_XML="$(printf '%s' "$REQ" | xml_escape)"
ORG_XML="$(printf '%s' "$ORG" | xml_escape)"
uuid() { uuidgen | tr '[:lower:]' '[:upper:]'; }

entry() { # service authorization comment
	cat <<EOF
			<key>$1</key>
			<array>
				<dict>
					<key>Identifier</key>
					<string>$BUNDLE_ID</string>
					<key>IdentifierType</key>
					<string>bundleID</string>
					<key>CodeRequirement</key>
					<string>$REQ_XML</string>
					<key>StaticCode</key>
					<false/>
					<key>Authorization</key>
					<string>$2</string>
					<key>Comment</key>
					<string>$3</string>
				</dict>
			</array>
EOF
}

mkdir -p "$(dirname "$OUT")"
cat > "$OUT" <<EOF
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
	<key>PayloadContent</key>
	<array>
		<dict>
			<key>PayloadType</key>
			<string>com.apple.TCC.configuration-profile-policy</string>
			<key>PayloadIdentifier</key>
			<string>$BUNDLE_ID.pppc.tcc</string>
			<key>PayloadUUID</key>
			<string>$(uuid)</string>
			<key>PayloadVersion</key>
			<integer>1</integer>
			<key>PayloadDisplayName</key>
			<string>Aegiron privacy permissions</string>
			<key>Services</key>
			<dict>
$(entry Accessibility Allow "Aegiron: remote mouse and keyboard")
$(entry PostEvent Allow "Aegiron: synthetic input events")
$(entry ScreenCapture AllowStandardUserToSetSystemService "Aegiron: lets a standard user turn on Screen Recording (MDM cannot grant it)")
			</dict>
		</dict>
	</array>
	<key>PayloadDisplayName</key>
	<string>Aegiron — สิทธิ์ของระบบ (PPPC)</string>
	<key>PayloadDescription</key>
	<string>อนุญาต Accessibility ให้ Aegiron และให้ผู้ใช้ทั่วไปเปิด Screen Recording เองได้ / Allows Accessibility for Aegiron and lets standard users enable Screen Recording.</string>
	<key>PayloadIdentifier</key>
	<string>$BUNDLE_ID.pppc</string>
	<key>PayloadOrganization</key>
	<string>$ORG_XML</string>
	<key>PayloadScope</key>
	<string>System</string>
	<key>PayloadType</key>
	<string>Configuration</string>
	<key>PayloadUUID</key>
	<string>$(uuid)</string>
	<key>PayloadVersion</key>
	<integer>1</integer>
</dict>
</plist>
EOF
plutil -lint "$OUT" >/dev/null || die "generated profile is not a valid plist"
echo "   $OUT"
echo "   $BUNDLE_ID, Team ID $TEAM_ID"
if [ -n "$TEMPLATE" ]; then
	echo "   TEMPLATE: replace __TEAM_ID__ with your Apple Developer Team ID before uploading to MDM" >&2
	echo "   (or run again with --team-id <TEAMID> / --app <signed Aegiron.app>)" >&2
fi
