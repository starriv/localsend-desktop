#!/bin/sh
# Experimental ARM64 APK. Requires nsh 0.4, JDK 17+, Android platform 35,
# build-tools 35.0.1, an NDK and Rust's aarch64-linux-android target.
# No release keystore is used: nsh signs with the host's debug key.
set -eu
cd "$(dirname "$0")/.."

nsh --version | grep -q '^nsh 0\.4\.'
: "${ANDROID_HOME:?set ANDROID_HOME to the Android SDK}"
tools="$ANDROID_HOME/build-tools/35.0.1"
test -x "$tools/apksigner"
test -x "$tools/aapt"

version=$(python3 - <<'PY'
import json, pathlib, re
version = re.search(r'(?m)^version = "([^"]+)"$', pathlib.Path("Cargo.toml").read_text()).group(1)
with open("nsh.json") as file:
    manifest = json.load(file)
assert manifest["version"] == version, "Cargo.toml and nsh.json versions differ"
print(version)
PY
)
stage=$(mktemp -d "${TMPDIR:-/tmp}/localsend-android.XXXXXX")
trap 'rm -rf "$stage"' EXIT
trap 'exit 1' INT TERM HUP
nsh package --target android --abi arm64-v8a --out "$stage"
apk="$stage/LocalSend.apk"
test -s "$apk"
"$tools/apksigner" verify --verbose --print-certs "$apk" > "$stage/signature.txt"
"$tools/aapt" dump badging "$apk" > "$stage/apk-info.txt"

# Check the actual packaged manifest and ABI, not just the output filename.
python3 - "$stage" "$version" <<'PY'
import json, pathlib, re, sys, zipfile
stage, version = pathlib.Path(sys.argv[1]), sys.argv[2]
metadata = json.loads(pathlib.Path("nsh.json").read_text())
badging = (stage / "apk-info.txt").read_text()
package = dict(re.findall(r"([A-Za-z]+)='([^']*)'", badging.splitlines()[0]))
assert package["name"] == metadata["id"], package
assert package["versionName"] == version, package
assert package["versionCode"] == str(metadata["build"]), package
with zipfile.ZipFile(stage / "LocalSend.apk") as apk:
    libraries = [n for n in apk.namelist() if n.startswith("lib/") and n.endswith(".so")]
    assert "lib/arm64-v8a/libnsh.so" in libraries, libraries
    assert all(n.startswith("lib/arm64-v8a/") for n in libraries), libraries
print(f"Verified {metadata['id']} {version} build {metadata['build']}, ARM64 APK")
PY

out=dist/android
mkdir -p "$out"
asset="LocalSend-$version-android-arm64-experimental.apk"
cp "$apk" "$out/$asset"
cp "$stage/apk-info.txt" "$out/apk-info.txt"
cp "$stage/signature.txt" "$out/signature.txt"
python3 - "$out" "$asset" <<'PY'
import hashlib, pathlib, sys
folder, name = pathlib.Path(sys.argv[1]), sys.argv[2]
with (folder / name).open("rb") as file:
    digest = hashlib.sha256()
    for chunk in iter(lambda: file.read(1024 * 1024), b""):
        digest.update(chunk)
    sha = digest.hexdigest()
(folder / (name + ".sha256")).write_text(f"{sha}  {name}\n")
PY
printf '%s\n' "$out/$asset"
