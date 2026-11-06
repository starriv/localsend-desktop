//! Closing the window and quitting.
//!
//! nsh 0.4 fixes the close policy at launch: `AppOptions::keep_alive_on_close`
//! makes macOS hide the window (and keep the engine alive) instead of closing
//! it, and there is no runtime setter. `main.rs` therefore sets it to `true`
//! — the app's default — so the window can stay in the background while
//! transfers run. This module implements the part that *is* dynamic: the
//! in-app close action (the File > Close Window item, a title-bar button)
//! honours [`Session`]'s `run_in_background` setting, quitting for real when
//! it is off.
//!
//! Quitting is the risky path: an in-flight transfer would be cut off and the
//! receiving side left with a `.part` file, so [`quit`] asks for confirmation
//! first when [`Session::has_active_transfers`] reports work in progress.
//! Every in-app quit then calls [`Session::shutdown`] (flush history/settings,
//! stop the network) before the process goes away. The menu's Quit item is a
//! custom item (with ⌘Q) rather than [`MenuRole::Quit`] for exactly this
//! reason: a native role would terminate without letting the app run the hook.
//! A system-level termination (log out, `kill`) still bypasses it — nsh 0.4
//! has no `applicationShouldTerminate` hook (see `docs/DESIGN.md`).
//!
//! On Windows and Linux there is no keep-alive in nsh 0.4, so closing the
//! window ends the process whatever the setting says; [`close`] still quits
//! cleanly through [`quit`] there.

use crate::i18n;
use crate::model::Session;
use nsh_base::alert_dialog;
use nsh_core::element::desktop::{close_window, order_front};
use nsh_core::prelude::*;

/// Whether closing the window should keep the app (and its transfers) running.
pub fn runs_in_background(session: &Session) -> bool {
    // Called from a menu/handler (untracked context), but subscribe narrowly
    // anyway so this stays cheap if it ever runs inside a binding.
    session.read_settings(|state| state.settings.run_in_background)
}

/// Closes the main window, following the "keep running in the background"
/// setting.
///
/// With it on, the window is hidden (`keep_alive_on_close` in `main.rs` keeps
/// macOS from terminating); with it off the app quits (with confirmation when
/// a transfer is in progress).
pub fn close(session: &Session) {
    if runs_in_background(session) {
        close_window();
    } else {
        quit(session);
    }
}

/// Quits the app, asking first when a transfer or pending request would be
/// interrupted.
pub fn quit(session: &Session) {
    if session.has_active_transfers() {
        confirm_quit(session.clone());
    } else {
        quit_now(session);
    }
}

/// Shows the "a transfer is still running" alert and quits only if the user
/// confirms. The answer is an action id (`quit`/`cancel`), never a label, so a
/// language change cannot break the comparison.
fn confirm_quit(session: Session) {
    let language = session.language.get_untracked();
    let title = i18n::text(language, "quit.active_title").to_owned();
    let body = i18n::text(language, "quit.active_body").to_owned();
    let cancel = i18n::text(language, "common.cancel").to_owned();
    let confirm = i18n::text(language, "quit.confirm").to_owned();
    let this = session.clone();
    session.task_detached(async move {
        let confirmed = alert_dialog()
            .title(title)
            .description(body)
            .cancel("cancel", cancel)
            .destructive("quit", confirm)
            .close_on_escape(true)
            .show()
            .await
            .is_ok_and(|result| result.action.as_deref() == Some("quit"));
        if confirmed {
            quit_now(&this);
        }
    });
}

/// Starts installing the downloaded update and quits so it can replace the
/// app (the installer relaunches it). Refused, with a toast, while a transfer
/// is running: the update waits instead of cutting it off.
pub fn restart_to_update(session: &Session) {
    if session.launch_update() {
        quit_now(session);
    }
}

/// Flushes the session, then terminates: `quit_app()` on macOS, and window
/// close (which ends the process when there is no keep-alive) elsewhere.
fn quit_now(session: &Session) {
    session.shutdown();
    #[cfg(target_os = "macos")]
    {
        nsh_platform_macos::quit_app();
    }
    #[cfg(not(target_os = "macos"))]
    {
        // Without a keep-alive policy the window is the app: closing it ends
        // the process.
        close_window();
    }
}

/// Shows and focuses the window — after a notification press, or from a menu
/// item that must not act on a hidden window.
pub fn show() {
    if !is_desktop() {
        return;
    }
    order_front();
}
