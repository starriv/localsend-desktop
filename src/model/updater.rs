//! The session side of self-update: when to check, the status the settings
//! sheet shows, and handing a verified update to [`crate::update::install`].
//!
//! With [`Settings::auto_update`](crate::model::Settings::auto_update) on, the
//! session checks [`FIRST_CHECK_DELAY`] after launch and then every
//! [`CHECK_INTERVAL`]; a newer release is downloaded and verified right away,
//! and the user is told it is ready. Installing always waits for the user
//! ("Restart to update"), and never interrupts a transfer. The network work
//! runs on the tokio runtime ([`tokio::spawn`]), so hashing a 20 MB download
//! never stalls a frame; the engine thread only polls the byte counter.

use crate::model::Session;
use crate::update::{self, FailureKind, install};
use nsh_core::prelude::*;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

/// The first automatic check, after launch (the network node starts first).
const FIRST_CHECK_DELAY: Duration = Duration::from_secs(15);

/// Between automatic checks while the app keeps running.
const CHECK_INTERVAL: Duration = Duration::from_secs(24 * 60 * 60);

/// How often the download progress is shown.
const PROGRESS_INTERVAL: Duration = Duration::from_millis(250);

/// What the updater is doing, for the About section.
#[derive(Clone, Debug, Default, PartialEq)]
pub enum UpdateStatus {
    /// Not checked yet.
    #[default]
    Idle,
    Checking,
    /// The last check found nothing newer.
    UpToDate,
    Downloading {
        version: String,
        received: u64,
        total: u64,
    },
    /// Downloaded and verified: "Restart to update".
    Ready {
        version: String,
    },
    /// Newer, but this copy cannot install it itself: link to the page.
    Manual {
        version: String,
        page: String,
        reason: install::Manual,
    },
    /// The installer was started; the app is quitting.
    Installing,
    Failed(FailureKind),
}

impl UpdateStatus {
    /// Whether a check, download or install is running.
    pub fn busy(&self) -> bool {
        matches!(
            self,
            Self::Checking | Self::Downloading { .. } | Self::Installing
        )
    }
}

/// The update waiting for "Restart to update".
#[derive(Default)]
pub(crate) struct Updater {
    prepared: Option<install::Prepared>,
    /// The last version a "new version" toast was shown for (once per
    /// version and launch, however often the daily check finds it).
    announced: Option<String>,
}

/// Self-update is off in the demo, offline (tests) and when asked.
fn updates_disabled() -> bool {
    ["LOCALSEND_DEMO", "LOCALSEND_OFFLINE", "LOCALSEND_NO_UPDATE"]
        .iter()
        .any(|name| std::env::var_os(name).is_some())
}

impl Session {
    /// Starts the daily automatic check (idempotent per session: called once
    /// from [`Session::start`]).
    pub(crate) fn start_update_checks(&self) {
        if updates_disabled() {
            return;
        }
        let this = self.clone();
        self.task_detached(async move {
            timer(FIRST_CHECK_DELAY).await;
            loop {
                if this.read_settings(|state| state.settings.auto_update) {
                    this.check_for_updates(false);
                }
                timer(CHECK_INTERVAL).await;
            }
        });
    }

    /// Checks for a newer release and downloads it when this copy can
    /// install it. `manual` (the "Check for updates" button) also reports
    /// "up to date" and failures as a toast; the automatic check only logs
    /// them and shows them in About.
    pub fn check_for_updates(&self, manual: bool) {
        let status = self.update_status.get_untracked();
        if status.busy() || matches!(status, UpdateStatus::Ready { .. }) {
            return;
        }
        if updates_disabled() {
            if manual {
                self.update_status
                    .set(UpdateStatus::Failed(FailureKind::Network));
                self.fail(self.text("update.failed_network"));
            }
            return;
        }
        self.update_status.set(UpdateStatus::Checking);
        let this = self.clone();
        self.task_detached(async move {
            let checked = tokio::spawn(async {
                let client = update::client()?;
                let release = update::check(&client, &update::current_version()).await?;
                Ok::<_, update::Error>((client, release))
            })
            .await;
            match checked {
                Ok(Ok((_, None))) => {
                    tracing::info!("No newer release");
                    this.update_status.set(UpdateStatus::UpToDate);
                    if manual {
                        this.toast(this.text("update.up_to_date"));
                    }
                }
                Ok(Ok((client, Some(release)))) => this.found_release(client, release),
                Ok(Err(error)) => this.update_failed(error, manual),
                Err(error) => this.update_failed(update::Error::install(error), manual),
            }
        });
    }

    /// A newer `release`: download it, or point to its page.
    fn found_release(&self, client: reqwest::Client, release: update::Release) {
        let version = release.version.to_string();
        tracing::info!(%version, "A newer release is available");
        let target = install::target();
        let (target, asset) = match (target, release.asset) {
            (Ok(target), Some(asset)) => (target, asset),
            (target, _) => {
                let reason = target.err().unwrap_or(install::Manual::Unsupported);
                tracing::info!(?reason, "This copy cannot update itself");
                self.update_status.set(UpdateStatus::Manual {
                    version: version.clone(),
                    page: release.page,
                    reason,
                });
                self.announce(&version, "update.available_manual");
                return;
            }
        };

        let total = asset.size;
        self.update_status.set(UpdateStatus::Downloading {
            version: version.clone(),
            received: 0,
            total,
        });
        let received = Arc::new(AtomicU64::new(0));
        let this = self.clone();
        self.task_detached(async move {
            let mut job = tokio::spawn({
                let received = Arc::clone(&received);
                let semver = release.version.clone();
                async move {
                    let dir = update::staging_dir().join(semver.to_string());
                    let file = update::download(&client, &asset, &dir, received).await?;
                    tokio::task::spawn_blocking(move || install::prepare(&target, &file, &semver))
                        .await
                        .map_err(update::Error::install)?
                }
            });
            let result = loop {
                tokio::select! {
                    result = &mut job => break result,
                    () = timer(PROGRESS_INTERVAL) => {
                        this.update_status.set(UpdateStatus::Downloading {
                            version: version.clone(),
                            received: received.load(Ordering::Relaxed),
                            total,
                        });
                    }
                }
            };
            match result {
                Ok(Ok(prepared)) => {
                    tracing::info!(%version, payload = %prepared.payload.display(), "The update is ready");
                    this.updater.borrow_mut().prepared = Some(prepared);
                    this.update_status.set(UpdateStatus::Ready {
                        version: version.clone(),
                    });
                    this.announce(&version, "update.ready_toast");
                }
                // A failed download is retried by the next check.
                Ok(Err(error)) => this.update_failed(error, true),
                Err(error) => this.update_failed(update::Error::install(error), true),
            }
        });
    }

    /// Shows `key`'s toast once per version and launch.
    fn announce(&self, version: &str, key: &'static str) {
        let mut updater = self.updater.borrow_mut();
        if updater.announced.as_deref() == Some(version) {
            return;
        }
        updater.announced = Some(version.to_owned());
        drop(updater);
        self.toast(self.text(key).replace("{version}", version));
    }

    fn update_failed(&self, error: update::Error, notify: bool) {
        tracing::warn!(%error, "Update failed");
        let key = failure_key(error.kind);
        self.update_status.set(UpdateStatus::Failed(error.kind));
        if notify {
            self.fail(self.text(key));
        }
    }

    /// Starts installing the downloaded update. On `true` the caller quits
    /// the app right away (`platform::quit::restart_to_update`); on `false`
    /// the reason was shown and the app keeps running.
    pub fn launch_update(&self) -> bool {
        if !matches!(
            self.update_status.get_untracked(),
            UpdateStatus::Ready { .. }
        ) {
            return false;
        }
        if self.has_active_transfers() {
            self.fail(self.text("update.wait_for_transfers"));
            return false;
        }
        let Some(prepared) = self.updater.borrow().prepared.clone() else {
            return false;
        };
        match install::launch(&prepared) {
            Ok(()) => {
                tracing::info!(target = %prepared.target.path.display(), "Installing the update");
                self.update_status.set(UpdateStatus::Installing);
                true
            }
            Err(error) => {
                self.updater.borrow_mut().prepared = None;
                self.update_failed(error, true);
                false
            }
        }
    }
}

/// The i18n key that explains `kind`.
pub fn failure_key(kind: FailureKind) -> &'static str {
    match kind {
        FailureKind::Network => "update.failed_network",
        FailureKind::Verification => "update.failed_verification",
        FailureKind::Install => "update.failed_install",
    }
}

/// The i18n key that explains why `reason` needs a manual download.
pub fn manual_key(reason: install::Manual) -> &'static str {
    match reason {
        install::Manual::Unsupported => "update.manual_unsupported",
        install::Manual::Portable => "update.manual_portable",
        install::Manual::Translocated => "update.manual_translocated",
        install::Manual::ReadOnly => "update.manual_read_only",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn busy_states_block_a_second_check() {
        assert!(UpdateStatus::Checking.busy());
        assert!(UpdateStatus::Installing.busy());
        assert!(
            UpdateStatus::Downloading {
                version: "1.0.2".into(),
                received: 0,
                total: 1,
            }
            .busy()
        );
        assert!(!UpdateStatus::Idle.busy());
        assert!(!UpdateStatus::Failed(FailureKind::Network).busy());
    }

    #[test]
    fn every_update_key_is_translated() {
        use crate::i18n::{Language, has_key};
        let keys = [
            FailureKind::Network,
            FailureKind::Verification,
            FailureKind::Install,
        ]
        .map(failure_key)
        .into_iter()
        .chain(
            [
                install::Manual::Unsupported,
                install::Manual::Portable,
                install::Manual::Translocated,
                install::Manual::ReadOnly,
            ]
            .map(manual_key),
        )
        .chain([
            "update.up_to_date",
            "update.available_manual",
            "update.ready_toast",
            "update.wait_for_transfers",
        ]);
        for key in keys {
            for language in Language::ALL {
                assert!(has_key(language, key), "{key} is missing in {language:?}");
            }
        }
    }
}
