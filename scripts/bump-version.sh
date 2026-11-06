#!/bin/sh
# Bump the app version everywhere it is written down, so a release cannot
# forget one of them:
#
#   ./scripts/bump-version.sh 0.2.0
#
# - Cargo.toml (the crate version; `cargo metadata` is the source of truth)
# - nsh.json (`version` and `build`: the latter is incremented, never reset)
# - ui-tests/App.test.tsx (the "Version x.y.z" string the settings sheet shows)
#
# Then run ./scripts/check.sh (it cross-checks all of them) and commit.
set -eu
cd "$(dirname "$0")/.."

if [ $# -ne 1 ]; then
    sed -n '2,12p' "$0" | sed 's/^# \{0,1\}//'
    exit 1
fi
new="$1"
case "$new" in
    [0-9]*.[0-9]*.[0-9]*) ;;
    *)
        echo "expected a MAJOR.MINOR.PATCH version, got '$new'" >&2
        exit 1
        ;;
esac

old=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)
[ -n "$old" ] || {
    echo "could not read the package version from Cargo.toml" >&2
    exit 1
}
[ "$old" != "$new" ] || {
    echo "Cargo.toml is already at $new" >&2
    exit 1
}

python3 - "$old" "$new" <<'PY'
import re
import sys

old, new = sys.argv[1], sys.argv[2]

# Only the [package] version (the first top-level `version = ` line).
with open("Cargo.toml", encoding="utf-8") as file:
    text = file.read()
text, count = re.subn(
    r'(?m)^version = "' + re.escape(old) + '"$',
    f'version = "{new}"',
    text,
    count=1,
)
if count != 1:
    sys.exit(f"Cargo.toml: could not replace version {old}")
with open("Cargo.toml", "w", encoding="utf-8") as file:
    file.write(text)
PY

python3 - "$new" <<'PY'
import json
import sys

version = sys.argv[1]
with open("nsh.json", encoding="utf-8") as file:
    manifest = json.load(file)
manifest["version"] = version
manifest["build"] = int(manifest.get("build", 0)) + 1
with open("nsh.json", "w", encoding="utf-8") as file:
    json.dump(manifest, file, indent=2)
    file.write("\n")
PY

python3 - "$old" "$new" <<'PY'
import sys

old, new = sys.argv[1], sys.argv[2]
path = "ui-tests/App.test.tsx"
with open(path, encoding="utf-8") as file:
    text = file.read()
needle = f"Version {old}"
if needle not in text:
    sys.exit(f"{path}: does not contain {needle!r}")
with open(path, "w", encoding="utf-8") as file:
    file.write(text.replace(needle, f"Version {new}", 1))
PY

build=$(python3 -c 'import json; print(json.load(open("nsh.json"))["build"])')
echo "version $old -> $new (nsh.json build -> $build)"
