//! The UI-thread application state and the reducer that folds network events
//! into it.
//!
//! Only the UI owner thread touches an [`AppState`]: the network actor reports
//! [`TransferEvent`]s through a channel and the session applies them here.
//! Nothing in this module awaits, opens files or touches the network.

use crate::i18n::Language;
use crate::net::{DeviceType, Direction, OfferedFile, Peer, TransferEvent};
use crate::ui::theme::Appearance;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// What the content pane shows.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum View {
    #[default]
    Send,
    Receive,
    History,
}

/// Something staged to send.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ComposerItem {
    /// A file or folder picked or dropped by the user.
    File {
        path: PathBuf,
        name: String,
        size: u64,
        mime: String,
        /// Files in a folder, when the item is a directory.
        is_dir: bool,
    },
    /// A block of text, sent as LocalSend's `text` file.
    Text(String),
}

impl ComposerItem {
    pub fn name(&self) -> String {
        match self {
            ComposerItem::File { name, .. } => name.clone(),
            ComposerItem::Text(text) => {
                // Blank text is rejected by `Session::add_text`, so this is
                // not an English fallback string: it is unreachable in the UI.
                text.trim().lines().next().unwrap_or("").to_string()
            }
        }
    }

    pub fn size(&self) -> u64 {
        match self {
            ComposerItem::File { size, .. } => *size,
            ComposerItem::Text(text) => text.len() as u64,
        }
    }

    pub fn is_text(&self) -> bool {
        matches!(self, ComposerItem::Text(_))
    }
}

/// An incoming request waiting for the user's accept / decline.
#[derive(Clone, Debug)]
pub struct IncomingRequest {
    pub id: String,
    pub peer: Peer,
    pub files: Vec<OfferedFile>,
    pub total: u64,
}

/// A transfer in flight or in the session's list.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Transfer {
    pub id: String,
    pub peer: Peer,
    pub direction: Direction,
    pub files: Vec<OfferedFile>,
    pub total: u64,
    pub transferred: u64,
    pub status: TransferStatus,
    pub error: Option<String>,
    /// Received files as written on disk.
    pub saved_paths: Vec<PathBuf>,
    /// Received text blocks.
    pub texts: Vec<String>,
    /// Unix milliseconds.
    pub started_at: u64,
    pub finished_at: Option<u64>,
    /// Files left out of [`Transfer::files`] when the record was compacted
    /// for the persisted history (see [`Transfer::compacted_for_history`]).
    #[serde(default, skip_serializing_if = "is_zero")]
    pub omitted_files: usize,
}

fn is_zero(value: &usize) -> bool {
    *value == 0
}

/// File rows (and saved paths) a persisted history record keeps.
pub const HISTORY_FILES_KEPT: usize = 50;

/// Text bytes a persisted history record keeps (all blocks together).
pub const HISTORY_TEXT_BYTES: usize = 64 * 1024;

impl Transfer {
    pub fn progress(&self) -> f32 {
        if self.total == 0 {
            0.0
        } else {
            (self.transferred as f32 / self.total as f32).clamp(0.0, 1.0)
        }
    }

    pub fn is_active(&self) -> bool {
        self.status == TransferStatus::Active
    }

    /// How many files the transfer had, including rows a compacted history
    /// record no longer lists.
    pub fn file_count(&self) -> usize {
        self.files.len() + self.omitted_files
    }

    /// The bounded copy of a finished transfer that goes into the persisted
    /// history: the first [`HISTORY_FILES_KEPT`] files and saved paths and at
    /// most [`HISTORY_TEXT_BYTES`] of text.
    ///
    /// A 10,000-file receive is several MiB as a full record; history and
    /// settings share one 5 MiB storage quota, so a single big transfer used
    /// to push every older entry (or, if it did not fit at all, the whole
    /// history) out. The count survives in `omitted_files`.
    pub fn compacted_for_history(&self) -> Transfer {
        let mut compact = Transfer {
            files: self
                .files
                .iter()
                .take(HISTORY_FILES_KEPT)
                .cloned()
                .collect(),
            saved_paths: self
                .saved_paths
                .iter()
                .take(HISTORY_FILES_KEPT)
                .cloned()
                .collect(),
            texts: Vec::new(),
            omitted_files: self.omitted_files + self.files.len().saturating_sub(HISTORY_FILES_KEPT),
            ..self.clone_without_lists()
        };
        let mut budget = HISTORY_TEXT_BYTES;
        for text in &self.texts {
            if budget == 0 {
                break;
            }
            let mut end = text.len().min(budget);
            while !text.is_char_boundary(end) {
                end -= 1;
            }
            compact.texts.push(text[..end].to_string());
            budget -= end;
        }
        compact
    }

    /// A copy with empty lists (the scalar fields only), so compaction does
    /// not clone the full lists first.
    fn clone_without_lists(&self) -> Transfer {
        Transfer {
            id: self.id.clone(),
            peer: self.peer.clone(),
            direction: self.direction,
            files: Vec::new(),
            total: self.total,
            transferred: self.transferred,
            status: self.status,
            error: self.error.clone(),
            saved_paths: Vec::new(),
            texts: Vec::new(),
            started_at: self.started_at,
            finished_at: self.finished_at,
            omitted_files: self.omitted_files,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum TransferStatus {
    Active,
    Completed,
    Cancelled,
    Failed,
}

/// Shortest receive PIN the settings accept for a new value. The core limits
/// wrong guesses per IP (3), but only for the last 200 addresses, so a peer
/// that rotates IPv6 addresses can keep guessing: a 4-digit PIN falls in
/// minutes. An existing shorter PIN is kept (dropping it would remove the
/// protection), only new ones are checked.
pub const MIN_PIN_CHARS: usize = 6;

/// Finished transfers kept in the history list (in memory and on disk).
///
/// One number for both: the views render the in-memory list and the store
/// keeps the persisted one, so a different cap would make the UI silently
/// shrink after a restart.
pub const HISTORY_LIMIT: usize = 100;

/// The schema version written into the settings and the history.
///
/// A file without a `schema` field predates versioning and is version 1. A
/// newer file is loaded best-effort (all fields have serde defaults), so a
/// downgrade does not reset the user's preferences; a future breaking change
/// bumps this and migrates in one place.
pub const SETTINGS_SCHEMA: u32 = 1;

/// History envelope schema: `{"schema": 2, "entries": [...]}`.
/// Version 1 is the legacy bare JSON array.
pub const HISTORY_SCHEMA: u32 = 2;

/// The longest device name kept (the settings field the UI limits too).
pub const MAX_ALIAS_CHARS: usize = 80;

/// The most favorites kept (deduplicated).
pub const MAX_FAVORITES: usize = 200;

/// Persisted user preferences.
///
/// Every field has an explicit serde default so that a settings file written
/// by another version (or the partially written `{"language":"en"}` a test
/// passes in) still loads without constructing [`Settings::default`]; the
/// default alias spawns a `scutil` subprocess on macOS, and a container-wide
/// `#[serde(default)]` would pay for it on every launch even though the file
/// carries all the fields.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Settings {
    /// The schema this value was written with (see [`SETTINGS_SCHEMA`]).
    #[serde(default = "default_schema")]
    pub schema: u32,
    /// The name other devices see.
    #[serde(default = "default_alias")]
    pub alias: String,
    #[serde(default = "default_device_type")]
    pub device_type: DeviceType,
    #[serde(default = "default_port")]
    pub port: u16,
    /// HTTPS (the protocol's default) or plain HTTP for legacy peers.
    #[serde(default = "default_true")]
    pub https: bool,
    /// The PIN peers must enter to send to this device.
    #[serde(default)]
    pub pin: Option<String>,
    /// Whether this device is visible on the network.
    #[serde(default = "default_true")]
    pub discoverable: bool,
    /// Where received files are written.
    #[serde(default = "default_downloads")]
    pub downloads: PathBuf,
    #[serde(default)]
    pub appearance: Appearance,
    #[serde(default)]
    pub language: Language,
    /// Accept incoming requests without asking.
    #[serde(default)]
    pub quick_save: bool,
    /// Limit [`Settings::quick_save`] to favorite devices: anyone else on the
    /// network still has to be accepted by hand.
    #[serde(default)]
    pub quick_save_favorites_only: bool,
    /// Fingerprints of favorite devices (shown first).
    #[serde(default)]
    pub favorites: Vec<String>,
    /// Keep running in the background when the window closes.
    #[serde(default = "default_true")]
    pub run_in_background: bool,
    /// Check GitHub for a newer release once a day and download it in the
    /// background (see `crate::update`).
    #[serde(default = "default_true")]
    pub auto_update: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            schema: SETTINGS_SCHEMA,
            alias: default_alias(),
            device_type: default_device_type(),
            port: default_port(),
            https: default_true(),
            pin: None,
            discoverable: default_true(),
            downloads: default_downloads(),
            appearance: Appearance::System,
            language: Language::default(),
            quick_save: false,
            quick_save_favorites_only: false,
            favorites: Vec::new(),
            run_in_background: default_true(),
            auto_update: default_true(),
        }
    }
}

impl Settings {
    /// Repairs values that a stale file, a hand edit or a non-UI caller can
    /// produce, so the rest of the app can trust the fields.
    ///
    /// Pure (no I/O): the caller probes [`Settings::downloads`] itself, as a
    /// write test needs the filesystem (see `session::writable_downloads`).
    /// Returns the names of the repaired fields so the caller can log them.
    pub fn sanitize(&mut self) -> Vec<&'static str> {
        let mut changed = Vec::new();
        let alias = self.alias.trim();
        if alias.is_empty() {
            self.alias = default_alias();
            changed.push("alias");
        } else if alias.chars().count() > MAX_ALIAS_CHARS {
            self.alias = alias.chars().take(MAX_ALIAS_CHARS).collect();
            changed.push("alias");
        } else if alias != self.alias {
            self.alias = alias.to_string();
            changed.push("alias");
        }
        // `port == 0` binds a random port: it works but is not reproducible,
        // and the registered address then disagrees with the setting.
        if self.port == 0 {
            self.port = default_port();
            changed.push("port");
        }
        if let Some(pin) = &self.pin {
            let trimmed = pin.trim();
            if trimmed.is_empty() {
                self.pin = None;
                changed.push("pin");
            } else if trimmed != pin {
                self.pin = Some(trimmed.to_string());
                changed.push("pin");
            }
        }
        // The history and settings are serialized to JSON, where a `PathBuf`
        // must be valid UTF-8; an unrepresentable download folder would make
        // every later settings save fail (and the app would silently fall back
        // to the defaults on the next launch).
        if self.downloads.as_os_str().is_empty() || self.downloads.to_str().is_none() {
            self.downloads = default_downloads();
            changed.push("downloads");
        }
        let mut seen = std::collections::HashSet::new();
        let before = self.favorites.len();
        self.favorites
            .retain(|favorite| !favorite.is_empty() && seen.insert(favorite.clone()));
        if self.favorites.len() > MAX_FAVORITES {
            self.favorites.truncate(MAX_FAVORITES);
        }
        if self.favorites.len() != before {
            changed.push("favorites");
        }
        changed
    }
}

fn default_schema() -> u32 {
    SETTINGS_SCHEMA
}

fn default_alias() -> String {
    crate::platform::machine_name()
}

fn default_device_type() -> DeviceType {
    DeviceType::Desktop
}

fn default_port() -> u16 {
    crate::net::DEFAULT_PORT
}

fn default_true() -> bool {
    true
}

pub(crate) fn default_downloads() -> PathBuf {
    crate::platform::downloads_dir()
}

/// Everything the views read.
#[derive(Default)]
pub struct AppState {
    pub view: View,
    /// Discovery is running and the network responded.
    pub discovering: bool,
    /// Known peers, favorites first, then online, then remembered.
    pub devices: Vec<Peer>,
    /// Fingerprint of the peer the next send goes to.
    pub selected: Option<String>,
    pub incoming: Option<IncomingRequest>,
    /// Active transfers first, then finished ones (newest first).
    pub transfers: Vec<Transfer>,
    pub composer: Vec<ComposerItem>,
    pub settings: Settings,
    /// A settings change needs a network restart but transfers are running:
    /// apply it once the last transfer (or pending request) finishes instead
    /// of cancelling them (see `Session::update_settings`). Views can show
    /// "applies after the current transfer" while this is set.
    pub pending_restart: bool,
    /// A non-fatal, user-visible error (the toast mirrors it).
    pub error: Option<String>,
}

impl AppState {
    /// The peer a send would go to.
    pub fn selected_peer(&self) -> Option<&Peer> {
        let selected = self.selected.as_deref()?;
        self.devices
            .iter()
            .find(|peer| peer.fingerprint == selected)
    }

    pub fn active_transfers(&self) -> impl Iterator<Item = &Transfer> {
        self.transfers
            .iter()
            .filter(|transfer| transfer.is_active())
    }

    pub fn sending(&self) -> bool {
        self.active_transfers()
            .any(|transfer| transfer.direction == Direction::Sending)
    }

    pub fn receiving(&self) -> bool {
        self.active_transfers()
            .any(|transfer| transfer.direction == Direction::Receiving)
    }

    /// Can a send start right now?
    pub fn can_send(&self) -> bool {
        // A remembered peer (no address) cannot receive: the actor fails the
        // send before it emits `Started`, which would clear the composer for
        // nothing. Keep Send disabled instead.
        !self.composer.is_empty()
            && self
                .selected_peer()
                .is_some_and(|peer| peer.address.is_some())
            && !self.sending()
            && self.incoming.is_none()
    }

    /// Whether anything still needs the current network node: an active
    /// transfer, or an incoming request waiting for the user's decision.
    pub fn has_active_work(&self) -> bool {
        self.active_transfers().next().is_some() || self.incoming.is_some()
    }

    /// Consumes a deferred settings restart if the network is idle again.
    ///
    /// Returns `true` exactly once per deferred restart; the caller then
    /// restarts the network. Pure: the restart itself is the caller's job.
    pub fn take_pending_restart(&mut self) -> bool {
        if self.pending_restart && !self.has_active_work() {
            self.pending_restart = false;
            true
        } else {
            false
        }
    }

    /// The network node stopped or never started: no peer is reachable
    /// anymore, so the device list and the "looking for devices" state must
    /// not keep claiming otherwise. History and settings are untouched.
    pub fn network_stopped(&mut self) {
        self.discovering = false;
        self.devices.clear();
        self.selected = None;
    }

    pub fn total_composer_size(&self) -> u64 {
        self.composer.iter().map(ComposerItem::size).sum()
    }

    /// The transfer with this id, if still known.
    pub fn transfer(&self, id: &str) -> Option<&Transfer> {
        self.transfers.iter().find(|transfer| transfer.id == id)
    }

    fn transfer_mut(&mut self, id: &str) -> Option<&mut Transfer> {
        self.transfers.iter_mut().find(|transfer| transfer.id == id)
    }

    /// Fold one network event into the state, using the wall clock for the
    /// event's timestamp. A thin wrapper around [`AppState::apply_at`].
    pub fn apply(&mut self, event: TransferEvent) {
        self.apply_at(event, crate::net::now_ms());
    }

    /// Fold one network event into the state with an injected clock.
    ///
    /// Pure: no I/O, no signals, no clock reads (the timestamp comes from
    /// `now`), so tests can pin time instead of sleeping.
    ///
    /// Terminal events (`Completed`/`Cancelled`/`Failed`) are final: a late
    /// `Progress`/`FileDone` after one is ignored, and a duplicated terminal
    /// event does not move `finished_at`. A terminal event for one transfer
    /// never touches the pending incoming request; that is cleared only by a
    /// terminal event for the request's own session id.
    pub fn apply_at(&mut self, event: TransferEvent, now: u64) {
        match event {
            TransferEvent::DiscoveryActive(active) => self.discovering = active,
            TransferEvent::PeerFound(peer) => self.upsert_peer(peer),
            TransferEvent::PeerLost(fingerprint) => {
                self.devices.retain(|peer| peer.fingerprint != fingerprint);
                if self.selected.as_deref() == Some(fingerprint.as_str()) {
                    self.selected = None;
                }
            }
            TransferEvent::IncomingRequest {
                id,
                peer,
                files,
                total,
            } => {
                self.incoming = Some(IncomingRequest {
                    id,
                    peer,
                    files,
                    total,
                });
            }
            TransferEvent::Started {
                id,
                peer,
                direction,
                files,
                total,
            } => {
                self.clear_incoming_if(&id);
                self.transfers.retain(|transfer| transfer.id != id);
                self.transfers.insert(
                    0,
                    Transfer {
                        id,
                        peer,
                        direction,
                        files,
                        total,
                        transferred: 0,
                        status: TransferStatus::Active,
                        error: None,
                        saved_paths: Vec::new(),
                        texts: Vec::new(),
                        started_at: now,
                        finished_at: None,
                        omitted_files: 0,
                    },
                );
                self.error = None;
            }
            TransferEvent::Accepted { .. } => {
                // `Started` already created the transfer; this only confirms
                // the peer took the prepare-upload. Nothing to change.
            }
            TransferEvent::Progress {
                id,
                transferred,
                total,
            } => {
                if let Some(transfer) = self.transfer_mut(&id)
                    && transfer.is_active()
                {
                    transfer.transferred = transferred;
                    if let Some(total) = total {
                        transfer.total = total;
                    }
                }
            }
            TransferEvent::FileDone { id, file } => {
                if let Some(transfer) = self.transfer_mut(&id)
                    && transfer.is_active()
                {
                    if let Some(saved) = file.saved_to.clone()
                        && !transfer.saved_paths.contains(&saved)
                    {
                        transfer.saved_paths.push(saved);
                    }
                    if let Some(slot) = transfer
                        .files
                        .iter_mut()
                        .find(|slot| slot.name == file.name)
                    {
                        slot.saved_to = file.saved_to;
                    }
                }
            }
            TransferEvent::TextReceived { id, text } => {
                if let Some(transfer) = self.transfer_mut(&id)
                    && transfer.is_active()
                {
                    transfer.texts.push(text);
                }
            }
            TransferEvent::Completed { id, files } => {
                self.finish(id.as_str(), TransferStatus::Completed, files, now)
            }
            TransferEvent::Cancelled { id } => {
                self.finish(id.as_str(), TransferStatus::Cancelled, Vec::new(), now)
            }
            TransferEvent::Failed { id, error } => self.finish_failed(id.as_str(), error, now),
            TransferEvent::PinRequired { .. } => {}
            TransferEvent::NetError(message) => self.error = Some(message),
            // A declined request never became a transfer; the session shows
            // a toast, and it is not a network fault.
            TransferEvent::RequestRefused(_) => {}
        }
    }

    /// Drops the pending incoming request when `id` is its session id.
    ///
    /// A transfer event for another id (an outgoing send finishing while the
    /// request sheet is open) must not dismiss the request: the network actor
    /// would keep waiting for a decision the UI can no longer give.
    fn clear_incoming_if(&mut self, id: &str) {
        if self
            .incoming
            .as_ref()
            .is_some_and(|incoming| incoming.id == id)
        {
            self.incoming = None;
        }
    }

    fn upsert_peer(&mut self, peer: Peer) {
        if peer.is_self {
            return;
        }
        if let Some(existing) = self
            .devices
            .iter_mut()
            .find(|existing| existing.fingerprint == peer.fingerprint)
        {
            // Keep a remembered address if the announcement did not carry one.
            let address = peer.address.clone().or_else(|| existing.address.clone());
            *existing = Peer { address, ..peer };
        } else {
            self.devices.push(peer);
        }
        self.sort_devices();
    }

    /// Favorites, then online peers, then remembered ones; by name inside.
    pub fn sort_devices(&mut self) {
        let favorites = self.settings.favorites.clone();
        self.devices.sort_by(|a, b| {
            let rank = |peer: &Peer| {
                if favorites.contains(&peer.fingerprint) {
                    0
                } else if peer.address.is_some() {
                    1
                } else {
                    2
                }
            };
            rank(a)
                .cmp(&rank(b))
                .then_with(|| a.alias.to_lowercase().cmp(&b.alias.to_lowercase()))
        });
    }

    fn finish(&mut self, id: &str, status: TransferStatus, files: Vec<OfferedFile>, now: u64) {
        self.clear_incoming_if(id);
        let Some(transfer) = self.transfer_mut(id) else {
            self.sort_transfers();
            return;
        };
        if !transfer.is_active() {
            // A duplicated (or out-of-order) terminal event: a finished
            // transfer must not get a new `finished_at`.
            return;
        }
        transfer.status = status;
        transfer.finished_at = Some(now);
        if status == TransferStatus::Completed {
            // The last progress event of a transfer is throttled, so the
            // byte count can lag behind by one tick; only a completed
            // transfer is known to be fully transferred. A cancelled one
            // keeps the bytes it actually moved.
            transfer.transferred = transfer.transferred.max(transfer.total);
        }
        for file in files {
            if let Some(saved) = file.saved_to.clone()
                && !transfer.saved_paths.contains(&saved)
            {
                transfer.saved_paths.push(saved);
            }
            if let Some(slot) = transfer
                .files
                .iter_mut()
                .find(|slot| slot.name == file.name)
            {
                slot.saved_to = file.saved_to;
            }
        }
        self.sort_transfers();
    }

    fn finish_failed(&mut self, id: &str, error: String, now: u64) {
        self.clear_incoming_if(id);
        if let Some(transfer) = self.transfer_mut(id)
            && transfer.is_active()
        {
            transfer.status = TransferStatus::Failed;
            transfer.error = Some(error.clone());
            transfer.finished_at = Some(now);
        }
        self.error = Some(error);
        self.sort_transfers();
    }

    /// Active first, then finished newest first; finished transfers beyond
    /// [`HISTORY_LIMIT`] are dropped so the in-memory list cannot grow for
    /// the lifetime of the app (the persisted history has the same cap).
    pub fn sort_transfers(&mut self) {
        self.transfers.sort_by(|a, b| {
            b.is_active()
                .cmp(&a.is_active())
                .then_with(|| b.started_at.cmp(&a.started_at))
        });
        let mut finished = 0usize;
        self.transfers.retain(|transfer| {
            if transfer.is_active() {
                return true;
            }
            finished += 1;
            finished <= HISTORY_LIMIT
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::net::{PeerAddress, Protocol};
    use std::path::PathBuf;

    fn peer(fingerprint: &str, online: bool) -> Peer {
        Peer {
            fingerprint: fingerprint.to_string(),
            alias: "Peer".to_string(),
            device_model: "Mac".to_string(),
            address: online.then(|| PeerAddress {
                host: "192.168.1.20".to_string(),
                port: 53317,
                protocol: Protocol::Https,
            }),
            device_type: DeviceType::Desktop,
            is_self: false,
        }
    }

    fn file(name: &str) -> OfferedFile {
        OfferedFile {
            name: name.to_string(),
            size: 10,
            mime: "application/octet-stream".to_string(),
            saved_to: None,
        }
    }

    fn incoming(state: &mut AppState, id: &str, peer: Peer) {
        state.apply(TransferEvent::IncomingRequest {
            id: id.to_string(),
            peer,
            files: vec![file("a.bin")],
            total: 10,
        });
    }

    fn started(state: &mut AppState, id: &str, peer: Peer) {
        state.apply(TransferEvent::Started {
            id: id.to_string(),
            peer,
            direction: Direction::Sending,
            files: vec![file("a.bin")],
            total: 10,
        });
    }

    /// A transfer event for one id must not dismiss a request with another id:
    /// the actor would keep waiting for a decision the sheet can no longer give.
    #[test]
    fn unrelated_terminal_event_keeps_the_pending_request() {
        let mut state = AppState::default();
        incoming(&mut state, "request", peer("A", true));
        started(&mut state, "send", peer("B", true));
        state.apply(TransferEvent::Completed {
            id: "send".to_string(),
            files: Vec::new(),
        });
        assert!(state.incoming.is_some(), "the request sheet must stay open");

        // The request's own terminal event does clear it.
        state.apply(TransferEvent::Cancelled {
            id: "request".to_string(),
        });
        assert!(state.incoming.is_none());
    }

    /// The accepting `Started` carries the request's id and clears the sheet;
    /// progress events after a terminal state are ignored.
    #[test]
    fn terminal_state_is_final() {
        let mut state = AppState::default();
        state.apply(TransferEvent::Started {
            id: "t".to_string(),
            peer: peer("A", true),
            direction: Direction::Receiving,
            files: vec![file("a.bin")],
            total: 10,
        });
        state.apply(TransferEvent::Started {
            id: "t".to_string(),
            peer: peer("A", true),
            direction: Direction::Receiving,
            files: vec![file("a.bin")],
            total: 10,
        });
        assert_eq!(state.transfers.len(), 1);
        state.apply(TransferEvent::Progress {
            id: "t".to_string(),
            transferred: 4,
            total: Some(10),
        });
        state.apply(TransferEvent::Completed {
            id: "t".to_string(),
            files: vec![file("a.bin")],
        });
        let finished_at = state.transfer("t").unwrap().finished_at;
        state.apply(TransferEvent::Progress {
            id: "t".to_string(),
            transferred: 1,
            total: Some(10),
        });
        let transfer = state.transfer("t").unwrap();
        assert_eq!(transfer.status, TransferStatus::Completed);
        assert_eq!(
            transfer.transferred, 10,
            "a late progress event must be ignored"
        );
        assert_eq!(transfer.finished_at, finished_at);
    }

    /// A cancelled transfer keeps the bytes it actually moved; only a
    /// completed one is bumped to its total (the final progress is throttled).
    #[test]
    fn cancelled_progress_is_not_rounded_up() {
        let mut state = AppState::default();
        started(&mut state, "t", peer("A", true));
        state.apply(TransferEvent::Progress {
            id: "t".to_string(),
            transferred: 3,
            total: Some(10),
        });
        state.apply(TransferEvent::Cancelled {
            id: "t".to_string(),
        });
        assert_eq!(state.transfer("t").unwrap().transferred, 3);
    }

    /// The reducer takes its timestamps from the caller, so the test pins
    /// them instead of sleeping and can assert the exact values.
    #[test]
    fn apply_at_uses_the_injected_clock() {
        let mut state = AppState::default();
        state.apply_at(
            TransferEvent::Started {
                id: "t".to_string(),
                peer: peer("A", true),
                direction: Direction::Receiving,
                files: vec![file("a.bin")],
                total: 10,
            },
            1_000,
        );
        state.apply_at(
            TransferEvent::Progress {
                id: "t".to_string(),
                transferred: 4,
                total: Some(10),
            },
            1_500,
        );
        state.apply_at(
            TransferEvent::Cancelled {
                id: "t".to_string(),
            },
            2_000,
        );
        let transfer = state.transfer("t").unwrap();
        assert_eq!(transfer.started_at, 1_000);
        assert_eq!(transfer.finished_at, Some(2_000));

        // A duplicated terminal event must not move `finished_at`.
        state.apply_at(
            TransferEvent::Cancelled {
                id: "t".to_string(),
            },
            3_000,
        );
        assert_eq!(state.transfer("t").unwrap().finished_at, Some(2_000));
    }

    /// A failed transfer keeps the error both on the row and as the current
    /// error, so the toast (M-01) and any future banner see it.
    #[test]
    fn failed_event_records_the_error() {
        let mut state = AppState::default();
        started(&mut state, "t", peer("A", true));
        state.apply_at(
            TransferEvent::Failed {
                id: "t".to_string(),
                error: "boom".to_string(),
            },
            7,
        );
        assert_eq!(state.error.as_deref(), Some("boom"));
        let transfer = state.transfer("t").unwrap();
        assert_eq!(transfer.status, TransferStatus::Failed);
        assert_eq!(transfer.error.as_deref(), Some("boom"));
        assert_eq!(transfer.finished_at, Some(7));
    }

    /// M-02: a restart requested while work is in flight is held back and
    /// released exactly once, when the last transfer finishes.
    #[test]
    fn pending_restart_waits_for_the_in_flight_work() {
        let mut state = AppState {
            pending_restart: true,
            ..AppState::default()
        };
        started(&mut state, "t", peer("A", true));
        assert!(
            !state.take_pending_restart(),
            "a running transfer defers it"
        );
        assert!(state.pending_restart);

        // A pending incoming request is work too: restarting would drop it.
        state.apply_at(
            TransferEvent::Completed {
                id: "t".to_string(),
                files: Vec::new(),
            },
            5,
        );
        state.apply(TransferEvent::IncomingRequest {
            id: "request".to_string(),
            peer: peer("B", true),
            files: vec![file("b.bin")],
            total: 10,
        });
        assert!(!state.take_pending_restart());
        state.apply(TransferEvent::Cancelled {
            id: "request".to_string(),
        });
        assert!(
            state.take_pending_restart(),
            "released once the work is done"
        );
        assert!(!state.take_pending_restart(), "and only once");
    }

    /// M-03: a stopped/failed node must not leave the UI claiming that
    /// discovery is running or that stale devices are reachable.
    #[test]
    fn network_stopped_clears_peers_but_keeps_history_and_settings() {
        let mut state = AppState {
            discovering: true,
            ..AppState::default()
        };
        state.apply(TransferEvent::PeerFound(peer("A", true)));
        state.selected = Some("A".to_string());
        state.apply(TransferEvent::Started {
            id: "t".to_string(),
            peer: peer("A", true),
            direction: Direction::Sending,
            files: vec![file("a.bin")],
            total: 10,
        });
        let port = state.settings.port;

        state.network_stopped();
        assert!(!state.discovering);
        assert!(state.devices.is_empty());
        assert!(state.selected.is_none());
        assert_eq!(state.transfers.len(), 1, "history is kept");
        assert_eq!(state.settings.port, port);
    }

    /// M-08: every invalid field is repaired in place; `downloads` emptiness
    /// is repaired here (the writability probe needs the filesystem and is
    /// done by the session).
    #[test]
    fn settings_sanitize_repairs_invalid_fields() {
        let mut settings = Settings::default();
        let machine = settings.alias.clone();
        settings.alias = "   ".to_string();
        settings.port = 0;
        settings.pin = Some("  ".to_string());
        settings.downloads = PathBuf::new();
        settings.favorites = vec![
            "a".to_string(),
            "b".to_string(),
            "a".to_string(),
            String::new(),
        ];
        let changed = settings.sanitize();
        assert_eq!(settings.alias, machine, "an empty alias falls back");
        assert_eq!(settings.port, crate::net::DEFAULT_PORT);
        assert_eq!(settings.pin, None);
        assert_eq!(settings.downloads, default_downloads());
        assert_eq!(settings.favorites, vec!["a".to_string(), "b".to_string()]);
        for field in ["alias", "port", "pin", "downloads", "favorites"] {
            assert!(changed.contains(&field), "{field} was repaired");
        }
    }

    #[test]
    fn settings_sanitize_keeps_valid_values_and_caps_lists() {
        let mut settings = Settings::default();
        let before = settings.clone();
        assert!(settings.sanitize().is_empty());
        assert_eq!(settings, before);

        settings.alias = format!("  {}  ", "x".repeat(MAX_ALIAS_CHARS + 10));
        settings.favorites = (0..MAX_FAVORITES + 10)
            .map(|index| format!("fingerprint-{index}"))
            .collect();
        let changed = settings.sanitize();
        assert!(changed.contains(&"alias"));
        assert_eq!(settings.alias.chars().count(), MAX_ALIAS_CHARS);
        assert_eq!(settings.alias, "x".repeat(MAX_ALIAS_CHARS));
        assert!(changed.contains(&"favorites"));
        assert_eq!(settings.favorites.len(), MAX_FAVORITES);
    }

    /// M-06: a `PathBuf` that JSON cannot represent (serde `Path` requires
    /// UTF-8) must be repaired before it makes every later save fail.
    #[cfg(unix)]
    #[test]
    fn settings_sanitize_replaces_a_non_utf8_downloads_path() {
        use std::os::unix::ffi::OsStringExt;
        let mut settings = Settings {
            downloads: PathBuf::from(std::ffi::OsString::from_vec(vec![b'/', b'f', 0xff])),
            ..Settings::default()
        };
        let changed = settings.sanitize();
        assert!(changed.contains(&"downloads"));
        assert!(settings.downloads.to_str().is_some());
        assert_eq!(settings.downloads, default_downloads());
    }

    /// The in-memory list has to follow the same cap as the persisted history,
    /// or it grows for the lifetime of the app (and shrinks after a restart).
    #[test]
    fn finished_transfers_are_capped_in_memory() {
        let mut state = AppState::default();
        for index in 0..(HISTORY_LIMIT + 20) {
            let id = format!("t{index}");
            started(&mut state, &id, peer("A", true));
            state.apply(TransferEvent::Completed {
                id,
                files: Vec::new(),
            });
        }
        let finished = state.transfers.iter().filter(|t| !t.is_active()).count();
        assert_eq!(finished, HISTORY_LIMIT);
        assert!(
            state.transfer("t0").is_none(),
            "the oldest entry was pruned"
        );
        assert!(
            state
                .transfer(&format!("t{}", HISTORY_LIMIT + 19))
                .is_some(),
            "the newest entry is kept"
        );
    }

    #[test]
    fn can_send_requires_a_reachable_peer_and_no_pending_request() {
        let mut state = AppState::default();
        state.composer.push(ComposerItem::Text("hi".to_string()));
        state.devices.push(peer("offline", false));
        state.selected = Some("offline".to_string());
        assert!(
            !state.can_send(),
            "a remembered peer without an address cannot receive"
        );

        state.devices[0] = peer("offline", true);
        assert!(state.can_send());

        incoming(&mut state, "request", peer("A", true));
        assert!(!state.can_send(), "a pending request owns the decision");
    }

    #[test]
    fn peer_lost_clears_the_selection() {
        let mut state = AppState::default();
        state.apply(TransferEvent::PeerFound(peer("A", true)));
        state.selected = Some("A".to_string());
        state.apply(TransferEvent::PeerLost("A".to_string()));
        assert!(state.selected.is_none());
        assert!(state.devices.is_empty());
    }

    /// Settings written by another version (or a test fixture with only the
    /// language) must keep the productive defaults instead of resetting them
    /// to zero/empty.
    #[test]
    fn partial_settings_keep_the_defaults() {
        let settings: Settings = serde_json::from_str(r#"{"language":"zh"}"#).unwrap();
        assert_eq!(settings.language, Language::Zh);
        assert_eq!(
            settings.schema, SETTINGS_SCHEMA,
            "a file without a schema is version 1"
        );
        assert_eq!(settings.port, crate::net::DEFAULT_PORT);
        assert!(settings.https);
        assert!(settings.discoverable);
        assert!(settings.run_in_background);
        assert_eq!(settings.device_type, DeviceType::Desktop);
        assert!(!settings.alias.is_empty());
        assert!(!settings.downloads.as_os_str().is_empty());
    }

    /// A settings file from a newer version must load best-effort instead of
    /// resetting every preference (unknown schema, unknown fields).
    #[test]
    fn newer_settings_schema_is_read_best_effort() {
        let settings: Settings =
            serde_json::from_str(r#"{"schema":99,"language":"zh","future_field":true}"#).unwrap();
        assert_eq!(settings.schema, 99);
        assert_eq!(settings.language, Language::Zh);
        assert_eq!(settings.port, crate::net::DEFAULT_PORT);
    }
}
