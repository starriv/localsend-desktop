//! Runtime logging: one `tracing` subscriber for the whole process.
//!
//! The app logs through `tracing::` macros, but nsh 0.4 installs no
//! subscriber of its own (and `env_logger`, which `main.rs` used to install,
//! is a `log` backend while the calls are `tracing`). Without this module
//! every `info!`/`warn!` in `net`/`model` is silently dropped.
//!
//! [`init`] installs a `tracing-subscriber` that writes each event to stderr
//! **and** to a log file, so `cargo run`, the `nsh` runner and the packaged
//! `.app` all have the same diagnostics. `RUST_LOG` selects the filter
//! (`EnvFilter` syntax, e.g. `RUST_LOG=debug` or `RUST_LOG=nsh_core=debug`);
//! the default is `warn,localsend_desktop=info,localsend=info`.
//!
//! The file lives where macOS users look for app logs, and under the platform
//! data directory elsewhere:
//!
//! - macOS: `~/Library/Logs/localsend-desktop/localsend-desktop.log`
//! - Windows/Linux: `<data dir>/localsend-desktop/localsend-desktop.log`
//!
//! `LOCALSEND_LOG_DIR` overrides the directory (tests, CI and support runs).
//! If the file cannot be created the app still logs to stderr.
//!
//! [`init`] is idempotent: the root [`crate::App`] component body calls it —
//! which is the path the `nsh` runner and the packaged app take — and
//! `main.rs` calls it as well before starting the desktop backend.

use std::fs::{File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, Once, OnceLock};
use tracing_subscriber::EnvFilter;

/// The default filter: quiet dependencies, the app's own events at `info`.
const DEFAULT_FILTER: &str = "warn,localsend_desktop=info,localsend=info";

/// Runs [`init`]'s work only once (the component body may run more than once).
static INIT: Once = Once::new();

/// Size at which the log file is rotated to `<name>.1` (one generation is
/// kept). Without a cap the file grew forever, and a peer can make the app log
/// (every declined request is a warning).
const MAX_LOG_BYTES: u64 = 8 * 1024 * 1024;

/// The shared log file, opened by [`init`]; `None` when it could not be
/// created (stderr still gets every line).
static LOG_FILE: OnceLock<Mutex<LogFile>> = OnceLock::new();

/// The open log file and how much it holds, for size-based rotation.
struct LogFile {
    path: PathBuf,
    file: File,
    written: u64,
}

impl LogFile {
    fn open(path: PathBuf) -> Option<Self> {
        if std::fs::metadata(&path).is_ok_and(|metadata| metadata.len() >= MAX_LOG_BYTES) {
            rotate(&path);
        }
        let file = open(&path)?;
        let written = file.metadata().map(|metadata| metadata.len()).unwrap_or(0);
        Some(Self {
            path,
            file,
            written,
        })
    }

    fn write_all(&mut self, buf: &[u8]) {
        let _ = self.file.write_all(buf);
        self.written = self.written.saturating_add(buf.len() as u64);
        if self.written >= MAX_LOG_BYTES {
            rotate(&self.path);
            if let Some(file) = open(&self.path) {
                self.file = file;
                self.written = 0;
            }
        }
    }
}

/// Moves `path` to `path.1`, replacing the previous generation.
fn rotate(path: &Path) {
    let mut rotated = path.as_os_str().to_owned();
    rotated.push(".1");
    let _ = std::fs::rename(path, PathBuf::from(rotated));
}

/// Installs the process-wide logging subscriber.
///
/// Idempotent and safe to call from any thread. The first call wins; later
/// calls return immediately, so `main.rs` and the [`crate::App`] body can
/// both call it.
pub fn init() {
    INIT.call_once(|| {
        if let Some(file) = log_file_path().and_then(LogFile::open) {
            let _ = LOG_FILE.set(Mutex::new(file));
        }
        let filter =
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(DEFAULT_FILTER));
        // `try_init` also bridges the `log` crate (the `tracing-log` feature is
        // on by default), so nsh's own `log::warn!` output lands here too.
        let _ = tracing_subscriber::fmt()
            .with_env_filter(filter)
            .with_writer(|| TeeWriter)
            .try_init();
        install_panic_hook();
    });
}

/// Where the log file goes, if the platform has a place for it.
fn log_file_path() -> Option<PathBuf> {
    if let Some(dir) = std::env::var_os("LOCALSEND_LOG_DIR").filter(|dir| !dir.is_empty()) {
        return Some(PathBuf::from(dir).join("localsend-desktop.log"));
    }
    #[cfg(target_os = "macos")]
    let dir = dirs::home_dir().map(|home| home.join("Library/Logs/localsend-desktop"));
    #[cfg(not(target_os = "macos"))]
    let dir = dirs::data_dir().map(|data| data.join("localsend-desktop"));
    dir.map(|dir| dir.join("localsend-desktop.log"))
}

/// Creates the log file (and its directory), appending to earlier runs.
/// Owner-only on Unix: the log names peers, their addresses and file names.
fn open(path: &Path) -> Option<File> {
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let mut options = OpenOptions::new();
    options.create(true).append(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let file = options.open(path).ok()?;
    // A file created by an older version may still be world-readable.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = file.set_permissions(std::fs::Permissions::from_mode(0o600));
    }
    Some(file)
}

/// Forwards every formatted event to stderr and the log file.
struct TeeWriter;

impl Write for TeeWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let _ = io::stderr().write_all(buf);
        if let Some(file) = LOG_FILE.get()
            && let Ok(mut file) = file.lock()
        {
            file.write_all(buf);
        }
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        let _ = io::stderr().flush();
        if let Some(file) = LOG_FILE.get()
            && let Ok(mut file) = file.lock()
        {
            let _ = file.file.flush();
        }
        Ok(())
    }
}

/// Records panics in the same log file before the default hook reports them.
fn install_panic_hook() {
    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        if let Some(file) = LOG_FILE.get()
            && let Ok(mut file) = file.lock()
        {
            file.write_all(format!("panic: {info}\n").as_bytes());
            let _ = file.file.flush();
        }
        // Keep the default behaviour (message + backtrace on stderr).
        default_hook(info);
    }));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_log_path_honours_the_override() {
        // The override is read on every call, so this does not race with
        // `init` in other tests (they do not call it).
        // SAFETY: the test process does not read this variable concurrently.
        unsafe { std::env::set_var("LOCALSEND_LOG_DIR", "/tmp/localsend-log-test") };
        assert_eq!(
            log_file_path(),
            Some(PathBuf::from(
                "/tmp/localsend-log-test/localsend-desktop.log"
            ))
        );
        unsafe { std::env::remove_var("LOCALSEND_LOG_DIR") };
    }

    #[test]
    fn opening_creates_the_directory_and_appends() {
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join("nested/localsend-desktop.log");
        {
            let mut file = open(&path).expect("create log");
            writeln!(file, "first").expect("write");
        }
        {
            let mut file = open(&path).expect("reopen log");
            writeln!(file, "second").expect("write");
        }
        let content = std::fs::read_to_string(&path).expect("read log");
        assert_eq!(content, "first\nsecond\n");
    }

    #[test]
    fn a_full_log_is_rotated_and_the_file_is_private() {
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join("localsend-desktop.log");
        let mut log = LogFile::open(path.clone()).expect("open log");
        log.write_all(&vec![b'x'; MAX_LOG_BYTES as usize]);
        log.write_all(b"fresh\n");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "fresh\n");
        assert_eq!(
            std::fs::metadata(dir.path().join("localsend-desktop.log.1"))
                .unwrap()
                .len(),
            MAX_LOG_BYTES
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o600);
        }
    }
}
