//! The receiving side: this device's LocalSend HTTP server (protocol v2.2).
//!
//! [`ServerRuntime::start`] binds the server on IPv4 and IPv6 and exposes the
//! core's [`ServerEventV2`] stream; the actor owns the stream and answers the
//! events. Every event that carries a responder (`decision_tx`, `target_tx`)
//! must be answered exactly once — dropping the responder turns into HTTP 500
//! for the peer.

use anyhow::{Context, Result};
use localsend::http::dto_v2::RegisterDtoV2;
use localsend::http::server::v2::ServerEventV2;
use localsend::http::server::{
    self, PeerIp, ServerConfigV2, ServerHandle, TlsConfig, web::WebConfig,
};
use localsend::http::state::ClientInfo;

use crate::net::{DeviceType, Peer, PeerAddress, Protocol};

/// Capacity of the server-event channel. Prepare-upload/upload events are
/// awaited by the server, so the actor must keep draining this channel; the
/// capacity only has to absorb bursts of lossy `Register` events.
const EVENT_CHANNEL_CAPACITY: usize = 64;

/// Everything needed to start the LocalSend HTTP server.
#[derive(Clone)]
pub struct ServerParams {
    /// Port to listen on. `0` lets the OS choose (tests, simulators).
    pub port: u16,
    /// TLS identity, or `None` for plain HTTP (encryption off).
    pub tls: Option<TlsConfig>,
    /// The identity this device presents to peers.
    pub info: ClientInfo,
    /// Receive PIN fixed at start; changing it requires a server restart.
    pub pin: Option<String>,
}

/// A running LocalSend HTTP server.
pub struct ServerRuntime {
    handle: ServerHandle,
    stop: Option<tokio::sync::oneshot::Sender<()>>,
}

impl ServerRuntime {
    /// Binds the server and returns it together with its event stream. The
    /// actor owns the stream for the lifetime of the node.
    pub async fn start(
        params: ServerParams,
    ) -> Result<(Self, tokio::sync::mpsc::Receiver<ServerEventV2>)> {
        let (event_tx, events) = tokio::sync::mpsc::channel(EVENT_CHANNEL_CAPACITY);
        let (stop_tx, stop_rx) = tokio::sync::oneshot::channel();
        let handle = server::start_with_port(
            params.port,
            params.tls,
            params.info,
            None,
            Some(ServerConfigV2 {
                pin: params.pin,
                // The server verifies sender-provided hashes and only reports
                // success when they match, which is what the app relies on.
                verify_checksums: true,
                event_tx,
            }),
            WebConfig::default(),
            stop_rx,
        )
        .await
        .context("Cannot listen for LocalSend transfers; another app may be using this port")?;
        Ok((
            Self {
                handle,
                stop: Some(stop_tx),
            },
            events,
        ))
    }

    /// The port the listeners are actually bound to (matters with port 0).
    pub fn port(&self) -> u16 {
        self.handle.port()
    }

    /// The concrete addresses this server can be dialed at (every non-loopback
    /// interface address, restricted to the bound families). Used to fill the
    /// self device's address so the Receive view shows a real target instead
    /// of `None` (MP-27).
    pub fn local_addresses(&self) -> Vec<std::net::SocketAddr> {
        self.handle.local_addresses()
    }

    /// Frees the single v2 session slot after the user cancelled a receive.
    ///
    /// Uploads already in flight still run to completion and no `SessionEnd`
    /// is emitted for a cancellation initiated here.
    pub async fn cancel_session(&self, session_id: &str) -> bool {
        self.handle.cancel_v2_session(session_id).await
    }

    /// Stops the listeners and waits until the port can be bound again.
    pub async fn stop(mut self) {
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }
        self.handle.wait_stopped().await;
    }
}

/// Builds the app-facing [`Peer`] for a peer that registered with this server.
///
/// The server only emits `Register` on TLS when the payload fingerprint matches
/// the client certificate, so `info.fingerprint` is trustworthy here.
pub fn peer_from_register(ip: &PeerIp, info: &RegisterDtoV2) -> Peer {
    Peer {
        fingerprint: info.fingerprint.clone(),
        alias: info.alias.clone(),
        device_model: info.device_model.clone().unwrap_or_default(),
        address: Some(PeerAddress {
            host: ip.to_string(),
            port: info.port,
            protocol: crate::net::protocol_from_core(info.protocol),
        }),
        device_type: crate::net::device_type_from_core(info.device_type.clone()),
        is_self: false,
    }
}

/// Builds the app-facing [`Peer`] for a sender of a prepare-upload request.
pub fn peer_from_prepare_upload(
    ip: &PeerIp,
    info: &RegisterDtoV2,
    cert_fingerprint: Option<&str>,
) -> Peer {
    // The certificate fingerprint cannot be spoofed; the payload value only
    // exists as a fallback for encryption-off mode.
    let fingerprint = cert_fingerprint
        .map(str::to_owned)
        .unwrap_or_else(|| info.fingerprint.clone());
    Peer {
        fingerprint,
        alias: info.alias.clone(),
        device_model: info.device_model.clone().unwrap_or_default(),
        address: Some(PeerAddress {
            host: ip.to_string(),
            port: info.port,
            protocol: crate::net::protocol_from_core(info.protocol),
        }),
        device_type: crate::net::device_type_from_core(info.device_type.clone()),
        is_self: false,
    }
}

/// Builds the `RegisterDtoV2` this device presents to peers.
pub fn register_dto(
    alias: &str,
    device_type: DeviceType,
    device_model: impl Into<String>,
    fingerprint: &str,
    port: u16,
    protocol: Protocol,
) -> RegisterDtoV2 {
    RegisterDtoV2 {
        alias: alias.to_string(),
        version: crate::net::PROTOCOL_VERSION.to_string(),
        device_model: Some(device_model.into()),
        device_type: crate::net::device_type_to_core(device_type),
        fingerprint: fingerprint.to_string(),
        port,
        protocol: crate::net::protocol_to_core(protocol),
        // The download API (browser "share via link") is not implemented.
        download: false,
    }
}

/// A human-readable device model, used in register/announce payloads.
pub fn device_model() -> &'static str {
    if cfg!(target_os = "macos") {
        "macOS"
    } else if cfg!(target_os = "windows") {
        "Windows"
    } else if cfg!(target_os = "linux") {
        "Linux"
    } else {
        "Desktop"
    }
}

/// The [`ClientInfo`] this device presents.
pub fn client_info(alias: &str, device_type: DeviceType, token: &str) -> ClientInfo {
    ClientInfo {
        alias: alias.to_string(),
        version: crate::net::PROTOCOL_VERSION.to_string(),
        device_model: Some(device_model().to_string()),
        device_type: crate::net::device_type_to_core(device_type),
        token: token.to_string(),
    }
}
