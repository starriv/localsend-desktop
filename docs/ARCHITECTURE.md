# Architecture

`localsend-desktop/` is a standalone Native Shell (nsh) Rust app. It is **not** part of
the Flutter app: it is its own Cargo workspace (`[workspace]` in
`localsend-desktop/Cargo.toml`) and compiles the LocalSend protocol crate from a
vendored snapshot:

```toml
localsend = { path = "vendor/localsend", features = ["discovery"] }
```

That is the whole dependency on LocalSend. `vendor/localsend` is an
unmodified copy of upstream `packages/core` at a pinned commit (recorded in
`vendor/localsend/UPSTREAM.md`); the directory is self-contained, so the app
builds and packages without the monorepo. Pulling upstream changes means
`scripts/sync-core.sh` (or `--from-git <repo> <rev>`), bumping the recorded
revision and rebuilding; nothing in this app is a fork of the core.
`features = ["discovery"]` is `crypto + http + multicast`; do not use `full`
(it pulls the WebRTC media stack, which LAN transfer does not need).

```
localsend-desktop/
├─ nsh.json                  app metadata: name, id, version, root, theme
├─ theme.json                design tokens (light/dark), read by theme()
├─ Cargo.toml                own workspace; localsend path dep; nsh 0.4 crates;
│                            macOS status item via objc2, objc2-app-kit,
│                            objc2-foundation, block2 and dispatch2
├─ src/
│  ├─ main.rs                `cargo run` entry (window size, title)
│  ├─ lib.rs                 root component App -> Session -> views::shell
│  ├─ logging.rs             process-wide `tracing` subscriber (stderr + log file)
│  ├─ net/                   protocol layer (no nsh types)
│  │  ├─ mod.rs              constants, re-exports
│  │  ├─ events.rs           Peer, OfferedFile, TransferEvent, Direction
│  │  └─ actor.rs            Node, NodeConfig, Handle: tokio actor
│  ├─ model/
│  │  ├─ state.rs            AppState + reducer (pure modulo injected clock)
│  │  └─ session.rs          Session: signals, commands, persistence
│  ├─ i18n.rs                EN/ZH string tables
│  ├─ ui/                    design system (tokens, icons, controls)
│  ├─ views/                 shell + screens + overlays
│  └─ platform/              menus, status item (macOS), dialogs, clipboard, notifications (no-op)
├─ ui-tests/
│  ├─ App.test.tsx           headless UI tests (`nsh test ui-tests/App.test.tsx`)
│  ├─ Screenshots.test.tsx   screenshots + interactions (`--target macos`)
│  └─ ScreenshotsZh.test.tsx Chinese captures for the README montage
└─ docs/
   ├─ ARCHITECTURE.md        this file
   ├─ DESIGN.md              visual + component contract
   ├─ BUILD.md               build / run / test in this checkout
   ├─ PERFORMANCE.md         measurements and how to reproduce them
   ├─ STATUS.md              what is implemented, verified, and known gaps
   └─ screenshots/           README montage (overview.png)
```

## Layers and the one rule

```
views ──► model::Session ──► net::Handle (commands)
  ▲           │
  └───────────┴── signals + AppState (read)      net::TransferEvent (channel)
```

The single rule: **the network actor never touches UI state, and the UI never
touches sockets.** `net` reports `TransferEvent`s; `Session` applies them to
`AppState` on the engine thread and bumps a revision signal.

## The network actor (`net`)

`Node::start(NodeConfig)` runs in an nsh task (nsh's engine owns a tokio
runtime) and spawns:

- the HTTP server (`localsend::http::server::start_with_port`) with the
  device identity, PIN and download directory;
- multicast discovery + unicast register (`localsend::discovery`);
- transfer tasks for outgoing uploads and incoming saves.

It returns `Node { device, handle, events }`. `Handle` only sends commands:

```rust
pub fn send(&self, peer: &Peer, items: Vec<ComposerItem>, pin: Option<String>) -> Result<String>;
pub fn decide(&self, id: &str, accept: bool) -> Result<()>;
pub fn cancel(&self, id: &str) -> Result<()>;
pub fn stop(&self) -> Result<()>;
```

Events (`TransferEvent`): `DiscoveryActive`, `PeerFound`, `PeerLost`,
`IncomingRequest`, `Started`, `Accepted`, `Progress`, `FileDone`,
`TextReceived`, `PinRequired`, `Completed`, `Cancelled`, `Failed`, `NetError`.

The core types must not leak past `net/events.rs`: that keeps `packages/core`
upgradable.

The actor must follow the core's contract: only one v2 session at a time;
answer `decision_tx` and `target_tx` exactly once (dropping them is an HTTP
500); pass `FileUploadTarget::Path` so the core enforces size/checksum before
publishing; a receive PIN is fixed at server start; changing alias/port/TLS/PIN
requires a network restart (`Session::update_settings` defers it while a
transfer runs).

Hardening added in the 2026-11 follow-up review (all with tests):

- Outgoing offers carry a streaming SHA-256 (cancellable, best-effort; a failed
  hash falls back to no checksum) and are reconciled against the receiver's
  accepted subset; HTTP 204 only means "completed" for a text message.
- A 90 s idle watchdog cancels an outgoing transfer that makes no byte progress;
  listener/multicast failures restart the server/discovery with 5/10/30/60 s
  backoff (five attempts) and then surface `NetError`.
- A receive is refused when the offered size does not fit the destination
  volume (Unix `statvfs`, 10 % headroom); per-session text is capped at 1 MiB
  (64 KiB per message); the app-side device table is capped at 500 entries and
  stops announcing beyond it (`PeerLost` is suppressed for active transfers).
- The staging sweep runs at start and shutdown (age > 10 min); collision names
  use a per-session suffix hint, so a batch of same-named files is O(n);
  identity loading verifies the private key matches the certificate.

## State (`model`)

`AppState` holds everything the views render:

```rust
view, discovering, devices: Vec<Peer>, selected: Option<String>,
incoming: Option<IncomingRequest>, transfers: Vec<Transfer>,
composer: Vec<ComposerItem>, settings: Settings, error: Option<String>
```

`AppState::apply_at(TransferEvent, now_ms)` is a pure reducer (the clock is
injected; `apply` is a thin wrapper). `Session` owns it and exposes:

```rust
read(|state| ...)          // subscribes to revision
read_peers / read_transfers / read_composer / read_settings
text(key)                  // translated SharedString
set_view / select / open_settings
pick_files / add_paths / add_text / paste_clipboard / paste_clipboard_files / remove_item / clear_composer
send / submit_pin / dismiss_pin / decide / cancel
update_settings / retry_network
start_network / restart_network / shutdown
has_active_transfers
```

Views call `read` (or a domain reader) at the top of their body and then read
state freely; every command bumps the revision once. Settings that require a
node restart are held in `AppState::pending_restart` while a transfer is active
and applied once when it finishes, so a settings change never cancels a
transfer. `Session::shutdown()` stops the node, finalises active transfers and
flushes history; the quit/close paths call it.

## Rendering rules

- One `#[component]` root (`App`); everything else is a plain function
  returning an element tree, called on render.
- Read signals in style closures (`move || theme().background`) so the platform
  restyles without a rebuild; read state through `session.read`.
- Interactive elements inside the custom title bar need `.window_drag(false)`.
- Motion: `animate_layout` for layout changes, `animated_value` for numbers,
  `reduce_motion()` respected in both.
- Text: system font (SF Pro on macOS). Do not ship a custom body font.
- Accessibility: interactive rows declare `focus`/`label` and keyboard focus
  rings; sheets hide the background from assistive technology, move the initial
  focus and restore it on close (a full Tab trap needs an nsh API that 0.4 does
  not expose — see `docs/STATUS.md`). Escape closes the topmost visible sheet.

## Threading / async rules

- Signals are engine-thread only. Await inside `spawn`, never in a handler.
- Blocking work (directory walks, file stats) goes through `blocking(..).await`.
- Progress events are coalesced in `net` (≤ 20/s per transfer); the UI may
  animate between events with `animated_value`.
- Do not hold `RefCell` borrows across an `await` (compile error in practice,
  but do not try).

## Persistence

- Settings: `nsh_core::storage::Storage` key `settings` (JSON, `schema: 1`);
  loaded values are repaired/validated (alias, port, save folder, favorites)
  and written back when repaired.
- History: `Storage` key `history` (JSON envelope with `schema: 2`, finished
  transfers, max 100); quota-aware trimming keeps the in-memory and on-disk
  lists identical, and non-UTF-8 paths are skipped instead of breaking every
  later write.
- Device identity: `identity.pem` in the same app data directory as the store
  (`<data>/org.localsend.desktop/`), written atomically with mode 0600, migrated
  once from `org.localsend.client`/`localsend-client`/`localsend-native`; never regenerate keys per
  launch, or peers see a new device every time. On load the private key is
  checked against the certificate.

## Platform integration (`platform`)

Application menu (File/Edit/View/Window/Help with shortcuts, including Restart
Network), file dialogs, clipboard (text + Finder files), open/reveal in Finder,
quit confirmation while a transfer runs, and the close-to-background policy.
System notifications are a documented no-op: nsh 0.4 has no notification API
(`platform::notify::available() == false`), and the notification strings/design
are kept in place for an nsh update. nsh 0.4 has no launch-at-login: do not
design around it.

The menu bar item (`platform::status_item`, macOS) is not an nsh feature:
nsh 0.4 has no status item API, so it uses AppKit through `objc2`. nsh runs
components and effects on its engine thread while AppKit objects belong to the
main thread, so the engine computes the menu rows (`menu_rows`, pure and
unit-tested) and posts them to the main queue; a click travels back through a
channel to a session task, which runs the same `Session` commands as the views.
A window-occlusion observer switches the app to the accessory policy (no Dock
icon) while the window is closed (hidden) and back when it shows; the menu
re-checks that policy when it opens (`NSMenuDelegate`), so a missed
notification cannot leave the app with the wrong Dock icon.

Its icons are the app's own glyphs (`ui::icons`): the glyphs draw through a
small `Pen` trait, which nsh's canvas implements for the views and an
`NSBezierPath` pen implements for template `NSImage`s in the menu. Apps linked
against the macOS 27 SDK must set `NSMenuItem.preferredImageVisibility` to
Visible, or AppKit hides every menu item image (SF Symbols included).

## Logging

`src/logging.rs` installs one process-wide `tracing` subscriber, idempotently,
from both the `App` component body (the path used by `nsh run`, the packaged
`.app` and the UI tests) and `main.rs` (so panics before the first frame are
captured). It writes to stderr and to
`~/Library/Logs/localsend-desktop/localsend-desktop.log` (other platforms: the
data directory; `LOCALSEND_LOG_DIR` overrides), uses `RUST_LOG` as an
`EnvFilter` (default `warn,localsend_desktop=info,localsend=info`) and installs a
panic hook that records the panic in the same file. Do not call `println!` for
diagnostics — use `tracing::{warn,info,debug}`.

## What "cross-platform" means here

The crate compiles on macOS, Windows and Linux (nsh platform crates). The
design is macOS-first: AppKit idioms, SF typography, `⌘` shortcuts. Windows and
Linux get the same screens with their platform's decorations; platform-specific
branches must be small and isolated (`cfg!` in `platform`, `platform.os`
checks in views) and never fork the screen code.
