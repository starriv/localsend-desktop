#!/bin/sh
# Export Apple's rendering of the vector Icon Composer source for non-Apple
# platforms. Native macOS packaging compiles the .icon directly using actool.
set -eu
cd "$(dirname "$0")/.."
python3 scripts/draw-icon.py
icon_tool="${ICON_COMPOSER_TOOL:-$(xcode-select -p)/../Applications/Icon Composer.app/Contents/Executables/ictool}"
[ -x "$icon_tool" ] || { echo 'Icon Composer from Xcode 26 or later is required' >&2; exit 1; }
"$icon_tool" design/AppIcon.icon --export-image --output-file assets/icon.png \
    --platform macOS --rendition Default --width 1024 --height 1024 --scale 1
# Preserve the rendered colors when a non-Apple platform reads this fallback.
sips --matchTo '/System/Library/ColorSync/Profiles/sRGB Profile.icc' assets/icon.png >/dev/null
