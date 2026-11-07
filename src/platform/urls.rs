//! `file:` URLs the system hands the app (drops and Finder copies).
//!
//! nsh 0.4 gives the app the URLs a drag or a copy put on the pasteboard. A
//! Finder drag or copy can carry a **file reference URL**
//! (`file:///.file/id=…`) instead of a path URL: it names no path until AppKit
//! resolves it. nsh resolves them since 0.4.0 was released
//! (`local_file_uri` uses `-[NSURL filePathURL]`); on 0.4.0 the raw reference
//! URL reaches the app, and the path a plain `file:` decode produces does not
//! exist, so dropped or pasted files were silently dropped. Resolve them here.

use std::path::PathBuf;

/// The local path of a Finder file reference URL, or `None` when the URL names
/// no resolvable file or the platform has no file reference URLs.
pub fn resolved_reference_path(uri: &str) -> Option<PathBuf> {
    if !uri.starts_with("file:") {
        return None;
    }
    #[cfg(target_os = "macos")]
    {
        macos::path(uri)
    }
    #[cfg(not(target_os = "macos"))]
    {
        // Only macOS hands out `/.file/id=` URLs; another platform's decoder
        // reports the unresolvable path itself.
        None
    }
}

#[cfg(target_os = "macos")]
mod macos {
    use objc2_foundation::{NSString, NSURL};
    use std::path::PathBuf;

    /// `-[NSURL filePathURL]`: the path URL a file reference URL stands for.
    pub(super) fn path(uri: &str) -> Option<PathBuf> {
        // SAFETY: `NSString` and `NSURL` are immutable value objects; nothing
        // here needs the main thread or mutates shared state.
        let string = NSString::from_str(uri);
        let url = NSURL::URLWithString(&string)?;
        // A marker in an ordinary path or fragment is not a file reference.
        // Reject remote authorities before returning a path to either caller.
        if !url.isFileURL() || !url.isFileReferenceURL() {
            return None;
        }
        if url.host().is_some_and(|host| {
            let host = host.to_string();
            !host.is_empty() && !host.eq_ignore_ascii_case("localhost")
        }) {
            return None;
        }
        let path = url.filePathURL()?.path()?;
        Some(PathBuf::from(path.to_string()))
    }
}

#[cfg(all(test, target_os = "macos"))]
mod tests {
    use super::*;
    use objc2_foundation::{NSString, NSURL};

    #[test]
    fn ordinary_paths_and_fragments_are_not_file_references() {
        for uri in [
            "file://otherhost/etc/hosts#/.file/id=x",
            "file:///etc/hosts#/.file/id=x",
            "file://otherhost/tmp/.file/id=abc",
            "file:///tmp/.file/id=abc",
        ] {
            assert_eq!(resolved_reference_path(uri), None, "{uri}");
        }
    }

    #[test]
    fn finder_reference_urls_resolve_to_their_path() {
        let directory =
            std::env::temp_dir().join(format!("localsend-ref-url-{}", std::process::id()));
        std::fs::create_dir_all(&directory).expect("temp directory");
        let file = directory.join("a b.txt");
        std::fs::write(&file, b"x").expect("temp file");

        // What Finder puts on the drag pasteboard for this file.
        let path_url =
            NSURL::fileURLWithPath(&NSString::from_str(file.to_str().expect("utf-8 path")));
        let reference = path_url.fileReferenceURL().expect("reference URL");
        let uri = reference
            .absoluteString()
            .expect("absolute string")
            .to_string();
        assert!(
            reference.isFileReferenceURL(),
            "Finder reference URL: {uri}"
        );

        // Even a genuine reference URL must not bypass the local-host check.
        let remote = uri.replacen("file://", "file://otherhost", 1);
        assert_eq!(resolved_reference_path(&remote), None);

        // `/var` is a symlink to `/private/var` on macOS: compare real paths.
        let resolved =
            resolved_reference_path(&uri).and_then(|path| std::fs::canonicalize(path).ok());
        assert_eq!(resolved, std::fs::canonicalize(&file).ok());

        // A path URL is not a reference URL: its decoder handles it.
        assert_eq!(resolved_reference_path("file:///etc/hosts"), None);

        std::fs::remove_dir_all(&directory).ok();
    }
}
