# Contributing

Thanks for helping with localsend-desktop. This file is the short version of
how to build, test and change the app without breaking its guarantees.

## Requirements

- macOS 14+ (Apple Silicon) for the reference build; Windows and Linux build
  from the same crate.
- Rust 1.97.1 — `rust-toolchain.toml` pins it, `rustup` installs it on demand.
- `nsh` **0.4.x** from [nsh.dev](https://nsh.dev/docs/installation). The CLI and
  the `nsh-*` crates in `Cargo.lock` must stay on the same minor: `nsh test`
  and `nsh package` are part of the build, not an optional extra.
- Xcode command line tools on macOS.

`nsh` generates the `.nsh/` directory (TypeScript types, test runner) on its
first run. It is gitignored; the first `nsh test`/`nsh run` creates it, and
`tsconfig.json` does not depend on it existing beforehand.

## Build and test

```sh
cargo check --locked                      # fast type check
cargo test --lib --locked                 # unit tests
LOCALSEND_OFFLINE=1 nsh test ui-tests/App.test.tsx # headless UI tests (safe: no network)
./scripts/check.sh                        # everything the local gate runs
```

`./scripts/check.sh` also runs the seven `#[ignore]` network e2e tests (they bind
port 53317 and generate a throwaway identity) and requires a LocalSend
`packages/core` checkout to verify `vendor/localsend` (see below).

Visual checks need a real window:

```sh
LOCALSEND_DEMO=1 nsh test ui-tests/Screenshots.test.tsx --target macos
```

`LOCALSEND_DEMO` seeds peers/transfers and **does not start the network**;
`LOCALSEND_DEMO_PEERS=<n>` changes the seeded device count. The generated
`screenshots/` directory is gitignored.

## The vendored core

`vendor/localsend` is a byte-identical copy of upstream LocalSend's
`packages/core` (Apache-2.0), pinned by the revision in
`vendor/localsend/UPSTREAM.md`. Do not edit it by hand.

- `./scripts/sync-core.sh /path/to/packages/core` replaces it (staged in a
  temporary directory, then swapped in; a failed copy restores the old tree).
- `./scripts/check.sh` runs `./scripts/sync-core.sh --check --require`, which
  fails when the copy drifted or the recorded revision no longer matches the
  source. Point `LOCALSEND_CORE` at the checkout when it is not a sibling of
  this repository.

A core update is a reviewed change: re-run the full gate, update the revision,
and say so in the commit message.

## Media and licenses

- The app is MIT; the core and nsh are Apache-2.0. `scripts/package-macos.sh`
  copies `LICENSE`, `NOTICE` and the core's license into `Contents/Resources`.
- New images/icons need a stated source and license; the icon lives in
  `assets/` and is referenced from `nsh.json`.

## Commits and pull requests

- Keep the existing gates green; a pull request that changes behaviour should
  add a test next to the code it changes.
- Run `./scripts/check.sh` before opening a pull request (it is the local gate
  with the e2e/UI suites; it is not part of the default edit loop). CI runs
  formatting and lints on every push; `.github/workflows/release.yml` builds and
  publishes a release when dispatched.
- Version bumps go through `./scripts/bump-version.sh <x.y.z>`, which updates
  `Cargo.toml`, `nsh.json` (and increments `build`) and the version string in
  `ui-tests/App.test.tsx`. Do not edit those three by hand.
