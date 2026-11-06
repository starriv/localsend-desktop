//! Runs the LocalSend app directly on this machine's native desktop backend
//! (`cargo run`); `nsh run` builds and starts the same root with the release
//! runner.

#![cfg_attr(target_os = "windows", windows_subsystem = "windows")]

#[cfg(target_os = "linux")]
use nsh_platform_linux as platform;
#[cfg(target_os = "macos")]
use nsh_platform_macos as platform;
#[cfg(target_os = "windows")]
use nsh_platform_windows as platform;

/// Design size of the main window, in points.
const WINDOW_SIZE: (i32, i32) = (1200, 800);

#[cfg(any(target_os = "linux", target_os = "macos", target_os = "windows"))]
fn main() -> impl std::process::Termination {
    // The same subscriber the App body installs (idempotent): installing it
    // here as well means a panic in the platform backend, before the root
    // component mounts, still reaches the log file.
    localsend_desktop::logging::init();
    platform::run_app(
        platform::AppSource::Rust(platform::Root::of::<localsend_desktop::App>()),
        platform::AppOptions {
            title: "LocalSend".to_owned(),
            #[cfg(any(target_os = "linux", target_os = "windows"))]
            app_id: "org.localsend.desktop".to_owned(),
            size: WINDOW_SIZE,
            // Closing the window keeps the app (and transfers) running; the
            // Dock icon reopens it. File > Quit still quits.
            #[cfg(target_os = "macos")]
            keep_alive_on_close: true,
            ..Default::default()
        },
    )
}

#[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
compile_error!(
    "Run this binary on macOS, Windows or Linux; `nsh run --target ios|android` mounts the root on phones."
);
