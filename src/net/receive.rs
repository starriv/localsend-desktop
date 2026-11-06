//! The receiving side bookkeeping: the single incoming session, its offered
//! files, its accept/decline decision and the per-file progress aggregation.
//!
//! The actual bytes are written by the *core* (`FileUploadTarget::Path`), so
//! size enforcement, checksum verification and file timestamps are the core's
//! job. This module decides where the bytes go, publishes the finished file
//! under a collision-safe name (`naming`), and tracks what the UI should see.

use std::collections::{HashMap, HashSet};
use std::time::Instant;

use anyhow::{Context, Result, ensure};
use localsend::http::server::v2::PrepareUploadDecisionV2;
use localsend::model::transfer::FileDto;
use tokio::sync::oneshot;
use tokio_util::sync::CancellationToken;

use crate::net::naming;
use crate::net::{OfferedFile, Peer, TransferEvent, emit};

/// How often aggregated receive progress is reported (<= 20 events/s).
const PROGRESS_INTERVAL: std::time::Duration = std::time::Duration::from_millis(50);

/// Largest message text forwarded to the UI. A LocalSend message carries its
/// whole body in the `preview` field, which a peer controls: without a cap a
/// single request could clone an arbitrarily large string into the UI. Longer
/// text is still accepted (the message is acknowledged), only the shown copy is
/// truncated. Matches the limit used for received `.txt` files.
pub const MAX_MESSAGE_BYTES: usize = 64 * 1024;

/// Largest text file whose body is forwarded to the UI as `TextReceived`.
pub const MAX_TEXT_FILE_BYTES: u64 = 64 * 1024;

/// Total text forwarded to the UI over one receive session. A peer may offer
/// up to 10,000 tiny `text/plain` files (each under the per-file cap); without
/// a session budget the model would accumulate hundreds of megabytes of
/// strings and its history JSON would blow the storage quota (NET-05).
pub const MAX_SESSION_TEXT_BYTES: u64 = 1024 * 1024;

/// Most files one transfer may carry. The receiver refuses larger offers, and
/// the sender refuses to stage them (a 403 would not say why).
pub const MAX_OFFER_FILES: usize = 10_000;

/// Never fill the destination volume completely: a receive is refused when the
/// offer does not fit into this fraction of the free space.
const DISK_HEADROOM_PERCENT: u64 = 90;

/// What the actor should do with a `decide` command for the current incoming
/// request. Split out of the actor so the cancel/accept races are testable
/// without a server: deciding twice, or declining after a quick-save already
/// accepted the request, must not silently drop the session bookkeeping while
/// the core session keeps running.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DecisionAction {
    /// Accept every offered file (a normal file transfer).
    AcceptFiles,
    /// Answer with an empty accept set (204): a message transfer needs no
    /// download.
    AcceptMessage,
    /// Decline a request the app has not answered yet (403).
    Decline,
    /// The request was already accepted: the user is cancelling a running
    /// transfer, so cancel the session instead of declining a dead responder.
    CancelAccepted,
    /// Unknown/stale id, or the decision was already answered: ignore.
    Ignore,
}

/// Classifies a `decide` command against the current incoming session.
pub fn decision_action(incoming: Option<&Incoming>, id: &str, accept: bool) -> DecisionAction {
    let Some(incoming) = incoming.filter(|incoming| incoming.id == id) else {
        return DecisionAction::Ignore;
    };
    if accept {
        if incoming.accepted {
            // A double accept (the quick-save auto-answer racing the UI) is
            // already handled; answering the responder twice would be wrong.
            DecisionAction::Ignore
        } else if incoming.message().is_some() {
            DecisionAction::AcceptMessage
        } else {
            DecisionAction::AcceptFiles
        }
    } else if incoming.accepted {
        DecisionAction::CancelAccepted
    } else {
        DecisionAction::Decline
    }
}

/// Caps a peer-supplied message body for the UI. Cuts on a character boundary.
pub fn clamp_text(text: &str) -> String {
    if text.len() <= MAX_MESSAGE_BYTES {
        return text.to_string();
    }
    let mut end = MAX_MESSAGE_BYTES;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    text[..end].to_string()
}

/// The session the single v2 upload slot currently holds.
pub struct Incoming {
    /// The peer's session id; also the transfer id the UI uses.
    pub id: String,
    pub peer: Peer,
    /// Every offered file, by file id (the peer's view of the transfer).
    pub files: HashMap<String, FileDto>,
    /// Sum of all offered file sizes; cached so progress emission stays O(1)
    /// per event instead of summing the map at up to 20 Hz.
    total_bytes: u64,
    /// The responder for a prepare-upload that is still waiting for a decision.
    pub decision: Option<oneshot::Sender<PrepareUploadDecisionV2>>,
    /// Marks the session abandoned and stops the app-side progress relay.
    ///
    /// The bytes of an upload already in flight are written by the *core*
    /// (`FileUploadTarget::Path`); its API has no abort, so a local cancel
    /// cannot stop that write (BLOCKED-UPSTREAM).
    /// Cancelling this token only stops the actor's progress relay and lets
    /// the terminal event be emitted; the core still drains the upload.
    pub cancel: CancellationToken,
    /// Whether the decision has been answered with accept.
    pub accepted: bool,
    /// Files whose result (success) has been recorded.
    pub done: usize,
    /// Files with an upload request currently in flight.
    pub pending: HashSet<String>,
    /// Bytes received per file id.
    pub progress: HashMap<String, u64>,
    /// Sum of [`Self::progress`], kept incrementally so progress emission is
    /// O(1) instead of O(files) per event (NET-07).
    transferred_total: u64,
    /// Text bytes already forwarded to the UI as `TextReceived` (NET-05).
    text_bytes: u64,
    /// Next collision suffix to try per destination path (NET-04).
    collision_next: HashMap<std::path::PathBuf, u32>,
    /// Staging file per file id, reused across checksum retries.
    pub temps: HashMap<String, std::path::PathBuf>,
    /// Upload attempts per file id (the core allows 3 after a checksum mismatch).
    pub attempts: HashMap<String, u32>,
    /// When progress was last reported to the UI.
    last_emit: Instant,
    /// Last time the session made any progress (used for inactivity timeouts).
    pub last_activity: Instant,
    /// The server reported that all accepted files reached a final state.
    pub ended: bool,
}

impl Incoming {
    pub fn new(
        id: String,
        peer: Peer,
        files: HashMap<String, FileDto>,
        decision: oneshot::Sender<PrepareUploadDecisionV2>,
    ) -> Self {
        let now = Instant::now();
        let total_bytes = files
            .values()
            .fold(0u64, |sum, file| sum.saturating_add(file.size));
        Self {
            id,
            peer,
            files,
            total_bytes,
            decision: Some(decision),
            cancel: CancellationToken::new(),
            accepted: false,
            done: 0,
            pending: HashSet::new(),
            progress: HashMap::new(),
            transferred_total: 0,
            text_bytes: 0,
            collision_next: HashMap::new(),
            temps: HashMap::new(),
            attempts: HashMap::new(),
            last_emit: now - PROGRESS_INTERVAL,
            last_activity: now,
            ended: false,
        }
    }

    /// Total size of all offered files.
    pub fn total(&self) -> u64 {
        self.total_bytes
    }

    /// The offered files in a stable order (by name) for the UI.
    pub fn offered(&self) -> Vec<OfferedFile> {
        let mut offered: Vec<OfferedFile> = self.files.values().map(offered_file).collect();
        offered.sort_by(|a, b| a.name.cmp(&b.name));
        offered
    }

    /// Whether this request is a LocalSend message transfer: exactly one text
    /// file whose body is already in the preview. Nothing has to be
    /// downloaded; answering with an empty accept set (204) is what the
    /// official clients expect.
    pub fn message(&self) -> Option<&str> {
        message_body(&self.files)
    }

    /// Answers the prepare-upload with all offered files accepted. Returns
    /// `false` when the decision was already answered (or the peer is gone).
    pub fn accept(&mut self) -> bool {
        let Some(decision) = self.decision.take() else {
            return false;
        };
        let ids: HashSet<String> = self.files.keys().cloned().collect();
        self.accepted = decision.send(PrepareUploadDecisionV2::Accept(ids)).is_ok();
        self.last_activity = Instant::now();
        self.accepted
    }

    /// Answers the prepare-upload with "no files accepted" (HTTP 204); used
    /// for message transfers, which need no file download.
    pub fn accept_nothing(&mut self) -> bool {
        let Some(decision) = self.decision.take() else {
            return false;
        };
        self.accepted = decision
            .send(PrepareUploadDecisionV2::Accept(HashSet::new()))
            .is_ok();
        self.last_activity = Instant::now();
        self.accepted
    }

    /// Answers the prepare-upload with a decline (HTTP 403).
    pub fn decline(&mut self) {
        if let Some(decision) = self.decision.take() {
            let _ = decision.send(PrepareUploadDecisionV2::Decline);
        }
    }

    /// Records progress of one file and reports the aggregated transfer
    /// progress, throttled to <= 20 events per second.
    pub fn report_progress(
        &mut self,
        events: &async_channel::Sender<TransferEvent>,
        file_id: &str,
        bytes: u64,
    ) {
        self.last_activity = Instant::now();
        let size = self.files.get(file_id).map_or(bytes, |file| file.size);
        let slot = self.progress.entry(file_id.to_string()).or_insert(0);
        let next = (*slot).max(bytes.min(size));
        if next > *slot {
            self.transferred_total = self.transferred_total.saturating_add(next - *slot);
            *slot = next;
        }
        if self.last_emit.elapsed() < PROGRESS_INTERVAL {
            return;
        }
        self.last_emit = Instant::now();
        self.emit_progress(events);
    }

    /// Emits the final byte count before a terminal event; progress events
    /// may have been throttled away.
    pub fn finish_progress(&mut self, events: &async_channel::Sender<TransferEvent>) {
        self.emit_progress(events);
    }

    /// Records that `file_id` reached its full size, keeping the aggregate
    /// byte count in O(1) (a completed file may already have been partially
    /// counted by `report_progress`, so only the difference is added).
    pub fn record_file_done(&mut self, file_id: &str, size: u64) {
        let slot = self.progress.entry(file_id.to_string()).or_insert(0);
        let next = (*slot).max(size.min(self.files.get(file_id).map_or(size, |file| file.size)));
        if next > *slot {
            self.transferred_total = self.transferred_total.saturating_add(next - *slot);
            *slot = next;
        }
    }

    /// Reserves text budget for one received text file (NET-05). A file that
    /// would push the session over [`MAX_SESSION_TEXT_BYTES`] is still saved to
    /// disk; only the UI copy is dropped.
    pub fn allow_text(&mut self, size: u64) -> bool {
        if size > MAX_TEXT_FILE_BYTES {
            return false;
        }
        if self
            .text_bytes
            .checked_add(size)
            .is_none_or(|total| total > MAX_SESSION_TEXT_BYTES)
        {
            return false;
        }
        self.text_bytes += size;
        true
    }

    /// Next collision suffix to try when publishing `relative` (NET-04).
    pub fn collision_hint(&self, relative: &std::path::Path) -> u32 {
        self.collision_next.get(relative).copied().unwrap_or(0)
    }

    /// Remembers the next free suffix for `relative` (monotonic per session).
    pub fn set_collision_hint(&mut self, relative: std::path::PathBuf, next: u32) {
        self.collision_next.insert(relative, next);
    }

    fn emit_progress(&self, events: &async_channel::Sender<TransferEvent>) {
        let transferred = self.transferred_total.min(self.total());
        emit(
            events,
            TransferEvent::Progress {
                id: self.id.clone(),
                transferred,
                total: Some(self.total()),
            },
        );
    }

    /// Whether every accepted file reached a successful result.
    pub fn complete(&self) -> bool {
        self.done == self.files.len()
    }

    /// A staging path for a file that has no temp file yet (or the existing
    /// one, so a checksum retry overwrites the same staging file).
    ///
    /// No file system call: the staging directory is created once at startup
    /// and `naming::temp_path` only builds a unique path, so this stays cheap
    /// on the async worker (NET-10).
    pub fn temp_for(
        &mut self,
        file_id: &str,
        temp_dir: &std::path::Path,
    ) -> Result<std::path::PathBuf> {
        if let Some(path) = self.temps.get(file_id) {
            return Ok(path.clone());
        }
        let path = naming::temp_path(temp_dir);
        self.temps.insert(file_id.to_string(), path.clone());
        Ok(path)
    }

    /// The staging paths this session created (for off-loop cleanup).
    pub fn temp_paths(&self) -> Vec<std::path::PathBuf> {
        self.temps.values().cloned().collect()
    }
}

/// Best-effort removal of staging files off the async workers (NET-10): a
/// cancelled receive can leave thousands of `.part` files, and deleting them
/// with synchronous `remove_file` on the actor/worker would stall it.
///
/// Detached: the actor must not wait for the deletions either. While it
/// awaited them, server events (including prepare-upload requests the server
/// itself is waiting on) queued up behind a slow disk.
pub fn cleanup_staging(paths: Vec<std::path::PathBuf>) {
    if paths.is_empty() {
        return;
    }
    drop(tokio::task::spawn_blocking(move || remove_all(paths)));
}

/// [`cleanup_staging`], awaited: for shutdown, where the process may exit
/// right after and the files must be gone first.
pub async fn cleanup_staging_now(paths: Vec<std::path::PathBuf>) {
    if paths.is_empty() {
        return;
    }
    let _ = tokio::task::spawn_blocking(move || remove_all(paths)).await;
}

fn remove_all(paths: Vec<std::path::PathBuf>) {
    for path in paths {
        let _ = std::fs::remove_file(path);
    }
}

/// Free space available to the current user on the volume holding `path`
/// (Unix `statvfs(3)`). `None` when it cannot be determined.
pub fn available_space(path: &std::path::Path) -> Option<u64> {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        let c_path = std::ffi::CString::new(path.as_os_str().as_bytes()).ok()?;
        // SAFETY: `statvfs` only writes into the provided struct; the path is a
        // valid NUL-terminated C string for the duration of the call.
        let mut stats: libc::statvfs = unsafe { std::mem::zeroed() };
        if unsafe { libc::statvfs(c_path.as_ptr(), &mut stats) } != 0 {
            return None;
        }
        Some((stats.f_bavail as u64).saturating_mul(stats.f_frsize as u64))
    }
    #[cfg(not(unix))]
    {
        let _ = path;
        // TODO(NET-03): Windows has GetDiskFreeSpaceExW; the app has no direct
        // `windows-sys` dependency. The offer is accepted unchecked there.
        None
    }
}

/// Refuses an offer that would not fit into the free space of the destination
/// volume. Pure so the policy is testable without touching a disk.
pub fn ensure_capacity(total: u64, available: Option<u64>) -> Result<()> {
    let Some(available) = available else {
        return Ok(());
    };
    let budget = available / 100 * DISK_HEADROOM_PERCENT;
    ensure!(
        total <= budget,
        "There is not enough free space for this transfer ({total} bytes offered, {available} bytes free)"
    );
    Ok(())
}

/// The message body of a LocalSend *message* offer (one text file whose whole
/// content is already in the preview), if this is one. A message needs no disk
/// space and writes no bytes.
pub fn message_body(files: &HashMap<String, FileDto>) -> Option<&str> {
    if files.len() != 1 {
        return None;
    }
    let file = files.values().next()?;
    if file.file_type != "text" && !file.file_type.starts_with("text/") {
        return None;
    }
    file.preview.as_deref()
}

/// The app-facing view of one offered file.
pub fn offered_file(file: &FileDto) -> OfferedFile {
    OfferedFile {
        name: file.file_name.clone(),
        size: file.size,
        mime: file.file_type.clone(),
        saved_to: None,
    }
}

/// Validates an untrusted prepare-upload offer before it reaches the UI or the
/// file system: sane file count, matching ids, safe names and a total size
/// that fits in a `u64`.
pub fn validate_offer(files: &HashMap<String, FileDto>) -> Result<()> {
    ensure!(
        !files.is_empty() && files.len() <= MAX_OFFER_FILES,
        "The request must contain between 1 and 10,000 files"
    );
    let mut total = 0u64;
    for (id, file) in files {
        ensure!(*id == file.id, "The request contains inconsistent file IDs");
        // The peer-supplied name never appears in the error: it is unbounded
        // attacker text and the user does not need it (NET-08).
        naming::relative_name(&file.file_name)
            .map_err(|_| anyhow::anyhow!("The request contains an invalid file name"))?;
        total = total
            .checked_add(file.size)
            .context("The transfer is too large")?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn file(id: &str, name: &str, mime: &str, preview: Option<&str>) -> FileDto {
        FileDto {
            id: id.to_string(),
            file_name: name.to_string(),
            size: 3,
            file_type: mime.to_string(),
            sha256: None,
            preview: preview.map(str::to_string),
            metadata: None,
        }
    }

    #[test]
    fn validates_offers() {
        let mut files = HashMap::new();
        files.insert("0".to_string(), file("0", "photo.jpg", "image/jpeg", None));
        assert!(validate_offer(&files).is_ok());

        files.insert("1".to_string(), file("x", "other.jpg", "image/jpeg", None));
        assert!(validate_offer(&files).is_err());

        let mut files = HashMap::new();
        files.insert("0".to_string(), file("0", "../escape", "image/jpeg", None));
        assert!(validate_offer(&files).is_err());
    }

    #[test]
    fn recognizes_message_transfers() {
        let (decision, _rx) = oneshot::channel();
        let mut files = HashMap::new();
        files.insert(
            "0".to_string(),
            file("0", "Message.txt", "text/plain", Some("hello")),
        );
        let peer = Peer {
            fingerprint: "PEER".to_string(),
            alias: "Peer".to_string(),
            device_model: String::new(),
            address: None,
            device_type: crate::net::DeviceType::Desktop,
            is_self: false,
        };
        let incoming = Incoming::new("session".to_string(), peer, files, decision);
        assert_eq!(incoming.message(), Some("hello"));
    }

    fn peer() -> Peer {
        Peer {
            fingerprint: "PEER".to_string(),
            alias: "Peer".to_string(),
            device_model: String::new(),
            address: None,
            device_type: crate::net::DeviceType::Desktop,
            is_self: false,
        }
    }

    fn file_transfer() -> (Incoming, oneshot::Receiver<PrepareUploadDecisionV2>) {
        let (decision, rx) = oneshot::channel();
        let files = HashMap::from([("0".to_string(), file("0", "photo.jpg", "image/jpeg", None))]);
        (
            Incoming::new("session".to_string(), peer(), files, decision),
            rx,
        )
    }

    /// Deciding twice, or declining after the quick-save already accepted, must
    /// not drop the session bookkeeping of a running transfer: the actor turns
    /// the late decline into a cancel instead (a real race: the UI sheet can
    /// still be open when the auto-accept fires).
    #[test]
    fn decision_routing_handles_accept_decline_races() {
        let (pending, _pending_rx) = file_transfer();
        assert_eq!(
            decision_action(Some(&pending), "session", true),
            DecisionAction::AcceptFiles
        );
        assert_eq!(
            decision_action(Some(&pending), "session", false),
            DecisionAction::Decline
        );
        // A stale id (already answered and taken out of the actor) is ignored.
        assert_eq!(
            decision_action(Some(&pending), "other-session", true),
            DecisionAction::Ignore
        );
        assert_eq!(
            decision_action(None, "session", true),
            DecisionAction::Ignore
        );

        let (mut accepted, _accepted_rx) = file_transfer();
        assert!(accepted.accept());
        assert_eq!(
            decision_action(Some(&accepted), "session", true),
            DecisionAction::Ignore,
            "a double accept must not answer the responder again"
        );
        assert_eq!(
            decision_action(Some(&accepted), "session", false),
            DecisionAction::CancelAccepted,
            "a decline after accept must cancel, not strand the core session"
        );

        let files = HashMap::from([(
            "0".to_string(),
            file("0", "Message.txt", "text/plain", Some("hello")),
        )]);
        let message = Incoming::new("session".to_string(), peer(), files, oneshot::channel().0);
        assert_eq!(
            decision_action(Some(&message), "session", true),
            DecisionAction::AcceptMessage
        );
    }

    /// Receive progress is coalesced: a fast sender produces far more write
    /// callbacks than the UI may see, and the terminal count still arrives.
    #[test]
    fn progress_events_are_coalesced_but_the_final_count_is_emitted() {
        const SIZE: u64 = 1_000_000;
        let (events, emitted) = async_channel::unbounded();
        let (decision, _rx) = oneshot::channel();
        let files = HashMap::from([(
            "0".to_string(),
            FileDto {
                size: SIZE,
                ..file("0", "big.bin", "application/octet-stream", None)
            },
        )]);
        let mut incoming = Incoming::new("session".to_string(), peer(), files, decision);
        // 10,000 write callbacks inside one 50 ms window.
        for written in (0..10_000u64).map(|step| step * (SIZE / 10_000)) {
            incoming.report_progress(&events, "0", written);
        }
        let progress = drain(&emitted);
        assert!(
            progress.len() <= 2,
            "10,000 progress updates produced {} events, expected <= 2",
            progress.len()
        );
        // The bytes are still tracked, so the terminal event is exact.
        incoming.report_progress(&events, "0", SIZE);
        incoming.finish_progress(&events);
        let progress = drain(&emitted);
        let last = progress.last().expect("a final progress event");
        assert_eq!(
            last,
            &(SIZE, SIZE),
            "the final byte count must reach the UI"
        );
    }

    /// Collects `(transferred, total)` from every queued progress event.
    fn drain(events: &async_channel::Receiver<crate::net::TransferEvent>) -> Vec<(u64, u64)> {
        let mut progress = Vec::new();
        while let Ok(event) = events.try_recv() {
            if let crate::net::TransferEvent::Progress {
                transferred,
                total: Some(total),
                ..
            } = event
            {
                progress.push((transferred, total));
            }
        }
        progress
    }

    /// A peer-supplied message body is bounded before it reaches the UI.
    #[test]
    fn message_text_is_bounded() {
        assert_eq!(clamp_text("hello"), "hello");
        let huge = "x".repeat(MAX_MESSAGE_BYTES * 2);
        assert_eq!(clamp_text(&huge).len(), MAX_MESSAGE_BYTES);
        // Truncation must not split a multi-byte character.
        let mut unicode = "é".repeat(MAX_MESSAGE_BYTES);
        unicode.push('€');
        let clamped = clamp_text(&unicode);
        assert_eq!(clamped.len(), MAX_MESSAGE_BYTES);
    }

    /// NET-05: the per-file cap alone does not bound the UI: a session may
    /// offer 10,000 tiny text files. The session budget stops forwarding text
    /// once it is exhausted; oversized files never qualify.
    #[test]
    fn text_forwarding_is_budgeted_per_session() {
        let (mut incoming, _rx) = file_transfer();
        let per_file = MAX_SESSION_TEXT_BYTES / MAX_TEXT_FILE_BYTES;
        for _ in 0..per_file {
            assert!(
                incoming.allow_text(MAX_TEXT_FILE_BYTES),
                "the budget must accept its documented capacity"
            );
        }
        assert!(
            !incoming.allow_text(MAX_TEXT_FILE_BYTES),
            "the session budget must be enforced"
        );
        assert!(
            !incoming.allow_text(1),
            "an exhausted budget stays exhausted"
        );

        let (mut fresh, _rx) = file_transfer();
        assert!(
            !fresh.allow_text(MAX_TEXT_FILE_BYTES + 1),
            "a file over the per-file cap never qualifies"
        );
        assert!(
            fresh.allow_text(1),
            "a rejected file must not consume the session budget"
        );
        // Fill the rest of the session budget with per-file-cap-sized chunks,
        // then any further file must be refused.
        let mut remaining = MAX_SESSION_TEXT_BYTES - 1;
        while remaining > 0 {
            let chunk = remaining.min(MAX_TEXT_FILE_BYTES);
            if !fresh.allow_text(chunk) {
                break;
            }
            remaining -= chunk;
        }
        assert_eq!(remaining, 0, "the budget must accept exactly its capacity");
        assert!(
            !fresh.allow_text(1),
            "the budget is exact, not a lower bound"
        );
    }

    /// NET-07: the aggregate byte count is maintained incrementally, so the
    /// terminal progress event is exact without summing up to 10,000 entries.
    #[test]
    fn completed_files_are_counted_once_in_the_aggregate() {
        let (events, emitted) = async_channel::unbounded();
        let (decision, _rx) = oneshot::channel();
        let mut files = HashMap::new();
        for index in 0..1_000u64 {
            files.insert(
                index.to_string(),
                FileDto {
                    size: 10,
                    ..file(&index.to_string(), "t.txt", "text/plain", None)
                },
            );
        }
        let mut incoming = Incoming::new("session".to_string(), peer(), files, decision);
        for index in 0..1_000u64 {
            let id = index.to_string();
            incoming.report_progress(&events, &id, 5);
            incoming.record_file_done(&id, 10);
        }
        incoming.finish_progress(&events);
        let progress = drain(&emitted);
        assert_eq!(
            progress.last(),
            Some(&(10_000, 10_000)),
            "the final count must include every completed file exactly once"
        );
    }

    /// NET-03: an offer must not fill the destination volume. The policy is
    /// pure so it can be tested without touching a disk.
    #[test]
    fn offers_are_refused_when_free_space_is_insufficient() {
        assert!(ensure_capacity(100, Some(1_000)).is_ok());
        assert!(ensure_capacity(900, Some(1_000)).is_ok());
        assert!(
            ensure_capacity(901, Some(1_000)).is_err(),
            "a receive must leave headroom on the volume"
        );
        assert!(
            ensure_capacity(u64::MAX, None).is_ok(),
            "an unknown free space must not block receiving (documented fallback)"
        );
        // The check works on a real path too (the test directory exists).
        assert!(available_space(&std::env::temp_dir()).is_some_and(|space| space > 0));
    }

    /// NET-08: the rejection reason is bounded and never echoes the peer's raw
    /// file name.
    #[test]
    fn invalid_offer_errors_do_not_echo_peer_names() {
        let name = format!("../{}", "x".repeat(4096));
        let mut files = HashMap::new();
        files.insert("0".to_string(), file("0", &name, "text/plain", None));
        let error = validate_offer(&files).expect_err("traversal must be rejected");
        let message = format!("{error:#}");
        assert!(message.contains("invalid file name"));
        assert!(!message.contains("xxxx"), "the peer name leaked: {message}");
        assert!(message.len() < 128);
    }
}
