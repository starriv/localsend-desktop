//! System notifications for incoming requests and finished transfers.
//!
//! **Currently unavailable in this app's dependency set.** The pinned
//! `nsh-core = "0.4.0"` from crates.io predates `nsh_core::notifications`;
//! the newer nsh source in `/tmp/nsh` (git HEAD, *after* the 0.4.0 release)
//! provides `notification_permission`, `request_notification_permission`,
//! `show_notification`, `close_notification` and `next_notification_press`.
//! The app builds against the released crates, so this module stays a no-op
//! until the nsh dependencies are moved to a version that has them (or a
//! `[patch.crates-io]` points at the source checkout).
//!
//! When that happens, this module is where the feature goes:
//!
//! 1. [`install`] asks for permission once
//!    (`request_notification_permission().await`, granted → keep listening),
//! 2. it watches [`Session`]'s state (the revision signal) and shows a
//!    notification with a stable id when `state.incoming` appears
//!    (`localsend.incoming`) or a transfer finishes (`localsend.finished`) —
//!    `show_notification(Notification::new(id, title, body))` replaces the
//!    older notification with the same id, and the strings are
//!    `notify.incoming_title`/`notify.incoming_body`/… in `i18n.rs`,
//! 3. a task loops on `next_notification_press().await` and calls
//!    [`crate::platform::quit::show`] to bring the window back.
//!
//! The permission request is contextually sensible (it is asked for when the
//! app starts because a LocalSend receive can arrive at any time), and the
//! notification must not stack: one id per event kind.

use crate::model::Session;

/// Whether this build can show system notifications.
///
/// `false` for now: the pinned nsh 0.4.0 crates have no notification API (see
/// the module docs). Views can use it to hide a "notifications" preference
/// until the dependency is updated.
pub const fn available() -> bool {
    false
}

/// Requests notification permission once and wires notification presses to
/// the window.
///
/// No-op until nsh's notification API is available in the dependency set (see
/// the module docs). Kept in [`crate::platform::install`] so activating the
/// feature is a change to this file alone.
pub fn install(_session: Session) {
    // No-op: `available()` is false for the pinned nsh 0.4.0 crates (see the
    // module docs). Kept in `platform::install` so enabling the feature is a
    // change to this file alone — the wiring goes where this comment is.
}
