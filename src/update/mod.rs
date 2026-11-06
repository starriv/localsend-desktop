//! Self-update from the project's GitHub releases.
//!
//! The release workflow uploads two assets last, after every installer:
//! `latest.json` (the [`Manifest`]: version, and per platform the installer's
//! URL, size and SHA-256) and `latest.json.sig` (an Ed25519 signature of the
//! manifest's exact bytes, made with the key in the `UPDATE_SIGNING_KEY`
//! secret). The app fetches both through GitHub's
//! `releases/latest/download/…` redirect, so no API call (and no API rate
//! limit) is involved, and a release whose installers are still building has
//! no manifest yet: it is simply not offered.
//!
//! Trust: the signature is checked against [`PUBLIC_KEY`] before the manifest
//! is even parsed, and the downloaded installer must match the size and
//! SHA-256 the signed manifest states. A compromised GitHub account cannot
//! ship an update without the signing key.
//!
//! This module is plain tokio code (no nsh types), like [`crate::net`];
//! `model::updater` drives it from the session and [`install`] replaces the
//! installed app.

pub mod install;

use anyhow::{Context, anyhow};
use ed25519_dalek::{Signature, VerifyingKey};
use futures_util::StreamExt;
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;
use tokio::io::AsyncWriteExt;

/// The releases page (shown when an update has to be installed by hand).
pub const RELEASES_URL: &str = "https://github.com/starriv/localsend-desktop/releases";

/// The source repository (About › GitHub).
pub const REPOSITORY_URL: &str = "https://github.com/starriv/localsend-desktop";

/// The manifest of the newest published release.
const MANIFEST_URL: &str =
    "https://github.com/starriv/localsend-desktop/releases/latest/download/latest.json";

/// Overrides [`MANIFEST_URL`] (and allows plain HTTP) for testing the
/// updater against a local server; the signature is still required.
const MANIFEST_URL_ENV: &str = "LOCALSEND_UPDATE_MANIFEST";

/// The Ed25519 public key the manifest signature is checked against. The
/// private half is the `UPDATE_SIGNING_KEY` repository secret (PEM); see
/// `docs/BUILD.md` for rotating it.
pub const PUBLIC_KEY: [u8; 32] = [
    0x96, 0x47, 0x6b, 0xb4, 0x70, 0xc7, 0xf4, 0x18, 0x85, 0xa8, 0xd0, 0x40, 0x17, 0x14, 0xb4, 0x55,
    0x51, 0x7b, 0x36, 0xd7, 0x95, 0x92, 0x89, 0x9f, 0xd2, 0xf6, 0xa4, 0xe8, 0xa3, 0x64, 0x73, 0xbb,
];

/// The manifest schema this build understands.
const MANIFEST_SCHEMA: u32 = 1;

/// The largest manifest read (it is a few hundred bytes).
const MANIFEST_LIMIT: usize = 64 * 1024;

/// The largest installer accepted, whatever the manifest says.
const ASSET_LIMIT: u64 = 512 * 1024 * 1024;

/// What failed, as the UI words it. The detail goes to the log only.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FailureKind {
    /// The server could not be reached or answered with an error.
    Network,
    /// The manifest signature, the installer size or its SHA-256 did not
    /// match: the files were discarded.
    Verification,
    /// Unpacking, checking or starting the new version failed.
    Install,
}

/// An updater error: its [`FailureKind`] and the detail for the log.
#[derive(Debug)]
pub struct Error {
    pub kind: FailureKind,
    pub detail: anyhow::Error,
}

impl Error {
    fn network(detail: impl Into<anyhow::Error>) -> Self {
        Self {
            kind: FailureKind::Network,
            detail: detail.into(),
        }
    }

    fn verification(detail: impl Into<anyhow::Error>) -> Self {
        Self {
            kind: FailureKind::Verification,
            detail: detail.into(),
        }
    }

    pub(crate) fn install(detail: impl Into<anyhow::Error>) -> Self {
        Self {
            kind: FailureKind::Install,
            detail: detail.into(),
        }
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:?}: {:#}", self.kind, self.detail)
    }
}

impl std::error::Error for Error {}

pub type Result<T> = std::result::Result<T, Error>;

/// `latest.json`, as the release workflow writes it.
#[derive(Clone, Debug, Deserialize, PartialEq)]
pub struct Manifest {
    pub schema: u32,
    /// The release version, without the `v` of its tag.
    pub version: String,
    /// The release page.
    #[serde(default)]
    pub notes_url: Option<String>,
    /// Installers by [`platform_key`].
    #[serde(default)]
    pub assets: BTreeMap<String, Asset>,
}

/// One installer in the manifest.
#[derive(Clone, Debug, Deserialize, PartialEq)]
pub struct Asset {
    /// The file name (also used for the downloaded file).
    pub name: String,
    pub url: String,
    /// Lowercase hex.
    pub sha256: String,
    pub size: u64,
}

/// A release newer than the running app.
#[derive(Clone, Debug, PartialEq)]
pub struct Release {
    pub version: semver::Version,
    /// Where to download it by hand.
    pub page: String,
    /// The installer for this platform, if the release has one.
    pub asset: Option<Asset>,
}

/// The manifest key of the installer this build updates itself with.
pub const fn platform_key() -> Option<&'static str> {
    if cfg!(all(target_os = "macos", target_arch = "aarch64")) {
        Some("macos-aarch64")
    } else if cfg!(all(target_os = "windows", target_arch = "x86_64")) {
        Some("windows-x86_64")
    } else {
        None
    }
}

/// The running version.
pub fn current_version() -> semver::Version {
    semver::Version::parse(env!("CARGO_PKG_VERSION")).expect("Cargo.toml has a semver version")
}

/// The HTTP client for update requests.
///
/// Unlike the LAN client of the core it trusts the system's certificate
/// store, follows GitHub's redirects to its download CDN and honours the
/// proxy environment variables.
pub fn client() -> Result<reqwest::Client> {
    // reqwest is built without a bundled crypto provider (the core's choice);
    // ring is the one the rest of the app installs.
    let _ = rustls::crypto::ring::default_provider().install_default();
    let https_only = std::env::var_os(MANIFEST_URL_ENV).is_none();
    reqwest::Client::builder()
        .user_agent(concat!("LocalSend-Desktop/", env!("CARGO_PKG_VERSION")))
        .connect_timeout(Duration::from_secs(20))
        .read_timeout(Duration::from_secs(60))
        .redirect(reqwest::redirect::Policy::limited(10))
        .https_only(https_only)
        .build()
        .map_err(Error::network)
}

fn manifest_url() -> String {
    std::env::var(MANIFEST_URL_ENV).unwrap_or_else(|_| MANIFEST_URL.to_owned())
}

/// Fetches and verifies the latest manifest: `Ok(None)` when it is not newer
/// than `current`, or when the latest release has no manifest (yet).
pub async fn check(client: &reqwest::Client, current: &semver::Version) -> Result<Option<Release>> {
    let url = manifest_url();
    let Some(manifest) = fetch(client, &url, MANIFEST_LIMIT).await? else {
        tracing::info!(%url, "The latest release has no update manifest");
        return Ok(None);
    };
    let signature = fetch(client, &format!("{url}.sig"), 1024)
        .await?
        .ok_or_else(|| Error::verification(anyhow!("the manifest has no signature")))?;
    let key = VerifyingKey::from_bytes(&PUBLIC_KEY).map_err(Error::verification)?;
    let manifest = verified_manifest(&manifest, &signature, &key)?;
    newer_release(manifest, current)
}

/// GETs `url` into memory, at most `limit` bytes; `Ok(None)` on 404.
async fn fetch(client: &reqwest::Client, url: &str, limit: usize) -> Result<Option<Vec<u8>>> {
    let response = client.get(url).send().await.map_err(Error::network)?;
    if response.status() == reqwest::StatusCode::NOT_FOUND {
        return Ok(None);
    }
    let response = response.error_for_status().map_err(Error::network)?;
    let mut body = Vec::new();
    let mut stream = response.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(Error::network)?;
        if body.len() + chunk.len() > limit {
            return Err(Error::verification(anyhow!(
                "{url} is larger than {limit} bytes"
            )));
        }
        body.extend_from_slice(&chunk);
    }
    Ok(Some(body))
}

/// Checks `signature` over the exact `bytes`, then parses them.
pub fn verified_manifest(bytes: &[u8], signature: &[u8], key: &VerifyingKey) -> Result<Manifest> {
    let signature: &[u8; 64] = signature
        .try_into()
        .map_err(|_| Error::verification(anyhow!("the signature is not 64 bytes")))?;
    key.verify_strict(bytes, &Signature::from_bytes(signature))
        .map_err(|error| Error::verification(anyhow!("bad manifest signature: {error}")))?;
    let manifest: Manifest = serde_json::from_slice(bytes)
        .context("the manifest is not valid JSON")
        .map_err(Error::verification)?;
    if manifest.schema != MANIFEST_SCHEMA {
        return Err(Error::verification(anyhow!(
            "unsupported manifest schema {}",
            manifest.schema
        )));
    }
    Ok(manifest)
}

/// The release `manifest` describes, when it is newer than `current`.
pub fn newer_release(manifest: Manifest, current: &semver::Version) -> Result<Option<Release>> {
    let version = semver::Version::parse(manifest.version.trim_start_matches('v'))
        .context("the manifest version is not semver")
        .map_err(Error::verification)?;
    if version <= *current {
        return Ok(None);
    }
    let asset = match platform_key() {
        Some(key) => manifest.assets.get(key).cloned(),
        None => None,
    };
    if let Some(asset) = &asset {
        validate_asset(asset)?;
    }
    let page = manifest
        .notes_url
        .filter(|url| url.starts_with("https://"))
        .unwrap_or_else(|| format!("{RELEASES_URL}/tag/v{version}"));
    Ok(Some(Release {
        version,
        page,
        asset,
    }))
}

/// Rejects a manifest entry the download must not act on.
fn validate_asset(asset: &Asset) -> Result<()> {
    let plain_name = !asset.name.is_empty()
        && !asset.name.starts_with('.')
        && !asset.name.contains(['/', '\\', ':'])
        && asset.name.len() <= 200;
    if !plain_name {
        bail_verification(format!("unsafe asset name {:?}", asset.name))?;
    }
    if asset.size == 0 || asset.size > ASSET_LIMIT {
        bail_verification(format!("unexpected asset size {}", asset.size))?;
    }
    if asset.sha256.len() != 64 || !asset.sha256.bytes().all(|b| b.is_ascii_hexdigit()) {
        bail_verification(format!("malformed sha256 {:?}", asset.sha256))?;
    }
    Ok(())
}

fn bail_verification(message: String) -> Result<()> {
    Err(Error::verification(anyhow!(message)))
}

/// The folder downloads and unpacked updates are staged in.
pub fn staging_dir() -> PathBuf {
    std::env::temp_dir().join("localsend-update")
}

/// Downloads `asset` into a fresh `dir/<version>/`, counting the bytes in
/// `received`, and checks its size and SHA-256. A file that does not match is
/// deleted.
pub async fn download(
    client: &reqwest::Client,
    asset: &Asset,
    dir: &Path,
    received: Arc<AtomicU64>,
) -> Result<PathBuf> {
    validate_asset(asset)?;
    // Older staged versions are of no use any more.
    let _ = tokio::fs::remove_dir_all(dir).await;
    tokio::fs::create_dir_all(dir)
        .await
        .map_err(Error::install)?;
    let path = dir.join(&asset.name);
    let partial = dir.join(format!("{}.part", asset.name));
    let result = download_to(client, asset, &partial, &received).await;
    match result {
        Ok(()) => {
            tokio::fs::rename(&partial, &path)
                .await
                .map_err(Error::install)?;
            Ok(path)
        }
        Err(error) => {
            let _ = tokio::fs::remove_file(&partial).await;
            Err(error)
        }
    }
}

async fn download_to(
    client: &reqwest::Client,
    asset: &Asset,
    partial: &Path,
    received: &AtomicU64,
) -> Result<()> {
    let response = client
        .get(&asset.url)
        .send()
        .await
        .and_then(reqwest::Response::error_for_status)
        .map_err(Error::network)?;
    let mut file = tokio::fs::File::create(partial)
        .await
        .map_err(Error::install)?;
    let mut hasher = Sha256::new();
    let mut total = 0_u64;
    let mut stream = response.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(Error::network)?;
        total += chunk.len() as u64;
        if total > asset.size {
            return Err(Error::verification(anyhow!(
                "{} is larger than the {} bytes the manifest states",
                asset.name,
                asset.size
            )));
        }
        hasher.update(&chunk);
        file.write_all(&chunk).await.map_err(Error::install)?;
        received.store(total, Ordering::Relaxed);
    }
    file.flush().await.map_err(Error::install)?;
    file.sync_all().await.map_err(Error::install)?;
    if total != asset.size {
        return Err(Error::verification(anyhow!(
            "{} is {total} bytes, the manifest states {}",
            asset.name,
            asset.size
        )));
    }
    let digest = hex(&hasher.finalize());
    if !digest.eq_ignore_ascii_case(&asset.sha256) {
        return Err(Error::verification(anyhow!(
            "{} has sha256 {digest}, the manifest states {}",
            asset.name,
            asset.sha256
        )));
    }
    Ok(())
}

fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write;
    bytes.iter().fold(String::new(), |mut out, byte| {
        let _ = write!(out, "{byte:02x}");
        out
    })
}

/// Fails unless `version` parses (for callers holding a string).
pub fn parse_version(version: &str) -> anyhow::Result<semver::Version> {
    semver::Version::parse(version.trim_start_matches('v'))
        .map_err(|error| anyhow!("{version:?} is not a version: {error}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_dalek::{Signer, SigningKey};

    fn key() -> SigningKey {
        SigningKey::from_bytes(&[7; 32])
    }

    fn manifest_json(version: &str) -> String {
        format!(
            r#"{{"schema":1,"version":"{version}","notes_url":"https://example.com/notes","assets":{{
                "macos-aarch64":{{"name":"LocalSend-{version}-macos-arm64.zip","url":"https://example.com/a.zip","sha256":"{sha}","size":3}},
                "windows-x86_64":{{"name":"LocalSend-{version}-windows-x86_64-setup.exe","url":"https://example.com/a.exe","sha256":"{sha}","size":3}}
            }}}}"#,
            sha = "ab".repeat(32)
        )
    }

    #[test]
    fn a_signed_manifest_is_accepted() {
        let json = manifest_json("1.2.0");
        let signature = key().sign(json.as_bytes()).to_bytes();
        let manifest =
            verified_manifest(json.as_bytes(), &signature, &key().verifying_key()).unwrap();
        assert_eq!(manifest.version, "1.2.0");
        assert_eq!(manifest.assets.len(), 2);
    }

    #[test]
    fn a_tampered_manifest_is_rejected() {
        let json = manifest_json("1.2.0");
        let signature = key().sign(json.as_bytes()).to_bytes();
        let tampered = json.replace("1.2.0", "9.9.9");
        let error =
            verified_manifest(tampered.as_bytes(), &signature, &key().verifying_key()).unwrap_err();
        assert_eq!(error.kind, FailureKind::Verification);
        let other = SigningKey::from_bytes(&[8; 32]);
        let error =
            verified_manifest(json.as_bytes(), &signature, &other.verifying_key()).unwrap_err();
        assert_eq!(error.kind, FailureKind::Verification);
        let error = verified_manifest(json.as_bytes(), &signature[..63], &key().verifying_key())
            .unwrap_err();
        assert_eq!(error.kind, FailureKind::Verification);
    }

    #[test]
    fn the_embedded_public_key_is_valid() {
        assert!(VerifyingKey::from_bytes(&PUBLIC_KEY).is_ok());
    }

    #[test]
    fn only_newer_versions_are_offered() {
        let current = semver::Version::parse("1.0.1").unwrap();
        let parse =
            |version: &str| serde_json::from_str::<Manifest>(&manifest_json(version)).unwrap();
        assert_eq!(newer_release(parse("1.0.1"), &current).unwrap(), None);
        assert_eq!(newer_release(parse("1.0.0"), &current).unwrap(), None);
        let release = newer_release(parse("1.0.2"), &current).unwrap().unwrap();
        assert_eq!(release.version, semver::Version::parse("1.0.2").unwrap());
        assert_eq!(release.page, "https://example.com/notes");
        assert_eq!(release.asset.is_some(), platform_key().is_some());
    }

    #[test]
    fn unsafe_assets_are_rejected() {
        let asset = Asset {
            name: "LocalSend.zip".into(),
            url: "https://example.com/a.zip".into(),
            sha256: "ab".repeat(32),
            size: 10,
        };
        assert!(validate_asset(&asset).is_ok());
        for name in ["../evil.zip", "a/b.zip", "a\\b.exe", ".hidden", ""] {
            let bad = Asset {
                name: name.into(),
                ..asset.clone()
            };
            assert!(validate_asset(&bad).is_err(), "{name:?} must be rejected");
        }
        let bad = Asset {
            sha256: "xyz".into(),
            ..asset.clone()
        };
        assert!(validate_asset(&bad).is_err());
        let bad = Asset {
            size: ASSET_LIMIT + 1,
            ..asset
        };
        assert!(validate_asset(&bad).is_err());
    }

    #[test]
    fn hex_is_lowercase() {
        assert_eq!(hex(&[0x00, 0xab, 0x0f]), "00ab0f");
    }
}
