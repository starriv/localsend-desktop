//! Plain data the network layer reports, independent of the `localsend` core
//! types so the UI survives a core upgrade.

use std::path::PathBuf;

/// A device on the local network that speaks the LocalSend protocol.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Peer {
    /// SHA-256 of the device certificate, uppercase hex. The stable identity.
    pub fingerprint: String,
    pub alias: String,
    pub device_model: String,
    /// `Some` while the device answers; `None` for a remembered peer.
    pub address: Option<PeerAddress>,
    pub device_type: DeviceType,
    /// Set for the peer that is this device itself (matched by fingerprint).
    pub is_self: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct PeerAddress {
    pub host: String,
    pub port: u16,
    pub protocol: Protocol,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum Protocol {
    #[default]
    Https,
    Http,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DeviceType {
    Desktop,
    Mobile,
    Web,
    Headless,
    Server,
    #[default]
    Unknown,
}

/// One file offered by a peer, or staged to send.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct OfferedFile {
    pub name: String,
    pub size: u64,
    pub mime: String,
    /// Where a received file was written (set when the transfer finishes).
    pub saved_to: Option<PathBuf>,
}

/// What the network layer tells the app.
#[derive(Clone, Debug)]
pub enum TransferEvent {
    /// Discovery started or stopped answering / scanning.
    DiscoveryActive(bool),
    PeerFound(Peer),
    PeerLost(String),
    IncomingRequest {
        id: String,
        peer: Peer,
        files: Vec<OfferedFile>,
        total: u64,
    },
    Started {
        id: String,
        peer: Peer,
        direction: Direction,
        files: Vec<OfferedFile>,
        total: u64,
    },
    Accepted {
        id: String,
    },
    Progress {
        id: String,
        transferred: u64,
        /// `None` when the peer did not report a total.
        total: Option<u64>,
    },
    FileDone {
        id: String,
        file: OfferedFile,
    },
    TextReceived {
        id: String,
        text: String,
    },
    /// The peer requires a PIN before it will accept the upload.
    PinRequired {
        id: String,
        peer: Peer,
    },
    Completed {
        id: String,
        files: Vec<OfferedFile>,
    },
    Cancelled {
        id: String,
    },
    Failed {
        id: String,
        error: String,
    },
    NetError(String),
    /// An incoming request was declined before the UI saw it. Peer-triggered,
    /// so it is *not* a network fault: the UI shows the reason, not "the
    /// network was lost".
    RequestRefused(RefuseReason),
}

/// Why an incoming request was declined without asking the user.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RefuseReason {
    /// The offer is malformed (file count, ids, names or total size).
    Invalid,
    /// The offer does not fit into the free space of the receive volume.
    NoSpace,
    /// Encryption is on but the sender connected without a certificate.
    Unencrypted,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum Direction {
    Sending,
    Receiving,
}
