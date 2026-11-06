//! File naming for received transfers: peer-supplied names are untrusted.
//!
//! A LocalSend `file_name` is a *relative path* for folder transfers (v2
//! convention), so it may contain `/`. Everything in here treats it as such:
//! traversal and absolute paths are rejected, each segment is sanitized for a
//! portable filesystem, directory structure is kept, and the final publish
//! never replaces an existing file.

use anyhow::{Context, Result, bail, ensure};
use std::io::ErrorKind;
use std::path::{Path, PathBuf};

/// How many `name (n).ext` variants are tried before giving up.
const MAX_COLLISION_SUFFIX: u32 = 10_000;

/// Deepest directory structure accepted in a peer-supplied path.
const MAX_DEPTH: usize = 16;

/// Turns a peer-supplied `file_name` into a safe relative path below the
/// downloads directory.
///
/// Accepted: `photo.jpg`, `folder/photo.jpg`. Rejected: absolute paths,
/// `..`/`.` segments, NUL bytes and empty names. Backslashes are treated as
/// separators (Windows senders) and every segment is sanitized with the
/// core's portable rules, so characters that are illegal on the destination
/// filesystem cannot reach the file system layer.
pub fn relative_name(name: &str) -> Result<PathBuf> {
    ensure!(!name.is_empty(), "the request contains an empty file name");
    ensure!(
        !name.contains('\0'),
        "the request contains an invalid file name"
    );

    // Windows senders may use `\`; the protocol always uses `/`.
    let normalized = name.replace('\\', "/");
    ensure!(
        !normalized.starts_with('/'),
        "absolute file paths are not accepted"
    );
    // A Windows drive prefix (`C:`) is absolute even without a leading slash.
    ensure!(
        !normalized
            .as_bytes()
            .get(1)
            .is_some_and(|separator| *separator == b':'),
        "absolute file paths are not accepted"
    );

    let mut relative = PathBuf::new();
    let mut depth = 0usize;
    for segment in normalized.split('/') {
        if segment.is_empty() || segment == "." {
            continue;
        }
        ensure!(
            segment != "..",
            "file paths may not leave the receive folder"
        );
        let sanitized = localsend::util::filename::sanitize(
            segment,
            localsend::util::filename::Rules::Universal,
        );
        ensure!(
            !sanitized.is_empty() && sanitized != "." && sanitized != "..",
            "the request contains an invalid file name"
        );
        depth += 1;
        ensure!(depth <= MAX_DEPTH, "the request nests folders too deeply");
        relative.push(sanitized);
    }

    ensure!(
        !relative.as_os_str().is_empty(),
        "the request contains an empty file name"
    );
    Ok(relative)
}

/// A private staging path inside the receive directory, used while an upload
/// is still incomplete. The temporary directory is on the same filesystem as
/// the destination, which is what makes the final publish atomic.
///
/// No file system access: the staging directory is created once by the actor
/// at startup, so this can run on a tokio worker without blocking it (NET-10).
pub fn temp_path(temp_dir: &Path) -> PathBuf {
    temp_dir.join(format!("{}.part", uuid::Uuid::new_v4()))
}

// Counts link attempts on the current thread in tests only, to prove the
// collision cache keeps a batch of same-named files O(n) instead of O(n^2)
// (NET-04). Thread-local so parallel tests cannot skew the measurement.
#[cfg(test)]
std::thread_local! {
    static LINK_ATTEMPTS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

#[cfg(test)]
fn note_link_attempt() {
    LINK_ATTEMPTS.with(|attempts| attempts.set(attempts.get() + 1));
}

#[cfg(test)]
fn link_attempt_count() -> usize {
    LINK_ATTEMPTS.with(std::cell::Cell::get)
}

#[cfg(not(test))]
fn note_link_attempt() {}

/// Publishes a completed temporary file under `relative` below `downloads`,
/// never replacing an existing file: collisions become `name (1).ext`,
/// `name (2).ext`, ... . Returns the path actually written.
///
/// On Unix and on NTFS a hard link is created first (atomic, fails if the
/// name exists) and the temporary file is then removed. Filesystems without
/// hard links fall back to a rename guarded by an existence check.
pub fn publish(temp: &Path, downloads: &Path, relative: &Path) -> Result<PathBuf> {
    publish_from(temp, downloads, relative, 0).map(|(path, _)| path)
}

/// Like [`publish`], but starts probing at `start_index` and returns the next
/// index to use for the same name.
///
/// A receive session keeps one hint per destination path. Without it, the k-th
/// same-named file re-tries every suffix from 0, so a batch of n identical
/// names costs n(n+1)/2 hard links (NET-04: a peer-triggerable DoS because the
/// actor awaits this publish). With the monotonic hint each file costs O(1)
/// attempts in the normal case.
pub fn publish_from(
    temp: &Path,
    downloads: &Path,
    relative: &Path,
    start_index: u32,
) -> Result<(PathBuf, u32)> {
    let directory = match relative
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        Some(parent) => create_inside(downloads, parent)?,
        None => downloads.to_path_buf(),
    };

    let file_name = relative
        .file_name()
        .context("received file has no name")?
        .to_string_lossy()
        .into_owned();
    let path = Path::new(&file_name);
    let stem = path
        .file_stem()
        .map(|stem| stem.to_string_lossy().into_owned())
        .unwrap_or_else(|| file_name.clone());
    let extension = path
        .extension()
        .map(|extension| format!(".{}", extension.to_string_lossy()))
        .unwrap_or_default();

    let mut index = start_index;
    while index < MAX_COLLISION_SUFFIX {
        // Only allocate when a suffix is actually needed.
        let candidate = if index == 0 {
            directory.join(&file_name)
        } else {
            directory.join(format!("{stem} ({index}){extension}"))
        };
        note_link_attempt();
        match link_without_replace(temp, &candidate) {
            Ok(()) => {
                mark_quarantined(&candidate);
                return Ok((candidate, index + 1));
            }
            Err(error) if error.kind() == ErrorKind::AlreadyExists => {
                index += 1;
                continue;
            }
            Err(error) => {
                return Err(error).with_context(|| {
                    format!("Cannot store the received file {}", candidate.display())
                });
            }
        }
    }

    bail!("Too many files are already named {file_name}")
}

/// Creates `relative` (a sanitized directory path) below `downloads`, one
/// component at a time, and refuses any component that already exists as a
/// symlink or a non-directory.
///
/// Checking each component *before* descending means a symlink planted in the
/// receive folder (e.g. `Downloads/album -> /etc`) can neither receive a file
/// nor make us create directories outside the folder (CWE-59). Every failure
/// is fail-closed: an entry that cannot be inspected is refused.
fn create_inside(downloads: &Path, relative: &Path) -> Result<PathBuf> {
    let mut directory = downloads.to_path_buf();
    for component in relative.components() {
        let std::path::Component::Normal(segment) = component else {
            bail!("received paths may not resolve outside the receive folder");
        };
        directory.push(segment);
        match std::fs::symlink_metadata(&directory) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                bail!("received paths may not resolve outside the receive folder")
            }
            Ok(metadata) if metadata.is_dir() => {}
            Ok(_) => bail!(
                "Cannot create the receive subdirectory {}: a file with that name exists",
                directory.display()
            ),
            Err(error) if error.kind() == ErrorKind::NotFound => {
                match std::fs::create_dir(&directory) {
                    Ok(()) => {}
                    // Another file of the same transfer created it meanwhile;
                    // re-check that it is a real directory.
                    Err(error) if error.kind() == ErrorKind::AlreadyExists => {
                        let metadata = std::fs::symlink_metadata(&directory)?;
                        ensure!(
                            metadata.is_dir() && !metadata.file_type().is_symlink(),
                            "received paths may not resolve outside the receive folder"
                        );
                    }
                    Err(error) => {
                        return Err(error).with_context(|| {
                            format!(
                                "Cannot create the receive subdirectory {}",
                                directory.display()
                            )
                        });
                    }
                }
            }
            Err(error) => {
                return Err(error).with_context(|| {
                    format!(
                        "Cannot inspect the receive subdirectory {}",
                        directory.display()
                    )
                });
            }
        }
    }
    Ok(directory)
}

/// Marks a received file as downloaded from the network (macOS
/// `com.apple.quarantine`), like AirDrop and browsers do. Without it, Gatekeeper
/// never checks a received app, script or installer before it runs, and a LAN
/// peer could hand the user something that executes on a double-click.
/// Best effort: a filesystem without extended attributes keeps the file.
#[cfg(target_os = "macos")]
pub fn mark_quarantined(path: &Path) {
    use std::os::unix::ffi::OsStrExt;
    let Ok(c_path) = std::ffi::CString::new(path.as_os_str().as_bytes()) else {
        return;
    };
    let seconds = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0);
    // flags;timestamp (hex);agent;event id — the format LaunchServices writes.
    let value = format!(
        "0081;{seconds:08x};LocalSend;{}",
        uuid::Uuid::new_v4().to_string().to_uppercase()
    );
    // SAFETY: both strings are valid NUL-terminated C strings and `value` is a
    // live buffer of `value.len()` bytes for the duration of the call.
    let result = unsafe {
        libc::setxattr(
            c_path.as_ptr(),
            c"com.apple.quarantine".as_ptr(),
            value.as_ptr().cast(),
            value.len(),
            0,
            0,
        )
    };
    if result != 0 {
        tracing::debug!(
            path = %path.display(),
            error = %std::io::Error::last_os_error(),
            "Cannot mark a received file as quarantined"
        );
    }
}

#[cfg(not(target_os = "macos"))]
pub fn mark_quarantined(_path: &Path) {}

/// Creates `target` as a link to `source` without ever replacing an existing
/// file. Removes `source` on success, so the caller's staging file is gone.
fn link_without_replace(source: &Path, target: &Path) -> std::io::Result<()> {
    match std::fs::hard_link(source, target) {
        Ok(()) => {
            let _ = std::fs::remove_file(source);
            Ok(())
        }
        Err(error)
            // `rename` cannot cross devices either, so only a filesystem
            // without hard links takes the fallback.
            if error.kind() == ErrorKind::Unsupported =>
        {
            // Filesystems without hard links (some FAT/exFAT mounts): fall
            // back to a rename. The existence check narrows the race window;
            // LocalSend has no way to atomically rename without replacing.
            if std::fs::symlink_metadata(target).is_ok() {
                return Err(std::io::Error::from(ErrorKind::AlreadyExists));
            }
            std::fs::rename(source, target)
        }
        Err(error) => Err(error),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_plain_names_and_folder_trees() {
        assert_eq!(
            relative_name("photo.jpg").unwrap(),
            PathBuf::from("photo.jpg")
        );
        assert_eq!(
            relative_name("album/2024/photo.jpg").unwrap(),
            PathBuf::from("album/2024/photo.jpg")
        );
        // The sender may use Windows separators.
        assert_eq!(
            relative_name("album\\photo.jpg").unwrap(),
            PathBuf::from("album/photo.jpg")
        );
    }

    #[test]
    fn rejects_traversal_absolute_and_invalid_names() {
        for name in [
            "",
            "..",
            "../secret",
            "a/../../secret",
            "/etc/passwd",
            "C:\\Windows\\win.ini",
            "a\0b",
            "../",
        ] {
            assert!(relative_name(name).is_err(), "{name:?} must be rejected");
        }
    }

    #[test]
    fn sanitizes_illegal_characters_per_segment() {
        let path = relative_name("we?ird:name*.txt").unwrap();
        assert_eq!(path, PathBuf::from("we_ird_name_.txt"));
    }

    #[test]
    fn publish_never_overwrites_and_suffixes_collisions() {
        let root = tempfile::tempdir().unwrap();
        let downloads = root.path().join("Downloads");
        std::fs::create_dir_all(&downloads).unwrap();

        let first = downloads.join("report.pdf");
        std::fs::write(&first, b"first").unwrap();

        let staging = root.path().join("staging.part");
        std::fs::write(&staging, b"second").unwrap();
        let published = publish(&staging, &downloads, Path::new("report.pdf")).unwrap();
        assert_eq!(published, downloads.join("report (1).pdf"));
        assert_eq!(std::fs::read(&first).unwrap(), b"first");
        assert_eq!(std::fs::read(&published).unwrap(), b"second");
        assert!(!staging.exists());

        let staging = root.path().join("staging2.part");
        std::fs::write(&staging, b"third").unwrap();
        let published = publish(&staging, &downloads, Path::new("report.pdf")).unwrap();
        assert_eq!(published, downloads.join("report (2).pdf"));
        assert_eq!(std::fs::read(&published).unwrap(), b"third");
    }

    #[test]
    fn publish_creates_subdirectories_and_keeps_the_name() {
        let root = tempfile::tempdir().unwrap();
        let downloads = root.path().join("Downloads");
        std::fs::create_dir_all(&downloads).unwrap();
        let staging = root.path().join("staging.part");
        std::fs::write(&staging, b"data").unwrap();

        let relative = relative_name("album/photo.jpg").unwrap();
        let published = publish(&staging, &downloads, &relative).unwrap();
        assert_eq!(published, downloads.join("album/photo.jpg"));
        assert_eq!(std::fs::read(&published).unwrap(), b"data");
    }

    #[test]
    fn temp_files_are_unique_and_inside_the_receive_directory() {
        let root = tempfile::tempdir().unwrap();
        let temp_dir = root.path().join(".staging");
        std::fs::create_dir_all(&temp_dir).unwrap();
        let first = temp_path(&temp_dir);
        let second = temp_path(&temp_dir);
        assert_ne!(first, second);
        assert!(first.starts_with(&temp_dir));
    }

    /// A folder name is peer-controlled. If a directory of the same name in the
    /// receive folder is a symlink, the resolved path is outside the folder and
    /// the file must not be written through it.
    #[cfg(unix)]
    #[test]
    fn publish_refuses_symlinked_subdirectories_outside_the_receive_folder() {
        let root = tempfile::tempdir().unwrap();
        let downloads = root.path().join("Downloads");
        let outside = root.path().join("outside");
        std::fs::create_dir_all(&downloads).unwrap();
        std::fs::create_dir_all(&outside).unwrap();
        std::os::unix::fs::symlink(&outside, downloads.join("album")).unwrap();

        let staging = root.path().join("staging.part");
        std::fs::write(&staging, b"evil").unwrap();
        let relative = relative_name("album/evil.txt").unwrap();
        let error = publish(&staging, &downloads, &relative).expect_err("must refuse");
        assert!(format!("{error:#}").contains("outside the receive folder"));
        assert!(
            !outside.join("evil.txt").exists(),
            "a symlinked folder must not let a peer escape the receive folder"
        );
    }

    /// A batch of same-named files must publish with O(n) link attempts, not
    /// O(n^2): a peer can force thousands of identical names, and the actor
    /// awaits this call (NET-04).
    #[test]
    fn same_named_batches_use_the_collision_hint_and_stay_linear() {
        const FILES: u32 = 200;
        let root = tempfile::tempdir().unwrap();
        let downloads = root.path().join("Downloads");
        std::fs::create_dir_all(&downloads).unwrap();
        let relative = relative_name("photo.jpg").unwrap();

        let before = link_attempt_count();
        let mut hint = 0u32;
        for index in 0..FILES {
            let staging = root.path().join(format!("staging-{index}.part"));
            std::fs::write(&staging, format!("payload {index}")).unwrap();
            let (published, next) =
                publish_from(&staging, &downloads, &relative, hint).expect("publish");
            hint = next;
            assert_eq!(
                published,
                downloads.join(if index == 0 {
                    "photo.jpg".to_string()
                } else {
                    format!("photo ({index}).jpg")
                })
            );
        }
        let attempts = link_attempt_count() - before;
        assert_eq!(
            attempts, FILES as usize,
            "each same-named file must need exactly one link attempt, got {attempts}"
        );
        assert_eq!(
            std::fs::read(downloads.join("photo (199).jpg")).unwrap(),
            b"payload 199"
        );
    }

    /// The hint survives a publish failure without corrupting names: an
    /// existing file forces the next publish to try the next suffix.
    #[test]
    fn a_publish_hint_skips_known_collisions() {
        let root = tempfile::tempdir().unwrap();
        let downloads = root.path().join("Downloads");
        std::fs::create_dir_all(&downloads).unwrap();
        std::fs::write(downloads.join("a.txt"), b"existing").unwrap();
        let relative = relative_name("a.txt").unwrap();
        let staging = root.path().join("staging.part");
        std::fs::write(&staging, b"new").unwrap();
        let (published, next) = publish_from(&staging, &downloads, &relative, 0).unwrap();
        assert_eq!(published, downloads.join("a (1).txt"));
        assert_eq!(next, 2);

        let staging = root.path().join("staging2.part");
        std::fs::write(&staging, b"newer").unwrap();
        let (published, _) = publish_from(&staging, &downloads, &relative, next).unwrap();
        assert_eq!(published, downloads.join("a (2).txt"));
    }

    /// A symlinked folder must not even get directories created through it:
    /// the old check ran after `create_dir_all` had already followed the link.
    #[cfg(unix)]
    #[test]
    fn publish_creates_nothing_through_a_symlinked_folder() {
        let root = tempfile::tempdir().unwrap();
        let downloads = root.path().join("Downloads");
        let outside = root.path().join("outside");
        std::fs::create_dir_all(&downloads).unwrap();
        std::fs::create_dir_all(&outside).unwrap();
        std::os::unix::fs::symlink(&outside, downloads.join("album")).unwrap();

        let staging = root.path().join("staging.part");
        std::fs::write(&staging, b"evil").unwrap();
        let relative = relative_name("album/nested/evil.txt").unwrap();
        assert!(publish(&staging, &downloads, &relative).is_err());
        assert!(
            !outside.join("nested").exists(),
            "no directory may be created outside the receive folder"
        );
    }

    /// A file in the way of a peer-supplied folder name is reported, not
    /// replaced.
    #[test]
    fn publish_refuses_a_file_where_a_folder_is_needed() {
        let root = tempfile::tempdir().unwrap();
        let downloads = root.path().join("Downloads");
        std::fs::create_dir_all(&downloads).unwrap();
        std::fs::write(downloads.join("album"), b"a file").unwrap();
        let staging = root.path().join("staging.part");
        std::fs::write(&staging, b"data").unwrap();
        let relative = relative_name("album/photo.jpg").unwrap();
        assert!(publish(&staging, &downloads, &relative).is_err());
        assert_eq!(std::fs::read(downloads.join("album")).unwrap(), b"a file");
    }

    /// Received files carry the quarantine attribute so Gatekeeper checks
    /// them before they run.
    #[cfg(target_os = "macos")]
    #[test]
    fn published_files_are_quarantined() {
        use std::os::unix::ffi::OsStrExt;
        let root = tempfile::tempdir().unwrap();
        let downloads = root.path().join("Downloads");
        std::fs::create_dir_all(&downloads).unwrap();
        let staging = root.path().join("staging.part");
        std::fs::write(&staging, b"#!/bin/sh").unwrap();
        let published = publish(&staging, &downloads, Path::new("run.command")).unwrap();

        let c_path = std::ffi::CString::new(published.as_os_str().as_bytes()).unwrap();
        let mut buffer = [0u8; 128];
        // SAFETY: valid C strings and a live buffer of the given length.
        let length = unsafe {
            libc::getxattr(
                c_path.as_ptr(),
                c"com.apple.quarantine".as_ptr(),
                buffer.as_mut_ptr().cast(),
                buffer.len(),
                0,
                0,
            )
        };
        assert!(length > 0, "the quarantine attribute must be set");
        let value = std::str::from_utf8(&buffer[..length as usize]).unwrap();
        assert!(value.starts_with("0081;") && value.contains(";LocalSend;"));
    }
}
