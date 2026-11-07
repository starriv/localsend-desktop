//! Application state shown by the views (placeholder module; the network
//! layer and the session fill it in).

pub mod session;
pub mod state;
pub mod updater;
pub mod verify;

pub use crate::net::Direction;
pub use session::{PinRequest, Session, Toast};
pub use state::{
    AppState, ComposerItem, IncomingRequest, Settings, Transfer, TransferStatus, View,
};
pub use updater::UpdateStatus;

/// Whether the system lets LocalSend show notifications.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum NotificationAccess {
    /// Not known yet, or not asked yet (the system asks on first use).
    #[default]
    Unknown,
    /// Allowed (also provisional or ephemeral delivery).
    Allowed,
    /// Turned off by the user: nothing shows until it is turned on again in
    /// the system's settings.
    Denied,
}
