//! The app shell and its screens.
//!
//! `shell` is the only screen the root component mounts: a sidebar of
//! destinations and nearby devices, the content pane, and the overlays
//! (incoming request, transfers, settings, PIN prompt).

pub mod history;
pub mod incoming;
pub mod pin;
pub mod receive;
pub mod send;
pub mod settings;
pub mod shell;
pub mod toast;
pub mod transfer;

pub use shell::shell;
