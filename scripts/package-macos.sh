#!/bin/sh
# Package the macOS app: `nsh package` builds the bundle, then this script
# strips the local symbols nsh's runner keeps, completes Info.plist (local
# network usage text, no microphone/speech keys), ships the licenses, signs
# (ad hoc by default, Developer ID on request) and verifies the result. The
# byte sizes it reports are measured with `stat`, never hard-coded.
#
#   ./scripts/package-macos.sh                 # ad-hoc signed .nsh/out/LocalSend.app
#   ./scripts/package-macos.sh --sign "Developer ID Application: Name (TEAMID)"
#   ./scripts/package-macos.sh --sign "Developer ID Application: Name (TEAMID)" --notarize
#
# --notarize needs notarytool credentials: either NOTARYTOOL_PROFILE (a
# `xcrun notarytool store-credentials` profile) or APPLE_ID + APPLE_TEAM_ID +
# APPLE_APP_PASSWORD. Without them notarization is skipped with a warning — the
# script never pretends a release is notarized.
#
# nsh package's own `--sign` is for mobile device signing; on macOS this script
# re-signs the bundle with `codesign` after changing it.
set -eu
cd "$(dirname "$0")/.."

app_name="LocalSend"
out=".nsh/out"
app="$out/$app_name.app"
identity=""
notarize=0
app_version=""
build=""

usage() {
    cat <<'EOF'
Package the macOS app.

Usage:
  ./scripts/package-macos.sh [options]

Options:
  --sign <identity>     Developer ID Application identity for codesign
                        (default: ad hoc)
  --notarize            Submit to Apple's notary service and staple the ticket
                        (needs NOTARYTOOL_PROFILE or APPLE_ID/APPLE_TEAM_ID/
                        APPLE_APP_PASSWORD; skipped with a warning otherwise)
  --app-version <v>     Version override for nsh package (default: nsh.json)
  --build <n>           Build number override for nsh package (default: nsh.json)
  -h, --help            Show this help
EOF
    exit "${1:-0}"
}

die() {
    echo "$*" >&2
    exit 1
}

while [ $# -gt 0 ]; do
    case "$1" in
        --sign)
            [ $# -ge 2 ] || die "--sign needs an identity"
            identity="$2"
            shift 2
            ;;
        --notarize)
            notarize=1
            shift
            ;;
        --app-version)
            [ $# -ge 2 ] || die "--app-version needs a version"
            app_version="$2"
            shift 2
            ;;
        --build)
            [ $# -ge 2 ] || die "--build needs a number"
            build="$2"
            shift 2
            ;;
        -h|--help)
            usage 0
            ;;
        *)
            die "unknown option: $1 (see --help)"
            ;;
    esac
done

case "$app_version" in
    ''|*[!0-9.]*)
        [ -z "$app_version" ] || die "invalid --app-version: $app_version"
        ;;
esac
case "$build" in
    ''|*[!0-9]*)
        [ -z "$build" ] || die "invalid --build (digits only): $build"
        ;;
esac

# Build and modify the bundle in a temporary directory, then swap it in: a
# failed strip/sign leaves the previous .app (and .nsh/out) intact.
stage=$(mktemp -d "${TMPDIR:-/tmp}/localsend-package.XXXXXX")
trap 'rm -rf "$stage"' EXIT INT TERM HUP

set -- --target macos --out "$stage"
if [ -n "$app_version" ]; then
    set -- "$@" --app-version "$app_version"
fi
if [ -n "$build" ]; then
    set -- "$@" --build "$build"
fi
nsh package "$@"

staged="$stage/$app_name.app"
binary="$staged/Contents/MacOS/$app_name"
[ -d "$staged" ] || die "nsh package did not produce $staged"
[ -f "$binary" ] || die "missing app binary: $binary"
plist="$staged/Contents/Info.plist"
[ -f "$plist" ] || die "missing Info.plist: $plist"

echo "== strip local symbols"
before=$(stat -f%z "$binary")
strip -x "$binary"
after=$(stat -f%z "$binary")

echo "== Info.plist"
plist_buddy=/usr/libexec/PlistBuddy
[ -x "$plist_buddy" ] || die "PlistBuddy is required (macOS)"

# Adds a string key, replacing an existing value. `Set` fails when the key
# does not exist, so try it first and fall back to `Add`.
plist_set_string() {
    key="$1"
    value="$2"
    "$plist_buddy" -c "Set :$key $value" "$plist" >/dev/null 2>&1 \
        || "$plist_buddy" -c "Add :$key string $value" "$plist" >/dev/null
}

# macOS 15+ asks for local-network access on the first multicast sent; without
# this string the prompt has no app-specific explanation.
plist_set_string NSLocalNetworkUsageDescription \
    "LocalSend uses your local network to find nearby devices and transfer files."
plist_set_string LSApplicationCategoryType public.app-category.utilities
plist_set_string NSHumanReadableCopyright \
    "Copyright © 2026 LocalSend Native contributors. MIT License."

# The HTTP endpoint and the LocalSend service name. Discovery itself is UDP
# multicast (224.0.0.167:53317), but declaring the services keeps the Bonjour
# advertisement the LocalSend ecosystem expects and documents the port use.
"$plist_buddy" -c "Delete :NSBonjourServices" "$plist" >/dev/null 2>&1 || true
"$plist_buddy" -c "Add :NSBonjourServices array" "$plist" >/dev/null
"$plist_buddy" -c "Add :NSBonjourServices:0 string _http._tcp" "$plist" >/dev/null
"$plist_buddy" -c "Add :NSBonjourServices:1 string _localsend._tcp" "$plist" >/dev/null

# These come from nsh's default template; the app never records audio or
# converts speech, and a privacy prompt for them would be misleading.
"$plist_buddy" -c "Delete :NSMicrophoneUsageDescription" "$plist" >/dev/null 2>&1 || true
"$plist_buddy" -c "Delete :NSSpeechRecognitionUsageDescription" "$plist" >/dev/null 2>&1 || true

echo "== licenses"
resources="$staged/Contents/Resources"
mkdir -p "$resources"
cp LICENSE "$resources/LICENSE"
cp NOTICE "$resources/NOTICE"
# The core's Apache-2.0 text, kept separate from the app's MIT license.
cp vendor/localsend/LICENSE "$resources/LICENSE-LocalSend-core"
# The bundled Material Icons font's CC BY 4.0 license and attribution (see
# NOTICE; the font arrives with that license from the Flutter SDK).
cp assets/MaterialIcons_LICENSE.txt "$resources/LICENSE-MaterialIcons"
# Readable by every user of the machine, like any other bundled resource.
chmod 0644 "$resources/LICENSE" "$resources/NOTICE" "$resources/LICENSE-LocalSend-core" \
    "$resources/LICENSE-MaterialIcons"
for file in LICENSE NOTICE LICENSE-LocalSend-core LICENSE-MaterialIcons; do
    [ -f "$resources/$file" ] || die "missing bundled license: $resources/$file"
done

echo "== sign"
if [ -n "$identity" ]; then
    codesign --force --options runtime --timestamp --sign "$identity" "$staged" >/dev/null \
        || die "codesign with '$identity' failed"
    signature="Developer ID ($identity), hardened runtime"
else
    codesign --force --sign - --timestamp=none "$staged" >/dev/null 2>&1 \
        || die "ad-hoc codesigning failed"
    signature="ad-hoc"
fi
# A bad signature must fail the release, not print a warning.
codesign --verify --strict "$staged" 2>/dev/null \
    || die "codesign --verify failed for $staged"

notarized="no"
if [ "$notarize" -eq 1 ]; then
    echo "== notarize"
    if [ -z "$identity" ]; then
        echo "WARNING: --notarize needs --sign \"Developer ID Application: ...\"; notarization skipped." >&2
    elif ! command -v xcrun >/dev/null 2>&1; then
        echo "WARNING: xcrun not found; notarization skipped." >&2
    elif [ -n "${NOTARYTOOL_PROFILE:-}" ]; then
        zip="$stage/$app_name.zip"
        ditto -c -k --keepParent "$staged" "$zip"
        xcrun notarytool submit "$zip" --keychain-profile "$NOTARYTOOL_PROFILE" --wait \
            || die "notarytool submit failed"
        xcrun stapler staple "$staged" || die "stapler failed"
        notarized="yes"
    elif [ -n "${APPLE_ID:-}" ] && [ -n "${APPLE_TEAM_ID:-}" ] \
        && [ -n "${APPLE_APP_PASSWORD:-}${APPLE_PASSWORD:-}" ]; then
        password="${APPLE_APP_PASSWORD:-${APPLE_PASSWORD:-}}"
        zip="$stage/$app_name.zip"
        ditto -c -k --keepParent "$staged" "$zip"
        xcrun notarytool submit "$zip" \
            --apple-id "$APPLE_ID" --team-id "$APPLE_TEAM_ID" --password "$password" --wait \
            || die "notarytool submit failed"
        xcrun stapler staple "$staged" || die "stapler failed"
        notarized="yes"
    else
        echo "WARNING: notarization skipped: set NOTARYTOOL_PROFILE or" >&2
        echo "         APPLE_ID + APPLE_TEAM_ID + APPLE_APP_PASSWORD (BLOCKED-CREDENTIALS)." >&2
        echo "         The .app is signed but not notarized; Gatekeeper will warn." >&2
    fi
fi

echo "== install"
mkdir -p "$out"
old="$out/$app_name.app.old.$$"
rm -rf "$old"
if [ -d "$app" ]; then
    mv "$app" "$old"
fi
if ! mv "$staged" "$app"; then
    rm -rf "$app"
    if [ -d "$old" ]; then
        mv "$old" "$app"
    fi
    die "could not replace $app; the previous bundle was restored"
fi
rm -rf "$old"

codesign --verify --strict "$app" 2>/dev/null || die "the installed bundle does not verify"
plutil -extract NSLocalNetworkUsageDescription raw -o - "$app/Contents/Info.plist" >/dev/null \
    || die "NSLocalNetworkUsageDescription is missing from the installed Info.plist"

echo "== result"
saved=$((before - after))
mib=$(awk -v bytes="$saved" 'BEGIN { printf "%.1f", bytes / 1048576 }')
printf '%s\n' "$app"
printf '  binary: %s -> %s bytes (%s MiB stripped)\n' "$before" "$after" "$mib"
printf '  signature: %s; notarized: %s\n' "$signature" "$notarized"
printf '  resources: %s\n' "$(ls "$app/Contents/Resources" | tr '\n' ' ')"
