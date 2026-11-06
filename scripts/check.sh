#!/bin/sh
# The local gate: formatting, lints, unit tests, the network e2e suite,
# headless UI tests and the machine-checkable parts of the build hygiene
# (toolchain pin, vendor revision, version metadata).
#
# Rust and nsh come from the environment (see docs/BUILD.md). CI runs the same
# commands plus the macOS screenshot/UI and packaging jobs; see
# .github/workflows/ci.yml.
set -eu
cd "$(dirname "$0")/.."

echo "== toolchain"
# The Cargo.lock pins nsh's 0.4.0 crates, so the nsh CLI must be 0.4.x too: a
# 0.5 CLI can change `run/test/package` behaviour under the same lockfile.
nsh --version | grep -q '^nsh 0\.4\.' || {
    echo "nsh 0.4.x is required; found: $(nsh --version 2>&1)" >&2
    exit 1
}
echo "$(nsh --version), $(rustc --version)"

echo "== vendored core"
# --require: a missing LocalSend checkout must fail instead of silently
# skipping the comparison (that was the old "all green" false positive). Set
# LOCALSEND_CORE when packages/core is not a sibling checkout.
./scripts/sync-core.sh --check --require

echo "== version metadata"
# Read the crate version from cargo's metadata (not a sed over Cargo.toml) and
# keep nsh.json, the app test's displayed version and a built Info.plist in
# sync with it.
metadata=$(cargo metadata --locked --offline --no-deps --format-version 1)
cargo_version=$(printf '%s' "$metadata" | python3 -c \
    'import json,sys; print(next(p["version"] for p in json.load(sys.stdin)["packages"] if p["name"] == "localsend-desktop"))')
nsh_json=$(cat nsh.json)
nsh_version=$(printf '%s' "$nsh_json" | python3 -c 'import json,sys; print(json.load(sys.stdin)["version"])')
build=$(printf '%s' "$nsh_json" | python3 -c 'import json,sys; print(json.load(sys.stdin)["build"])')
[ "$cargo_version" = "$nsh_version" ] || {
    echo "nsh.json version ($nsh_version) != Cargo.toml version ($cargo_version)" >&2
    exit 1
}
case "$build" in
    ''|*[!0-9]*)
        echo "nsh.json build must be a positive integer, found '$build'" >&2
        exit 1
        ;;
esac
[ "$build" -ge 1 ] || {
    echo "nsh.json build must be >= 1, found '$build'" >&2
    exit 1
}
# The settings sheet shows this string; bump it together with the version
# (scripts/bump-version.sh does all three).
grep -q "Version $cargo_version" ui-tests/App.test.tsx || {
    echo "ui-tests/App.test.tsx does not contain 'Version $cargo_version' (version bump missed a file)" >&2
    exit 1
}
plist=".nsh/out/LocalSend.app/Contents/Info.plist"
if [ -f "$plist" ] && command -v plutil >/dev/null 2>&1; then
    plist_version=$(plutil -extract CFBundleShortVersionString raw -o - "$plist")
    plist_build=$(plutil -extract CFBundleVersion raw -o - "$plist")
    [ "$plist_version" = "$cargo_version" ] || {
        echo "built Info.plist version ($plist_version) != $cargo_version; re-run nsh package" >&2
        exit 1
    }
    [ "$plist_build" = "$build" ] || {
        echo "built Info.plist build ($plist_build) != nsh.json build ($build); re-run nsh package" >&2
        exit 1
    }
fi
echo "version $cargo_version build $build"

echo "== cargo fmt"
# Only the app: vendor/localsend is a dependency and must stay byte-identical.
# (cargo fmt has no --locked: it never resolves or writes the lockfile.)
cargo fmt --package localsend-desktop --check

echo "== cargo clippy"
# --locked: never resolve dependencies away from the audited Cargo.lock.
# The vendored core compiles as a dependency (its own warnings are upstream's).
cargo clippy --locked --package localsend-desktop --all-targets --no-deps -- -D warnings

echo "== cargo test (unit)"
cargo test --lib --locked

echo "== cargo test (network e2e, ignored by default)"
echo "   binds the LocalSend port (53317) and generates a throwaway RSA identity"
cargo test --lib --locked -- --ignored

echo "== nsh test (headless UI)"
# Offline: the headless run must not bind the LocalSend port or write a
# device identity into the user profile. CI sets NSH_JUNIT to collect the
# report its UI shows.
if [ -n "${NSH_JUNIT:-}" ]; then
    LOCALSEND_OFFLINE=1 nsh test ui-tests/App.test.tsx --junit "$NSH_JUNIT"
else
    LOCALSEND_OFFLINE=1 nsh test ui-tests/App.test.tsx
fi

echo "all checks passed"
echo "(not run here: ui-tests/Screenshots.test.tsx / ui-tests/ScreenshotsZh.test.tsx / packaging / cargo deny — see CI and docs/BUILD.md)"
