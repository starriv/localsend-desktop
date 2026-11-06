//! The LocalSend protocol layer.
//!
//! Everything that speaks to the network lives here: discovery, the HTTP
//! server that receives, the client that sends, and the translation between
//! the `localsend` core's types and the app's own [`Peer`] / [`TransferEvent`]
//! vocabulary. Views never touch `localsend` types directly, so the core can
//! be updated (or swapped) without changing the UI.

pub mod actor;
pub mod discovery;
pub mod events;
pub mod identity;
pub mod naming;
pub mod receive;
pub mod send;
pub mod server;

pub use actor::{Handle, Node, NodeConfig};

pub use events::{
    DeviceType, Direction, OfferedFile, Peer, PeerAddress, Protocol, RefuseReason, TransferEvent,
};

/// LocalSend's well-known port (protocol v2 default).
pub const DEFAULT_PORT: u16 = 53317;

/// Wall clock in Unix milliseconds, for transfer timestamps.
pub fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or(0)
}

/// The LocalSend protocol version this client speaks.
pub const PROTOCOL_VERSION: &str = localsend::model::discovery::PROTOCOL_VERSION_V2;

/// Report one event to the UI. The channel is unbounded, so this never waits
/// and never drops; a closed channel (the UI is gone) is ignored on purpose.
///
/// Unbounded is safe here because every producer is rate-limited: progress is
/// coalesced to <= 20 events/s per transfer (and dropped below 50 ms), the
/// discovery snapshot diff runs at 1 Hz, and requests are bounded by the
/// protocol (one v2 session, `Register` bursts). A stalled UI therefore grows
/// the queue with a small constant per transfer, not without bound. Terminal
/// events always carry the final byte count via a preceding `Progress`.
pub(crate) fn emit(events: &async_channel::Sender<TransferEvent>, event: TransferEvent) {
    let _ = events.try_send(event);
}

/// Translate a core device type into the app's own.
pub(crate) fn device_type_from_core(
    device_type: Option<localsend::model::discovery::DeviceType>,
) -> DeviceType {
    use localsend::model::discovery::DeviceType as Core;
    match device_type {
        Some(Core::Mobile) => DeviceType::Mobile,
        Some(Core::Desktop) => DeviceType::Desktop,
        Some(Core::Web) => DeviceType::Web,
        Some(Core::Headless) => DeviceType::Headless,
        Some(Core::Server) => DeviceType::Server,
        None => DeviceType::Unknown,
    }
}

/// Translate the app's device type into the core type used on the wire.
pub(crate) fn device_type_to_core(
    device_type: DeviceType,
) -> Option<localsend::model::discovery::DeviceType> {
    use localsend::model::discovery::DeviceType as Core;
    match device_type {
        DeviceType::Mobile => Some(Core::Mobile),
        DeviceType::Desktop => Some(Core::Desktop),
        DeviceType::Web => Some(Core::Web),
        DeviceType::Headless => Some(Core::Headless),
        DeviceType::Server => Some(Core::Server),
        DeviceType::Unknown => None,
    }
}

/// Translate the app's protocol enum into the core's.
pub(crate) fn protocol_to_core(protocol: Protocol) -> localsend::model::discovery::ProtocolType {
    match protocol {
        Protocol::Https => localsend::model::discovery::ProtocolType::Https,
        Protocol::Http => localsend::model::discovery::ProtocolType::Http,
    }
}

/// Translate the core's protocol enum into the app's.
pub(crate) fn protocol_from_core(protocol: localsend::model::discovery::ProtocolType) -> Protocol {
    match protocol {
        localsend::model::discovery::ProtocolType::Https => Protocol::Https,
        localsend::model::discovery::ProtocolType::Http => Protocol::Http,
    }
}
