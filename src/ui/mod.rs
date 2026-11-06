//! The design system: tokens, formatting, icons and controls the screens are
//! built from. Screens import from here (`crate::ui::{...}`), never from the
//! framework's style layer directly, so light/dark and motion stay coherent.

pub mod controls;
pub mod format;
pub mod icons;
pub mod theme;

pub use controls::{
    Badge, BadgeTone, Button, ButtonKind, DeviceTile, FileRow, IconButton, PeerRow, PeerRowKind,
    ProgressBar, ProgressRing, Segmented, Spinner, Toggle, badge, button, device_tile, file_row,
    icon_button, peer_row, progress_bar, progress_ring, segmented, spinner, toggle,
};
pub use format::{DayLabel, bytes, clock, eta, speed};
pub use icons::{Icon, IconName, icon};
pub use theme::{
    Appearance, card_shadow, colors, control, floating_shadow, focus_ring, motion, motion_ms,
    radius, space, text_size, theme, tile_radius,
};
