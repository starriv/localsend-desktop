//! Desktop integration: the application menu, the window, file dialogs,
//! clipboard, notifications and the close-window policy.
//!
//! The app is macOS-first but compiles on Windows and Linux too: every
//! platform-specific call is either behind `#[cfg]` or guarded by nsh's
//! `is_desktop()`/`os()`, and nsh 0.4's desktop APIs are no-ops where the
//! platform has no desktop windows. nsh 0.4 has no status item API, so the
//! menu bar item ([`status_item`]) talks to AppKit directly on macOS; it has
//! no launch-at-login or single-instance API either.
//!
//! Call [`install`] once from the root component; it installs the menu, sets
//! up the window and starts the notification listener. The helpers in the
//! submodules are for places that need one of them alone (a "Show in Finder"
//! action, the send screen reading the clipboard, …):
//!
//! - [`menu`] — the application menu (File/Edit/View/Window/Help).
//! - [`window`] — title bar, window title and the minimum window size.
//! - [`files`] — open/reveal, the file and folder pickers, the download
//!   folder picker.
//! - [`clipboard`] — read text and files copied in Finder.
//! - [`notify`] — the notification hook. nsh 0.4.0 has no notification API, so
//!   [`notify::available`] is `false` and [`notify::install`] is a no-op for
//!   now; the module documents the wiring for when the dependency allows it.
//! - [`status_item`] — the menu bar item and its menu (macOS): devices,
//!   status, recent transfers, Open and Quit; the app leaves the Dock while
//!   its window is hidden.
//! - [`quit`] — closing the window and quitting (with a confirmation while a
//!   transfer is in progress, and a session flush before the process exits).

pub mod clipboard;
pub mod files;
pub mod menu;
pub mod notify;
pub mod quit;
pub mod status_item;
pub mod window;

use crate::model::Session;
use std::path::PathBuf;
use std::sync::OnceLock;

/// Installs everything the desktop needs, once, from the root component.
///
/// Creates nsh effects and tasks, so it must run inside a component body
/// (the root [`crate::App`] is the right place) — never outside an owner.
pub fn install(session: Session) {
    window::setup(session.clone());
    menu::install(session.clone());
    status_item::install(session.clone());
    notify::install(session);
}

/// Where received files go by default.
pub fn downloads_dir() -> PathBuf {
    dirs::download_dir()
        .or_else(|| dirs::home_dir().map(|home| home.join("Downloads")))
        .unwrap_or_else(|| PathBuf::from("."))
}

/// A readable default device name, as LocalSend's clients pick one.
///
/// On macOS this asks `scutil` (a subprocess, measured ~10 ms); the answer is
/// cached per process, and only the first launch (or a settings file without
/// an alias) needs it — [`crate::model::state::Settings`] deserializes every
/// other field from the saved JSON without constructing the default.
pub fn machine_name() -> String {
    static NAME: OnceLock<String> = OnceLock::new();
    NAME.get_or_init(computer_name).clone()
}

fn computer_name() -> String {
    #[cfg(target_os = "macos")]
    {
        if let Ok(output) = std::process::Command::new("scutil")
            .args(["--get", "ComputerName"])
            .output()
            && output.status.success()
        {
            let name = String::from_utf8_lossy(&output.stdout).trim().to_string();
            if !name.is_empty() {
                return name;
            }
        }
    }
    gethostname::gethostname()
        .to_string_lossy()
        .trim()
        .to_string()
}
