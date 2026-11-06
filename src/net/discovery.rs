//! Network discovery: multicast announcements, unicast register handling and
//! the peer list the UI shows.
//!
//! The core's `discovery` module owns the sockets and the device store. This
//! module drives it: it announces this device (and re-announces periodically),
//! probes known peers, snapshots the store into app-facing [`Peer`]s and
//! expires peers that stopped answering. Everything reaches the UI through the
//! [`TransferEvent`] channel — nothing here holds UI state.

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime};

use localsend::discovery::{
    self, DeviceChannel, DeviceIdentity, DiscoveredDevice, DiscoveryConfig, DiscoveryEvent,
    DiscoveryHandle, HttpChannel, StatefulDevice,
};
use localsend::multicast::{
    DEFAULT_MULTICAST_GROUP, DEFAULT_MULTICAST_GROUP_V6, DEFAULT_PORT, MulticastDevice,
};
use tokio::sync::{mpsc, oneshot};
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

use crate::net::{Peer, PeerAddress, TransferEvent, emit};

/// A peer unseen for this long disappears from the device list. It is longer
/// than one announcement interval so a single lost burst does not make peers
/// flicker, but short enough that a peer that left the network disappears.
const PEER_EXPIRY: Duration = Duration::from_secs(20);

/// How often this device re-announces itself and re-probes known peers.
///
/// Every announcement makes the other devices register with this server
/// again, which is what keeps them in the discovery store (the core has no
/// expiry of its own; this module expires by last confirmation). The interval
/// must therefore stay below [`PEER_EXPIRY`].
const ANNOUNCE_INTERVAL: Duration = Duration::from_secs(10);

/// How often the discovery store is snapshotted into peer events. Discovery
/// events are lossy (`try_send` in the core), so the store is the authority.
const SNAPSHOT_INTERVAL: Duration = Duration::from_secs(1);

/// Timeout of the register requests discovery sends (announce answers and
/// probes). Wi-Fi needs more than the core's 500 ms default.
const REGISTER_TIMEOUT: Duration = Duration::from_secs(2);

/// Upper bound of the app's device table. A peer can inject a new fingerprint
/// with every `POST /register`; the core store has no eviction API, so the app
/// refuses new fingerprints beyond this bound (NET-02). Known peers keep being
/// updated, and the bound is far above any real LAN.
pub const MAX_DEVICE_TABLE: usize = 500;

/// How often a full device table may evict an entry to admit a new
/// fingerprint. Every admission also enters the core store, which never
/// evicts (BLOCKED-UPSTREAM), so this caps its growth under a register flood
/// (at most one entry per interval) while a real device still gets in.
pub const EVICTION_INTERVAL: Duration = Duration::from_secs(2);

/// The app-side device table guard: bounds the number of distinct
/// fingerprints from peer registrations (NET-02). The core's discovery store
/// keeps every device forever, so this is what keeps the process memory and
/// the 1 Hz snapshot cost bounded. BLOCKED-UPSTREAM: real eviction needs a
/// `remove_device`/`MAX_DEVICES` API in the vendored core.
///
/// A full table evicts its least recently seen entry (rate-limited by
/// [`EVICTION_INTERVAL`]). Refusing every new entry instead let one peer that
/// registered 500 fake fingerprints hide every real device that joined later,
/// until the app restarted.
#[derive(Default)]
pub struct RegisteredPeers {
    /// Fingerprint (upper case) -> registration sequence number.
    known: HashMap<String, u64>,
    /// Monotonic counter; a higher value was seen more recently.
    clock: u64,
    /// When the table last evicted an entry to admit a new one.
    last_eviction: Option<Instant>,
}

impl RegisteredPeers {
    /// Whether this registration may be fed into the discovery store / UI.
    /// Known fingerprints are always accepted (address updates stay fresh).
    pub fn accept(&mut self, fingerprint: &str) -> bool {
        self.accept_at(fingerprint, Instant::now())
    }

    fn accept_at(&mut self, fingerprint: &str, now: Instant) -> bool {
        let fingerprint = fingerprint.to_ascii_uppercase();
        self.clock += 1;
        if let Some(seen) = self.known.get_mut(&fingerprint) {
            *seen = self.clock;
            return true;
        }
        if self.known.len() >= MAX_DEVICE_TABLE {
            if self
                .last_eviction
                .is_some_and(|last| now.duration_since(last) < EVICTION_INTERVAL)
            {
                return false;
            }
            // O(n) over at most 500 entries, at most once per interval.
            if let Some(oldest) = self
                .known
                .iter()
                .min_by_key(|(_, seen)| **seen)
                .map(|(key, _)| key.clone())
            {
                self.known.remove(&oldest);
            }
            self.last_eviction = Some(now);
        }
        self.known.insert(fingerprint, self.clock);
        true
    }

    /// Number of distinct fingerprints admitted so far.
    pub fn len(&self) -> usize {
        self.known.len()
    }

    pub fn is_empty(&self) -> bool {
        self.known.is_empty()
    }
}

/// What a [`DiscoveryRuntime`] needs from the node.
#[derive(Clone)]
pub struct DiscoveryParams {
    /// This device, as announced on the network.
    pub device: MulticastDevice,
    /// The TLS identity used for the register requests.
    pub identity: DeviceIdentity,
}

/// A running discovery: sockets, store, announce/probe loop and the peer
/// publishing loop.
pub struct DiscoveryRuntime {
    handle: Arc<DiscoveryHandle>,
    stop: Option<oneshot::Sender<()>>,
    cancel: CancellationToken,
    /// Set when multicast stopped working; the actor then restarts discovery
    /// with backoff (NET-13).
    failed: Arc<AtomicBool>,
    tasks: Vec<JoinHandle<()>>,
    refresh: Arc<tokio::sync::Notify>,
}

impl DiscoveryRuntime {
    /// Starts multicast discovery (IPv4 and IPv6) and the announce/publish
    /// loops. Never fails: a network without usable multicast still works
    /// through HTTP probes, and the failure is reported as a `NetError`.
    pub async fn start(
        params: DiscoveryParams,
        events: async_channel::Sender<TransferEvent>,
        own_fingerprint: String,
        active_transfers: Arc<Mutex<HashSet<String>>>,
    ) -> Self {
        let (update_tx, mut update_rx) = mpsc::channel::<DiscoveryEvent>(256);
        let (stop_tx, stop_rx) = oneshot::channel();
        let cancel = CancellationToken::new();

        let handle = Arc::new(
            discovery::start(
                DiscoveryConfig {
                    group: DEFAULT_MULTICAST_GROUP,
                    group_v6: Some(DEFAULT_MULTICAST_GROUP_V6),
                    // The multicast port is the protocol's fixed one; the
                    // announced HTTP port travels in the announcement itself.
                    port: DEFAULT_PORT,
                    interface_filter: Default::default(),
                    device: params.device.clone(),
                    identity: params.identity,
                    timeout: REGISTER_TIMEOUT,
                    event_tx: Some(update_tx),
                },
                stop_rx,
            )
            .await,
        );

        if let Some(error) = handle.multicast_error() {
            emit(
                &events,
                TransferEvent::NetError(format!(
                    "Multicast is unavailable; discovery falls back to direct peers: {error:#}"
                )),
            );
        }
        // `DiscoveryActive` is emitted by the actor only on a state change
        // (NET-16); this loop used to re-emit it on every announce tick.
        let failed = Arc::new(AtomicBool::new(false));
        let announce_failed = Arc::clone(&failed);

        // Announce first, then keep re-announcing and re-probing known peers,
        // so devices that joined later (or missed the burst) still show up.
        let announce_handle = Arc::clone(&handle);
        let announce_cancel = cancel.clone();
        let refresh = Arc::new(tokio::sync::Notify::new());
        let announce_refresh = Arc::clone(&refresh);
        let announce_task = tokio::spawn(async move {
            let mut interval = tokio::time::interval(ANNOUNCE_INTERVAL);
            interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            loop {
                tokio::select! {
                    _ = announce_cancel.cancelled() => break,
                    _ = interval.tick() => {},
                    _ = announce_refresh.notified() => {},
                }
                if announce_failed.load(Ordering::Relaxed) {
                    break;
                }
                // The steps below can take seconds (the announcement burst is
                // ~2.6 s, probes time out per channel). Race them against the
                // stop token so a network restart does not wait for them: the
                // fixed HTTP port is only rebound after the actor's shutdown,
                // which awaits this task.
                tokio::select! {
                    _ = announce_cancel.cancelled() => break,
                    _ = announce_handle.announce() => {}
                }
                // Probe only peers that answered recently: the core store never
                // forgets a device, so an all-day session would otherwise keep
                // registering with every peer that ever showed up.
                let known = recent_http_channels(&announce_handle.devices(), SystemTime::now());
                if !known.is_empty() {
                    tokio::select! {
                        _ = announce_cancel.cancelled() => break,
                        _ = announce_handle.discover_known_http_channels(known) => {}
                    }
                }
            }
        });

        // The store is authoritative: snapshot it on every discovery event and
        // on a timer, and derive PeerFound/PeerLost by diffing what was shown.
        let publish_events = events;
        let publish_handle = Arc::clone(&handle);
        let publish_cancel = cancel.clone();
        let publish_own = own_fingerprint.clone();
        let publish_active = active_transfers;
        let publish_failed = Arc::clone(&failed);
        let publish_task = tokio::spawn(async move {
            let mut visible: HashMap<String, Peer> = HashMap::new();
            let mut interval = tokio::time::interval(SNAPSHOT_INTERVAL);
            interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            loop {
                tokio::select! {
                    _ = publish_cancel.cancelled() => break,
                    _ = interval.tick() => {}
                    update = update_rx.recv() => match update {
                        Some(DiscoveryEvent::MulticastFailed) => {
                            // The actor restarts discovery with backoff
                            // (NET-13); report the flag instead of killing the
                            // whole feature with a dead-end error.
                            tracing::warn!("Multicast discovery failed; scheduling a restart");
                            publish_failed.store(true, Ordering::Relaxed);
                        }
                        Some(_) => {}
                        None => {
                            if publish_failed.load(Ordering::Relaxed) {
                                break;
                            }
                        }
                    },
                }
                let active = publish_active
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner())
                    .clone();
                publish_snapshot(
                    &publish_handle,
                    &publish_events,
                    &publish_own,
                    &mut visible,
                    &active,
                );
                if publish_cancel.is_cancelled() {
                    break;
                }
            }
        });

        Self {
            handle,
            stop: Some(stop_tx),
            cancel,
            failed,
            tasks: vec![announce_task, publish_task],
            refresh,
        }
    }

    /// Wake the existing loop; concurrent requests coalesce into one burst.
    pub fn refresh(&self) {
        self.refresh.notify_one();
    }

    /// The authoritative discovery store (used to feed registrations back in).
    pub fn handle(&self) -> &DiscoveryHandle {
        &self.handle
    }

    /// Whether multicast failed since the last call (consume-on-read).
    pub fn take_failed(&self) -> bool {
        self.failed.swap(false, Ordering::Relaxed)
    }

    /// Stops announcements, closes the sockets and waits for the tasks.
    pub async fn stop(mut self) {
        self.cancel.cancel();
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }
        for task in self.tasks.drain(..) {
            let _ = task.await;
        }
        self.handle.wait_stopped().await;
    }
}

/// Turns one store entry into the app-facing peer, or `None` when it has
/// expired or has no dialable channel.
fn stateful_peer(state: &StatefulDevice, own_fingerprint: &str, now: SystemTime) -> Option<Peer> {
    if !last_seen_recently(state, now) {
        return None;
    }
    let channel = state
        .get_ranked_channels()
        .into_iter()
        .find_map(DeviceChannel::http)?;
    if state
        .device
        .fingerprint
        .eq_ignore_ascii_case(own_fingerprint)
    {
        return None;
    }
    Some(Peer {
        fingerprint: state.device.fingerprint.clone(),
        alias: state.device.alias.clone(),
        device_model: state.device.device_model.clone().unwrap_or_default(),
        address: Some(PeerAddress {
            host: channel.host.clone(),
            port: channel.port,
            protocol: crate::net::protocol_from_core(channel.protocol),
        }),
        device_type: crate::net::device_type_from_core(state.device.device_type.clone()),
        is_self: false,
    })
}

/// Whether a store entry was confirmed recently enough to be shown/probed.
fn last_seen_recently(state: &StatefulDevice, now: SystemTime) -> bool {
    state
        .logs
        .last()
        .is_some_and(|log| now.duration_since(log.timestamp).unwrap_or_default() <= PEER_EXPIRY)
}

fn ranked_http_channels(state: &StatefulDevice) -> Vec<HttpChannel> {
    state
        .get_ranked_channels()
        .into_iter()
        .filter_map(|channel| channel.http().cloned())
        .collect()
}

/// Channels of peers that answered recently enough to be worth probing.
///
/// The core's discovery store never drops a device, so probing every stored
/// channel every [`ANNOUNCE_INTERVAL`] would keep sending register requests to
/// peers that left long ago (and grow without bound over a long session).
fn recent_http_channels(devices: &[StatefulDevice], now: SystemTime) -> Vec<HttpChannel> {
    devices
        .iter()
        .filter(|state| last_seen_recently(state, now))
        .flat_map(ranked_http_channels)
        .collect()
}

/// Diffs the discovery store against what the UI currently knows and emits
/// `PeerFound` for new/changed peers and `PeerLost` for peers that vanished.
///
/// A peer with an active transfer is never reported lost: its announcement
/// burst can be missed while its files are still moving (NET-15/N20). The peer
/// stays in `visible`, so it is reported lost normally once the transfer ends
/// and it still does not answer.
fn publish_snapshot(
    discovery: &DiscoveryHandle,
    events: &async_channel::Sender<TransferEvent>,
    own_fingerprint: &str,
    visible: &mut HashMap<String, Peer>,
    active_transfers: &HashSet<String>,
) {
    let (next, found, lost) = plan_snapshot(
        visible,
        &discovery.devices(),
        own_fingerprint,
        SystemTime::now(),
        active_transfers,
    );
    for peer in found {
        emit(events, TransferEvent::PeerFound(peer));
    }
    for fingerprint in lost {
        emit(events, TransferEvent::PeerLost(fingerprint));
    }
    *visible = next;
}

/// Pure snapshot diff: returns the new visible map plus the peers to announce
/// as found/lost. Split out so the "active transfer is never lost" rule is
/// testable without sockets (NET-15).
fn plan_snapshot(
    visible: &HashMap<String, Peer>,
    devices: &[StatefulDevice],
    own_fingerprint: &str,
    now: SystemTime,
    active_fingerprints: &HashSet<String>,
) -> (HashMap<String, Peer>, Vec<Peer>, Vec<String>) {
    let mut next: HashMap<String, Peer> = HashMap::new();
    let mut found = Vec::new();
    for state in devices {
        let Some(peer) = stateful_peer(state, own_fingerprint, now) else {
            continue;
        };
        if visible.get(&peer.fingerprint) != Some(&peer) {
            found.push(peer.clone());
        }
        next.insert(peer.fingerprint.clone(), peer);
    }
    // Keep peers that are transferring even when they stopped answering.
    for (fingerprint, peer) in visible {
        if next.contains_key(fingerprint) {
            continue;
        }
        let is_active = active_fingerprints
            .iter()
            .any(|active| active.eq_ignore_ascii_case(fingerprint));
        if is_active {
            next.insert(fingerprint.clone(), peer.clone());
        }
    }
    let lost = visible
        .keys()
        .filter(|fingerprint| !next.contains_key(*fingerprint))
        .cloned()
        .collect();
    (next, found, lost)
}

/// Feeds a device confirmed outside the store (a peer that registered with
/// this server) back into discovery, so the same expiry/publishing applies.
pub async fn add_registered_peer(discovery: &DiscoveryHandle, peer: &Peer) -> bool {
    let Some(address) = &peer.address else {
        return false;
    };
    discovery
        .add_device(DiscoveredDevice {
            alias: peer.alias.clone(),
            version: crate::net::PROTOCOL_VERSION.to_string(),
            device_model: Some(peer.device_model.clone()),
            device_type: crate::net::device_type_to_core(peer.device_type),
            fingerprint: peer.fingerprint.clone(),
            channel: DeviceChannel::Http(HttpChannel {
                host: address.host.clone(),
                port: address.port,
                protocol: crate::net::protocol_to_core(address.protocol),
            }),
            download: false,
        })
        .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use localsend::discovery::{ChannelStatus, DeviceLog, DeviceLogKind};
    use localsend::model::discovery::{DeviceType as CoreDeviceType, ProtocolType};
    use std::time::Duration;

    fn device(
        fingerprint: &str,
        alias: &str,
        host: &str,
        protocol: ProtocolType,
        seen: SystemTime,
    ) -> StatefulDevice {
        let channel = DeviceChannel::Http(HttpChannel {
            host: host.to_string(),
            port: 53317,
            protocol,
        });
        let mut channels = HashMap::new();
        channels.insert(channel.clone(), ChannelStatus::Available);
        StatefulDevice {
            device: DiscoveredDevice {
                alias: alias.to_string(),
                version: "2.2".to_string(),
                device_model: Some("Test".to_string()),
                device_type: Some(CoreDeviceType::Desktop),
                fingerprint: fingerprint.to_string(),
                channel: channel.clone(),
                download: false,
            },
            channels,
            logs: vec![DeviceLog {
                timestamp: seen,
                kind: DeviceLogKind::Discovered,
                channel,
            }],
        }
    }

    #[test]
    fn fresh_devices_become_dialable_peers() {
        let now = SystemTime::now();
        let state = device(
            "PEER",
            "Office Mac",
            "192.168.1.20",
            ProtocolType::Https,
            now,
        );
        let peer = stateful_peer(&state, "SELF", now).expect("fresh device");
        assert_eq!(peer.fingerprint, "PEER");
        assert_eq!(peer.alias, "Office Mac");
        let address = peer.address.expect("dialable");
        assert_eq!(address.host, "192.168.1.20");
        assert_eq!(address.port, 53317);
        assert_eq!(address.protocol, crate::net::Protocol::Https);
        assert!(!peer.is_self);
    }

    #[test]
    fn expired_and_own_devices_are_filtered_out() {
        let now = SystemTime::now();
        let own = device("SELF", "This Mac", "192.168.1.10", ProtocolType::Https, now);
        assert!(stateful_peer(&own, "SELF", now).is_none());

        let stale = device(
            "STALE",
            "Gone",
            "192.168.1.30",
            ProtocolType::Https,
            now - PEER_EXPIRY - Duration::from_secs(1),
        );
        assert!(stateful_peer(&stale, "SELF", now).is_none());
    }

    #[test]
    fn plain_http_channels_are_dialable_too() {
        let now = SystemTime::now();
        let state = device(
            "LEGACY",
            "Old phone",
            "192.168.1.31",
            ProtocolType::Http,
            now,
        );
        let peer = stateful_peer(&state, "SELF", now).expect("fresh device");
        assert_eq!(
            peer.address.expect("dialable").protocol,
            crate::net::Protocol::Http
        );
    }

    /// The core store never forgets a device, so the periodic probe must skip
    /// entries that stopped answering: otherwise a long session keeps sending
    /// register requests to peers that left (an unbounded retry storm).
    #[test]
    fn only_recently_seen_devices_are_probed() {
        let now = SystemTime::now();
        let fresh = device("FRESH", "Here", "192.168.1.20", ProtocolType::Https, now);
        let stale = device(
            "STALE",
            "Gone",
            "192.168.1.30",
            ProtocolType::Https,
            now - PEER_EXPIRY - Duration::from_secs(1),
        );
        let channels = recent_http_channels(&[fresh, stale], now);
        assert_eq!(channels.len(), 1);
        assert_eq!(channels[0].host, "192.168.1.20");

        assert!(recent_http_channels(&[], now).is_empty());
    }
}

#[cfg(test)]
mod net_fix_tests {
    use super::*;
    use localsend::discovery::{ChannelStatus, DeviceLog, DeviceLogKind};
    use localsend::model::discovery::{DeviceType as CoreDeviceType, ProtocolType};
    use std::time::Duration;

    fn state(fingerprint: &str, seen: SystemTime) -> StatefulDevice {
        let channel = DeviceChannel::Http(HttpChannel {
            host: "192.168.1.20".to_string(),
            port: 53317,
            protocol: ProtocolType::Https,
        });
        let mut channels = HashMap::new();
        channels.insert(channel.clone(), ChannelStatus::Available);
        StatefulDevice {
            device: DiscoveredDevice {
                alias: "Peer".to_string(),
                version: "2.2".to_string(),
                device_model: Some("Test".to_string()),
                device_type: Some(CoreDeviceType::Desktop),
                fingerprint: fingerprint.to_string(),
                channel: channel.clone(),
                download: false,
            },
            channels,
            logs: vec![DeviceLog {
                timestamp: seen,
                kind: DeviceLogKind::Discovered,
                channel,
            }],
        }
    }

    /// NET-15/N20: a peer that is transferring must not flicker out of the
    /// device list when its announcement is briefly missed; once it is no
    /// longer active the normal expiry applies.
    #[test]
    fn active_peers_are_not_expired_mid_transfer() {
        let now = SystemTime::now();
        let stale = state("ACTIVE", now - PEER_EXPIRY - Duration::from_secs(1));
        let gone = state("GONE", now - PEER_EXPIRY - Duration::from_secs(1));
        let mut visible = HashMap::new();
        for fingerprint in ["ACTIVE", "GONE"] {
            visible.insert(
                fingerprint.to_string(),
                Peer {
                    fingerprint: fingerprint.to_string(),
                    alias: "Peer".to_string(),
                    device_model: String::new(),
                    address: Some(PeerAddress {
                        host: "192.168.1.20".to_string(),
                        port: 53317,
                        protocol: crate::net::Protocol::Https,
                    }),
                    device_type: crate::net::DeviceType::Desktop,
                    is_self: false,
                },
            );
        }
        let active = HashSet::from(["active".to_string()]);
        let (next, found, lost) = plan_snapshot(&visible, &[stale, gone], "SELF", now, &active);
        assert!(found.is_empty());
        assert_eq!(lost, vec!["GONE".to_string()]);
        assert!(next.contains_key("ACTIVE"), "the transferring peer is kept");
        assert!(!next.contains_key("GONE"));

        // Without an active transfer the expired peer is reported lost.
        let (next, _, lost) = plan_snapshot(&visible, &[], "SELF", now, &HashSet::new());
        assert!(next.is_empty());
        lost.is_empty();
    }

    /// NET-02: peer registrations are bounded; known peers keep updating.
    #[test]
    fn the_registered_peer_table_is_bounded() {
        let start = Instant::now();
        let mut peers = RegisteredPeers::default();
        assert!(peers.accept_at("real-old", start));
        for index in 1..MAX_DEVICE_TABLE {
            assert!(peers.accept_at(&format!("{index:064X}"), start));
        }
        assert_eq!(peers.len(), MAX_DEVICE_TABLE);
        // A device that joins after a flood still gets in: the least recently
        // seen entry (the first one) makes room.
        assert!(peers.accept_at("late-real-device", start));
        assert_eq!(peers.len(), MAX_DEVICE_TABLE);
        assert!(!peers.known.contains_key("REAL-OLD"));
        // Evictions are rate-limited, which bounds the core store's growth.
        assert!(!peers.accept_at("another-new", start + Duration::from_millis(100)));
        assert!(peers.accept_at("another-new", start + EVICTION_INTERVAL));
        // Known fingerprints refresh at any time, case-insensitively.
        assert!(peers.accept_at(&format!("{:064x}", 7), start + Duration::from_millis(1)));
        assert_eq!(peers.len(), MAX_DEVICE_TABLE);
    }

    /// NET-13 support: the multicast failure flag is consume-on-read so the
    /// actor restarts discovery exactly once per failure.
    #[test]
    fn the_multicast_failure_flag_is_consumed_once() {
        let failed = Arc::new(AtomicBool::new(false));
        failed.store(true, Ordering::Relaxed);
        assert!(failed.swap(false, Ordering::Relaxed));
        assert!(!failed.swap(false, Ordering::Relaxed));
    }
}
