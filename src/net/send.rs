//! The sending side: prepare-upload, uploads with progress, PIN retry and
//! cancellation.
//!
//! One [`SendJob`] runs per outgoing transfer on the actor's `JoinSet`. It
//! reports its outcome to the actor, which owns the transfer bookkeeping and
//! turns the outcome into a terminal [`TransferEvent`]. The PIN path returns
//! [`SendOutcome::PinRequired`] instead of failing: the actor keeps the job and
//! retries it with the PIN the user entered, under the same transfer id.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, ensure};
use futures_util::StreamExt;
use localsend::crypto::hash::{HashError, sha256_file_content, sha256_hex};
use localsend::http::client::{ClientError, LsHttpClientV2};
use localsend::http::dto_v2::{
    PrepareUploadRequestDtoV2, PrepareUploadResponseDtoV2, RegisterDtoV2,
};
use localsend::model::transfer::{FileContent, FileDto, FileMetadata};
use tokio_util::sync::CancellationToken;

use crate::net::identity::Identity;
use crate::net::{OfferedFile, Peer, PeerAddress, Protocol, TransferEvent, emit};

/// Files above this size are offered without a checksum. Hashing reads the
/// whole file before the offer goes out, with no progress to show: a 20 GB
/// video stalled the transfer at 0 % for minutes and read the disk twice.
/// TLS (the default) still protects the bytes in transit.
const MAX_HASHED_BYTES: u64 = 1024 * 1024 * 1024;

/// How long a receiver may take to answer a prepare-upload request before the
/// send is failed (the receiver's user may leave the dialog open).
const PREPARE_TIMEOUT: Duration = Duration::from_secs(300);

/// How long the best-effort `/cancel` notification may take.
const CANCEL_NOTIFY_TIMEOUT: Duration = Duration::from_secs(3);

/// How often upload progress is reported (at most 20 events per second).
const PROGRESS_INTERVAL: Duration = Duration::from_millis(50);

/// How often one file is retried after a checksum mismatch (the receiver
/// resets the file to pending; the token stays valid).
const UPLOAD_ATTEMPTS: u32 = 3;

/// Largest text sent as a LocalSend message (carried in the preview). Longer
/// text is offered as a regular `.txt` file, matching the official clients.
const MAX_MESSAGE_BYTES: usize = 64_000;

/// User-facing text for "the receiver accepted nothing". Shared by the 200
/// empty-token-map and the 204-on-a-file-transfer cases: both are protocol
/// violations that must never be reported as success.
const NOTHING_ACCEPTED: &str = "The receiver did not accept any files";

/// Progress events are unthrottled per chunk; this caps them per transfer.
struct ProgressReporter {
    events: async_channel::Sender<TransferEvent>,
    id: String,
    total: u64,
    transferred: u64,
    last: Instant,
}

impl ProgressReporter {
    fn new(events: async_channel::Sender<TransferEvent>, id: String, total: u64) -> Self {
        Self {
            events,
            id,
            total,
            transferred: 0,
            last: Instant::now() - PROGRESS_INTERVAL,
        }
    }

    /// Reports the bytes sent so far, throttled to [`PROGRESS_INTERVAL`].
    fn report(&mut self, transferred: u64) {
        self.transferred = transferred.min(self.total);
        if self.last.elapsed() < PROGRESS_INTERVAL {
            return;
        }
        self.last = Instant::now();
        self.emit();
    }

    /// Reports the final byte count; terminal events follow this one.
    fn finish(&mut self, transferred: u64) {
        self.transferred = transferred.min(self.total);
        self.emit();
    }

    fn emit(&self) {
        emit(
            &self.events,
            TransferEvent::Progress {
                id: self.id.clone(),
                transferred: self.transferred,
                total: Some(self.total),
            },
        );
    }
}

/// Where an outgoing file's bytes come from.
#[derive(Clone, Debug)]
pub enum SendContent {
    File(PathBuf),
    Text(String),
}

/// One item of an outgoing transfer.
#[derive(Clone, Debug)]
pub struct Upload {
    pub offered: OfferedFile,
    pub content: SendContent,
}

impl Upload {
    /// A regular file offered under `name` (the composer's display name).
    pub fn file(path: PathBuf, name: String) -> Result<Self> {
        let metadata =
            std::fs::metadata(&path).with_context(|| format!("Cannot read {}", path.display()))?;
        ensure!(
            metadata.is_file(),
            "{} is not a regular file",
            path.display()
        );
        // The wire name always uses `/` (a Windows `PathBuf` would print `\`).
        let name = crate::net::naming::relative_name(&name)
            .with_context(|| format!("Invalid file name for {}", path.display()))?
            .components()
            .map(|component| component.as_os_str().to_string_lossy())
            .collect::<Vec<_>>()
            .join("/");
        let mime = mime_guess::from_path(&path)
            .first_or_octet_stream()
            .to_string();
        Ok(Self {
            offered: OfferedFile {
                name,
                size: metadata.len(),
                mime,
                saved_to: None,
            },
            content: SendContent::File(path),
        })
    }

    /// A text block, sent the way LocalSend sends messages: a single
    /// `text/plain` file whose `preview` carries the text, so a receiver that
    /// answers 204 has nothing left to download.
    pub fn text(text: String) -> Self {
        Self {
            offered: OfferedFile {
                name: "Message.txt".to_string(),
                size: text.len() as u64,
                mime: "text/plain".to_string(),
                saved_to: None,
            },
            content: SendContent::Text(text),
        }
    }

    fn dto(&self, id: String, sha256: Option<String>) -> FileDto {
        let preview = match &self.content {
            // A message carries its whole text in the preview so a receiver
            // can answer 204 (nothing to download). Text too long for a
            // message is sent as a normal .txt file instead, exactly like the
            // official clients.
            SendContent::Text(text) if text.len() <= MAX_MESSAGE_BYTES => Some(text.clone()),
            SendContent::Text(_) | SendContent::File(_) => None,
        };
        let metadata = match &self.content {
            SendContent::File(path) => FileMetadata::from_path(path),
            SendContent::Text(_) => None,
        };
        FileDto {
            id,
            file_name: self.offered.name.clone(),
            size: self.offered.size,
            file_type: self.offered.mime.clone(),
            sha256,
            preview,
            metadata,
        }
    }
}

/// Whether an upload is a single message that a receiver may acknowledge with
/// 204: the whole body is already in the `preview`, so there is nothing left
/// to download.
fn is_message(uploads: &[Upload]) -> bool {
    match uploads {
        [upload] => {
            matches!(&upload.content, SendContent::Text(text) if text.len() <= MAX_MESSAGE_BYTES)
        }
        _ => false,
    }
}

/// The outcome of a `204 No Content` answer to prepare-upload.
///
/// The protocol reserves 204 for "there is nothing to upload" and the only
/// valid reason is a message transfer whose text the receiver took from the
/// preview. For a file transfer
/// 204 means the receiver accepted *nothing*; reporting `Completed` there would
/// be a visible fake success (the same protocol violation as a 200 with an
/// empty token map).
fn zero_response_outcome(uploads: &[Upload]) -> SendOutcome {
    if is_message(uploads) {
        SendOutcome::Completed
    } else {
        SendOutcome::Failed(NOTHING_ACCEPTED.to_string())
    }
}

/// Sum of the sizes of every offered upload (the denominator the UI showed in
/// `Started`). Never shrinks to the accepted subset: a receiver that unchecks
/// files must stay visible as "skipped" in the progress display (NET-09).
fn offer_total(uploads: &[Upload]) -> u64 {
    uploads
        .iter()
        .try_fold(0u64, |sum, upload| sum.checked_add(upload.offered.size))
        .unwrap_or(u64::MAX)
}

/// The uploads the receiver accepted, with their wire file id and token, in
/// send order.
///
/// A receiver may accept a subset of the offer (the stock app lets its user
/// uncheck files): the rest are skipped, not a failure. An *empty* token map on
/// a 200 is a protocol violation — "nothing accepted" is expressed as a 204 —
/// so it must not be reported as a completed transfer.
fn accepted_uploads<'a>(
    uploads: &'a [Upload],
    response: &'a PrepareUploadResponseDtoV2,
) -> std::result::Result<Vec<(usize, &'a Upload, &'a str)>, SendOutcome> {
    if response.files.is_empty() {
        return Err(SendOutcome::Failed(NOTHING_ACCEPTED.to_string()));
    }
    Ok(uploads
        .iter()
        .enumerate()
        .filter_map(|(index, upload)| {
            response
                .files
                .get(&index.to_string())
                .map(|token| (index, upload, token.as_str()))
        })
        .collect())
}

/// User-facing text for a failed request. `ClientError`'s `Display` for a
/// status is `403;Some("Invalid token or IP address")`, and reqwest errors
/// carry request internals; neither belongs in the UI.
fn describe_client_error(error: &ClientError) -> String {
    match error {
        ClientError::Cancelled => "The transfer was cancelled".to_string(),
        ClientError::StatusCode(error) => match error.status {
            401 => "The receiver requires a PIN".to_string(),
            403 => "The receiver cancelled the transfer or rejected the file".to_string(),
            409 => "The receiver is busy with another transfer".to_string(),
            422 => "The receiver reported a checksum mismatch".to_string(),
            status => format!("The receiver rejected the file (HTTP {status})"),
        },
        ClientError::Reqwest(_) | ClientError::Io(_) => {
            "The connection to the receiver failed".to_string()
        }
        ClientError::Json(_) | ClientError::Other(_) => {
            "The receiver sent an invalid response".to_string()
        }
    }
}

/// Everything one outgoing transfer needs.
pub struct SendJob {
    pub id: String,
    pub peer: Peer,
    pub uploads: Vec<Upload>,
    pub pin: Option<String>,
    pub identity: Identity,
    pub register: RegisterDtoV2,
    pub cancel: CancellationToken,
    /// Set by the actor when the peer cancelled the session itself; the job
    /// must then not echo `/cancel` back (protocol checklist #17).
    pub peer_cancelled: Arc<AtomicBool>,
    pub remote_session: Arc<Mutex<Option<String>>>,
    /// Bumped by the send task on every sign of life (prepare answered, bytes
    /// written to the socket). The actor's 1 s tick turns a counter that stopped
    /// moving into an idle-timeout failure (NET-12).
    pub activity: Arc<AtomicU64>,
    pub events: async_channel::Sender<TransferEvent>,
}

/// Why a send job ended. The actor maps this (plus the cancel token) to the
/// terminal event the UI sees.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SendOutcome {
    /// Everything offered was transferred (or the receiver answered 204 for a
    /// message transfer, which means it took the text from the preview).
    Completed,
    /// The user cancelled, or the receiver cancelled.
    Cancelled,
    /// The receiver wants a PIN; retry the same job with one.
    PinRequired,
    /// The transfer failed for good; the string is user-facing.
    Failed(String),
}

impl SendJob {
    /// Bumps the liveness counter the actor's idle watchdog watches.
    fn note_activity(&self) {
        self.activity.fetch_add(1, Ordering::Relaxed);
    }

    /// Hashes every file before the offer is sent, so the receiver can verify
    /// the bytes end to end (NET-11/N17). Text messages are hashed from memory.
    ///
    /// Hashing can be cancelled; a failure to read a file is not fatal — the
    /// offer then simply carries no checksum for that file, which is the old
    /// behaviour and leaves TLS as the only integrity check.
    async fn hashes(&self) -> Option<Vec<Option<String>>> {
        let mut hashes = Vec::with_capacity(self.uploads.len());
        for upload in &self.uploads {
            let hash = match &upload.content {
                SendContent::Text(text) => Some(sha256_hex(text.as_bytes())),
                SendContent::File(_) if upload.offered.size > MAX_HASHED_BYTES => None,
                SendContent::File(path) => {
                    match sha256_file_content(FileContent::Path(path.clone()), &self.cancel, |_| {})
                        .await
                    {
                        Ok(hash) => Some(hash),
                        Err(HashError::Cancelled) => return None,
                        Err(error) => {
                            tracing::warn!(
                                file = %upload.offered.name,
                                %error,
                                "Cannot hash the outgoing file; the offer will not carry a checksum"
                            );
                            None
                        }
                    }
                }
            };
            hashes.push(hash);
        }
        Some(hashes)
    }

    pub async fn run(self) -> SendOutcome {
        let Some(address) = self.peer.address.clone() else {
            return SendOutcome::Failed("This device is not reachable right now".to_string());
        };
        let protocol = address.protocol;
        let client = match self.client(protocol) {
            Ok(client) => client,
            Err(error) => return SendOutcome::Failed(format!("{error:#}")),
        };

        // Hash before the offer: the receiver verifies each declared checksum
        // and may retry a mismatched file (the 422 retry path below). A
        // cancellation during hashing ends the transfer before any network I/O.
        let Some(hashes) = self.hashes().await else {
            return SendOutcome::Cancelled;
        };
        let files: HashMap<String, FileDto> = self
            .uploads
            .iter()
            .enumerate()
            .map(|(index, upload)| {
                let id = index.to_string();
                let sha256 = hashes.get(index).cloned().flatten();
                (id.clone(), upload.dto(id, sha256))
            })
            .collect();
        if files.is_empty() {
            return SendOutcome::Failed("There is nothing to send".to_string());
        }

        let prepared = match tokio::time::timeout(
            PREPARE_TIMEOUT,
            client.prepare_upload(
                crate::net::protocol_to_core(protocol),
                &address.host,
                address.port,
                None,
                PrepareUploadRequestDtoV2 {
                    info: self.register.clone(),
                    files,
                },
                self.pin.as_deref(),
                self.cancel.clone(),
            ),
        )
        .await
        {
            Err(_) => {
                return SendOutcome::Failed("The receiver did not respond".to_string());
            }
            Ok(Ok(prepared)) => prepared,
            Ok(Err(error)) => return classify_prepare_error(&error),
        };

        // 204: nothing needs to be uploaded. Only valid for a message transfer
        // whose text the receiver took from the preview; for files it means
        // "nothing accepted" and must never surface as success (NET-01).
        let Some(response) = prepared.response else {
            return zero_response_outcome(&self.uploads);
        };

        *self
            .remote_session
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(response.session_id.clone());
        self.note_activity();
        emit(
            &self.events,
            TransferEvent::Accepted {
                id: self.id.clone(),
            },
        );

        let outcome = self.upload_all(&client, &response).await;
        if outcome != SendOutcome::Completed && !self.peer_cancelled.load(Ordering::Relaxed) {
            // Best effort: tell the receiver we gave up, otherwise it keeps
            // waiting for the remaining files. Skipped when the peer initiated
            // the cancellation: it already knows and does not need the echo.
            let _ = tokio::time::timeout(
                CANCEL_NOTIFY_TIMEOUT,
                client.cancel(
                    crate::net::protocol_to_core(protocol),
                    &address.host,
                    address.port,
                    &response.session_id,
                ),
            )
            .await;
        }
        outcome
    }

    fn client(&self, protocol: Protocol) -> Result<LsHttpClientV2> {
        // The core's fourth argument is a *total* request timeout, not a
        // connect timeout: setting it would abort a legitimate slow transfer
        // mid-upload. A black-holed peer therefore still costs the OS-level
        // connect timeout here; the actor's 90 s no-byte watchdog (NET-12)
        // ends the transfer once prepare-upload was answered. An actual
        // `connect_timeout` is BLOCKED-UPSTREAM: `LsHttpClientV2` exposes only
        // the total timeout.
        match protocol {
            Protocol::Https => LsHttpClientV2::try_new(
                &self.identity.private_key_pem,
                &self.identity.cert_pem,
                // Pin the peer: a mismatching certificate must never receive
                // the offer or any file bytes.
                Some(self.peer.fingerprint.clone()),
                None,
            )
            .context("Cannot set up the encrypted connection"),
            Protocol::Http => {
                LsHttpClientV2::try_new_without_cert().context("Cannot set up the HTTP connection")
            }
        }
    }

    async fn upload_all(
        &self,
        client: &LsHttpClientV2,
        response: &PrepareUploadResponseDtoV2,
    ) -> SendOutcome {
        let address = self.peer.address.as_ref().expect("checked in run");
        let protocol = crate::net::protocol_to_core(address.protocol);

        let accepted = match accepted_uploads(&self.uploads, response) {
            Ok(accepted) => accepted,
            Err(outcome) => return outcome,
        };
        // The denominator is the *full* offer, not the accepted subset: a
        // receiver that unchecks files must not make them vanish from the
        // progress display (NET-09). The byte count only covers what was
        // actually uploaded, so the skipped files stay visible as a gap.
        let total = offer_total(&self.uploads);
        let mut progress = ProgressReporter::new(self.events.clone(), self.id.clone(), total);

        for (index, upload, token) in accepted {
            if self.cancel.is_cancelled() {
                progress.finish(progress.transferred);
                return SendOutcome::Cancelled;
            }
            // Bytes reported by a failed attempt must not be added twice when
            // the same file is retried after a checksum mismatch.
            let attempts_base = progress.transferred;
            let mut attempt = 0u32;
            loop {
                attempt += 1;
                match self
                    .upload_one(
                        client,
                        protocol,
                        &address.host,
                        address.port,
                        &response.session_id,
                        &index.to_string(),
                        token,
                        upload,
                        &mut progress,
                    )
                    .await
                {
                    Ok(()) => break,
                    Err(ClientError::Cancelled) => {
                        progress.finish(progress.transferred);
                        return SendOutcome::Cancelled;
                    }
                    Err(ClientError::StatusCode(error))
                        if error.status == 422 && attempt < UPLOAD_ATTEMPTS =>
                    {
                        // Checksum mismatch: the receiver reset the file to
                        // pending, retry with the same token.
                        tracing::warn!(
                            file = %upload.offered.name,
                            attempt,
                            "Receiver reported a checksum mismatch, retrying"
                        );
                        progress.transferred = attempts_base;
                        tokio::time::sleep(Duration::from_millis(100)).await;
                    }
                    Err(error) => {
                        progress.finish(progress.transferred);
                        return SendOutcome::Failed(format!(
                            "{}: {}",
                            upload.offered.name,
                            describe_client_error(&error)
                        ));
                    }
                }
            }
            // `upload_one` already reported this file's final byte count.
            self.note_activity();
        }

        progress.finish(progress.transferred);
        // A receiver may accept a subset of the offer (the stock app lets the
        // user uncheck files); the skipped ones are not an error, exactly like
        // the Flutter sender marks them "skipped" and finishes.
        SendOutcome::Completed
    }

    #[allow(clippy::too_many_arguments)]
    async fn upload_one(
        &self,
        client: &LsHttpClientV2,
        protocol: localsend::model::discovery::ProtocolType,
        host: &str,
        port: u16,
        session_id: &str,
        file_id: &str,
        token: &str,
        upload: &Upload,
        progress: &mut ProgressReporter,
    ) -> Result<(), ClientError> {
        let base = progress.transferred;
        let size = upload.offered.size;
        // Bytes actually read from the file; a failed upload reports the
        // partial count so the terminal event carries a real number.
        let sent = Arc::new(AtomicU64::new(0));
        let events = self.events.clone();
        let id = self.id.clone();
        let total = progress.total;
        let mut last = Instant::now();
        let body = match &upload.content {
            SendContent::Text(text) => localsend::reqwest::Body::from(text.clone()),
            SendContent::File(path) => {
                let sent = Arc::clone(&sent);
                let activity = Arc::clone(&self.activity);
                let stream = FileContent::Path(path.clone())
                    .into_stream()
                    .map(move |chunk| {
                        let chunk = chunk?;
                        activity.fetch_add(1, Ordering::Relaxed);
                        let count = sent.fetch_add(chunk.len() as u64, Ordering::Relaxed)
                            + chunk.len() as u64;
                        if last.elapsed() >= PROGRESS_INTERVAL {
                            last = Instant::now();
                            emit(
                                &events,
                                TransferEvent::Progress {
                                    id: id.clone(),
                                    transferred: (base + count).min(total),
                                    total: Some(total),
                                },
                            );
                        }
                        Ok::<_, std::io::Error>(chunk)
                    });
                localsend::reqwest::Body::wrap_stream(stream)
            }
        };

        let outcome = client
            .upload(
                protocol,
                host,
                port,
                None,
                session_id,
                file_id,
                token,
                body,
                self.cancel.clone(),
            )
            .await;
        match outcome {
            Ok(()) => {
                self.note_activity();
                progress.report(base + size);
                Ok(())
            }
            Err(error) => {
                let partial = sent.load(Ordering::Relaxed).min(size);
                progress.finish(base + partial);
                Err(error)
            }
        }
    }
}

/// Turns a failed prepare-upload into the right outcome. 401 means "ask for a
/// PIN", everything else is a user-facing failure.
fn classify_prepare_error(error: &ClientError) -> SendOutcome {
    match error {
        ClientError::Cancelled => SendOutcome::Cancelled,
        ClientError::StatusCode(error) => match error.status {
            401 => SendOutcome::PinRequired,
            403 => SendOutcome::Failed("The receiver declined the transfer".to_string()),
            409 => SendOutcome::Failed("The receiver is busy with another transfer".to_string()),
            429 => SendOutcome::Failed("Too many PIN attempts; try again later".to_string()),
            422 => SendOutcome::Failed("The receiver reported a checksum mismatch".to_string()),
            status => {
                let message = error
                    .message
                    .as_deref()
                    .map(|message| format!(": {message}"))
                    .unwrap_or_default();
                SendOutcome::Failed(format!(
                    "The receiver rejected the request ({status}){message}"
                ))
            }
        },
        other => SendOutcome::Failed(describe_client_error(other)),
    }
}

/// Best-effort `/cancel` for a receive session the user cancelled locally.
/// The peer only learns about a *decline* through the 403 response, so an
/// active upload has to be told explicitly.
pub async fn notify_cancel(
    identity: &Identity,
    address: &PeerAddress,
    fingerprint: &str,
    session_id: &str,
) {
    let client = match address.protocol {
        Protocol::Https => LsHttpClientV2::try_new(
            &identity.private_key_pem,
            &identity.cert_pem,
            Some(fingerprint.to_string()),
            Some(CANCEL_NOTIFY_TIMEOUT),
        ),
        Protocol::Http => LsHttpClientV2::try_new_without_cert(),
    };
    let Ok(client) = client else {
        return;
    };
    let _ = tokio::time::timeout(
        CANCEL_NOTIFY_TIMEOUT,
        client.cancel(
            crate::net::protocol_to_core(address.protocol),
            &address.host,
            address.port,
            session_id,
        ),
    )
    .await;
}

#[cfg(test)]
mod tests {
    use super::*;
    use localsend::http::StatusCodeError;

    fn status(status: u16) -> ClientError {
        ClientError::StatusCode(StatusCodeError {
            status,
            message: None,
        })
    }

    #[test]
    fn pin_is_the_only_outcome_that_keeps_the_job_alive() {
        assert_eq!(
            classify_prepare_error(&status(401)),
            SendOutcome::PinRequired
        );
        for status_code in [403, 409, 429, 404, 500] {
            assert!(matches!(
                classify_prepare_error(&status(status_code)),
                SendOutcome::Failed(_)
            ));
        }
        assert_eq!(
            classify_prepare_error(&ClientError::Cancelled),
            SendOutcome::Cancelled
        );
    }

    #[test]
    fn text_uploads_carry_the_message_in_the_preview() {
        let upload = Upload::text("hello".to_string());
        let dto = upload.dto("0".to_string(), None);
        assert_eq!(dto.file_type, "text/plain");
        assert_eq!(dto.size, 5);
        assert_eq!(dto.preview.as_deref(), Some("hello"));
    }

    #[test]
    fn uploads_reject_unsafe_display_names() {
        // The composer never produces such a name, but a command must not be
        // able to point the receiver outside its folder.
        let upload = Upload::file(PathBuf::from("/etc/hosts"), "../hosts".to_string());
        assert!(upload.is_err());
    }

    fn response(tokens: &[(&str, &str)]) -> PrepareUploadResponseDtoV2 {
        PrepareUploadResponseDtoV2 {
            session_id: "session".to_string(),
            files: tokens
                .iter()
                .map(|(id, token)| (id.to_string(), token.to_string()))
                .collect(),
        }
    }

    /// A receiver that unchecks files keeps the ones it accepts: the sender
    /// uploads the accepted subset and finishes successfully (Flutter marks the
    /// rest "skipped"; it does not fail the session).
    #[test]
    fn a_subset_acceptance_is_not_a_failure() {
        let uploads = vec![
            Upload::text("one".to_string()),
            Upload::text("two".to_string()),
            Upload::text("three".to_string()),
        ];
        let subset = response(&[("0", "t0"), ("2", "t2")]);
        let accepted = accepted_uploads(&uploads, &subset).expect("a subset is accepted");
        assert_eq!(accepted.len(), 2);
        assert_eq!(
            accepted
                .iter()
                .map(|(index, _, token)| (*index, *token))
                .collect::<Vec<_>>(),
            vec![(0, "t0"), (2, "t2")]
        );

        let all_accepted = response(&[("0", "a"), ("1", "b"), ("2", "c")]);
        let all = accepted_uploads(&uploads, &all_accepted).expect("all accepted");
        assert_eq!(all.len(), 3);
    }

    /// A 200 with no tokens is a protocol violation: reporting it as completed
    /// would claim files were sent that the receiver never took.
    #[test]
    fn an_empty_token_map_is_rejected() {
        let uploads = vec![Upload::text("one".to_string())];
        let empty = response(&[]);
        let outcome = accepted_uploads(&uploads, &empty).expect_err("must fail");
        assert!(
            matches!(outcome, SendOutcome::Failed(message) if message.contains("did not accept"))
        );
    }

    /// Status errors are translated into user-facing text; their `Display` is
    /// `403;Some("...")`, which must not reach the UI.
    #[test]
    fn status_errors_are_described_without_internals() {
        for (code, expected) in [
            (401, "PIN"),
            (403, "cancelled"),
            (409, "busy"),
            (422, "checksum"),
        ] {
            let description = describe_client_error(&status(code));
            assert!(
                description.contains(expected),
                "{code} produced {description:?}"
            );
            assert!(
                !description.contains("Some("),
                "internals leaked: {description:?}"
            );
        }
        assert!(describe_client_error(&ClientError::Cancelled).contains("cancelled"));
    }

    /// A 204 on a *file* transfer means the receiver took nothing: reporting it
    /// as `Completed` would claim a transfer that never happened. Only a
    /// message (whole body in the preview) may legitimately end with 204.
    #[test]
    fn zero_response_is_a_completion_only_for_a_message() {
        let message = vec![Upload::text("hello".to_string())];
        assert_eq!(zero_response_outcome(&message), SendOutcome::Completed);

        let file = Upload::file(
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("Cargo.toml"),
            "Cargo.toml".to_string(),
        )
        .expect("a readable file");
        assert_eq!(
            zero_response_outcome(&[file]),
            SendOutcome::Failed(NOTHING_ACCEPTED.to_string())
        );
        // Several text uploads are a file transfer, not a message.
        let two = vec![
            Upload::text("one".to_string()),
            Upload::text("two".to_string()),
        ];
        assert!(matches!(
            zero_response_outcome(&two),
            SendOutcome::Failed(_)
        ));
        // Text too long for the preview is offered as a real file.
        let long = vec![Upload::text("x".repeat(MAX_MESSAGE_BYTES + 1))];
        assert!(matches!(
            zero_response_outcome(&long),
            SendOutcome::Failed(_)
        ));
    }

    /// The progress denominator must be the full offer so unchecked files stay
    /// visible; only the accepted subset contributes bytes.
    #[test]
    fn the_progress_total_never_shrinks_to_the_accepted_subset() {
        let uploads = vec![
            Upload::text("one".to_string()),
            Upload::text("two".to_string()),
            Upload::text("three".to_string()),
        ];
        assert_eq!(offer_total(&uploads), 11, "3 + 3 + 5 bytes");
        assert_eq!(offer_total(&[]), 0);
        // A total that does not fit in u64 saturates rather than wrapping.
        let mut huge = Upload::text(String::new());
        huge.offered.size = u64::MAX;
        assert_eq!(
            offer_total(&[huge.clone(), huge]),
            u64::MAX,
            "an overflowing total must saturate"
        );
    }

    fn test_job(uploads: Vec<Upload>, cancel: CancellationToken) -> SendJob {
        SendJob {
            id: "transfer".to_string(),
            peer: Peer {
                fingerprint: "PEER".to_string(),
                alias: "Peer".to_string(),
                device_model: String::new(),
                address: None,
                device_type: crate::net::DeviceType::Desktop,
                is_self: false,
            },
            uploads,
            pin: None,
            identity: Identity {
                cert_pem: String::new(),
                private_key_pem: String::new(),
                fingerprint: "SELF".to_string(),
            },
            register: localsend::http::dto_v2::RegisterDtoV2 {
                alias: "Test".to_string(),
                version: crate::net::PROTOCOL_VERSION.to_string(),
                device_model: None,
                device_type: None,
                fingerprint: "SELF".to_string(),
                port: 0,
                protocol: localsend::model::discovery::ProtocolType::Https,
                download: false,
            },
            peer_cancelled: Arc::new(AtomicBool::new(false)),
            remote_session: Arc::new(Mutex::new(None)),
            cancel,
            activity: Arc::new(AtomicU64::new(0)),
            events: async_channel::unbounded().0,
        }
    }

    /// NET-11: an outgoing file offer carries the SHA-256 of its content, so
    /// the receiver can verify the bytes end to end (and the 422 retry path
    /// becomes reachable for this client).
    #[tokio::test]
    async fn file_offers_carry_the_sha256_of_their_content() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("payload.bin");
        let payload: Vec<u8> = (0..4096u32).map(|index| (index % 251) as u8).collect();
        std::fs::write(&path, &payload).unwrap();
        let upload = Upload::file(path, "payload.bin".to_string()).unwrap();
        let job = test_job(
            vec![upload, Upload::text("hello".to_string())],
            CancellationToken::new(),
        );
        let hashes = job.hashes().await.expect("not cancelled");
        assert_eq!(
            hashes[0].as_deref(),
            Some(localsend::crypto::hash::sha256_hex(&payload).as_str())
        );
        assert_eq!(
            hashes[1].as_deref(),
            Some(localsend::crypto::hash::sha256_hex(b"hello").as_str()),
            "a message body is hashed from memory"
        );
    }

    /// Cancelling during hashing must stop before any network request.
    #[tokio::test]
    async fn hashing_respects_the_cancel_token() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("payload.bin");
        std::fs::write(&path, vec![0u8; 1024 * 1024]).unwrap();
        let upload = Upload::file(path, "payload.bin".to_string()).unwrap();
        let cancel = CancellationToken::new();
        cancel.cancel();
        let job = test_job(vec![upload], cancel);
        assert!(job.hashes().await.is_none(), "cancelled hashing must stop");
    }
}
