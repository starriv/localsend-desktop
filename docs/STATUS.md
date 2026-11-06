# Status

What is implemented, what was verified, and what is deliberately not there.
Last updated 2026-11-06 for the 1.0.1 release: device verification (LocalSend's
combined fingerprint, Material icon view and text), the send/receive design
pass (paper-plane flight, flatter lists, primary-action hierarchy), the
CC BY 4.0 Material Icons font, and the GitHub Actions build + release workflow
(1.0.0 shipped first).

## Rename (localsend-native → localsend-client → localsend-desktop)

The project is named `localsend-desktop`: crate `localsend-desktop` (Rust
`localsend_desktop`), macOS bundle id `org.localsend.desktop`, app data
directory `~/Library/Application Support/org.localsend.desktop`, staging folder
`.localsend-desktop-tmp`, logs in `~/Library/Logs/localsend-desktop/`. Identity,
settings and history live in that one directory (`identity.pem`,
`storage.json`). On first start the app migrates once from the earlier names,
newest first — `org.localsend.client`, then `localsend-client`/`localsend-native`
for the identity, and `org.localsend.client` then `org.localsend.native`
`storage.json` for settings/history — so peers still recognise the device and
settings survive. The old `.localsend-client-tmp` staging folder is removed.
macOS asks for Local Network permission again because the bundle id changed.

## Distribution

The app is a self-contained Cargo workspace: the LocalSend core is vendored
unmodified at `vendor/localsend` (upstream commit `92e133aa…`, Apache-2.0) and
statically linked into the binary, so production needs no Flutter/Dart/Node and
no LocalSend runtime component. `./scripts/package-macos.sh` produces a
41.0 MB stripped, ad-hoc signed, arm64 `.app` and copies
`LICENSE`/`NOTICE`/`vendor/localsend/LICENSE` into `Contents/Resources/`.
For distribution sign with a Developer ID and notarize (`--sign`/`--notarize`
arguments are wired; without credentials the script skips and says so — it
never claims a signature it did not make). The official core is not on
crates.io (the `localsend` crate there is a different project) and release tag
v1.18.2 predates two APIs used here, so pin the vendored SHA — see
`docs/BUILD.md` "Standalone".

## Audits (2026-11)

A first full read-and-fix audit covered 98 findings in four areas (1 P0, 17 P1, 42 P2, 38 P3) plus 8
cross-cutting items; all P0/P1 and the cheap P2/P3 were fixed.

A second, independent review re-read every file, re-verified the first audit's fixes and found 88 further
findings (84 in the four areas + 4 cross-cutting; 0 P0, 8 P1, 40 P2, 40 P3).
All fixable ones are now fixed and gated:
network hardening (SHA-256 offers, idle watchdog, listener restart with
backoff, disk-space refusal, bounded text/devices, O(n) collision handling),
silent-failure removal (network errors render; logging subscriber installed),
transfer-preserving settings restarts, persistence schema/quota robustness,
accessibility focus handling, contrast fixes, CI, vendor-drift enforcement,
`--locked` gates, reproducible packaging, and documentation reconciliation.
Remaining items are upstream/credential limitations, listed below.

## Implemented

| Area | State |
|---|---|
| Protocol core | official `packages/core` (crate `localsend`, feature `discovery`) vendored unmodified at a pinned revision (`vendor/localsend`), statically linked |
| Device identity | TLS certificate + key generated once, persisted atomically (0600), fingerprint stable across launches; key↔certificate pairing validated on load |
| Discovery | protocol v2.2 multicast announce + unicast register, IPv4 and IPv6, peer expiry, favorites-first ordering, bounded device table, automatic listener/multicast restart with backoff |
| HTTP server | protocol v2.2, PIN, mandatory client certificates, one session at a time, cancel, free-space refusal on receive |
| Send | files, folders, text; prepare-upload, PIN retry, sequential uploads, SHA-256 offers, progress, cancel, idle watchdog, error surfacing |
| Receive | accept/decline, PIN, auto-accept option, collision-safe atomic file publish, received-text capture, bounded text budget |
| Transfers UI | bottom progress card (bytes/speed/ETA/cancel), detail sheet, completed state with Open/Reveal |
| Device verification | combined fingerprint (both certificates, sorted) as LocalSend's 16 Material icons or as text; 260-entry icon alphabet and the CC BY 4.0 Material Icons font are bundled; entries on device rows, incoming requests and transfer details |
| History | grouped by day, persisted (max 100), open/reveal/copy, clear, schema-versioned with quota-aware trimming |
| Settings | alias, device type, discoverable, PIN, save folder, encryption toggle, auto-accept, appearance, language, run-in-background; changes that need a node restart are deferred while a transfer runs |
| Desktop | hidden title bar + traffic-light inset, application menu with shortcuts, file dialogs, clipboard (text + Finder files), open/reveal, notifications hook (see gaps), close-to-background policy, menu bar item (macOS: devices, status, recent transfers, Open/Quit; the Dock icon hides while the window is closed), device verification sheet (combined fingerprint as LocalSend's 16 Material icons or as text), quit confirmation during transfers |
| i18n | English + Simplified Chinese, key/placeholder parity enforced by tests |
| Design | custom Rust design system over `nsh-base`: tokens, canvas icon set, buttons/toggles/segmented/progress/rings/device tiles/badges, light and dark, WCAG AA text contrast |
| Packaging | `nsh package --target macos` + strip/re-sign script; arm64 thin, license files bundled, `NSLocalNetworkUsageDescription`/`NSBonjourServices` added; the release workflow publishes a dmg, a zip and the Windows x64 build (portable zip + NSIS installer), each with sha256; the bundled Material Icons font ships under its CC BY 4.0 license (macOS `Contents/Resources/LICENSE-MaterialIcons`, NSH assets elsewhere) |
| Observability | `src/logging.rs` installs a `tracing` subscriber in every entry point: stderr + `~/Library/Logs/localsend-desktop/localsend-desktop.log`, `RUST_LOG` filter, panic hook |

## Verified

- `cargo fmt --check`, `cargo clippy --all-targets --no-deps -- -D warnings`
  clean for the app crate; `cargo test --lib` **152 passing, 0 failed, 7
  ignored** (159 total; `cargo test --lib -- --ignored`: 7/7, re-run
  2026-11-06). Headless only: the nsh UI/screenshot suites below were **not**
  re-run after the verification sheet and the send-screen design pass.
- End-to-end with the real core, on real sockets/TLS: independent
  `LsHttpClientV2` peer → register → PIN prepare → checksum upload →
  `FileDone`/`Completed` with byte comparison; Node → core server stub; two
  Nodes with wrong-PIN retry, text message over 204, subset acceptance, sender
  abort while receiver decides; identity persisted; cross-process
  `localsend-cli send` → node receives, bytes identical.
- UI: 6 headless interaction tests (`LOCALSEND_OFFLINE=1 nsh test ui-tests/App.test.tsx`)
  and 18 screenshot/interaction tests (`LOCALSEND_DEMO=1 nsh test
  ui-tests/Screenshots.test.tsx --target macos`), including keyboard Escape, focus
  restoration, favorites, clipboard-file paste and transfer-progress traces.
  Both suites passed on the menu bar change (6/6 and 18/18, re-run 2026-11-06).
  A Chinese-UI variant (`ui-tests/ScreenshotsZh.test.tsx`, 5 tests) captured the README
  montage with the same screens (5/5). The screenshot suites were **not** re-run
  after the verification sheet and send-screen redesign (headless UI was: 6/6 on
  1.0.1), so the README montage still shows the previous empty state.
- Menu bar item (macOS): the packaged `.app` shows the app's paper-plane glyph
  in the status bar (launched and captured 2026-11-06), and the menu contents
  are computed by the unit-tested `menu_rows`; opening the menu itself is a
  manual check, not covered by the suites.
- `./scripts/check.sh` is the local gate: toolchain/vendor/version checks, fmt,
  clippy, unit + network e2e tests and the headless UI suite (run it explicitly;
  it is not part of the default edit loop). It passes on **1.0.1**: 152 unit
  tests (7 ignored), 7/7 network e2e, 6/6 headless UI (re-run 2026-11-06).
  CI (`.github/workflows/ci.yml`) runs fmt + clippy on every push;
  `.github/workflows/release.yml` builds the `.app` and publishes it to a
  GitHub Release on dispatch or a `v*` tag.
- Packaging: `nsh build` and `nsh package --target macos` succeed; the `.app`
  launches from LaunchServices with the native window and its traffic lights
  (red/yellow/green inset into the sidebar), the localized app menu
  (文件/编辑/显示/窗口/帮助 on a Chinese macOS session) and the window drawn by
  AppKit. The stripped bundle is 40,989,776 B (39.1 MiB), arm64 thin, ad-hoc
  signed, plist keys and bundled licenses verified.
- **Live run with real peers** (macOS session, packaged `.app`):
  - discovery listed an Android LocalSend device and a `localsend-cli` process on the LAN;
  - the phone sent a 234 KB screenshot → the incoming sheet waited for a decision, Accept
    saved it to `~/Downloads` (byte-for-byte), and the Receive list shows it completed;
  - `localsend-cli --port 53318 send --to 127.0.0.1` delivered a 43 B text file the same way
    (decline was also exercised: the client answered "Declined" and saved nothing);
  - the HTTP server held `127.0.0.1:53317`; the identity persisted
    (`~/Library/Application Support/org.localsend.desktop/identity.pem`, same fingerprint
    after quit and relaunch).

## Known gaps / not implemented

- **Notifications**: nsh 0.4.0 on crates.io predates `nsh_core::notifications`; the module
  is a documented no-op (`platform::notify::available() == false`). In-app toasts and the
  menu bar work. Activating them needs an nsh version bump that exposes the API.
- **Request-body limit (upstream)**: the vendored core parses `prepare-upload`/`register`
  bodies without a size limit before app validation (`BLOCKED-UPSTREAM`; the app caps what
  reaches the UI and refuses offers that do not fit). Needs an upstream patch.
- **Discovery store eviction (upstream)**: the core's discovery store never evicts
  fingerprints; the app bounds its own device table and stops announcing beyond it, but the
  core structure still grows under a register flood. Needs an upstream eviction API.
- **Multi-homed peer cancellation**: cancellation matches session id **and** host; a peer
  that dials from a different address fails via 403 instead of stopping immediately. This
  is a deliberate security trade-off (session ids are receiver-chosen; matching by id alone
  would let any peer cancel a transfer) — documented in `WONTFIX` comments.
- **Notarization / Developer ID / universal binary**: the packaging script supports
  `--sign`/`--notarize` but this checkout has no credentials, so the committed artifact is
  ad-hoc signed and arm64 thin (`BLOCKED-CREDENTIALS`).
- **Same-host multicast**: macOS does not deliver multicast between two processes on one
  machine here (the repo's own CLI fails the same way), so two local instances discover
  each other by direct register only. Real LAN discovery is verified with a phone; the
  code paths above the socket are unit-tested.
- **Windows / Linux** compile paths exist (nsh platform crates, `#[cfg]` branches) but
  were not built or run here; the disk-space preflight is Unix-only (Windows falls back to
  the actor's error path).
- **iOS / Android**: the UI is desktop-first; mobile hosts would need a phone layout
  (the shell currently renders the desktop sidebar at any width). The protocol layer is
  platform independent.
- **Web**: the LocalSend core cannot compile to wasm32, so a `--target web` build would
  need the network layer cfg-gated out; not done.
- **Modal focus trap**: nsh 0.4 has no focus-scope/trap API. Sheets hide the background
  from assistive technology, move the initial focus and restore it on close, but keyboard
  Tab can still reach content-area controls behind the scrim; revisit after an nsh bump.
- Launch at login and single-instance are not in nsh 0.4 (no such API): not
  implemented, not faked. The menu bar item exists on macOS only
  (`platform::status_item`, AppKit through `objc2`); Windows/Linux have no tray.
- Received text over 64 KiB is saved as a file (protocol behaviour inherited from core).
- Notifications/web-send page for browsers (`web_send: false`) is off; LocalSend's
  browser download flow is not exposed by this client.

## How to run

See [`BUILD.md`](BUILD.md). Quick version:

```sh
cd localsend-desktop
nsh run                                  # development window
nsh test ui-tests/App.test.tsx              # headless UI tests
LOCALSEND_DEMO=1 nsh test ui-tests/Screenshots.test.tsx --target macos   # screenshots
nsh package --target macos               # LocalSend.app
```

`LOCALSEND_DATA_DIR=/some/dir` keeps the device identity outside the user profile
(portable installs, sandboxed CI); `LOCALSEND_DEMO=1|incoming|pin` seeds the UI for
design review (and does not start the network); `LOCALSEND_LOG_DIR` overrides the
log directory and `RUST_LOG` the filter.
