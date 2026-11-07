//! Clipboard helpers.
//!
//! nsh 0.4 reads text, images and the `file:` URLs of files copied in Finder
//! (`clipboard_files()`), and writes text and images. So a file can be staged
//! for sending from the clipboard, but files cannot be put back on it.
//!
//! The send screen's "Send text" action reads [`text`]; its "Paste files"
//! action reads [`clipboard_files_as_paths`] (the files copied in Finder).

use nsh_core::prelude::*;
use std::path::{Path, PathBuf};

/// The text on the system clipboard (`""` when it holds none).
pub async fn text() -> String {
    clipboard_string().await
}

/// The `file:` URLs of the local files copied in Finder.
pub async fn files() -> Vec<String> {
    clipboard_files().await
}

/// The local files copied in Finder, as paths (`[]` when there are none).
///
/// nsh's [`Answer`] has no public constructor in 0.4, so a request cannot be
/// wrapped in a mapped `Answer`; an `async fn` is the same awaitable value:
///
/// ```ignore
/// let session = session.clone();
/// session.task(async move {
///     let paths = platform::clipboard::clipboard_files_as_paths().await;
///     if paths.is_empty() {
///         session.fail(session.text("send.clipboard_empty"));
///     } else {
///         session.add_paths(paths);
///     }
/// });
/// ```
pub async fn clipboard_files_as_paths() -> Vec<PathBuf> {
    clipboard_files()
        .await
        .iter()
        .filter_map(|url| path_from_file_url(url))
        .collect()
}

/// The `file:` URL for `path` (absolute; relative paths are made absolute
/// against the working directory).
pub fn file_url(path: impl AsRef<Path>) -> String {
    let path = path.as_ref();
    let path = std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf());
    let mut url = String::from("file://");
    for &byte in path.to_string_lossy().as_bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' | b'/' => {
                url.push(byte as char);
            }
            _ => url.push_str(&format!("%{byte:02X}")),
        }
    }
    url
}

/// The local path of a `file:` URL; `None` for another host or an empty URL.
///
/// Finder copies percent-encoded URLs (`file:///Users/me/My%20File.pdf`); a
/// platform that reports plain paths is accepted as-is. A copy can also carry
/// a file reference URL (`file:///.file/id=…`), which is resolved through
/// AppKit first ([`super::urls`]).
fn path_from_file_url(url: &str) -> Option<PathBuf> {
    if let Some(path) = super::urls::resolved_reference_path(url) {
        return Some(path);
    }
    let Some(rest) = url.strip_prefix("file:") else {
        return (!url.is_empty()).then(|| PathBuf::from(url));
    };
    let path = match rest.strip_prefix("//") {
        Some(rest) => {
            let (authority, path) = match rest.find('/') {
                Some(index) => (&rest[..index], &rest[index..]),
                None => (rest, "/"),
            };
            if !authority.is_empty() && !authority.eq_ignore_ascii_case("localhost") {
                // A file URL on another host is not a local file.
                return None;
            }
            path
        }
        None => rest,
    };
    let decoded = normalize_drive_path(percent_decode(path));
    (!decoded.is_empty()).then(|| PathBuf::from(decoded))
}

/// Windows file URLs carry the drive as the first path segment
/// (`file:///C:/Users/me/a.txt`): parsed like a Unix URL it becomes
/// `/C:/Users/me/a.txt`, which is not a path on that drive. Drop the leading
/// slash so `PathBuf::from("C:/Users/me/a.txt")` names the file; a URL path
/// that is not a drive letter (shared folders, plain paths) is left alone.
#[cfg(windows)]
fn normalize_drive_path(path: String) -> String {
    let bytes = path.as_bytes();
    let drive = bytes.len() >= 3
        && bytes[0] == b'/'
        && bytes[1].is_ascii_alphabetic()
        && bytes[2] == b':'
        && (bytes.len() == 3 || bytes[3] == b'/' || bytes[3] == b'\\');
    if drive { path[1..].to_owned() } else { path }
}

#[cfg(not(windows))]
fn normalize_drive_path(path: String) -> String {
    path
}

/// `%XX` escapes decoded as UTF-8 (`+` stays a literal plus, as in URLs).
fn percent_decode(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%'
            && index + 2 < bytes.len()
            && let (Some(high), Some(low)) = (hex(bytes[index + 1]), hex(bytes[index + 2]))
        {
            decoded.push(high << 4 | low);
            index += 3;
            continue;
        }
        decoded.push(bytes[index]);
        index += 1;
    }
    String::from_utf8_lossy(&decoded).into_owned()
}

/// The value of one hex digit.
fn hex(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finder_file_urls_become_paths() {
        assert_eq!(
            path_from_file_url("file:///Users/me/My%20File.pdf"),
            Some(PathBuf::from("/Users/me/My File.pdf"))
        );
        assert_eq!(
            path_from_file_url("file://localhost/tmp/%E4%BD%A0%E5%A5%BD.txt"),
            Some(PathBuf::from("/tmp/你好.txt"))
        );
        // A plain path is accepted as well.
        assert_eq!(
            path_from_file_url("/tmp/a b.txt"),
            Some(PathBuf::from("/tmp/a b.txt"))
        );
        // Another host and an empty value are not local files.
        assert_eq!(path_from_file_url("file://otherhost/tmp/x"), None);
        assert_eq!(
            path_from_file_url("file://otherhost/etc/hosts#/.file/id=x"),
            None
        );
        assert_eq!(
            path_from_file_url("file://otherhost/tmp/.file/id=abc"),
            None
        );
        assert_eq!(path_from_file_url(""), None);
    }

    #[test]
    fn file_urls_round_trip() {
        for path in ["/tmp/a b.txt", "/Users/me/100% done.png", "/tmp/你好.txt"] {
            let url = file_url(path);
            assert_eq!(path_from_file_url(&url), Some(PathBuf::from(path)), "{url}");
        }
    }

    /// A Windows clipboard reports `file:///C:/...`; the returned path must not
    /// start with a slash, or `C:` would be read as a directory on the current
    /// drive. (Only compiled and run on Windows.)
    #[cfg(windows)]
    #[test]
    fn windows_file_urls_keep_the_drive_letter() {
        assert_eq!(
            path_from_file_url("file:///C:/Users/me/a.txt"),
            Some(PathBuf::from("C:/Users/me/a.txt"))
        );
        assert_eq!(
            path_from_file_url("file:///C:/Program%20Files/LocalSend/a.txt"),
            Some(PathBuf::from("C:/Program Files/LocalSend/a.txt"))
        );
        // A path that merely starts with a letter is not a drive.
        assert_eq!(
            path_from_file_url("file:///Users/me/a.txt"),
            Some(PathBuf::from("/Users/me/a.txt"))
        );
    }
}
