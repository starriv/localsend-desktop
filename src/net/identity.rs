//! The device identity: a self-signed certificate and its private key.
//!
//! In HTTPS mode the certificate *is* the device: every peer identifies this
//! device by the uppercase-hex SHA-256 of the certificate's DER encoding. It
//! is therefore generated once, persisted under the app data directory and
//! reused for every later launch. Regenerating it would make every other
//! device see a brand-new machine (and invalidate pairings).

use anyhow::{Context, Result, anyhow};
use localsend::crypto::cert::{
    fingerprint_from_cert_der, generate_self_signed, verify_cert_from_pem,
};
use localsend::http::server::TlsConfig;
use std::path::{Path, PathBuf};

/// File name of the persisted identity inside the data directory.
const IDENTITY_FILE: &str = "identity.pem";

/// The app id from `nsh.json`: nsh keeps the app's storage (settings, transfer
/// history) in `<system data dir>/<app id>/storage.json`. The identity must
/// live in the same directory, so a backup/portable install carries both
/// (M-17).
const APP_DIR: &str = "org.localsend.desktop";

/// Directories this app used before [`APP_DIR`], newest first. Only read once,
/// when the new directory has no identity yet: peers, favorites and transfer
/// history refer to the old fingerprint, so the device must not appear new
/// after an upgrade.
const PREVIOUS_APP_DIRS: [&str; 3] = [
    // The app id before the rename to localsend-desktop.
    "org.localsend.client",
    // The identity directory before the app storage was unified.
    "localsend-client",
    // The app's name before the rename.
    "localsend-native",
];

/// This device's TLS identity.
#[derive(Clone)]
pub struct Identity {
    /// PEM-encoded certificate.
    pub cert_pem: String,
    /// PEM-encoded PKCS#8 private key.
    pub private_key_pem: String,
    /// Uppercase-hex SHA-256 of the certificate's DER encoding.
    pub fingerprint: String,
}

impl Identity {
    /// Loads the persisted identity, generating and storing one on first run.
    ///
    /// Generation is RSA-2048 and CPU-heavy, so it always runs on a blocking
    /// thread. An existing but unreadable/unparsable file is an error, never
    /// a reason to silently replace the device identity.
    pub async fn load_or_generate() -> Result<Self> {
        Self::load_or_generate_in(None).await
    }

    /// Like [`Identity::load_or_generate`], but with an explicit data
    /// directory instead of the platform one (tests, portable installs).
    pub async fn load_or_generate_in(directory: Option<PathBuf>) -> Result<Self> {
        let directory = match directory {
            Some(directory) => {
                std::fs::create_dir_all(&directory).with_context(|| {
                    format!(
                        "Cannot create the identity directory {}",
                        directory.display()
                    )
                })?;
                directory
            }
            None => data_dir()?,
        };
        tokio::task::spawn_blocking(move || Self::load_or_generate_sync(&directory))
            .await
            .context("The identity task failed")?
    }

    fn load_or_generate_sync(directory: &Path) -> Result<Self> {
        let path = directory.join(IDENTITY_FILE);
        match std::fs::read_to_string(&path) {
            Ok(text) => {
                // NET-19/N25: the 0600 mode is set at creation; a file that was
                // copied or chmod-ed afterwards must not keep leaking the key.
                #[cfg(unix)]
                ensure_private_file(&path)?;
                Self::from_pem(&text).with_context(|| {
                    format!(
                        "Invalid identity file {}; delete it to generate a new identity \
                     (other devices will then see this device as new)",
                        path.display()
                    )
                })
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                let generated = generate_self_signed()
                    .context("Could not generate the device identity certificate")?;
                let identity = Self {
                    cert_pem: generated.certificate_pem,
                    private_key_pem: generated.private_key_pem,
                    fingerprint: generated.fingerprint,
                };
                let contents = format!("{}{}", identity.cert_pem, identity.private_key_pem);
                write_private(&path, contents.as_bytes()).with_context(|| {
                    format!("Could not save the device identity to {}", path.display())
                })?;
                Ok(identity)
            }
            Err(error) => Err(error)
                .with_context(|| format!("Could not read the identity file {}", path.display())),
        }
    }

    /// Rebuilds the identity from a persisted `certificate PEM + key PEM` file.
    fn from_pem(text: &str) -> Result<Self> {
        let blocks = pem::parse_many(text)?;
        let certificate = blocks
            .iter()
            .find(|block| block.tag() == "CERTIFICATE")
            .ok_or_else(|| anyhow!("missing CERTIFICATE block"))?;
        let private_key = blocks
            .iter()
            .find(|block| block.tag().ends_with("PRIVATE KEY"))
            .ok_or_else(|| anyhow!("missing PRIVATE KEY block"))?;
        let cert_pem = pem::encode(certificate);
        let private_key_pem = pem::encode(private_key);
        // Reject a corrupt certificate now instead of failing (or, worse,
        // presenting a different fingerprint than the persisted one) at the
        // TLS handshake later.
        verify_cert_from_pem(cert_pem.clone(), None)
            .context("the persisted certificate is not a valid self-signed certificate")?;
        // NET-19/N25: a certificate and a key from different identities would
        // load "fine" and only fail as a confusing TLS error at the next
        // start. The core's client constructor builds a rustls config, whose
        // `CertifiedKey` compares the private key's public key with the
        // certificate's SubjectPublicKeyInfo and rejects a mismatch.
        localsend::http::client::LsHttpClientV2::try_new(&private_key_pem, &cert_pem, None, None)
            .context(
            "the persisted private key does not match the persisted certificate \
                 (delete the identity file to generate a new identity)",
        )?;
        let fingerprint = fingerprint_from_cert_der(certificate.contents());
        Ok(Self {
            cert_pem,
            private_key_pem,
            fingerprint,
        })
    }

    /// The TLS configuration for the HTTP server and the HTTP clients.
    pub fn tls_config(&self) -> TlsConfig {
        TlsConfig {
            cert: self.cert_pem.clone(),
            private_key: self.private_key_pem.clone(),
        }
    }

    /// The identity discovery uses to answer announcements over HTTPS.
    pub fn discovery_identity(&self) -> localsend::discovery::DeviceIdentity {
        localsend::discovery::DeviceIdentity {
            cert_pem: self.cert_pem.clone(),
            private_key_pem: self.private_key_pem.clone(),
        }
    }
}

/// The directory holding the identity.
///
/// The identity lives in `<system data dir>/org.localsend.desktop`, the same
/// directory nsh keeps `storage.json` (settings and transfer history) in, so a
/// backup or portable install carries the whole app state (M-17).
///
/// `LOCALSEND_DATA_DIR` overrides it (tests, portable installs). When the
/// platform data directory cannot be created — a sandboxed dev run, for
/// instance — a directory under the temp directory is used instead, so the
/// fingerprint still survives restarts while the temp directory lives.
pub fn data_dir() -> Result<PathBuf> {
    if let Some(directory) = std::env::var_os("LOCALSEND_DATA_DIR") {
        let directory = PathBuf::from(directory);
        std::fs::create_dir_all(&directory)
            .with_context(|| format!("Cannot create {}", directory.display()))?;
        return Ok(directory);
    }

    if let Some(data_dir) = dirs::data_dir() {
        let directory = data_dir.join(APP_DIR);
        if std::fs::create_dir_all(&directory).is_ok() {
            migrate_previous_identity(&data_dir, &directory);
            return Ok(directory);
        }
        tracing::warn!(
            "Cannot use the app data directory {}; falling back to the working directory",
            directory.display()
        );
    }

    // A read-only or sandboxed home: keep the identity in the temp directory
    // rather than failing to start (it is regenerated if the temp is cleared).
    // Use the same app id as the platform directory so a later start that can
    // reach the real data directory does not look like a new device.
    let root = std::env::temp_dir();
    let fallback = root.join(APP_DIR);
    std::fs::create_dir_all(&fallback)
        .with_context(|| format!("Cannot create {}", fallback.display()))?;
    // NET-19/N25: `/tmp/<name>` may have been pre-created by another local
    // user; writing the private key into a world-writable directory they
    // control would let them replace the identity. Refuse in that case.
    ensure_private_directory(&fallback)?;
    migrate_previous_identity(&root, &fallback);
    Ok(fallback)
}

/// One-time migration of the device identity from the app's previous data
/// directories (`org.localsend.client` before the rename to localsend-desktop,
/// the pre-unification `localsend-client` location and the pre-rename
/// `localsend-native`). Runs only when no identity exists under the
/// new name; a failure is never fatal (a new identity is generated later and
/// the peer continuity is reported in the log). The copied file is written with
/// the same atomic 0600 path as a generated one.
fn migrate_previous_identity(root: &Path, directory: &Path) {
    let target = directory.join(IDENTITY_FILE);
    if target.exists() {
        return;
    }
    for previous in PREVIOUS_APP_DIRS {
        let source = root.join(previous).join(IDENTITY_FILE);
        let Ok(contents) = std::fs::read(&source) else {
            continue;
        };
        match write_private(&target, &contents) {
            Ok(()) => {
                tracing::info!(
                    from = %source.display(),
                    to = %target.display(),
                    "migrated the device identity into the app data directory"
                );
                return;
            }
            Err(error) => {
                tracing::warn!(%error, "could not migrate the previous device identity");
                return;
            }
        }
    }
}

/// Re-checks the permissions of an existing identity file and tightens them
/// back to 0600 when they were widened after creation (NET-19/N25).
#[cfg(unix)]
fn ensure_private_file(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let metadata = std::fs::metadata(path)?;
    let mode = metadata.permissions().mode() & 0o777;
    if mode & 0o077 != 0 {
        tracing::warn!(
            mode = format!("{mode:o}"),
            path = %path.display(),
            "identity file was accessible to other users; restoring 0600"
        );
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
            .with_context(|| format!("Cannot restrict the identity file {}", path.display()))?;
    }
    Ok(())
}

/// A directory holding a private key must be owned by the current user and
/// not writable by group/world (NET-19/N25). Creates it 0700 when possible.
fn ensure_private_directory(directory: &Path) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};
        let metadata = std::fs::metadata(directory)?;
        let mode = metadata.permissions().mode() & 0o777;
        // SAFETY: `geteuid` only reads the process credential.
        let own = unsafe { libc::geteuid() };
        if metadata.uid() != own {
            anyhow::bail!(
                "the identity directory {} is owned by another user",
                directory.display()
            );
        }
        if mode & 0o077 != 0 {
            std::fs::set_permissions(directory, std::fs::Permissions::from_mode(0o700))
                .with_context(|| {
                    format!(
                        "Cannot restrict the identity directory {}",
                        directory.display()
                    )
                })?;
        }
    }
    #[cfg(not(unix))]
    let _ = directory;
    Ok(())
}

/// Writes the identity atomically, owner-readable only on Unix (it contains a
/// private key). The rename is what makes a half-written file impossible.
fn write_private(path: &Path, contents: &[u8]) -> Result<()> {
    use std::io::Write;

    let directory = path.parent().unwrap_or_else(|| Path::new("."));
    let temporary = directory.join(format!(".{IDENTITY_FILE}.{}.tmp", uuid::Uuid::new_v4()));

    let result = (|| -> Result<()> {
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options
            .open(&temporary)
            .with_context(|| format!("Cannot write {}", temporary.display()))?;
        file.write_all(contents)?;
        file.sync_all()?;
        drop(file);
        std::fs::rename(&temporary, path)?;
        Ok(())
    })();

    if result.is_err() {
        let _ = std::fs::remove_file(&temporary);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_files_without_a_certificate() {
        // `Identity` intentionally has no `Debug` (it holds a private key).
        let error = match Identity::from_pem(
            "-----BEGIN PRIVATE KEY-----\nAAAA\n-----END PRIVATE KEY-----\n",
        ) {
            Ok(_) => panic!("a key without a certificate must be rejected"),
            Err(error) => error,
        };
        assert!(format!("{error:#}").contains("CERTIFICATE"));
    }

    #[test]
    fn rejects_garbage() {
        assert!(Identity::from_pem("not a pem file").is_err());
    }

    /// NET-19/N25: a private key that does not belong to the certificate is
    /// rejected at load time instead of failing at the next TLS handshake.
    ///
    /// Ignored by default: generates two RSA identities (slow in debug).
    #[test]
    #[ignore = "generates RSA identities (slow in debug)"]
    fn rejects_a_key_that_does_not_match_the_certificate() {
        let first = generate_self_signed().expect("first identity");
        let second = generate_self_signed().expect("second identity");
        let combined = format!("{}{}", first.certificate_pem, second.private_key_pem);
        let error = match Identity::from_pem(&combined) {
            Ok(_) => panic!("a mismatched private key must be rejected"),
            Err(error) => error,
        };
        assert!(
            format!("{error:#}").contains("does not match"),
            "unexpected error: {error:#}"
        );
    }

    /// NET-19/N25: an identity file widened to group/world-readable is fixed
    /// back to 0600 on load.
    #[cfg(unix)]
    #[test]
    fn widened_identity_files_are_tightened_on_load() {
        use std::os::unix::fs::PermissionsExt;
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join(IDENTITY_FILE);
        std::fs::write(&path, b"placeholder").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        ensure_private_file(&path).expect("permissions can be tightened");
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
    }

    /// M-17: the identity moves to `<data dir>/org.localsend.desktop` (the
    /// directory nsh keeps settings/history in) and is carried over from the
    /// previous app directories exactly once.
    #[test]
    fn migrates_the_identity_into_the_app_data_directory_once() {
        let root = tempfile::tempdir().unwrap();
        let directory = root.path().join(APP_DIR);
        std::fs::create_dir_all(&directory).unwrap();

        // Newest previous location wins over the older app name.
        let old = root.path().join(PREVIOUS_APP_DIRS[0]);
        std::fs::create_dir_all(&old).unwrap();
        std::fs::write(old.join(IDENTITY_FILE), b"from-localsend-client").unwrap();
        let oldest = root.path().join(PREVIOUS_APP_DIRS[1]);
        std::fs::create_dir_all(&oldest).unwrap();
        std::fs::write(oldest.join(IDENTITY_FILE), b"from-localsend-native").unwrap();

        migrate_previous_identity(root.path(), &directory);
        let target = directory.join(IDENTITY_FILE);
        assert_eq!(std::fs::read(&target).unwrap(), b"from-localsend-client");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&target).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o600, "a migrated key must be private");
        }

        // A second run never overwrites the identity that is already in use.
        std::fs::write(old.join(IDENTITY_FILE), b"stale").unwrap();
        migrate_previous_identity(root.path(), &directory);
        assert_eq!(std::fs::read(&target).unwrap(), b"from-localsend-client");
    }

    /// The pre-rename location alone still keeps the fingerprint (peers refer
    /// to it); a missing old identity simply generates a new one later.
    #[test]
    fn migrates_from_the_previous_app_name_when_needed() {
        let root = tempfile::tempdir().unwrap();
        let directory = root.path().join(APP_DIR);
        std::fs::create_dir_all(&directory).unwrap();
        let previous = root.path().join(PREVIOUS_APP_DIRS[1]);
        std::fs::create_dir_all(&previous).unwrap();
        std::fs::write(previous.join(IDENTITY_FILE), b"legacy").unwrap();

        migrate_previous_identity(root.path(), &directory);
        assert_eq!(
            std::fs::read(directory.join(IDENTITY_FILE)).unwrap(),
            b"legacy"
        );

        // No previous identity at all: nothing is created here.
        let empty = root.path().join("fresh");
        std::fs::create_dir_all(&empty).unwrap();
        let empty_root = root.path().join("no-previous");
        std::fs::create_dir_all(&empty_root).unwrap();
        migrate_previous_identity(&empty_root, &empty);
        assert!(!empty.join(IDENTITY_FILE).exists());
    }

    /// NET-19/N25: a world-writable identity directory is tightened (or
    /// refused); the temp-dir fallback must never keep a peer-replaceable key.
    #[cfg(unix)]
    #[test]
    fn private_directories_are_tightened_or_refused() {
        use std::os::unix::fs::PermissionsExt;
        let root = tempfile::tempdir().unwrap();
        let directory = root.path().join("identity");
        std::fs::create_dir_all(&directory).unwrap();
        std::fs::set_permissions(&directory, std::fs::Permissions::from_mode(0o777)).unwrap();
        ensure_private_directory(&directory).expect("owned directory can be tightened");
        let mode = std::fs::metadata(&directory).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o700);
    }

    /// The fingerprint is the device identity: it must survive a restart.
    ///
    /// Ignored by default: the first load generates an RSA-2048 key, which is
    /// slow in an unoptimized build.
    #[tokio::test]
    #[ignore = "generates an RSA identity (slow in debug)"]
    async fn identity_is_persisted_across_loads() {
        let root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("target")
            .join(format!("net-identity-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();

        let first = Identity::load_or_generate_in(Some(root.clone()))
            .await
            .expect("first load generates");
        let second = Identity::load_or_generate_in(Some(root.clone()))
            .await
            .expect("second load reads the same file");
        assert_eq!(first.fingerprint, second.fingerprint);

        // The PEM text is re-encoded on load (line endings), so compare the
        // DER contents, which is what the TLS stack and the fingerprint use.
        let contents = |pem_text: &str, tag: &str| -> Vec<u8> {
            pem::parse_many(pem_text)
                .unwrap()
                .into_iter()
                .find(|block| block.tag() == tag)
                .unwrap_or_else(|| panic!("missing {tag} block"))
                .into_contents()
        };
        assert_eq!(
            contents(&first.cert_pem, "CERTIFICATE"),
            contents(&second.cert_pem, "CERTIFICATE")
        );
        let key_tag = "PRIVATE KEY";
        assert_eq!(
            contents(&first.private_key_pem, key_tag),
            contents(&second.private_key_pem, key_tag)
        );

        // The fingerprint is the uppercase-hex SHA-256 of the certificate DER.
        let certificate = contents(&first.cert_pem, "CERTIFICATE");
        assert_eq!(
            localsend::crypto::cert::fingerprint_from_cert_der(&certificate),
            first.fingerprint
        );
        assert!(
            first
                .fingerprint
                .chars()
                .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_lowercase())
        );

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(root.join("identity.pem"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777;
            assert_eq!(mode, 0o600, "the private key must stay owner-readable");
        }

        let _ = std::fs::remove_dir_all(&root);
    }
}
