# Performance

Measured on an Apple Silicon Mac (macOS 27.0.1), 1200×800 window, `--release`
packaged `LocalSend.app` unless stated. Every number comes from a command you
can re-run. The package figures below and the idle footprint were re-measured
after the macOS menu bar item (2026-11-06).

## Startup

| Metric | Value | How |
|---|---|---|
| Cold start: launch → HTTP server accepting on :53317 (identity present) | 0.44–1.22 s (3 runs: 1.22 / 0.44 / 0.47) | `open LocalSend.app`, poll `nc -z 127.0.0.1 53317` |
| First frame inside the UI test harness | 52–97 ms (281–638 mount items depending on demo data) | `nsh test ui-tests/App.test.tsx --target macos --trace` |
| First ever launch (identity generation) | seconds, once | RSA-2048 on a blocking thread; `[profile.dev.package.rsa] opt-level = 2` keeps debug runs usable |

Startup work that used to run twice per launch (`scutil --get ComputerName`,
~10 ms/call) now runs at most once and is cached; a normal launch never
constructs default settings, because `Settings` has per-field serde defaults.

## Memory

| Metric | Value |
|---|---|
| Physical footprint, idle (window open, menu bar item, demo data) | 29 MB |
| Peak during idle measurement | 29 MB |

`footprint -p <pid>` / `vmmap -summary <pid>`. Single process: the nsh engine,
its tokio runtime and the LocalSend core are linked into the app.

## Rendering

From `nsh test … --target macos --trace` (one commit per turn; `mount items` is
the deterministic number):

| Interaction | Result |
|---|---|
| Shell first frame (3 demo devices) | 638 mount items, 174 native nodes |
| Destination switch / rail update | 0.24–0.57 ms per commit, 2–21 mount items |
| Switch Receive → History → Send + open/close Settings (headless, English) | 5–16 commits, 361–1 070 mount items per test |
| Window resize with Settings open | **63 mount items** (was 716 — the sheet re-mounted per resize) |
| Revision bump while on Receive (progress tick) | **250 µs / 1 mount** (was 3 070 µs / 173 mounts) |
| Revision bump on History | 953 µs / 8 mounts (was 1 539 µs / 11) |
| Transfer card progress tick | 633 µs mean, 18.4 ms over 2 s (was 918 µs / 25.7 ms) |
| Device list scaling (`LOCALSEND_DEMO_PEERS`) | 3 → 638, 30 → 1 502, 200 → 6 942 mount items; ≈32 ops + 8 nodes per row, linear (not virtualized: the expected peer count is small) |

Follow-up review (2026-11) re-measured the hot paths on the fixed tree with
`nsh test --trace`; the first-audit numbers above still hold: first frame
635 mount items / 173 nodes, transfer-card progress tick 0.3–0.9 ms with 4
mounts, History 1–4 mounts/tick (216 nodes), Receive 3–4 mounts/tick (187
nodes). The details-sheet/history/recent-list clones were removed in that
review.

Rust nsh apps run no JavaScript (`list js` 0–5 µs in the trace); one commit
crosses to the platform once per turn.

## Binary and package

| Metric | Value |
|---|---|
| `nsh package --target macos` (unstripped) | 53,310,000 B (~53 MB) |
| `scripts/package-macos.sh` (strip -x + ad-hoc re-sign) | **40,989,776 B (39.1 MiB)** |
| Bundle resources | 0.46 MB |
| Dependency graph | 1,001 packages; 111 crates resolve to more than one version, pulled by nsh's graphics/parser stack and the vendored core's HTTP/crypto stack, not by this crate's own code |

`nsh package` keeps the runner's local symbols and has no strip/sign flags, so
the script strips and re-signs. For distribution, sign with your Developer ID
after stripping.

## What the code does for performance

- One signal revision per network event, split into per-domain revisions
  (`peers/transfers/composer/settings`): a progress tick cannot re-diff the
  device list, and a peer announcement cannot re-diff the composer.
- Network work runs on nsh's tokio runtime, never on the engine thread; the UI
  sees plain events.
- Progress events are coalesced in the network layer (≤20/s per transfer, one
  final event with exact bytes); bars animate with `animated_value` and the
  speed/ETA text is quantised to 4×/s, so a 1 GB transfer does not produce a
  commit per chunk.
- Lists are keyed (`each`); history is capped at 100 entries in memory and on
  disk; long lists are the only unvirtualized UI (device list, staging list),
  measured linear above.
- Expensive derived values are `computed` snapshots (transfer card, history
  rows, device chip, detail sheet) or signature-cached, so a tick re-uses them.
