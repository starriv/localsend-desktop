//! LocalSend, rebuilt as a native app on Native Shell (nsh).
//!
//! The crate is split so protocol work, application state and rendering never
//! reach into each other:
//!
//! - [`net`] owns the LocalSend protocol: it drives `localsend` (the same Rust
//!   core the Flutter app runs) on a tokio actor and reports plain events.
//! - [`model`] is the UI-thread state: peers, transfers, history, settings.
//!   It is the only thing views read.
//! - [`ui`] is the design system: tokens, icons, controls.
//! - [`views`] are the screens, composed from [`ui`] and [`model`].
//! - [`platform`] holds desktop integration (menus, dialogs, window,
//!   notifications, clipboard).
//! - [`update`] checks the GitHub releases for a newer version and installs
//!   it (signed manifest, verified download).
//!
//! `App` is the root component named by `nsh.json`; it owns one
//! [`model::Session`] and mounts the shell.

use nsh_core::prelude::*;

pub mod i18n;
pub mod logging;
pub mod model;
pub mod net;
pub mod platform;
pub mod ui;
pub mod update;
pub mod views;

/// The app's UI set (`nsh.json` `"rust"`): what a TSX entry may import from
/// `localsend/client`.
#[native_module("localsend/client")]
fn native(registry: &mut Registry) {
    // Ships `assets/` with the app and has the engine register the fonts in it
    // before the first frame (the Material Icons font of the verify page).
    registry.assets(assets!());
    registry.register::<App>();
}

/// The application's root component.
#[component]
pub fn App() -> impl IntoElement {
    // Every entry point (cargo run, `nsh run`, the packaged app and the UI
    // tests) mounts this component, so this is the one place that guarantees
    // the logging subscriber exists before the network starts. Idempotent.
    logging::init();
    let session = model::Session::persistent();
    let appearance = session.appearance.get_untracked();
    session.start();
    platform::install(session.clone());
    views::shell(session, appearance)
}
