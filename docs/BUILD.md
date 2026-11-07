# Build, run, test

## Standalone

The core is vendored in `vendor/localsend`, so this directory is the whole
project: no Flutter, no Dart, no monorepo checkout. `cargo build --locked
--release`, then `./scripts/package-macos.sh`.

The app statically links the core; production artifacts need only the `.app`
and macOS frameworks. For a hermetic build run `cargo vendor` once and build
with `--offline --locked`; the committed `Cargo.lock` is used by every gate.

To update the core, copy a `packages/core` checkout
(`./scripts/sync-core.sh /path/to/packages/core`) or fetch a pinned revision
(`./scripts/sync-core.sh --from-git https://github.com/localsend/localsend <rev>`),
bump the revision in `vendor/localsend/UPSTREAM.md`, and run `./scripts/check.sh`.
`sync-core.sh --check` searches `$LOCALSEND_CORE`, `../localsend/packages/core`
and `../packages/core`; `--require` turns "no checkout to compare with" into a
failure (CI uses it), and the recorded `UPSTREAM.md` revision is cross-checked
against the source repository's HEAD.

## Requirements

- macOS 14+ (Apple Silicon) for the reference platform; the same crate also
  builds for Windows and Linux with nsh's platform crates.
- Rust 1.97.1 (`rust-toolchain.toml` pins it; `rustup` installs it on first use;
  `Cargo.toml` declares the same `rust-version`).
- `nsh` **0.4.x** from [nsh.dev](https://nsh.dev/docs/installation):
  `curl -fsSL https://nsh.dev/install.sh | sh` (macOS/Linux), or
  `brew install longbridge/tap/nsh` — then assert `nsh --version` starts with
  `nsh 0.4.` (`scripts/check.sh` fails otherwise; the installer tracks the
  latest release, so pin/verify it in CI).
- macOS: Xcode command line tools (for `strip`, `codesign`, `PlistBuddy`).

## Build loop

```sh
cargo check                   # fast type check loop
cargo test --lib              # unit tests
LOCALSEND_OFFLINE=1 nsh test ui-tests/App.test.tsx          # headless UI tests
LOCALSEND_DEMO=1 nsh test ui-tests/Screenshots.test.tsx --target macos  # screenshots (AppKit)
nsh run                       # debug run in a window, hot restart on Rust edits
nsh build                     # build the app (runnable artifact)
nsh package --target macos    # .app bundle
./scripts/check.sh            # the full local gate (see below)
```

The first `nsh` build compiles nsh's core (~60 crates) and the LocalSend core;
expect a few minutes. Incremental rebuilds are seconds.

The UI test files live in `ui-tests/` (`*.test.tsx`, separated from the Rust
unit tests): a bare `nsh test` from the repository root runs every test file
below the app root, plus the crate's `cargo test` suites, while the commands
above name one file for a focused run. The app entry is the repository root,
one level above the test files, so each test opens it with `open("..")`.

## Running against a real LocalSend peer

1. Install stock LocalSend on a phone/Mac and keep both on the same Wi-Fi/LAN.
2. `nsh run` the client; it announces on multicast and lists peers within a
   few seconds.
3. Send a file from LocalSend to the native client: accept it in the incoming
   sheet; the file lands in `~/Downloads` (or the folder in settings).
4. Send from the native client to LocalSend: stage files, select the device,
   press Send (or ⌘↩).
5. Check the reverse direction after toggling `Encrypt transfers` off (legacy
   HTTP mode).

Two instances on one Mac also see each other (multicast loopback is on), which
is the fastest way to test without a phone.

## Screenshots and UI review

The app seeds sample peers/transfers when `LOCALSEND_DEMO=1` is set, so design
review does not need a second device:

```sh
LOCALSEND_DEMO=1 nsh test ui-tests/Screenshots.test.tsx --target macos   # screenshots/*.png
LOCALSEND_DEMO=incoming nsh test ui-tests/Screenshots.test.tsx --target macos --filter "overlay"
LOCALSEND_DEMO=pin      nsh test ui-tests/Screenshots.test.tsx --target macos --filter "overlay"
```

`app.screenshot(path)` writes relative to the repository root; the `screenshots/`
directory is kept in git via `.gitkeep` (its contents are ignored). The
per-screen captures stay local for design review; the only committed screenshot
is the README montage below.

`README.md`'s hero image, `docs/screenshots/overview.png`, is a 3 × 3 montage of
the app in Chinese: the README is Chinese, so the montage uses
`ui-tests/ScreenshotsZh.test.tsx` (same screens and ids, Chinese labels) while
`ui-tests/Screenshots.test.tsx` above keeps the English label-based assertions. Capture
the nine tiles into `screenshots/zh/` (ignored), then compose the montage:

```sh
mkdir -p screenshots/zh
LOCALSEND_DEMO=1        nsh test ui-tests/ScreenshotsZh.test.tsx --target macos
LOCALSEND_DEMO=incoming nsh test ui-tests/ScreenshotsZh.test.tsx --target macos --filter overlay
LOCALSEND_DEMO=pin      nsh test ui-tests/ScreenshotsZh.test.tsx --target macos --filter overlay
python3 scripts/montage-screenshots.py   # needs Pillow; docs-only tool
```

The script reproduces the committed geometry — 2512 × 1711 canvas, 802 × 535
tiles, 31 px margin, 22 px gap, 15 px corner radius — and the tile order: send,
receive, history, incoming, pin, transfer-sheet, settings, send-dark,
history-dark. Only the montage is committed; its source tiles stay in
`screenshots/`. There is no nsh capture for the menu bar item, so it does not
appear in the montage.

Headless UI tests (fast, no platform; offline so they do not bind the
LocalSend port or write a device identity):

```sh
LOCALSEND_OFFLINE=1 nsh test ui-tests/App.test.tsx
```

Screenshots require `nsh test --target macos` (a real platform): headless runs
cannot draw, and the suite skips the captures there.

## Packaging

```sh
./scripts/package-macos.sh     # nsh package + strip + ad-hoc re-sign: 41.0 MB
nsh package --target macos     # plain nsh output: ~53 MB, symbols kept

./scripts/package-macos.sh --sign "Developer ID Application: …" --notarize
```

The release workflow (`.github/workflows/release.yml`) runs this script on a
macOS runner, wraps the `.app` into a dmg (with an `/Applications` symlink) and
a zip, and uploads both next to the Windows x64 build to the GitHub Release
named by the version in `Cargo.toml`.

`scripts/package-macos.sh` exists because `nsh package` has no strip/sign flags:
it strips the runner's local symbols (53 MB → 41.0 MB), re-signs ad hoc, adds
`NSLocalNetworkUsageDescription`/`NSBonjourServices` (and removes unrelated
microphone/speech keys) with `PlistBuddy`, and copies the app's, nsh's and the
vendored core's license files into `Contents/Resources/`. `--sign` and
`--notarize` are optional: without credentials the script skips them and says
so — it never claims a signature/notarization it did not perform. A failed
`codesign --verify` makes the script exit non-zero.

## Checks

```sh
./scripts/check.sh
```

runs, with `--locked` throughout: nsh 0.4 assertion, vendor revision check
(`--require`), version/build consistency (`Cargo.toml`, `nsh.json`,
`ui-tests/App.test.tsx`, built `Info.plist`), `cargo fmt --check`, `cargo clippy
--all-targets --no-deps -- -D warnings`, unit tests, the 7 network e2e tests
(`--ignored`) and the headless UI suite. It is the local gate: run it when you
mean to test, it is not part of the default edit loop.

`.github/workflows/ci.yml` runs formatting and lints on every push and pull
request. `.github/workflows/release.yml` builds once and attaches the macOS
`.app` (zip and dmg) and the Windows x64 build (`nsh package --target
windows`) to a GitHub Release — portable zip and an NSIS installer built from
`scripts/windows-installer.nsi` — each with a `.sha256`; dispatch it from the
Actions tab or push a `v*` tag.

## Updates

The app updates itself from GitHub releases (`src/update/`, `src/model/updater.rs`).
The release workflow's last job, `manifest`, runs once every installer is on the
release and uploads:

- `latest.json`: the version, the release page and, per platform
  (`macos-aarch64`, `windows-x86_64`), the installer's URL, size and SHA-256
  (from the `.sha256` assets);
- `latest.json.sig`: the raw 64-byte Ed25519 signature of `latest.json`'s
  exact bytes.

The app fetches both from `releases/latest/download/` (no API call), verifies
the signature against `PUBLIC_KEY` in `src/update/mod.rs`, then downloads the
installer and checks its size and SHA-256. A release without `latest.json`
(still building, or the job failed) is not offered.

**Signing key.** The private key is the `UPDATE_SIGNING_KEY` repository secret
(PEM). The job fails when the secret is missing or is not the key the app
embeds. To rotate it:

```sh
openssl genpkey -algorithm ed25519 -out update-signing-key.pem
openssl pkey -in update-signing-key.pem -pubout -outform DER | tail -c 32 | xxd -p -c 32
gh secret set UPDATE_SIGNING_KEY < update-signing-key.pem
```

then put the printed 32 bytes in `PUBLIC_KEY` and release that build signed with
the **old** key: installed copies only trust the key they were built with.
Keep the PEM in a password manager; losing it means users must reinstall by hand.

**Installing.** macOS unpacks the `.zip`, checks its bundle id, version and
`codesign --verify`, and a detached script swaps the `.app` once the app has
quit. Windows runs the installer with `/S /UPDATE` (it waits for
`LocalSend.exe` to exit). "Restart to update" relaunches the app afterwards;
an ordinary quit installs the ready update without reopening it (Windows adds
`/NORUN`). Native Windows closes, including Alt+F4, use the same quit flow.
A verified download left by an earlier launch is reused after checking its
size and SHA-256 again. Copies that cannot replace
themselves (a `cargo run` build, the Windows portable zip, a translocated or
read-only bundle) link to the release page instead.

**Testing locally.** `LOCALSEND_UPDATE_MANIFEST=http://127.0.0.1:8765/latest.json`
points the updater at a local server (plain HTTP is allowed only with it);
the manifest still has to be signed with the real key.
`LOCALSEND_NO_UPDATE=1` turns the updater off (as do `LOCALSEND_DEMO` and
`LOCALSEND_OFFLINE`).

The scripts use the `cargo` and `nsh` on your `PATH`; nothing else is required.

## Troubleshooting

- `nsh: command not found` — install it (`curl -fsSL https://nsh.dev/install.sh | sh`)
  and make sure `~/.local/bin` is on `PATH`.
- `nsh 0.4.x is required` — the installer picked a newer CLI; install 0.4.x
  (`scripts/check.sh` and the release workflow both assert this, because run/test/package
  behaviour is pinned to the 0.4 crates in `Cargo.lock`).
- Rust version errors — the pinned toolchain matters; `nsh` builds with the
  `rust-toolchain.toml` of the directory it runs in.
- Port 53317 in use — another LocalSend instance owns it; the client retries for
  a few seconds and reports the conflict in the UI.
- The device identity, settings and history live in the app data directory
  (`~/Library/Application Support/org.localsend.desktop` on macOS; identity is
  `identity.pem`). Set `LOCALSEND_DATA_DIR=/some/dir` to keep it elsewhere
  (portable install, CI, or a sandboxed environment that cannot write to your
  profile). Logs go to `~/Library/Logs/localsend-desktop/localsend-desktop.log`
  (`LOCALSEND_LOG_DIR` overrides, `RUST_LOG` selects the filter).
- No peers discovered — check the firewall prompt (incoming connections must
  be allowed), that both devices are on the same subnet, and that multicast is
  not blocked by the router (HTTP scan fallback covers most cases).
