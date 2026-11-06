//! File dialogs, the Finder and the download folder.
//!
//! All of this is nsh's file-request layer ([`nsh_core::file_dialog`]): the
//! platform shows its own panels and reports the paths. Errors are reported
//! through [`Session::fail`] rather than swallowed, because "Open" doing
//! nothing is worse than a toast saying why.

use crate::model::Session;
use nsh_core::file_dialog::OpenDialog;
use nsh_core::prelude::*;
use std::path::{Path, PathBuf};

/// Extensions whose default "open" action runs code (apps, scripts,
/// installers, terminal and location files). Received files come from any
/// device on the network, so these are revealed in their folder instead of
/// launched with one click; the quarantine attribute set on receipt still
/// makes Gatekeeper check them if the user opens them from there.
const RUNS_CODE: &[&str] = &[
    "app",
    "applescript",
    "bat",
    "cmd",
    "com",
    "command",
    "cpl",
    "desktop",
    "dmg",
    "exe",
    "fileloc",
    "inetloc",
    "jar",
    "js",
    "jse",
    "lnk",
    "mpkg",
    "msi",
    "msix",
    "pkg",
    "ps1",
    "py",
    "rb",
    "reg",
    "scpt",
    "scptd",
    "scr",
    "sh",
    "terminal",
    "tool",
    "url",
    "vbe",
    "vbs",
    "webloc",
    "workflow",
    "wsf",
];

/// Whether opening `path` in its default application could run code.
pub fn runs_code(path: &Path) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| {
            RUNS_CODE
                .iter()
                .any(|risky| risky.eq_ignore_ascii_case(extension))
        })
}

/// Opens `path` in the default application, reporting a failure. A file type
/// that can run code is revealed in its folder instead (see [`RUNS_CODE`]).
pub fn open(session: &Session, path: impl AsRef<Path>) {
    let path = absolute(path.as_ref());
    if runs_code(&path) {
        session.toast(session.text("error.open_risky"));
        reveal(session, path);
        return;
    }
    let this = session.clone();
    session.task_detached(async move {
        if let Err(error) = open_path(path).await {
            this.fail(problem(&this, "error.open_failed", &error));
        }
    });
}

/// Shows `path` selected in the Finder (File Explorer / the file manager),
/// reporting a failure.
pub fn reveal(session: &Session, path: impl AsRef<Path>) {
    let path = absolute(path.as_ref());
    let this = session.clone();
    session.task_detached(async move {
        if let Err(error) = reveal_path(path).await {
            this.fail(problem(&this, "error.reveal_failed", &error));
        }
    });
}

/// Opens the configured download folder.
pub fn open_downloads(session: &Session) {
    let downloads = session.read(|state| state.settings.downloads.clone());
    open(session, downloads);
}

/// Shows the open panel for files and stages what the user picked.
pub fn choose_files(session: &Session) {
    let picked = pick_paths(
        OpenDialog::files()
            .multiple(true)
            .title(session.text("send.pick_files").to_string()),
    );
    let this = session.clone();
    session.task_detached(async move {
        match picked.await {
            Ok(Some(paths)) => this.add_paths(paths),
            Ok(None) => {}
            Err(error) => this.fail(problem(&this, "error.choose_failed", &error)),
        }
    });
}

/// Shows the open panel for folders and stages what the user picked.
pub fn choose_folder(session: &Session) {
    let picked =
        pick_paths(OpenDialog::folders().title(session.text("send.pick_folder").to_string()));
    let this = session.clone();
    session.task_detached(async move {
        match picked.await {
            Ok(Some(paths)) => this.add_paths(paths),
            Ok(None) => {}
            Err(error) => this.fail(problem(&this, "error.choose_failed", &error)),
        }
    });
}

/// Shows the open panel for folders and stores the chosen one as the download
/// folder; cancelling changes nothing.
pub fn choose_downloads_dir(session: &Session) {
    let current = session.read(|state| state.settings.downloads.clone());
    let picked = pick_paths(
        OpenDialog::folders()
            .title(session.text("receive.change").to_string())
            .directory(current),
    );
    let this = session.clone();
    session.task_detached(async move {
        match picked.await {
            Ok(Some(mut paths)) => {
                if let Some(dir) = paths.pop() {
                    this.update_settings(|settings| settings.downloads = dir);
                }
            }
            Ok(None) => {}
            Err(error) => this.fail(problem(&this, "error.choose_failed", &error)),
        }
    });
}

/// `path` as an absolute path, as the open/reveal requests want it.
fn absolute(path: &Path) -> PathBuf {
    std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf())
}

/// `key`'s message with the platform's reason appended.
fn problem(session: &Session, key: &'static str, error: &str) -> String {
    format!("{}: {error}", session.text(key))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn code_carrying_types_are_not_opened_directly() {
        for name in [
            "run.command",
            "Setup.PKG",
            "x.terminal",
            "a/b/Tool.app",
            "go.webloc",
        ] {
            assert!(runs_code(Path::new(name)), "{name} must be revealed");
        }
        for name in ["photo.jpg", "notes.txt", "Downloads", "archive.zip"] {
            assert!(!runs_code(Path::new(name)), "{name} may be opened");
        }
    }
}
