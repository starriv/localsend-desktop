# Upstream source

- Project: LocalSend — <https://github.com/localsend/localsend>
- Package: `packages/core`, crate `localsend`
- Revision: `92e133aa43869e94d394fd04fa70c50a0f796a64` (upstream `main`, 2026-11-06)
- License: Apache-2.0 (see `LICENSE` in this directory)
- Local patches: **none** — this directory is a copy of `packages/core` at the
  revision above (`scripts/sync-core.sh --check` verifies it while the app is
  developed inside the LocalSend monorepo).

## Why a vendored copy

The app is a self-contained Cargo workspace and must build without the Flutter
app or the rest of the monorepo. The official LocalSend core is not published
to crates.io (the `localsend` crate there is a different, third-party
implementation; do not use it). Pinning a reviewed revision in tree makes
production builds reproducible and network-independent, and records exactly
which protocol code was audited.

## Updating

```sh
./scripts/sync-core.sh                 # from ../packages/core in the monorepo
./scripts/sync-core.sh /path/to/core   # from any checkout of packages/core
./scripts/sync-core.sh --from-git https://github.com/localsend/localsend <rev>
```

Then bump the revision above, run `./scripts/check.sh`, and note the update in
the commit message. Do not edit files in this directory by hand.
