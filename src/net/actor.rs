//! The network actor: owns the LocalSend server, discovery and transfer tasks
//! on tokio, and exposes a command handle plus an event channel to the UI.
//!
//! Shape (see `docs/ARCHITECTURE.md`):
//!
//! * [`Node::start`] builds the identity, the HTTP server and discovery, then
//!   spawns [`Actor::run`] on the runtime's threads.
//! * [`Handle`] only pushes [`Command`]s into an unbounded channel; it never
//!   blocks and never touches actor state.
//! * The actor is the single owner of session state. It polls the server's
//!   event stream, a progress channel, its `JoinSet` of transfer tasks and a
//!   one-second tick, and reports everything to the UI as a
//!   [`TransferEvent`] on an unbounded `async_channel`.
//! * Shutdown cancels transfers, answers a pending decision, stops discovery
//!   and the server, waits for both, and aborts the transfer tasks.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime};

use anyhow::{Context, Result, anyhow, ensure};
use localsend::http::dto_v2::RegisterDtoV2;
use localsend::http::server::PeerIp;
use localsend::http::server::common::save::FileUploadTarget;
use localsend::http::server::v2::{PrepareUploadDecisionV2, ServerEventV2, SessionEndReasonV2};
use localsend::model::transfer::FileDto;
use localsend::multicast::MulticastDevice;
use tokio::sync::mpsc;
use tokio::task::{JoinHandle, JoinSet};

use crate::model::{ComposerItem, Settings};
use crate::net::discovery::{self, DiscoveryParams, DiscoveryRuntime};
use crate::net::identity::Identity;
use crate::net::naming;
use crate::net::receive::{self, DecisionAction, Incoming};
use crate::net::send::{self, SendJob, SendOutcome, Upload};
use crate::net::server::{self, ServerParams, ServerRuntime};
use crate::net::{Direction, OfferedFile, Peer, RefuseReason, TransferEvent, emit};

/// How long a receive session may go without any progress before it is
/// cancelled (a sender that vanished must not hold the single session slot).
const RECEIVE_IDLE_TIMEOUT: Duration = Duration::from_secs(90);

/// How long a prepare-upload decision may wait before the session is
/// cancelled (the user may have walked away from the dialog).
const DECISION_IDLE_TIMEOUT: Duration = Duration::from_secs(300);

/// How long an *outgoing* transfer may go without a byte before it is failed:
/// a half-open TCP connection (Wi-Fi silently gone, receiver hung) would
/// otherwise stay "Active" until the user cancels (NET-12/N18).
const OUTGOING_IDLE_TIMEOUT: Duration = Duration::from_secs(90);

/// Staging files older than this are removed at startup and shutdown: an
/// interrupted receive (crash, Windows file still held at cancel) leaves
/// `.part` files behind (NET-18/N23).
const STALE_STAGING_AGE: Duration = Duration::from_secs(10 * 60);

/// Longest peer-controlled text that may reach the UI in a `NetError`.
const MAX_NET_ERROR_CHARS: usize = 256;

/// How many times the actor restarts the server / discovery after a failure
/// before giving up and telling the user to restart the network (NET-13).
const MAX_RESTART_ATTEMPTS: u32 = 5;

/// Everything the actor needs to start a LocalSend node.
#[derive(Clone)]
pub struct NodeConfig {
    pub alias: String,
    pub device_type: crate::net::DeviceType,
    pub port: u16,
    pub https: bool,
    pub pin: Option<String>,
    pub discoverable: bool,
    pub downloads: PathBuf,
    /// Override for the identity directory (tests / portable installs).
    /// `None` uses the platform app data directory.
    pub data_dir: Option<PathBuf>,
}

impl std::fmt::Debug for NodeConfig {
    /// Redacts the PIN: configs end up in logs and error reports.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NodeConfig")
            .field("alias", &self.alias)
            .field("device_type", &self.device_type)
            .field("port", &self.port)
            .field("https", &self.https)
            .field("pin", &self.pin.as_ref().map(|_| "<set>"))
            .field("discoverable", &self.discoverable)
            .field("downloads", &self.downloads)
            .field("data_dir", &self.data_dir)
            .finish()
    }
}

impl NodeConfig {
    pub fn from_settings(settings: &Settings) -> Self {
        Self {
            alias: settings.alias.clone(),
            device_type: settings.device_type,
            port: settings.port,
            https: settings.https,
            pin: settings.pin.clone(),
            discoverable: settings.discoverable,
            downloads: settings.downloads.clone(),
            // `LOCALSEND_DATA_DIR` keeps the identity beside the app (portable
            // installs, tests, and runs under a file sandbox that cannot write
            // to the user's application support folder).
            data_dir: std::env::var_os("LOCALSEND_DATA_DIR").map(PathBuf::from),
        }
    }
}

/// Exponential backoff for automatically recreating a failed runtime
/// (NET-13/N19). The actor owns the retry state; a successful restart resets it.
#[derive(Debug, Clone, Copy, Default)]
struct RestartPlan {
    attempts: u32,
    due: Option<Instant>,
}

impl RestartPlan {
    /// Schedules the next attempt. Returns `false` when the attempt budget is
    /// exhausted (the failure is then escalated to a visible `NetError`).
    fn schedule(&mut self) -> bool {
        if self.attempts >= MAX_RESTART_ATTEMPTS {
            return false;
        }
        self.due = Some(Instant::now() + restart_backoff(self.attempts));
        self.attempts += 1;
        true
    }

    fn ready(&self) -> bool {
        self.due.is_some_and(|due| Instant::now() >= due)
    }

    fn clear_due(&mut self) {
        self.due = None;
    }

    fn reset(&mut self) {
        *self = Self::default();
    }
}

/// Backoff before the n-th restart attempt: 5 s, 10 s, 30 s, then 60 s.
fn restart_backoff(attempt: u32) -> Duration {
    match attempt {
        0 => Duration::from_secs(5),
        1 => Duration::from_secs(10),
        2 => Duration::from_secs(30),
        _ => Duration::from_secs(60),
    }
}

/// Truncates a peer-influenced message for a `NetError` without splitting a
/// character (NET-08).
fn truncate_message(message: &str, max_chars: usize) -> String {
    if message.chars().count() <= max_chars {
        return message.to_string();
    }
    let mut truncated: String = message.chars().take(max_chars).collect();
    truncated.push('…');
    truncated
}

/// A running node: the device identity it presents, the command handle and
/// the stream of events the UI applies.
pub struct Node {
    pub device: Peer,
    /// The port the HTTP server is actually bound to (matters with port 0).
    pub port: u16,
    pub handle: Handle,
    pub events: async_channel::Receiver<TransferEvent>,
    task: Option<JoinHandle<()>>,
}

impl Node {
    /// Start the server, discovery and the identity (or load the persisted
    /// one). Must run inside a tokio runtime context (the nsh engine polls
    /// tasks inside one).
    pub async fn start(config: NodeConfig) -> Result<Self> {
        // The save folder is the user's choice, but it can be read-only (a
        // sandbox, a removed volume, a managed Mac): fall back to a folder
        // under the app's own data directory rather than failing to start.
        let (downloads, temp_dir) = match prepare_receive_dir(&config.downloads).await {
            Ok(dirs) => dirs,
            Err(error) => {
                let fallback = crate::net::identity::data_dir()?.join("Received");
                tracing::warn!(
                    %error,
                    fallback = %fallback.display(),
                    "Downloads folder is not writable, using the app data folder"
                );
                prepare_receive_dir(&fallback).await?
            }
        };
        remove_stale_staging_files(&temp_dir).await;

        let identity = Identity::load_or_generate_in(config.data_dir.clone()).await?;
        let protocol = if config.https {
            crate::net::Protocol::Https
        } else {
            crate::net::Protocol::Http
        };
        let (events, event_rx) = async_channel::unbounded();
        // Kept on the actor so a failed listener can be recreated with the
        // same identity, port and PIN (NET-13).
        let mut server_params = ServerParams {
            port: config.port,
            tls: config.https.then(|| identity.tls_config()),
            info: server::client_info(&config.alias, config.device_type, &identity.fingerprint),
            pin: config.pin.clone(),
        };
        let (server, server_events) = start_server_with_retry(server_params.clone()).await?;
        let port = server.port();
        // A restart must rebind the port that was actually chosen (port 0
        // would otherwise move the device to a new address).
        server_params.port = port;

        let register = server::register_dto(
            &config.alias,
            config.device_type,
            server::device_model(),
            &identity.fingerprint,
            port,
            protocol,
        );
        // MP-27: the self device is dialable at every non-loopback interface
        // address. The Receive card otherwise shows `None` next to this device.
        let address = server
            .local_addresses()
            .into_iter()
            .min_by_key(|address| !address.is_ipv4())
            .map(|address| crate::net::PeerAddress {
                host: address.ip().to_string(),
                port: address.port(),
                protocol,
            });
        let device = Peer {
            fingerprint: identity.fingerprint.clone(),
            alias: config.alias.clone(),
            device_model: server::device_model().to_string(),
            address,
            device_type: config.device_type,
            is_self: true,
        };

        let active_fingerprints: Arc<Mutex<HashSet<String>>> = Arc::default();
        let discovery_params = config.discoverable.then(|| DiscoveryParams {
            device: MulticastDevice {
                alias: config.alias.clone(),
                version: crate::net::PROTOCOL_VERSION.to_string(),
                device_model: Some(server::device_model().to_string()),
                device_type: crate::net::device_type_to_core(config.device_type),
                fingerprint: identity.fingerprint.clone(),
                port,
                protocol: crate::net::protocol_to_core(protocol),
                download: false,
            },
            identity: identity.discovery_identity(),
        });
        let discovery = match &discovery_params {
            Some(params) => {
                // DiscoveryActive is only emitted on a state change (NET-16).
                let discovery = DiscoveryRuntime::start(
                    params.clone(),
                    events.clone(),
                    identity.fingerprint.clone(),
                    Arc::clone(&active_fingerprints),
                )
                .await;
                emit(&events, TransferEvent::DiscoveryActive(true));
                Some(discovery)
            }
            None => {
                emit(&events, TransferEvent::DiscoveryActive(false));
                None
            }
        };

        let (commands, command_rx) = mpsc::unbounded_channel();
        let handle = Handle { commands };
        let actor = Actor {
            identity,
            register,
            server: Some(server),
            server_params,
            discovery,
            discovery_params,
            server_restart: RestartPlan::default(),
            discovery_restart: RestartPlan::default(),
            registered: discovery::RegisteredPeers::default(),
            active_fingerprints,
            pending_server_events: None,
            events,
            downloads,
            temp_dir,
            https: config.https,
            incoming: None,
            outgoing: HashMap::new(),
            pin_pending: HashMap::new(),
        };
        let task = tokio::spawn(actor.run(command_rx, server_events));
        Ok(Self {
            device,
            port,
            handle,
            events: event_rx,
            task: Some(task),
        })
    }

    /// Stop discovery, the server and every transfer, then wait for them.
    pub async fn shutdown(mut self) {
        let _ = self.handle.commands.send(Command::Stop);
        if let Some(task) = self.task.take() {
            let _ = task.await;
        }
    }
}

impl Drop for Node {
    fn drop(&mut self) {
        // The app can drop the node without awaiting `shutdown` (e.g. when the
        // UI is torn down); the actor must not keep sockets open.
        if self.task.is_some() {
            let _ = self.handle.commands.send(Command::Stop);
        }
    }
}

/// Whether a staging file is old enough to sweep (NET-18/N23). A receive that
/// was cancelled while the core still held the file open leaves `.part` files
/// behind; sweeping only at startup with a one-hour threshold kept them for the
/// rest of the day (and until the next launch).
fn is_stale_staging(modified: SystemTime, now: SystemTime) -> bool {
    now.duration_since(modified)
        .is_ok_and(|age| age > STALE_STAGING_AGE)
}

/// Removes `.part` staging files left behind by an interrupted receive (a
/// crash or a quit mid-transfer). Run at startup and shutdown (NET-18).
async fn remove_stale_staging_files(temp_dir: &std::path::Path) {
    let Ok(mut entries) = tokio::fs::read_dir(temp_dir).await else {
        return;
    };
    let now = SystemTime::now();
    while let Ok(Some(entry)) = entries.next_entry().await {
        let path = entry.path();
        if path.extension().is_none_or(|extension| extension != "part") {
            continue;
        }
        let stale = entry
            .metadata()
            .await
            .ok()
            .and_then(|metadata| metadata.modified().ok())
            .is_some_and(|modified| is_stale_staging(modified, now));
        if stale {
            let _ = tokio::fs::remove_file(&path).await;
        }
    }
}

/// Starts the server, retrying for a moment while the previous node is still
/// shutting down (a settings change restarts the network, and the old server
/// only releases the port a little later). Any other error is returned
/// immediately.
async fn start_server_with_retry(
    params: ServerParams,
) -> Result<(ServerRuntime, mpsc::Receiver<ServerEventV2>)> {
    const RETRIES: u32 = 30;
    let mut last_error = None;
    for attempt in 0..RETRIES {
        let attempt_params = ServerParams {
            port: params.port,
            tls: params.tls.clone(),
            info: params.info.clone(),
            pin: params.pin.clone(),
        };
        match ServerRuntime::start(attempt_params).await {
            Ok(started) => return Ok(started),
            Err(error) => {
                if !is_address_in_use(&error) {
                    return Err(error);
                }
                last_error = Some(error);
                if attempt + 1 < RETRIES {
                    tokio::time::sleep(Duration::from_millis(100)).await;
                }
            }
        }
    }
    Err(last_error.expect("at least one attempt ran"))
}

/// Whether a server start failed because the port is still held by another
/// listener (usually the node that is being restarted).
fn is_address_in_use(error: &anyhow::Error) -> bool {
    error.chain().any(|cause| {
        cause
            .downcast_ref::<std::io::Error>()
            .is_some_and(|io| io.kind() == std::io::ErrorKind::AddrInUse)
    })
}

/// Pulls the next server event, or waits forever while no server is running.
/// `Option` keeps the actor loop alive after a listener failure so the server
/// can be recreated (NET-13).
async fn next_server_event(
    events: &mut Option<mpsc::Receiver<ServerEventV2>>,
) -> Option<ServerEventV2> {
    match events {
        Some(events) => events.recv().await,
        None => std::future::pending().await,
    }
}

/// The UI's only way to command the network. Cheap to clone; never blocks.
#[derive(Clone, Debug)]
pub struct Handle {
    commands: mpsc::UnboundedSender<Command>,
}

impl Default for Handle {
    fn default() -> Self {
        // A detached handle: commands fail with "network service is not
        // running", which is what a default value should mean.
        let (commands, _receiver) = mpsc::unbounded_channel();
        Self { commands }
    }
}

impl Handle {
    /// Request an immediate discovery announcement without restarting transfers.
    pub fn refresh_discovery(&self) -> Result<()> {
        self.command(Command::RefreshDiscovery)
    }

    fn command(&self, command: Command) -> Result<()> {
        self.commands
            .send(command)
            .map_err(|_| anyhow!("The network service is not running"))
    }

    /// Offer items to `peer`, asking for `pin` when the peer requires one.
    /// Returns the local transfer id the events will carry.
    pub fn send(
        &self,
        peer: &Peer,
        items: Vec<ComposerItem>,
        pin: Option<String>,
    ) -> Result<String> {
        ensure!(!items.is_empty(), "Add something to send first");
        let uploads = items
            .into_iter()
            .map(upload_from_item)
            .collect::<Result<Vec<_>>>()?;
        let id = uuid::Uuid::new_v4().to_string();
        self.command(Command::Send {
            id: id.clone(),
            peer: peer.clone(),
            uploads,
            pin,
        })?;
        Ok(id)
    }

    /// Retries a send that ended with [`TransferEvent::PinRequired`] using the
    /// PIN the user entered. The transfer keeps its id.
    pub fn submit_pin(&self, id: &str, pin: String) -> Result<()> {
        self.command(Command::SubmitPin {
            id: id.to_string(),
            pin,
        })
    }

    /// Answer an incoming request. Unknown ids are ignored on purpose: the
    /// quick-save auto-answer may race this command.
    pub fn decide(&self, id: &str, accept: bool) -> Result<()> {
        self.command(Command::Decide {
            id: id.to_string(),
            accept,
        })
    }

    /// Cancel a transfer in either direction. Unknown ids are ignored.
    pub fn cancel(&self, id: &str) -> Result<()> {
        self.command(Command::Cancel(id.to_string()))
    }

    /// Stop the node; the app is quitting or restarting the network.
    pub fn stop(&self) -> Result<()> {
        let _ = self.commands.send(Command::Stop);
        Ok(())
    }
}

/// The actor's inbox.
enum Command {
    RefreshDiscovery,
    Send {
        id: String,
        peer: Peer,
        uploads: Vec<Upload>,
        pin: Option<String>,
    },
    SubmitPin {
        id: String,
        pin: String,
    },
    Decide {
        id: String,
        accept: bool,
    },
    Cancel(String),
    Stop,
}

/// Turns a composer item into an upload descriptor. Runs on the UI thread: a
/// single `stat` per item is all it does, so a vanished file is reported
/// synchronously as a command error.
fn upload_from_item(item: ComposerItem) -> Result<Upload> {
    match item {
        ComposerItem::File {
            path, name, is_dir, ..
        } => {
            ensure!(
                !is_dir,
                "Folders are expanded into their files when they are added"
            );
            Upload::file(path, name)
        }
        ComposerItem::Text(text) => Ok(Upload::text(text)),
    }
}

/// A send whose prepare-upload answered 401; it survives until the user
/// enters a PIN (`submit_pin`) or the transfer is cancelled.
struct PendingSend {
    peer: Peer,
    uploads: Vec<Upload>,
    cancel: tokio_util::sync::CancellationToken,
}

/// Bookkeeping for a send that is currently running.
struct Outgoing {
    host: String,
    remote_session: Arc<Mutex<Option<String>>>,
    cancel: tokio_util::sync::CancellationToken,
    /// Set when the peer cancelled the session. The send task then must not
    /// echo `/cancel` back: the peer already knows (protocol checklist #17).
    peer_cancelled: Arc<AtomicBool>,
    /// Bumped by the send task on every sign of life; see [`SendJob::activity`].
    activity: Arc<AtomicU64>,
    /// Last `activity` value the watchdog saw.
    last_seq: u64,
    /// When `activity` last changed.
    last_activity: Instant,
    /// Set by the watchdog: the terminal event is `Failed` with this text
    /// instead of the generic `Cancelled` (NET-12).
    failure: Arc<Mutex<Option<String>>>,
    retry: PendingSend,
}

/// Progress reported by an in-flight receive file (the core already coalesces
/// writes; the actor aggregates and throttles per transfer).
struct ProgressUpdate {
    session_id: String,
    file_id: String,
    bytes: u64,
}

/// Why receiving one file failed.
enum ReceiveFailure {
    /// The receiver-side checksum did not match; the sender may retry the same
    /// file with the same token (the core resets it to pending).
    ChecksumMismatch,
    /// Any other error: the session is over.
    Fatal(String),
}

/// Result of a spawned transfer task.
enum Finished {
    Send {
        id: String,
        outcome: SendOutcome,
    },
    Receive {
        session_id: String,
        file_id: String,
        result: std::result::Result<(), ReceiveFailure>,
    },
}

/// What the UI should be told about a finished send task. Pure so the
/// cancellation races are testable without a server.
#[derive(Debug, PartialEq, Eq)]
enum SendTerminal {
    Completed,
    Cancelled,
    PinRequired,
    Failed(String),
}

/// Maps a send outcome plus the local cancel flag to the terminal event.
///
/// Order matters:
/// * `Completed` wins even if a cancel fired at the same moment: the bytes are
///   gone, so reporting "cancelled" would be a lie (and the receiver has the
///   files).
/// * A cancelled transfer must never surface `PinRequired`: a 401 that raced a
///   user cancel used to open a phantom PIN sheet and leak an entry in
///   `pin_pending` until the app restarted.
fn send_terminal(outcome: SendOutcome, cancelled: bool) -> SendTerminal {
    match outcome {
        SendOutcome::Completed => SendTerminal::Completed,
        _ if cancelled => SendTerminal::Cancelled,
        SendOutcome::PinRequired => SendTerminal::PinRequired,
        SendOutcome::Cancelled => SendTerminal::Cancelled,
        SendOutcome::Failed(error) => SendTerminal::Failed(error),
    }
}

struct Actor {
    identity: Identity,
    register: RegisterDtoV2,
    server: Option<ServerRuntime>,
    /// Parameters for recreating the server after a listener failure (NET-13).
    server_params: ServerParams,
    discovery: Option<DiscoveryRuntime>,
    /// Parameters for recreating discovery after a multicast failure (NET-13).
    discovery_params: Option<DiscoveryParams>,
    server_restart: RestartPlan,
    discovery_restart: RestartPlan,
    /// App-side device table guard: the core store cannot shrink (NET-02).
    registered: discovery::RegisteredPeers,
    /// Peers with an active transfer; discovery must not report them lost
    /// (NET-15/N20). Shared with the discovery publish task.
    active_fingerprints: Arc<Mutex<HashSet<String>>>,
    /// A freshly restarted server's event stream, installed by the run loop.
    pending_server_events: Option<mpsc::Receiver<ServerEventV2>>,
    events: async_channel::Sender<TransferEvent>,
    downloads: PathBuf,
    temp_dir: PathBuf,
    https: bool,
    incoming: Option<Incoming>,
    outgoing: HashMap<String, Outgoing>,
    pin_pending: HashMap<String, PendingSend>,
}

impl Actor {
    async fn run(
        mut self,
        mut commands: mpsc::UnboundedReceiver<Command>,
        server_events: mpsc::Receiver<ServerEventV2>,
    ) {
        let mut server_events = Some(server_events);
        let mut tasks: JoinSet<Finished> = JoinSet::new();
        let (progress_tx, mut progress_rx) = mpsc::unbounded_channel::<ProgressUpdate>();
        let mut ticker = tokio::time::interval(Duration::from_secs(1));
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

        loop {
            tokio::select! {
                command = commands.recv() => match command {
                    None | Some(Command::Stop) => break,
                    Some(Command::Send { id, peer, uploads, pin }) => {
                        self.start_send(id, peer, uploads, pin, &mut tasks);
                    }
                    Some(Command::SubmitPin { id, pin }) => {
                        self.submit_pin(&id, pin, &mut tasks);
                    }
                    Some(Command::Decide { id, accept }) => self.decide(&id, accept).await,
                    Some(Command::Cancel(id)) => self.cancel(&id).await,
                    Some(Command::RefreshDiscovery) => {
                        if let Some(discovery) = &self.discovery {
                            discovery.refresh();
                        }
                    }
                },
                event = next_server_event(&mut server_events) => match event {
                    Some(event) => self.server_event(event, &mut tasks, &progress_tx).await,
                    // The server is stopped (listener failure): the actor keeps
                    // running and recreates it with backoff (NET-13).
                    None => server_events = None,
                },
                Some(update) = progress_rx.recv() => self.on_progress(update),
                Some(result) = tasks.join_next(), if !tasks.is_empty() => match result {
                    Ok(finished) => self.finished(finished).await,
                    Err(error) if !error.is_cancelled() => emit(
                        &self.events,
                        TransferEvent::NetError(format!("A transfer task stopped: {error}")),
                    ),
                    Err(_) => {}
                },
                _ = ticker.tick() => self.tick().await,
            }
            // A tick or a listener failure may have recreated the server.
            if let Some(events) = self.pending_server_events.take() {
                server_events = Some(events);
            }
        }

        self.shutdown(&mut tasks).await;
    }

    // --- commands ---------------------------------------------------------

    fn start_send(
        &mut self,
        id: String,
        peer: Peer,
        uploads: Vec<Upload>,
        pin: Option<String>,
        tasks: &mut JoinSet<Finished>,
    ) {
        if self.outgoing.contains_key(&id) || self.pin_pending.contains_key(&id) {
            return;
        }
        let total = match uploads
            .iter()
            .try_fold(0u64, |sum, upload| sum.checked_add(upload.offered.size))
        {
            Some(total) => total,
            None => {
                emit(
                    &self.events,
                    TransferEvent::Failed {
                        id,
                        error: "The selection is too large to transfer".to_string(),
                    },
                );
                return;
            }
        };
        if peer.address.is_none() {
            emit(
                &self.events,
                TransferEvent::Failed {
                    id,
                    error: "This device is not reachable right now".to_string(),
                },
            );
            return;
        }
        emit(
            &self.events,
            TransferEvent::Started {
                id: id.clone(),
                peer: peer.clone(),
                direction: Direction::Sending,
                files: uploads
                    .iter()
                    .map(|upload| upload.offered.clone())
                    .collect(),
                total,
            },
        );
        let cancel = tokio_util::sync::CancellationToken::new();
        let remote_session = Arc::new(Mutex::new(None));
        let peer_cancelled = Arc::new(AtomicBool::new(false));
        let activity = Arc::new(AtomicU64::new(0));
        let host = peer
            .address
            .as_ref()
            .map(|address| address.host.clone())
            .unwrap_or_default();
        self.outgoing.insert(
            id.clone(),
            Outgoing {
                host,
                remote_session: Arc::clone(&remote_session),
                cancel: cancel.clone(),
                peer_cancelled: Arc::clone(&peer_cancelled),
                activity: Arc::clone(&activity),
                last_seq: 0,
                last_activity: Instant::now(),
                failure: Arc::default(),
                retry: PendingSend {
                    peer: peer.clone(),
                    uploads: uploads.clone(),
                    cancel: cancel.clone(),
                },
            },
        );
        self.spawn_send(
            id,
            peer,
            uploads,
            pin,
            cancel,
            remote_session,
            peer_cancelled,
            activity,
            tasks,
        );
    }

    fn submit_pin(&mut self, id: &str, pin: String, tasks: &mut JoinSet<Finished>) {
        let Some(pending) = self.pin_pending.remove(id) else {
            return;
        };
        let remote_session = Arc::new(Mutex::new(None));
        let peer_cancelled = Arc::new(AtomicBool::new(false));
        let activity = Arc::new(AtomicU64::new(0));
        let host = pending
            .peer
            .address
            .as_ref()
            .map(|address| address.host.clone())
            .unwrap_or_default();
        self.outgoing.insert(
            id.to_string(),
            Outgoing {
                host,
                remote_session: Arc::clone(&remote_session),
                cancel: pending.cancel.clone(),
                peer_cancelled: Arc::clone(&peer_cancelled),
                activity: Arc::clone(&activity),
                last_seq: 0,
                last_activity: Instant::now(),
                failure: Arc::default(),
                retry: PendingSend {
                    peer: pending.peer.clone(),
                    uploads: pending.uploads.clone(),
                    cancel: pending.cancel.clone(),
                },
            },
        );
        self.spawn_send(
            id.to_string(),
            pending.peer,
            pending.uploads,
            Some(pin),
            pending.cancel,
            remote_session,
            peer_cancelled,
            activity,
            tasks,
        );
    }

    #[allow(clippy::too_many_arguments)]
    fn spawn_send(
        &mut self,
        id: String,
        peer: Peer,
        uploads: Vec<Upload>,
        pin: Option<String>,
        cancel: tokio_util::sync::CancellationToken,
        remote_session: Arc<Mutex<Option<String>>>,
        peer_cancelled: Arc<AtomicBool>,
        activity: Arc<AtomicU64>,
        tasks: &mut JoinSet<Finished>,
    ) {
        let job = SendJob {
            id: id.clone(),
            peer,
            uploads,
            pin,
            identity: self.identity.clone(),
            register: self.register.clone(),
            peer_cancelled,
            cancel,
            remote_session,
            activity,
            events: self.events.clone(),
        };
        tasks.spawn(async move {
            let outcome = job.run().await;
            Finished::Send { id, outcome }
        });
    }

    async fn decide(&mut self, id: &str, accept: bool) {
        // The routing (including the accept/decline races) lives in `receive`
        // so it is testable without a server; this method only performs the
        // side effects.
        match receive::decision_action(self.incoming.as_ref(), id, accept) {
            DecisionAction::Ignore => {
                // Already answered/canceled (e.g. the UI auto-accept racing the
                // actor's quick-save): not an error.
            }
            DecisionAction::CancelAccepted => {
                // A decline arrived after the request was accepted: the user
                // wants the running transfer gone. Declining a consumed
                // responder is a no-op and would strand the core session.
                self.cancel_incoming(id).await;
            }
            DecisionAction::AcceptMessage => {
                if let Some(incoming) = self.incoming.take() {
                    self.accept_message(incoming);
                }
            }
            DecisionAction::AcceptFiles => {
                let (files, total, peer, answered) = {
                    let incoming = self.incoming.as_mut().expect("checked by decision_action");
                    let files = incoming.offered();
                    let total = incoming.total();
                    let peer = incoming.peer.clone();
                    let answered = incoming.accept();
                    (files, total, peer, answered)
                };
                if answered {
                    emit(
                        &self.events,
                        TransferEvent::Started {
                            id: id.to_string(),
                            peer,
                            direction: Direction::Receiving,
                            files,
                            total,
                        },
                    );
                } else {
                    // The sender vanished while we were deciding; the core has
                    // already dropped the responder.
                    self.incoming = None;
                    emit(
                        &self.events,
                        TransferEvent::Cancelled { id: id.to_string() },
                    );
                }
            }
            DecisionAction::Decline => {
                if let Some(incoming) = self.incoming.as_mut() {
                    incoming.decline();
                }
                self.incoming = None;
                emit(
                    &self.events,
                    TransferEvent::Cancelled { id: id.to_string() },
                );
            }
        }
    }

    async fn cancel(&mut self, id: &str) {
        if let Some(outgoing) = self.outgoing.get(id) {
            outgoing.cancel.cancel();
            return;
        }
        if self.pin_pending.remove(id).is_some() {
            emit(
                &self.events,
                TransferEvent::Cancelled { id: id.to_string() },
            );
            return;
        }
        if self
            .incoming
            .as_ref()
            .is_some_and(|incoming| incoming.id == id)
        {
            self.cancel_incoming(id).await;
        }
    }

    // --- server events ----------------------------------------------------

    async fn server_event(
        &mut self,
        event: ServerEventV2,
        tasks: &mut JoinSet<Finished>,
        progress_tx: &mpsc::UnboundedSender<ProgressUpdate>,
    ) {
        match event {
            ServerEventV2::Register { ip, info } => {
                // A peer's registration can echo our own announcement through
                // loopback; matching names alone must not hide other machines,
                // so the fingerprint decides.
                if info
                    .fingerprint
                    .eq_ignore_ascii_case(&self.identity.fingerprint)
                {
                    return;
                }
                let peer = server::peer_from_register(&ip, &info);
                // NET-02: every `Register` could inject a new fingerprint and
                // the core store never forgets one. Bound the app-side table;
                // known peers keep refreshing their address.
                if !self.registered.accept(&peer.fingerprint) {
                    tracing::debug!(
                        peer = %peer.fingerprint,
                        "device table is full and evicted recently; ignoring a new peer registration"
                    );
                    return;
                }
                match &self.discovery {
                    Some(discovery) => {
                        discovery::add_registered_peer(discovery.handle(), &peer).await;
                    }
                    // Discovery off (not discoverable): still show devices
                    // that reach this server directly.
                    None => emit(&self.events, TransferEvent::PeerFound(peer)),
                }
            }
            ServerEventV2::PrepareUpload {
                session_id,
                ip,
                info,
                cert_fingerprint,
                files,
                decision_tx,
            } => self.prepare_upload(session_id, ip, info, cert_fingerprint, files, decision_tx),
            ServerEventV2::FileUpload {
                session_id,
                file_id,
                file,
                target_tx,
            } => {
                self.file_upload(session_id, file_id, file, target_tx, tasks, progress_tx)
                    .await;
            }
            ServerEventV2::SessionEnd { session_id, reason } => {
                self.session_end(&session_id, reason).await;
            }
            ServerEventV2::PrepareUploadAborted { session_id } => {
                if self
                    .incoming
                    .as_ref()
                    .is_some_and(|incoming| incoming.id == session_id)
                {
                    // The sender is gone; the decision responder is dead, so
                    // answering it is pointless.
                    if let Some(mut incoming) = self.incoming.take() {
                        let _ = incoming.decision.take();
                        if incoming.accepted {
                            incoming.finish_progress(&self.events);
                        }
                        let paths = incoming.temp_paths();
                        incoming.cancel.cancel();
                        receive::cleanup_staging(paths);
                        emit(&self.events, TransferEvent::Cancelled { id: incoming.id });
                    }
                }
            }
            ServerEventV2::CancelReceived { ip, session_id } => {
                // NET-17/N22 WONTFIX (security trade-off): matching on the
                // session id *and* the peer's host is deliberate. Matching only
                // the id would let any device on the LAN cancel another
                // device's outgoing transfer by guessing/observing a session
                // id. A multi-homed peer that cancels from a different address
                // misses this fast path and fails later with 403 instead, which
                // is a bounded, non-security regression; do not weaken it.
                let host = ip.to_string();
                for outgoing in self.outgoing.values() {
                    let matches_session = outgoing
                        .remote_session
                        .lock()
                        .unwrap_or_else(|poisoned| poisoned.into_inner())
                        .as_deref()
                        == Some(session_id.as_str());
                    if outgoing.host == host && matches_session {
                        // The peer already knows; the send task must not send
                        // `/cancel` back (protocol checklist #17).
                        outgoing.peer_cancelled.store(true, Ordering::Relaxed);
                        outgoing.cancel.cancel();
                    }
                }
            }
            ServerEventV2::ListenerFailed { error } => {
                // The core stopped the server and released the port; recreate
                // it with backoff instead of leaving the app unreachable
                // (NET-13/N19). Only an exhausted budget is a user-visible
                // dead end.
                tracing::warn!(%error, "The HTTP server stopped listening");
                if let Some(server) = self.server.take() {
                    server.stop().await;
                }
                if self.server_restart.schedule() {
                    emit(
                        &self.events,
                        TransferEvent::NetError(format!(
                            "Receiving stopped: {error}. Retrying automatically."
                        )),
                    );
                } else {
                    emit(
                        &self.events,
                        TransferEvent::NetError(format!(
                            "Receiving stopped: {error}. Restart the network in Settings."
                        )),
                    );
                }
            }
        }
    }

    fn prepare_upload(
        &mut self,
        session_id: String,
        ip: PeerIp,
        info: RegisterDtoV2,
        cert_fingerprint: Option<String>,
        files: HashMap<String, FileDto>,
        decision_tx: tokio::sync::oneshot::Sender<PrepareUploadDecisionV2>,
    ) {
        if self.incoming.is_some() {
            // Only one v2 session exists at a time; the core usually rejects
            // this with 409 before it even reaches us.
            let _ = decision_tx.send(PrepareUploadDecisionV2::Decline);
            return;
        }
        // NET-03: refuse an offer that would swallow the destination volume
        // (quick-save accepts without any user interaction). The free space is
        // read from the staging directory, which lives on the same volume as
        // the downloads folder.
        let total: u64 = files
            .values()
            .fold(0u64, |sum, file| sum.saturating_add(file.size));
        let refused = if let Err(error) = receive::validate_offer(&files) {
            Some((RefuseReason::Invalid, error))
        } else if self.https && cert_fingerprint.is_none() {
            Some((
                RefuseReason::Unencrypted,
                anyhow!("The sender did not use an encrypted connection"),
            ))
        } else if receive::message_body(&files).is_none() {
            // A message transfer (body in the preview) writes no file: it
            // needs no space and must not be refused on a full volume.
            receive::ensure_capacity(total, receive::available_space(&self.temp_dir))
                .err()
                .map(|error| (RefuseReason::NoSpace, error))
        } else {
            None
        };
        if let Some((reason, error)) = refused {
            // NET-08: the UI has never seen this session id, so a terminal
            // `Failed` would only be dropped. The reason is shown; the detail
            // (bounded, never the peer-controlled name) goes to the log.
            let _ = decision_tx.send(PrepareUploadDecisionV2::Decline);
            tracing::warn!(
                ?reason,
                peer = %ip,
                detail = %truncate_message(&format!("{error:#}"), MAX_NET_ERROR_CHARS),
                "Declined an incoming transfer request"
            );
            emit(&self.events, TransferEvent::RequestRefused(reason));
            return;
        }

        let peer = server::peer_from_prepare_upload(&ip, &info, cert_fingerprint.as_deref());
        let incoming = Incoming::new(session_id.clone(), peer, files, decision_tx);
        let files = incoming.offered();
        emit(
            &self.events,
            TransferEvent::IncomingRequest {
                id: session_id.clone(),
                peer: incoming.peer.clone(),
                files,
                total,
            },
        );
        // The decision is always answered by the app: `Handle::decide` is
        // what implements quick-save/auto rules, using the live setting. The
        // actor must not apply a start-time snapshot of those rules, or a
        // setting change would only take effect after a network restart.
        self.incoming = Some(incoming);
    }

    async fn file_upload(
        &mut self,
        session_id: String,
        file_id: String,
        _file: FileDto,
        target_tx: tokio::sync::oneshot::Sender<FileUploadTarget>,
        tasks: &mut JoinSet<Finished>,
        progress_tx: &mpsc::UnboundedSender<ProgressUpdate>,
    ) {
        let (temp, cancel) = {
            let Some(incoming) = self
                .incoming
                .as_mut()
                .filter(|incoming| incoming.id == session_id && incoming.accepted)
            else {
                // Dropping the responder turns into HTTP 500; the session is
                // unknown or not accepted (e.g. a stale retry after a cancel).
                drop(target_tx);
                return;
            };
            if !incoming.files.contains_key(&file_id) {
                drop(target_tx);
                return;
            }
            incoming.last_activity = Instant::now();
            (
                incoming.temp_for(&file_id, &self.temp_dir),
                incoming.cancel.clone(),
            )
        };
        let temp = match temp {
            Ok(temp) => temp,
            Err(error) => {
                drop(target_tx);
                self.fail_incoming(format!("Cannot prepare the receive folder: {error:#}"))
                    .await;
                return;
            }
        };

        let (writer_progress_tx, mut writer_progress_rx) = mpsc::channel::<u64>(16);
        let (result_tx, result_rx) = tokio::sync::oneshot::channel::<Result<(), String>>();
        let target = FileUploadTarget::Path {
            path: temp,
            result_tx,
            progress_tx: Some(writer_progress_tx),
        };
        if target_tx.send(target).is_err() {
            return;
        }
        if let Some(incoming) = self
            .incoming
            .as_mut()
            .filter(|incoming| incoming.id == session_id)
        {
            incoming.pending.insert(file_id.clone());
        }

        let relay_tx = progress_tx.clone();
        let relay_session = session_id.clone();
        let relay_file = file_id.clone();
        tasks.spawn(async move {
            // Forward the core's write progress to the actor for aggregation.
            // The token stops the relay promptly when the user cancels: the
            // core cannot abort the in-flight write (BLOCKED-UPSTREAM), but
            // the app must not keep aggregating for an abandoned session.
            let relay = tokio::spawn(async move {
                loop {
                    tokio::select! {
                        biased;
                        _ = cancel.cancelled() => break,
                        bytes = writer_progress_rx.recv() => match bytes {
                            Some(bytes) => {
                                let _ = relay_tx.send(ProgressUpdate {
                                    session_id: relay_session.clone(),
                                    file_id: relay_file.clone(),
                                    bytes,
                                });
                            }
                            None => break,
                        },
                    }
                }
            });
            let result = match result_rx.await {
                Ok(Ok(())) => Ok(()),
                Ok(Err(message)) if message.eq_ignore_ascii_case("checksum mismatch") => {
                    Err(ReceiveFailure::ChecksumMismatch)
                }
                Ok(Err(message)) => Err(ReceiveFailure::Fatal(message)),
                Err(_) => Err(ReceiveFailure::Fatal(
                    "The upload was aborted before it finished".to_string(),
                )),
            };
            let _ = relay.await;
            Finished::Receive {
                session_id,
                file_id,
                result,
            }
        });
    }

    async fn session_end(&mut self, session_id: &str, reason: SessionEndReasonV2) {
        match reason {
            // Per-file results decide success; this event only says that no
            // more uploads will arrive.
            SessionEndReasonV2::Finished => {
                let (should_fail, should_complete) = match self
                    .incoming
                    .as_mut()
                    .filter(|incoming| incoming.id == session_id)
                {
                    Some(incoming) => {
                        incoming.ended = true;
                        (
                            incoming.pending.is_empty() && !incoming.complete(),
                            incoming.pending.is_empty() && incoming.complete(),
                        )
                    }
                    None => (false, false),
                };
                if should_complete {
                    if let Some(incoming) = self.incoming.take() {
                        self.complete_incoming(incoming);
                    }
                } else if should_fail {
                    self.fail_incoming(
                        "Some files could not be received (checksum mismatch or write error)"
                            .to_string(),
                    )
                    .await;
                }
            }
            SessionEndReasonV2::Cancelled => {
                // Compare before taking: `take().filter(..)` dropped the
                // *current* session (its responder, staging files and UI row)
                // when a late end of an earlier session arrived. The core frees
                // the slot before it sends this event, so a new request can
                // be handled in between.
                if self
                    .incoming
                    .as_ref()
                    .is_some_and(|incoming| incoming.id == session_id)
                    && let Some(mut incoming) = self.incoming.take()
                {
                    incoming.finish_progress(&self.events);
                    let paths = incoming.temp_paths();
                    incoming.cancel.cancel();
                    receive::cleanup_staging(paths);
                    emit(&self.events, TransferEvent::Cancelled { id: incoming.id });
                }
            }
        }
    }

    fn on_progress(&mut self, update: ProgressUpdate) {
        let Some(incoming) = self
            .incoming
            .as_mut()
            .filter(|incoming| incoming.id == update.session_id)
        else {
            return;
        };
        incoming.report_progress(&self.events, &update.file_id, update.bytes);
    }

    async fn finished(&mut self, finished: Finished) {
        match finished {
            Finished::Send { id, outcome } => {
                let Some(outgoing) = self.outgoing.remove(&id) else {
                    return;
                };
                let cancelled = outgoing.cancel.is_cancelled();
                let failure = outgoing
                    .failure
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner())
                    .clone();
                let terminal = send_terminal(outcome, cancelled);
                // A watchdog cancellation carries a reason: surface it as a
                // failure instead of a user cancel (NET-12).
                let terminal = match (terminal, failure) {
                    (SendTerminal::Cancelled, Some(error)) => SendTerminal::Failed(error),
                    (terminal, _) => terminal,
                };
                match terminal {
                    SendTerminal::PinRequired => {
                        let peer = outgoing.retry.peer.clone();
                        self.pin_pending.insert(id.clone(), outgoing.retry);
                        emit(&self.events, TransferEvent::PinRequired { id, peer });
                    }
                    SendTerminal::Completed => emit(
                        &self.events,
                        TransferEvent::Completed {
                            id,
                            files: outgoing
                                .retry
                                .uploads
                                .iter()
                                .map(|upload| upload.offered.clone())
                                .collect(),
                        },
                    ),
                    SendTerminal::Cancelled => emit(&self.events, TransferEvent::Cancelled { id }),
                    SendTerminal::Failed(error) => {
                        emit(&self.events, TransferEvent::Failed { id, error })
                    }
                }
            }
            Finished::Receive {
                session_id,
                file_id,
                result,
            } => self.finished_receive(session_id, file_id, result).await,
        }
    }

    async fn finished_receive(
        &mut self,
        session_id: String,
        file_id: String,
        result: std::result::Result<(), ReceiveFailure>,
    ) {
        let mut file: Option<FileDto> = None;
        let mut temp: Option<PathBuf> = None;
        if let Some(incoming) = self
            .incoming
            .as_mut()
            .filter(|incoming| incoming.id == session_id)
        {
            incoming.pending.remove(&file_id);
            incoming.last_activity = Instant::now();
            file = incoming.files.get(&file_id).cloned();
            temp = incoming.temps.get(&file_id).cloned();
        }
        let Some(file) = file else {
            return;
        };

        match result {
            Ok(()) => {
                let Some(temp) = temp else {
                    self.fail_incoming("The received file disappeared".to_string())
                        .await;
                    return;
                };
                let relative = match naming::relative_name(&file.file_name) {
                    Ok(relative) => relative,
                    Err(error) => {
                        self.fail_incoming(format!("Invalid received file name: {error:#}"))
                            .await;
                        return;
                    }
                };
                let downloads = self.downloads.clone();
                // NET-04: a per-session hint skips the suffixes already known
                // to be taken, so a batch of same-named files costs O(1) link
                // attempts each instead of re-scanning from 0 (O(n^2) total).
                let hint = self
                    .incoming
                    .as_ref()
                    .filter(|incoming| incoming.id == session_id)
                    .map(|incoming| incoming.collision_hint(&relative))
                    .unwrap_or(0);
                let relative_for_publish = relative.clone();
                let published = tokio::task::spawn_blocking(move || {
                    naming::publish_from(&temp, &downloads, &relative_for_publish, hint)
                })
                .await;
                let (saved, next_hint) = match published {
                    Ok(Ok(published)) => published,
                    Ok(Err(error)) => {
                        self.fail_incoming(format!("Cannot store the received file: {error:#}"))
                            .await;
                        return;
                    }
                    Err(error) => {
                        self.fail_incoming(format!("Cannot store the received file: {error}"))
                            .await;
                        return;
                    }
                };
                if let Some(incoming) = self
                    .incoming
                    .as_mut()
                    .filter(|incoming| incoming.id == session_id)
                {
                    incoming.set_collision_hint(relative, next_hint);
                }

                let (complete, ended) = {
                    let Some(incoming) = self
                        .incoming
                        .as_mut()
                        .filter(|incoming| incoming.id == session_id)
                    else {
                        return;
                    };
                    incoming.done += 1;
                    // NET-07: O(1) aggregate update instead of a map insert
                    // that forces a full O(files) sum on every progress event.
                    incoming.record_file_done(&file_id, file.size);
                    (
                        incoming.complete(),
                        incoming.ended && incoming.pending.is_empty(),
                    )
                };
                emit(
                    &self.events,
                    TransferEvent::FileDone {
                        id: session_id.clone(),
                        file: OfferedFile {
                            name: file.file_name.clone(),
                            size: file.size,
                            mime: file.file_type.clone(),
                            saved_to: Some(saved.clone()),
                        },
                    },
                );
                // Text files received as files (not as messages) also surface
                // as text. NET-05: besides the per-file cap, the session has a
                // total text budget; 10,000 small .txt files must not grow the
                // model's strings without bound.
                let forward_text = (file.file_type == "text"
                    || file.file_type.starts_with("text/"))
                    && self
                        .incoming
                        .as_mut()
                        .filter(|incoming| incoming.id == session_id)
                        .is_some_and(|incoming| incoming.allow_text(file.size));
                if forward_text && let Ok(text) = tokio::fs::read_to_string(&saved).await {
                    emit(
                        &self.events,
                        TransferEvent::TextReceived {
                            id: session_id.clone(),
                            text,
                        },
                    );
                }
                if complete {
                    if let Some(incoming) = self.incoming.take() {
                        self.complete_incoming(incoming);
                    }
                } else if ended {
                    self.fail_incoming(
                        "Some files could not be received (checksum mismatch or write error)"
                            .to_string(),
                    )
                    .await;
                }
            }
            Err(ReceiveFailure::ChecksumMismatch) => {
                let attempts = {
                    let Some(incoming) = self
                        .incoming
                        .as_mut()
                        .filter(|incoming| incoming.id == session_id)
                    else {
                        return;
                    };
                    let attempts = incoming.attempts.entry(file_id.clone()).or_insert(0);
                    *attempts += 1;
                    *attempts
                };
                if attempts >= 3 {
                    self.fail_incoming("The received file did not match its checksum".to_string())
                        .await;
                }
                // Otherwise the sender retries the same file with the same
                // token; the core truncates and rewrites the staging file.
            }
            Err(ReceiveFailure::Fatal(message)) => {
                self.fail_incoming(format!("Cannot receive {}: {message}", file.file_name))
                    .await;
            }
        }
    }

    /// Answers a message transfer with 204 (nothing to download) and reports
    /// the text, then closes the transfer. The text arrived in the preview.
    fn accept_message(&mut self, mut incoming: Incoming) {
        let message = incoming
            .message()
            .map(receive::clamp_text)
            .unwrap_or_default();
        let files = incoming.offered();
        let total = incoming.total();
        if !incoming.accept_nothing() {
            // The sender is gone; the responder was already dropped.
            emit(&self.events, TransferEvent::Cancelled { id: incoming.id });
            return;
        }
        emit(
            &self.events,
            TransferEvent::Started {
                id: incoming.id.clone(),
                peer: incoming.peer.clone(),
                direction: Direction::Receiving,
                files: files.clone(),
                total,
            },
        );
        emit(
            &self.events,
            TransferEvent::TextReceived {
                id: incoming.id.clone(),
                text: message,
            },
        );
        emit(
            &self.events,
            TransferEvent::Completed {
                id: incoming.id,
                files,
            },
        );
    }

    fn complete_incoming(&mut self, mut incoming: Incoming) {
        incoming.finish_progress(&self.events);
        emit(
            &self.events,
            TransferEvent::Completed {
                id: incoming.id.clone(),
                files: incoming.offered(),
            },
        );
    }

    async fn fail_incoming(&mut self, error: String) {
        let Some(mut incoming) = self.incoming.take() else {
            return;
        };
        // Flush the byte count first: progress events are throttled and may
        // have been dropped, and terminal events carry no counts.
        if incoming.accepted {
            incoming.finish_progress(&self.events);
        }
        let paths = incoming.temp_paths();
        incoming.cancel.cancel();
        receive::cleanup_staging(paths);
        if let Some(server) = &self.server {
            server.cancel_session(&incoming.id).await;
        }
        emit(
            &self.events,
            TransferEvent::Failed {
                id: incoming.id,
                error,
            },
        );
    }

    async fn cancel_incoming(&mut self, id: &str) {
        if self
            .incoming
            .as_ref()
            .is_none_or(|incoming| incoming.id != id)
        {
            return;
        }
        let Some(mut incoming) = self.incoming.take() else {
            return;
        };
        let was_accepted = incoming.accepted;
        if was_accepted {
            // Flush the byte count before the terminal event: progress is
            // throttled and may have been dropped.
            incoming.finish_progress(&self.events);
        }
        let paths = incoming.temp_paths();
        incoming.cancel.cancel();
        incoming.decline();
        receive::cleanup_staging(paths);
        if let Some(server) = &self.server {
            server.cancel_session(&incoming.id).await;
        }
        let address = incoming.peer.address.clone();
        let fingerprint = incoming.peer.fingerprint.clone();
        let session_id = incoming.id.clone();
        emit(
            &self.events,
            TransferEvent::Cancelled {
                id: session_id.clone(),
            },
        );
        // An accepted session needs an explicit `/cancel`; a still-pending one
        // already got the 403 from the declined responder, so there is nothing
        // to announce. Best effort, off the actor's loop.
        if was_accepted && let Some(address) = address {
            let identity = self.identity.clone();
            tokio::spawn(async move {
                send::notify_cancel(&identity, &address, &fingerprint, &session_id).await;
            });
        }
    }

    async fn tick(&mut self) {
        // --- receive decision / idle timeouts ---
        let stale = self.incoming.as_ref().is_some_and(|incoming| {
            let timeout = if incoming.decision.is_some() {
                DECISION_IDLE_TIMEOUT
            } else {
                RECEIVE_IDLE_TIMEOUT
            };
            incoming.last_activity.elapsed() > timeout
        });
        if stale {
            let id = self
                .incoming
                .as_ref()
                .map(|incoming| incoming.id.clone())
                .unwrap_or_default();
            tracing::warn!(session = %id, "Receive session timed out");
            self.cancel_incoming(&id).await;
        }

        // --- outgoing idle watchdog (NET-12) ---
        // `activity` only starts moving once prepare-upload was answered, so
        // the receiver's 300 s decision window is not mistaken for a stall.
        let now = Instant::now();
        for (id, outgoing) in &mut self.outgoing {
            let seq = outgoing.activity.load(Ordering::Relaxed);
            if seq != outgoing.last_seq {
                outgoing.last_seq = seq;
                outgoing.last_activity = now;
                continue;
            }
            if seq > 0 && now.duration_since(outgoing.last_activity) > OUTGOING_IDLE_TIMEOUT {
                tracing::warn!(transfer = %id, "Outgoing transfer stalled; giving up");
                *outgoing
                    .failure
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner()) =
                    Some("The receiver stopped responding".to_string());
                outgoing.cancel.cancel();
            }
        }

        // --- peers with an active transfer are never reported lost (NET-15) ---
        let mut active = HashSet::new();
        if let Some(incoming) = self.incoming.as_ref().filter(|incoming| incoming.accepted) {
            active.insert(incoming.peer.fingerprint.to_ascii_uppercase());
        }
        for outgoing in self.outgoing.values() {
            active.insert(outgoing.retry.peer.fingerprint.to_ascii_uppercase());
        }
        *self
            .active_fingerprints
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = active;

        // --- automatic recovery (NET-13/N19) ---
        self.restart_discovery_if_due().await;
        self.restart_server_if_due().await;
    }

    /// Recreates discovery after a multicast failure, with exponential backoff
    /// (NET-13). Discovery failures are not fatal: the store keeps serving
    /// direct peers, so a restart only restores announcements.
    async fn restart_discovery_if_due(&mut self) {
        let Some(params) = self.discovery_params.clone() else {
            return;
        };
        if self
            .discovery
            .as_ref()
            .is_some_and(|discovery| discovery.take_failed())
            && self.discovery_restart.schedule()
        {
            tracing::warn!("Multicast discovery failed; restarting it");
        }
        if !self.discovery_restart.ready() {
            return;
        }
        self.discovery_restart.clear_due();
        if let Some(discovery) = self.discovery.take() {
            discovery.stop().await;
        }
        self.discovery = Some(
            DiscoveryRuntime::start(
                params,
                self.events.clone(),
                self.identity.fingerprint.clone(),
                Arc::clone(&self.active_fingerprints),
            )
            .await,
        );
        self.discovery_restart.reset();
    }

    /// Recreates the HTTP server after a listener failure, with exponential
    /// backoff; an exhausted budget becomes a visible `NetError` (NET-13).
    async fn restart_server_if_due(&mut self) {
        if self.server.is_some() || !self.server_restart.ready() {
            return;
        }
        self.server_restart.clear_due();
        match ServerRuntime::start(self.server_params.clone()).await {
            Ok((server, events)) => {
                self.server = Some(server);
                self.pending_server_events = Some(events);
                self.server_restart.reset();
                tracing::info!("the HTTP server was restarted");
            }
            Err(error) => {
                if self.server_restart.schedule() {
                    tracing::warn!(%error, "cannot restart the HTTP server; retrying");
                } else {
                    emit(
                        &self.events,
                        TransferEvent::NetError(format!(
                            "Receiving is unavailable: {error}. Restart the network in Settings."
                        )),
                    );
                }
            }
        }
    }

    async fn shutdown(&mut self, tasks: &mut JoinSet<Finished>) {
        // Ending the node must end every transfer the UI may still be showing.
        // The event channel closes when the actor returns, so the terminal
        // events are emitted here: a settings-driven restart or quit would
        // otherwise leave rows Active forever, and a peer waiting for a
        // decision would hang until its own 300 s timeout.
        if let Some(mut incoming) = self.incoming.take() {
            if incoming.accepted {
                incoming.finish_progress(&self.events);
            } else {
                // Answer the pending prepare-upload so the sender fails fast
                // instead of waiting for its decision timeout.
                incoming.decline();
            }
            let paths = incoming.temp_paths();
            incoming.cancel.cancel();
            receive::cleanup_staging_now(paths).await;
            if let Some(server) = &self.server {
                server.cancel_session(&incoming.id).await;
            }
            emit(&self.events, TransferEvent::Cancelled { id: incoming.id });
        }
        for (id, outgoing) in self.outgoing.drain() {
            outgoing.cancel.cancel();
            emit(&self.events, TransferEvent::Cancelled { id });
        }
        for (id, pending) in self.pin_pending.drain() {
            pending.cancel.cancel();
            emit(&self.events, TransferEvent::Cancelled { id });
        }

        // NET-18: sweep staging leftovers at shutdown too (an interrupted
        // upload on Windows can leave files that the cleanup above cannot
        // remove); the next start no longer needs to wait for a crash.
        remove_stale_staging_files(&self.temp_dir).await;

        // Stop the server first: it owns the fixed TCP port that a
        // settings-driven restart wants to bind again, and `Node::start`
        // retries that bind for only ~3 s. Discovery can take longer to wind
        // down (it awaits an in-flight announce burst) and must not delay the
        // port release.
        if let Some(server) = self.server.take() {
            server.stop().await;
        }
        if let Some(discovery) = self.discovery.take() {
            discovery.stop().await;
        }
        // Cancelling the tokens makes in-flight uploads give up; the join
        // reaps the tasks so no staging files are written after shutdown.
        tasks.abort_all();
        while tasks.join_next().await.is_some() {}
    }
}

/// The staging folder inside the receive folder.
const STAGING_DIR: &str = ".localsend-desktop-tmp";

/// The staging folder's name before the rename to localsend-desktop.
const LEGACY_STAGING_DIR: &str = ".localsend-client-tmp";

/// Create the receive folder and its staging folder, and prove both are
/// writable (a sandbox can create neither, though the folder may exist).
async fn prepare_receive_dir(folder: &PathBuf) -> Result<(PathBuf, PathBuf)> {
    tokio::fs::create_dir_all(folder)
        .await
        .with_context(|| format!("Cannot create the folder {}", folder.display()))?;
    let folder = tokio::fs::canonicalize(folder)
        .await
        .with_context(|| format!("Cannot resolve the folder {}", folder.display()))?;
    // The staging folder of the app before its rename only ever held `.part`
    // files; remove it so it does not linger in the user's folder.
    // `remove_dir_all` does not follow a symlink in its place.
    let _ = tokio::fs::remove_dir_all(folder.join(LEGACY_STAGING_DIR)).await;
    let staging = folder.join(STAGING_DIR);
    tokio::fs::create_dir_all(&staging)
        .await
        .with_context(|| format!("Cannot create the staging folder {}", staging.display()))?;
    let probe = staging.join(".write-probe");
    tokio::fs::write(&probe, b"")
        .await
        .with_context(|| format!("Cannot write in {}", staging.display()))?;
    let _ = tokio::fs::remove_file(&probe).await;
    Ok((folder, staging))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::ComposerItem;
    use crate::net::{PeerAddress, Protocol};
    use std::time::Duration;

    /// Waits for the next event matching `predicate`, dropping the others.
    async fn wait_for(
        events: &async_channel::Receiver<TransferEvent>,
        predicate: impl Fn(&TransferEvent) -> bool,
        timeout: Duration,
    ) -> TransferEvent {
        let deadline = tokio::time::Instant::now() + timeout;
        loop {
            let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
            let event = tokio::time::timeout(remaining, events.recv())
                .await
                .expect("timed out waiting for a transfer event")
                .expect("the event channel closed");
            if predicate(&event) {
                return event;
            }
        }
    }

    /// Like [`wait_for`], but returns `None` instead of panicking on timeout;
    /// used by environment-dependent (multicast) tests to skip themselves.
    async fn wait_for_opt(
        events: &async_channel::Receiver<TransferEvent>,
        predicate: impl Fn(&TransferEvent) -> bool,
        timeout: Duration,
    ) -> Option<TransferEvent> {
        let deadline = tokio::time::Instant::now() + timeout;
        loop {
            let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
            let event = tokio::time::timeout(remaining, events.recv())
                .await
                .ok()?
                .ok()?;
            if predicate(&event) {
                return Some(event);
            }
        }
    }

    fn config(
        alias: &str,
        port: u16,
        pin: Option<&str>,
        discoverable: bool,
        root: &std::path::Path,
    ) -> NodeConfig {
        NodeConfig {
            alias: alias.to_string(),
            device_type: crate::net::DeviceType::Desktop,
            port,
            https: true,
            pin: pin.map(str::to_string),
            discoverable,
            downloads: root.join("downloads"),
            data_dir: Some(root.join("identity")),
        }
    }

    fn test_peer() -> Peer {
        Peer {
            fingerprint: "PEER".to_string(),
            alias: "Peer".to_string(),
            device_model: String::new(),
            address: None,
            device_type: crate::net::DeviceType::Desktop,
            is_self: false,
        }
    }

    fn test_file(id: &str, size: u64) -> FileDto {
        FileDto {
            id: id.to_string(),
            file_name: format!("{id}.bin"),
            size,
            file_type: "application/octet-stream".to_string(),
            sha256: None,
            preview: None,
            metadata: None,
        }
    }

    /// A bare actor for shutdown/terminal-event tests: no sockets, no
    /// discovery. `Identity` is only used by the cancel-notification client,
    /// which these tests avoid by using peers without an address.
    fn test_actor(events: async_channel::Sender<TransferEvent>) -> Actor {
        Actor {
            identity: Identity {
                cert_pem: String::new(),
                private_key_pem: String::new(),
                fingerprint: "SELF".to_string(),
            },
            register: server::register_dto(
                "Test",
                crate::net::DeviceType::Desktop,
                "Test",
                "SELF",
                0,
                crate::net::Protocol::Https,
            ),
            server: None,
            server_params: test_server_params(),
            discovery: None,
            discovery_params: None,
            server_restart: RestartPlan::default(),
            discovery_restart: RestartPlan::default(),
            registered: discovery::RegisteredPeers::default(),
            active_fingerprints: Arc::default(),
            pending_server_events: None,
            events,
            downloads: PathBuf::from("/nonexistent"),
            temp_dir: PathBuf::from("/nonexistent"),
            https: true,
            incoming: None,
            outgoing: HashMap::new(),
            pin_pending: HashMap::new(),
        }
    }

    fn test_server_params() -> ServerParams {
        ServerParams {
            port: 0,
            tls: None,
            info: server::client_info("Test", crate::net::DeviceType::Desktop, "SELF"),
            pin: None,
        }
    }

    fn outgoing(cancel: &tokio_util::sync::CancellationToken) -> Outgoing {
        Outgoing {
            host: String::new(),
            remote_session: Arc::new(Mutex::new(None)),
            cancel: cancel.clone(),
            peer_cancelled: Arc::new(AtomicBool::new(false)),
            activity: Arc::new(AtomicU64::new(0)),
            last_seq: 0,
            last_activity: Instant::now(),
            failure: Arc::default(),
            retry: PendingSend {
                peer: test_peer(),
                uploads: vec![Upload::text("hi".to_string())],
                cancel: cancel.clone(),
            },
        }
    }

    /// A cancel that races a 401 must not open a PIN sheet for a transfer the
    /// user already cancelled (and must not leak a `pin_pending` entry).
    #[test]
    fn a_cancel_beats_a_racing_pin_request() {
        assert_eq!(
            send_terminal(SendOutcome::PinRequired, true),
            SendTerminal::Cancelled
        );
        assert_eq!(
            send_terminal(SendOutcome::PinRequired, false),
            SendTerminal::PinRequired
        );
        assert_eq!(
            send_terminal(SendOutcome::Failed("boom".to_string()), true),
            SendTerminal::Cancelled
        );
        // Completed wins: the bytes are gone, so "cancelled" would be a lie.
        assert_eq!(
            send_terminal(SendOutcome::Completed, true),
            SendTerminal::Completed
        );
    }

    /// Shutdown (a settings-driven restart or quit) must end every transfer the
    /// UI may still be showing, answer a pending prepare-upload so the sender
    /// fails fast instead of waiting for its 300 s timeout, and reap the
    /// transfer tasks (no leaks).
    #[tokio::test]
    async fn shutdown_emits_terminal_events_and_answers_pending_decisions() {
        let (events_tx, events) = async_channel::unbounded();
        let mut actor = test_actor(events_tx);

        // A pending receive: the user never answered the dialog.
        let (decision_tx, decision_rx) = tokio::sync::oneshot::channel();
        let files = HashMap::from([("0".to_string(), test_file("0", 10))]);
        actor.incoming = Some(Incoming::new(
            "receive-1".to_string(),
            test_peer(),
            files,
            decision_tx,
        ));
        // An outgoing transfer and a send waiting for a PIN.
        let send_cancel = tokio_util::sync::CancellationToken::new();
        actor
            .outgoing
            .insert("send-1".to_string(), outgoing(&send_cancel));
        let pin_cancel = tokio_util::sync::CancellationToken::new();
        actor.pin_pending.insert(
            "pin-1".to_string(),
            PendingSend {
                peer: test_peer(),
                uploads: vec![Upload::text("hello".to_string())],
                cancel: pin_cancel.clone(),
            },
        );

        // A transfer task that never finishes: shutdown has to abort and reap
        // it, or the actor's JoinSet keeps the runtime busy forever.
        let mut tasks: JoinSet<Finished> = JoinSet::new();
        tasks.spawn(std::future::pending());

        actor.shutdown(&mut tasks).await;

        assert!(
            send_cancel.is_cancelled(),
            "the send task must be cancelled"
        );
        assert!(
            pin_cancel.is_cancelled(),
            "the pending PIN must be cancelled"
        );
        assert!(tasks.is_empty(), "shutdown must reap every transfer task");
        assert!(
            matches!(
                decision_rx.await.expect("the responder must be answered"),
                PrepareUploadDecisionV2::Decline
            ),
            "a pending request must not hang its sender"
        );

        let mut cancelled = Vec::new();
        while let Ok(event) = events.try_recv() {
            if let TransferEvent::Cancelled { id } = event {
                cancelled.push(id);
            }
        }
        cancelled.sort();
        assert_eq!(
            cancelled,
            vec![
                "pin-1".to_string(),
                "receive-1".to_string(),
                "send-1".to_string()
            ]
        );
    }

    /// An accepted receive reports its final byte count before the terminal
    /// event: progress is throttled, so the model would otherwise finish with a
    /// stale number.
    #[tokio::test]
    async fn shutdown_flushes_the_final_progress_of_an_accepted_receive() {
        let (events_tx, events) = async_channel::unbounded();
        let mut actor = test_actor(events_tx.clone());
        let (decision_tx, _decision_rx) = tokio::sync::oneshot::channel();
        let files = HashMap::from([("0".to_string(), test_file("0", 100))]);
        let mut incoming = Incoming::new("receive-1".to_string(), test_peer(), files, decision_tx);
        assert!(incoming.accept());
        incoming.report_progress(&events_tx, "0", 100);
        actor.incoming = Some(incoming);

        let mut tasks: JoinSet<Finished> = JoinSet::new();
        actor.shutdown(&mut tasks).await;

        let mut terminal = Vec::new();
        while let Ok(event) = events.try_recv() {
            terminal.push(event);
        }
        let progress = terminal.iter().position(|event| {
            matches!(
                event,
                TransferEvent::Progress {
                    transferred: 100,
                    total: Some(100),
                    ..
                }
            )
        });
        let cancelled = terminal.iter().position(
            |event| matches!(event, TransferEvent::Cancelled { id } if id == "receive-1"),
        );
        assert!(
            matches!((progress, cancelled), (Some(progress), Some(cancelled)) if progress < cancelled),
            "the final progress must precede the terminal event: {terminal:?}"
        );
    }

    /// End-to-end test over loopback with two nodes: a file transfer that
    /// first hits the PIN gate (twice), then a message transfer (204 path).
    ///
    /// Ignored by default: it binds real TCP ports, generates an RSA identity
    /// and takes a few seconds. Run it with `cargo test -- --ignored`.
    #[tokio::test]
    #[ignore = "loopback end-to-end test: binds ports and generates a TLS identity"]
    async fn pin_retry_and_message_round_trip_between_two_nodes() {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("target")
            .join(format!("net-loopback-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        let receiver = Node::start(config(
            "Test Receiver",
            0,
            Some("1234"),
            false,
            &root.join("receiver"),
        ))
        .await
        .expect("receiver node");
        let sender = Node::start(config("Test Sender", 0, None, false, &root.join("sender")))
            .await
            .expect("sender node");

        let peer = Peer {
            fingerprint: receiver.device.fingerprint.clone(),
            alias: receiver.device.alias.clone(),
            device_model: receiver.device.device_model.clone(),
            address: Some(PeerAddress {
                host: "127.0.0.1".to_string(),
                port: receiver.port,
                protocol: Protocol::Https,
            }),
            device_type: crate::net::DeviceType::Desktop,
            is_self: false,
        };

        // --- file transfer, PIN entered twice ---------------------------
        let payload = root.join("payload.bin");
        std::fs::write(&payload, vec![7u8; 4096]).unwrap();
        let item = ComposerItem::File {
            path: payload.clone(),
            name: "payload.bin".to_string(),
            size: 4096,
            mime: "application/octet-stream".to_string(),
            is_dir: false,
        };
        let id = sender.handle.send(&peer, vec![item], None).expect("send");

        wait_for(
            &sender.events,
            |event| matches!(event, TransferEvent::Started { id: event_id, .. } if event_id == &id),
            Duration::from_secs(30),
        )
        .await;
        wait_for(
            &sender.events,
            |event| matches!(event, TransferEvent::PinRequired { id: event_id, .. } if event_id == &id),
            Duration::from_secs(30),
        )
        .await;

        // Wrong PIN first: the receiver answers 401 again, the same transfer
        // id stays alive.
        sender.handle.submit_pin(&id, "0000".to_string()).unwrap();
        wait_for(
            &sender.events,
            |event| matches!(event, TransferEvent::PinRequired { id: event_id, .. } if event_id == &id),
            Duration::from_secs(30),
        )
        .await;

        sender.handle.submit_pin(&id, "1234".to_string()).unwrap();
        let incoming = wait_for(
            &receiver.events,
            |event| matches!(event, TransferEvent::IncomingRequest { files, .. } if files.len() == 1),
            Duration::from_secs(30),
        )
        .await;
        let TransferEvent::IncomingRequest { id: receive_id, .. } = incoming else {
            unreachable!();
        };
        receiver.handle.decide(&receive_id, true).unwrap();

        wait_for(
            &sender.events,
            |event| matches!(event, TransferEvent::Completed { id: event_id, .. } if event_id == &id),
            Duration::from_secs(60),
        )
        .await;
        let done = wait_for(
            &receiver.events,
            |event| matches!(event, TransferEvent::FileDone { .. }),
            Duration::from_secs(60),
        )
        .await;
        let TransferEvent::FileDone { file, .. } = done else {
            unreachable!();
        };
        let saved = file.saved_to.expect("received file path");
        assert_eq!(saved.file_name().unwrap(), "payload.bin");
        assert_eq!(
            std::fs::read(&saved).unwrap(),
            std::fs::read(&payload).unwrap()
        );
        wait_for(
            &receiver.events,
            |event| matches!(event, TransferEvent::Completed { id: event_id, .. } if event_id == &receive_id),
            Duration::from_secs(60),
        )
        .await;

        // --- message transfer: prepare-upload answers 204 ---------------
        let message_id = sender
            .handle
            .send(
                &peer,
                vec![ComposerItem::Text("hello peer".to_string())],
                None,
            )
            .expect("send message");
        wait_for(
            &sender.events,
            |event| matches!(event, TransferEvent::PinRequired { id: event_id, .. } if event_id == &message_id),
            Duration::from_secs(30),
        )
        .await;
        sender
            .handle
            .submit_pin(&message_id, "1234".to_string())
            .unwrap();
        let pending_message = wait_for(
            &receiver.events,
            |event| matches!(event, TransferEvent::IncomingRequest { .. }),
            Duration::from_secs(30),
        )
        .await;
        let TransferEvent::IncomingRequest {
            id: message_receive_id,
            ..
        } = pending_message
        else {
            unreachable!();
        };
        receiver.handle.decide(&message_receive_id, true).unwrap();
        wait_for(
            &sender.events,
            |event| matches!(event, TransferEvent::Completed { id: event_id, .. } if event_id == &message_id),
            Duration::from_secs(30),
        )
        .await;
        let text = wait_for(
            &receiver.events,
            |event| matches!(event, TransferEvent::TextReceived { .. }),
            Duration::from_secs(30),
        )
        .await;
        assert!(matches!(text, TransferEvent::TextReceived { text, .. } if text == "hello peer"));

        // --- sender aborts while the receiver is still deciding ---------
        let abort_payload = root.join("abort.bin");
        std::fs::write(&abort_payload, vec![1u8; 1024]).unwrap();
        let abort_id = sender
            .handle
            .send(
                &peer,
                vec![ComposerItem::File {
                    path: abort_payload,
                    name: "abort.bin".to_string(),
                    size: 1024,
                    mime: "application/octet-stream".to_string(),
                    is_dir: false,
                }],
                None,
            )
            .expect("send abortable file");
        wait_for(
            &sender.events,
            |event| matches!(event, TransferEvent::PinRequired { id: event_id, .. } if event_id == &abort_id),
            Duration::from_secs(30),
        )
        .await;
        sender
            .handle
            .submit_pin(&abort_id, "1234".to_string())
            .unwrap();
        let pending = wait_for(
            &receiver.events,
            |event| matches!(event, TransferEvent::IncomingRequest { .. }),
            Duration::from_secs(30),
        )
        .await;
        let TransferEvent::IncomingRequest { id: pending_id, .. } = pending else {
            unreachable!();
        };
        // The sender gives up before the receiver decides: the core reports a
        // PrepareUploadAborted and both sides must cancel.
        sender.handle.cancel(&abort_id).unwrap();
        wait_for(
            &sender.events,
            |event| matches!(event, TransferEvent::Cancelled { id: event_id } if event_id == &abort_id),
            Duration::from_secs(30),
        )
        .await;
        wait_for(
            &receiver.events,
            |event| matches!(event, TransferEvent::Cancelled { id: event_id } if event_id == &pending_id),
            Duration::from_secs(30),
        )
        .await;

        sender.shutdown().await;
        receiver.shutdown().await;
        let _ = std::fs::remove_dir_all(&root);
    }

    /// Reads `cert+key` from a node's persisted identity file and returns the
    /// PEM pair (used to build an independent peer for the tests below).
    fn read_identity_pem(root: &std::path::Path) -> (String, String) {
        let text = std::fs::read_to_string(root.join("identity").join("identity.pem"))
            .expect("the node persisted an identity");
        let blocks = pem::parse_many(&text).expect("valid identity pem");
        let certificate = blocks
            .iter()
            .find(|block| block.tag() == "CERTIFICATE")
            .expect("certificate block");
        let key = blocks
            .iter()
            .find(|block| block.tag().ends_with("PRIVATE KEY"))
            .expect("key block");
        (pem::encode(certificate), pem::encode(key))
    }

    /// Our *receiver* against a raw `LsHttpClientV2` peer: register,
    /// prepare-upload (user accepts), upload, checksum verified by the core,
    /// file saved under a collision-safe name.
    ///
    /// Ignored by default: binds ports and generates an RSA client identity.
    #[tokio::test]
    #[ignore = "binds ports; generates an RSA identity (slow in debug)"]
    async fn raw_client_protocol_against_the_node_receiver() {
        use localsend::http::client::LsHttpClientV2;
        use localsend::http::dto_v2::{PrepareUploadRequestDtoV2, RegisterDtoV2};
        use localsend::model::discovery::ProtocolType;
        use localsend::model::transfer::FileDto;
        use std::collections::HashMap;

        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("target")
            .join(format!("net-raw-client-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        let node = Node::start(config(
            "Raw Receiver",
            0,
            Some("1234"),
            false,
            &root.join("node"),
        ))
        .await
        .expect("receiver node");

        // A completely separate client identity, like a stock LocalSend peer.
        let client_identity =
            tokio::task::spawn_blocking(localsend::crypto::cert::generate_self_signed)
                .await
                .unwrap()
                .unwrap();
        let client = LsHttpClientV2::try_new(
            &client_identity.private_key_pem,
            &client_identity.certificate_pem,
            Some(node.device.fingerprint.clone()),
            None,
        )
        .unwrap();

        // Register: the server confirms the client certificate fingerprint and
        // the actor publishes the peer.
        let info = RegisterDtoV2 {
            alias: "Raw Client".to_string(),
            version: crate::net::PROTOCOL_VERSION.to_string(),
            device_model: Some("Raw".to_string()),
            device_type: Some(localsend::model::discovery::DeviceType::Desktop),
            fingerprint: client_identity.fingerprint.clone(),
            port: 12345,
            protocol: ProtocolType::Https,
            download: false,
        };
        client
            .register(ProtocolType::Https, "127.0.0.1", node.port, info.clone())
            .await
            .expect("register");
        wait_for(
            &node.events,
            |event| matches!(event, TransferEvent::PeerFound(peer) if peer.alias == "Raw Client"),
            Duration::from_secs(30),
        )
        .await;

        // prepare-upload with a checksum; the node asks the app, which accepts.
        let payload: Vec<u8> = (0..4096u32).map(|index| (index % 251) as u8).collect();
        let checksum = localsend::crypto::hash::sha256_hex(&payload);
        let file = FileDto {
            id: "0".to_string(),
            file_name: "raw.bin".to_string(),
            size: payload.len() as u64,
            file_type: "application/octet-stream".to_string(),
            sha256: Some(checksum),
            preview: None,
            metadata: None,
        };
        let files = HashMap::from([("0".to_string(), file)]);
        let request = PrepareUploadRequestDtoV2 { info, files };
        let node_port = node.port;
        let prepare = tokio::spawn({
            let cancel = tokio_util::sync::CancellationToken::new();
            async move {
                client
                    .prepare_upload(
                        ProtocolType::Https,
                        "127.0.0.1",
                        node_port,
                        None,
                        request,
                        Some("1234"),
                        cancel,
                    )
                    .await
            }
        });
        let incoming = wait_for(
            &node.events,
            |event| matches!(event, TransferEvent::IncomingRequest { .. }),
            Duration::from_secs(30),
        )
        .await;
        let TransferEvent::IncomingRequest { id: session_id, .. } = incoming else {
            unreachable!();
        };
        node.handle.decide(&session_id, true).unwrap();
        let prepared = prepare.await.unwrap().expect("prepare-upload accepted");
        let response = prepared.response.expect("an accepted session");
        let token = response.files.get("0").expect("file token").clone();

        let client = LsHttpClientV2::try_new(
            &client_identity.private_key_pem,
            &client_identity.certificate_pem,
            Some(node.device.fingerprint.clone()),
            None,
        )
        .unwrap();
        client
            .upload(
                ProtocolType::Https,
                "127.0.0.1",
                node.port,
                None,
                &response.session_id,
                "0",
                &token,
                localsend::reqwest::Body::from(payload.clone()),
                tokio_util::sync::CancellationToken::new(),
            )
            .await
            .expect("upload");

        let done = wait_for(
            &node.events,
            |event| matches!(event, TransferEvent::FileDone { .. }),
            Duration::from_secs(30),
        )
        .await;
        let TransferEvent::FileDone { file, .. } = done else {
            unreachable!();
        };
        let saved = file.saved_to.expect("saved path");
        assert_eq!(saved.file_name().unwrap(), "raw.bin");
        assert_eq!(std::fs::read(&saved).unwrap(), payload);
        wait_for(
            &node.events,
            |event| matches!(event, TransferEvent::Completed { id, .. } if id == &session_id),
            Duration::from_secs(30),
        )
        .await;

        node.shutdown().await;
        let _ = std::fs::remove_dir_all(&root);
    }

    /// Our *sender* against a raw core HTTP server: prepare-upload → upload →
    /// the stub writes the bytes to disk (no app code in between).
    ///
    /// Ignored by default: binds ports.
    #[tokio::test]
    #[ignore = "binds ports"]
    async fn node_sender_protocol_against_a_raw_server() {
        use localsend::http::server::common::save::FileUploadTarget;
        use localsend::http::server::v2::{PrepareUploadDecisionV2, ServerEventV2};
        use localsend::http::server::{self, ServerConfigV2, TlsConfig};
        use localsend::http::state::ClientInfo;

        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("target")
            .join(format!("net-raw-server-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        let node = Node::start(config("Raw Sender", 0, None, false, &root.join("node")))
            .await
            .expect("sender node");
        let (cert_pem, private_key_pem) = read_identity_pem(&root.join("node"));
        let stub_downloads = root.join("stub-downloads");
        std::fs::create_dir_all(&stub_downloads).unwrap();

        let (event_tx, mut event_rx) = tokio::sync::mpsc::channel::<ServerEventV2>(16);
        let (stop_tx, stop_rx) = tokio::sync::oneshot::channel();
        let stub = server::start_with_port(
            0,
            Some(TlsConfig {
                cert: cert_pem,
                private_key: private_key_pem,
            }),
            ClientInfo {
                alias: "Raw Server".to_string(),
                version: crate::net::PROTOCOL_VERSION.to_string(),
                device_model: Some("Raw".to_string()),
                device_type: Some(localsend::model::discovery::DeviceType::Desktop),
                token: node.device.fingerprint.clone(),
            },
            None,
            Some(ServerConfigV2 {
                pin: None,
                verify_checksums: true,
                event_tx,
            }),
            localsend::http::server::web::WebConfig::default(),
            stop_rx,
        )
        .await
        .expect("stub server");
        let stub_port = stub.port();
        let stub_downloads_task = stub_downloads.clone();
        let stub_task = tokio::spawn(async move {
            while let Some(event) = event_rx.recv().await {
                match event {
                    ServerEventV2::PrepareUpload {
                        files, decision_tx, ..
                    } => {
                        assert_eq!(files.len(), 1);
                        let _ = decision_tx.send(PrepareUploadDecisionV2::Accept(
                            files.keys().cloned().collect(),
                        ));
                    }
                    ServerEventV2::FileUpload {
                        file, target_tx, ..
                    } => {
                        let path = stub_downloads_task.join(&file.file_name);
                        let (result_tx, result_rx) =
                            tokio::sync::oneshot::channel::<Result<(), String>>();
                        let _ = target_tx.send(FileUploadTarget::Path {
                            path,
                            result_tx,
                            progress_tx: None,
                        });
                        tokio::spawn(async move {
                            let _ = result_rx.await;
                        });
                    }
                    ServerEventV2::SessionEnd { .. } => break,
                    _ => {}
                }
            }
        });

        let peer = Peer {
            fingerprint: node.device.fingerprint.clone(),
            alias: "Raw Server".to_string(),
            device_model: "Raw".to_string(),
            address: Some(PeerAddress {
                host: "127.0.0.1".to_string(),
                port: stub_port,
                protocol: Protocol::Https,
            }),
            device_type: crate::net::DeviceType::Desktop,
            is_self: false,
        };
        let payload: Vec<u8> = (0..8192u32).map(|index| (index % 239) as u8).collect();
        let path = root.join("outgoing.bin");
        std::fs::write(&path, &payload).unwrap();
        let id = node
            .handle
            .send(
                &peer,
                vec![ComposerItem::File {
                    path,
                    name: "outgoing.bin".to_string(),
                    size: payload.len() as u64,
                    mime: "application/octet-stream".to_string(),
                    is_dir: false,
                }],
                None,
            )
            .expect("send");
        wait_for(
            &node.events,
            |event| matches!(event, TransferEvent::Completed { id: event_id, .. } if event_id == &id),
            Duration::from_secs(60),
        )
        .await;

        // Wait for the stub to report the save result.
        let saved = stub_downloads.join("outgoing.bin");
        let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
        while std::fs::read(&saved).ok().as_deref() != Some(payload.as_slice()) {
            assert!(
                tokio::time::Instant::now() < deadline,
                "the stub receiver never wrote the file"
            );
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        stub_task.abort();
        let _ = stop_tx.send(());
        stub.wait_stopped().await;
        node.shutdown().await;
        let _ = std::fs::remove_dir_all(&root);
    }

    /// Cross-process check against the repository's own CLI: the CLI probes
    /// this node like any LocalSend peer (register → discovery store),
    /// prepares an upload, and the node accepts and saves the file.
    ///
    /// Ignored by default: binds TCP 53317 and spawns
    /// `target/debug/localsend-cli` (build it first).
    #[tokio::test]
    #[ignore = "spawns target/debug/localsend-cli and binds TCP 53317"]
    async fn stock_cli_client_can_register_and_send_to_the_node() {
        let cli = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .expect("workspace root")
            .join("target")
            .join("debug")
            .join("localsend-cli");
        if !cli.exists() {
            eprintln!("skipping: build target/debug/localsend-cli first");
            return;
        }
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("target")
            .join(format!("net-cli-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        let node = match Node::start(config(
            "Cli Target",
            crate::net::DEFAULT_PORT,
            None,
            true,
            &root.join("node"),
        ))
        .await
        {
            Ok(node) => node,
            Err(error) => {
                eprintln!("skipping: cannot bind TCP 53317: {error:#}");
                let _ = std::fs::remove_dir_all(&root);
                return;
            }
        };

        let payload: Vec<u8> = (0..3000u32).map(|index| (index % 241) as u8).collect();
        let path = root.join("from-cli.bin");
        std::fs::write(&path, &payload).unwrap();
        let config_dir = root.join("cli-config");
        std::fs::create_dir_all(&config_dir).unwrap();
        let mut child = tokio::process::Command::new(&cli)
            .env("XDG_CONFIG_HOME", &config_dir)
            // The CLI's own server must not fight this node for TCP 53317.
            .args(["--port", "0", "send", "--to", "127.0.0.1"])
            .arg(&path)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .expect("spawn localsend-cli");

        let Some(incoming) = wait_for_opt(
            &node.events,
            |event| matches!(event, TransferEvent::IncomingRequest { .. }),
            Duration::from_secs(60),
        )
        .await
        else {
            let _ = child.kill().await;
            let output = child.wait_with_output().await;
            eprintln!("skipping: the CLI never reached this node (multicast/IP probe): {output:?}");
            node.shutdown().await;
            let _ = std::fs::remove_dir_all(&root);
            return;
        };
        let TransferEvent::IncomingRequest { id: session_id, .. } = incoming else {
            unreachable!();
        };
        node.handle.decide(&session_id, true).unwrap();

        wait_for(
            &node.events,
            |event| matches!(event, TransferEvent::Completed { id, .. } if id == &session_id),
            Duration::from_secs(60),
        )
        .await;

        let exited = tokio::time::timeout(Duration::from_secs(60), child.wait_with_output()).await;
        let output = exited.expect("the CLI did not exit").expect("wait for CLI");
        assert!(
            output.status.success(),
            "the CLI failed: stdout={} stderr={}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        let downloads = std::fs::canonicalize(root.join("node").join("downloads")).unwrap();
        let saved = downloads.join("from-cli.bin");
        assert_eq!(std::fs::read(&saved).unwrap(), payload);

        // Second phase: the CLI has to find this node through the multicast
        // announcement handshake (alias resolution, no direct address).
        let payload2: Vec<u8> = (0..1500u32).map(|index| (index % 233) as u8).collect();
        let path2 = root.join("from-cli2.bin");
        std::fs::write(&path2, &payload2).unwrap();
        let mut child2 = tokio::process::Command::new(&cli)
            .env("XDG_CONFIG_HOME", &config_dir)
            .args([
                "--port",
                "0",
                "--alias",
                "Stock CLI Peer",
                "send",
                "--to",
                "Cli Target",
            ])
            .arg(&path2)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .expect("spawn localsend-cli again");
        let Some(incoming2) = wait_for_opt(
            &node.events,
            |event| matches!(event, TransferEvent::IncomingRequest { .. }),
            Duration::from_secs(60),
        )
        .await
        else {
            let _ = child2.kill().await;
            let output = child2.wait_with_output().await;
            eprintln!(
                "skipping multicast discovery phase: the CLI did not discover this node: {output:?}"
            );
            node.shutdown().await;
            let _ = std::fs::remove_dir_all(&root);
            return;
        };
        let TransferEvent::IncomingRequest {
            id: session_id2, ..
        } = incoming2
        else {
            unreachable!();
        };
        node.handle.decide(&session_id2, true).unwrap();
        wait_for(
            &node.events,
            |event| matches!(event, TransferEvent::Completed { id, .. } if id == &session_id2),
            Duration::from_secs(60),
        )
        .await;
        let exited2 =
            tokio::time::timeout(Duration::from_secs(60), child2.wait_with_output()).await;
        let output2 = exited2
            .expect("the CLI did not exit")
            .expect("wait for CLI");
        assert!(
            output2.status.success(),
            "the CLI failed on multicast discovery: stdout={} stderr={}",
            String::from_utf8_lossy(&output2.stdout),
            String::from_utf8_lossy(&output2.stderr)
        );
        assert_eq!(
            std::fs::read(downloads.join("from-cli2.bin")).unwrap(),
            payload2
        );

        node.shutdown().await;
        let _ = std::fs::remove_dir_all(&root);
    }

    /// Discovery test: two nodes announce themselves over multicast, learn
    /// about each other, and the discovered peer is directly dialable.
    ///
    /// Ignored by default: it binds the LocalSend multicast port (53317) on
    /// every network interface and announces this machine to the LAN.
    #[tokio::test]
    #[ignore = "binds UDP 53317 and announces this machine on the LAN"]
    async fn two_nodes_discover_each_other_and_transfer() {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("target")
            .join(format!("net-discovery-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        let receiver = Node::start(config(
            "Discovery Receiver",
            0,
            None,
            true,
            &root.join("receiver"),
        ))
        .await
        .expect("receiver node");
        let sender = Node::start(config(
            "Discovery Sender",
            0,
            None,
            true,
            &root.join("sender"),
        ))
        .await
        .expect("sender node");

        // Many networks (and some macOS configurations) do not deliver
        // multicast between two sockets on the same host. The core's own
        // multicast tests skip in that case; do the same instead of failing.
        let Some(found) = wait_for_opt(
            &sender.events,
            |event| matches!(event, TransferEvent::PeerFound(peer) if peer.alias == "Discovery Receiver"),
            Duration::from_secs(20),
        )
        .await
        else {
            eprintln!("skipping: multicast announcements are not delivered on this machine");
            sender.shutdown().await;
            receiver.shutdown().await;
            let _ = std::fs::remove_dir_all(&root);
            return;
        };
        let TransferEvent::PeerFound(peer) = found else {
            unreachable!();
        };
        assert!(peer.address.is_some(), "a discovered peer must be dialable");
        if wait_for_opt(
            &receiver.events,
            |event| matches!(event, TransferEvent::PeerFound(peer) if peer.alias == "Discovery Sender"),
            Duration::from_secs(20),
        )
        .await
        .is_none()
        {
            eprintln!("skipping: multicast announcements are not delivered symmetrically");
            sender.shutdown().await;
            receiver.shutdown().await;
            let _ = std::fs::remove_dir_all(&root);
            return;
        }

        let payload = root.join("discovered.bin");
        std::fs::write(&payload, vec![3u8; 2048]).unwrap();
        let id = sender
            .handle
            .send(
                &peer,
                vec![ComposerItem::File {
                    path: payload,
                    name: "discovered.bin".to_string(),
                    size: 2048,
                    mime: "application/octet-stream".to_string(),
                    is_dir: false,
                }],
                None,
            )
            .expect("send to discovered peer");
        let incoming = wait_for(
            &receiver.events,
            |event| matches!(event, TransferEvent::IncomingRequest { .. }),
            Duration::from_secs(30),
        )
        .await;
        let TransferEvent::IncomingRequest { id: receive_id, .. } = incoming else {
            unreachable!();
        };
        receiver.handle.decide(&receive_id, true).unwrap();
        wait_for(
            &sender.events,
            |event| matches!(event, TransferEvent::Completed { id: event_id, .. } if event_id == &id),
            Duration::from_secs(60),
        )
        .await;
        let done = wait_for(
            &receiver.events,
            |event| matches!(event, TransferEvent::FileDone { .. }),
            Duration::from_secs(60),
        )
        .await;
        assert!(matches!(
            done,
            TransferEvent::FileDone { file, .. } if file.saved_to.as_ref().is_some_and(|path| path.file_name().is_some_and(|name| name == "discovered.bin"))
        ));
        wait_for(
            &receiver.events,
            |event| matches!(event, TransferEvent::Completed { id: event_id, .. } if event_id == &receive_id),
            Duration::from_secs(60),
        )
        .await;

        sender.shutdown().await;
        receiver.shutdown().await;
        let _ = std::fs::remove_dir_all(&root);
    }

    /// NET-13: the restart backoff grows from 5 s to 60 s and stops after a
    /// bounded number of attempts instead of retrying forever.
    #[test]
    fn restart_backoff_is_bounded_and_exponential() {
        assert_eq!(restart_backoff(0), Duration::from_secs(5));
        assert_eq!(restart_backoff(1), Duration::from_secs(10));
        assert_eq!(restart_backoff(2), Duration::from_secs(30));
        assert_eq!(restart_backoff(3), Duration::from_secs(60));
        assert_eq!(restart_backoff(50), Duration::from_secs(60));

        let mut plan = RestartPlan::default();
        for _ in 0..MAX_RESTART_ATTEMPTS {
            assert!(plan.schedule(), "an attempt must be scheduled");
            assert!(plan.due.is_some());
            assert!(!plan.ready(), "a fresh attempt is not due immediately");
            plan.clear_due();
        }
        assert!(
            !plan.schedule(),
            "the attempt budget must be exhausted eventually"
        );
    }

    /// NET-18/N23: only staging files older than the threshold are swept.
    #[test]
    fn stale_staging_files_need_to_be_old() {
        let now = SystemTime::now();
        assert!(is_stale_staging(
            now - STALE_STAGING_AGE - Duration::from_secs(1),
            now
        ));
        assert!(!is_stale_staging(now - Duration::from_secs(30), now));
        assert!(!is_stale_staging(now, now));
        // A file with a timestamp in the future is never stale.
        assert!(!is_stale_staging(now + Duration::from_secs(60), now));
    }

    /// NET-08: a peer-controlled rejection message is bounded and never splits
    /// a character.
    #[test]
    fn net_error_messages_are_truncated_on_a_char_boundary() {
        assert_eq!(truncate_message("short", MAX_NET_ERROR_CHARS), "short");
        let long = "é".repeat(MAX_NET_ERROR_CHARS * 2);
        let truncated = truncate_message(&long, MAX_NET_ERROR_CHARS);
        assert_eq!(truncated.chars().count(), MAX_NET_ERROR_CHARS + 1);
        assert!(truncated.ends_with('…'));
        // Valid UTF-8 by construction (still a String, but check the boundary).
        assert_eq!(
            truncated.chars().filter(|c| *c == 'é').count(),
            MAX_NET_ERROR_CHARS
        );
    }

    /// NET-12: an outgoing transfer that was accepted but then stopped moving
    /// is cancelled by the tick and reports a failure reason, not a user
    /// cancel. A transfer still in prepare-upload (no activity yet) is left
    /// alone: the receiver's user may legitimately take minutes to decide.
    #[tokio::test]
    async fn the_tick_fails_a_stalled_outgoing_transfer() {
        let (events_tx, events) = async_channel::unbounded();
        let mut actor = test_actor(events_tx);

        let stalled_cancel = tokio_util::sync::CancellationToken::new();
        let mut stalled = outgoing(&stalled_cancel);
        // Activity stopped moving a minute ago (accepted, then silence).
        stalled.last_seq = 7;
        stalled.activity = Arc::new(AtomicU64::new(7));
        stalled.last_activity = Instant::now() - OUTGOING_IDLE_TIMEOUT - Duration::from_secs(1);
        actor.outgoing.insert("stalled".to_string(), stalled);

        // Still waiting for the receiver's decision: no activity yet.
        let pending_cancel = tokio_util::sync::CancellationToken::new();
        actor
            .outgoing
            .insert("deciding".to_string(), outgoing(&pending_cancel));

        actor.tick().await;

        assert!(
            stalled_cancel.is_cancelled(),
            "a stalled transfer must be given up"
        );
        assert!(
            !pending_cancel.is_cancelled(),
            "a pending decision is not a stall"
        );
        let failure = actor.outgoing["stalled"].failure.lock().unwrap().clone();
        assert!(failure.is_some(), "the watchdog must record a reason");
        let _ = events; // no event is emitted by the tick itself
    }

    /// NET-13: a listener failure schedules a server restart and tells the
    /// user receiving is retrying, instead of leaving the app unreachable
    /// until a manual network restart.
    #[tokio::test]
    async fn a_listener_failure_schedules_a_server_restart() {
        let (events_tx, events) = async_channel::unbounded();
        let mut actor = test_actor(events_tx);
        let mut tasks = JoinSet::new();
        let (_progress_tx, _progress_rx) = mpsc::unbounded_channel();
        actor
            .server_event(
                ServerEventV2::ListenerFailed {
                    error: "listener closed".to_string(),
                },
                &mut tasks,
                &_progress_tx,
            )
            .await;
        assert!(
            actor.server_restart.due.is_some(),
            "a restart must be scheduled"
        );
        let mut net_error = None;
        while let Ok(event) = events.try_recv() {
            if let TransferEvent::NetError(message) = event {
                net_error = Some(message);
            }
        }
        let net_error = net_error.expect("the failure must be visible");
        assert!(net_error.contains("Retrying automatically"), "{net_error}");
    }

    /// A late `SessionEnd(Cancelled)` for an earlier session must not drop
    /// the request that is current now (the core frees the slot before it
    /// sends the event).
    #[tokio::test]
    async fn a_stale_session_end_keeps_the_current_request() {
        let (events_tx, events) = async_channel::unbounded();
        let mut actor = test_actor(events_tx);
        let (decision, _decision_rx) = tokio::sync::oneshot::channel();
        let files = HashMap::from([("0".to_string(), test_file("0", 3))]);
        actor.incoming = Some(Incoming::new(
            "current".to_string(),
            test_peer(),
            files,
            decision,
        ));

        actor
            .session_end("earlier", SessionEndReasonV2::Cancelled)
            .await;
        assert!(
            actor
                .incoming
                .as_ref()
                .is_some_and(|incoming| incoming.id == "current"),
            "the current request must survive a stale end event"
        );
        assert!(
            events.try_recv().is_err(),
            "nothing to report for a stale id"
        );

        actor
            .session_end("current", SessionEndReasonV2::Cancelled)
            .await;
        assert!(actor.incoming.is_none());
    }

    /// NET-15: peers with an active transfer are exposed to the discovery
    /// publish loop so it can keep them visible.
    #[tokio::test]
    async fn the_tick_publishes_the_active_transfer_fingerprints() {
        let (events_tx, _events) = async_channel::unbounded();
        let mut actor = test_actor(events_tx);
        let cancel = tokio_util::sync::CancellationToken::new();
        let mut active = outgoing(&cancel);
        active.retry.peer.fingerprint = "peer-a".to_string();
        actor.outgoing.insert("send".to_string(), active);

        actor.tick().await;
        let exposed = actor.active_fingerprints.lock().unwrap().clone();
        assert_eq!(exposed, HashSet::from(["PEER-A".to_string()]));

        actor.outgoing.clear();
        actor.tick().await;
        assert!(actor.active_fingerprints.lock().unwrap().is_empty());
    }
}
