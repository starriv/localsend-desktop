//! Design tokens: colors, spacing, radii, type and motion. Screens read these
//! instead of raw literals so light and dark appearance stay consistent.
//!
//! The `colors` constants mirror `theme.json`: `theme()` is what the platform
//! switches natively, and the two files change together. Structural tokens
//! (`background`, `sidebar`, `border`, …) are read from `theme()` where the
//! shadcn role exists; the palette roles below are the design system's own
//! vocabulary so component code reads as design, not as color plumbing.

pub use nsh_base::theme;

use nsh_core::color::rgba;
use nsh_core::element::{AdaptiveColor, BoxShadow, StyleRefinement};
use nsh_core::geometry::px;
use nsh_core::prelude::{Styled, reduce_motion};

/// `0xRRGGBB` for each appearance, in pairs.
const fn tone(light: u32, dark: u32) -> AdaptiveColor {
    AdaptiveColor::rgb(light, dark)
}

/// A translucent pair, written as CSS's `rgba(r, g, b, a)`.
const fn tint(light: u32, dark: u32) -> AdaptiveColor {
    AdaptiveColor {
        light: rgba(light).to_hsla(),
        dark: rgba(dark).to_hsla(),
    }
}

/// The design system's colors, one [`AdaptiveColor`] per role.
///
/// Every value has the same light/dark counterpart in `theme.json`; change
/// both together (dark values are the second argument).
pub mod colors {
    use super::{tint, tone};
    use nsh_core::element::AdaptiveColor;

    /// The page behind the cards.
    pub const BACKGROUND: AdaptiveColor = tone(0xf4f4f6, 0x191a1d);
    /// The sidebar, one step off the page.
    pub const SIDEBAR: AdaptiveColor = tone(0xebebee, 0x141517);
    /// A card, sheet or popover.
    pub const SURFACE: AdaptiveColor = tone(0xffffff, 0x222327);
    /// A surface sunk into a card (grouped rows, fields).
    pub const INSET: AdaptiveColor = tone(0xf7f7f9, 0x1c1d20);
    /// A control raised off a card (buttons, tracks).
    pub const RAISED: AdaptiveColor = tone(0xfbfbfc, 0x26272b);
    /// A field's fill.
    pub const FIELD: AdaptiveColor = tone(0xffffff, 0x1f2023);
    /// The sidebar's selected row fill.
    pub const SIDEBAR_ACCENT: AdaptiveColor = tone(0xffffff, 0x2b2c32);
    /// A hairline between rows inside a surface.
    pub const LINE: AdaptiveColor = tone(0xe8e8ec, 0x2c2d32);
    /// The edge of cards and controls.
    pub const BORDER: AdaptiveColor = tone(0xdcdce1, 0x33343a);
    /// The fill under the pointer.
    pub const HOVER: AdaptiveColor = tone(0xf2f2f5, 0x28292e);
    /// A track behind progress, switches and sliders.
    pub const TRACK: AdaptiveColor = tone(0xe2e2e7, 0x33343a);
    /// A switch's knob.
    pub const KNOB: AdaptiveColor = tone(0xffffff, 0xf2f2f4);
    /// Primary text.
    pub const TEXT: AdaptiveColor = tone(0x1d1d1f, 0xf2f2f4);
    /// Secondary text.
    ///
    /// Light `#5f5f65` is 5.3:1 on the sidebar (the darkest light surface)
    /// and 5.8:1 on the page, so it stays above AA even for small text;
    /// `#6a6a70` was 4.52:1 there and is now the [`FAINT`] step (the two
    /// are deliberately one step apart instead of nearly equal).
    pub const MUTED: AdaptiveColor = tone(0x5f5f65, 0x9a9aa2);
    /// Captions, counts, quiet chevrons.
    ///
    /// Dark is `#8f8f97`: 4.6–5.7:1 on the dark surfaces. Light `#68686e` is
    /// 4.65:1 on the sidebar, 5.0:1 on the page and 5.5:1 on a card, so 11 pt
    /// captions clear 4.5:1 everywhere (was `#75757b`, 3.85–4.28:1 on the
    /// tinted surfaces). Icons and chevrons keep the same tone: it is still
    /// far above the 3:1 non-text threshold.
    pub const FAINT: AdaptiveColor = tone(0x68686e, 0x8f8f97);
    /// The teal accent: text, icons, the primary button.
    ///
    /// Light `#0c7d79` (was `#0e8f8b`) so a white button label is 4.97:1
    /// (AA) instead of 3.95:1, and accent text on the page is 4.52:1.
    pub const ACCENT: AdaptiveColor = tone(0x0c7d79, 0x35cfc6);
    /// The accent under the pointer.
    pub const ACCENT_HOVER: AdaptiveColor = tone(0x0a6b68, 0x46dad2);
    /// The accent while pressed.
    pub const ACCENT_ACTIVE: AdaptiveColor = tone(0x085a57, 0x2bb8b0);
    /// Text and glyphs on the accent.
    pub const ON_ACCENT: AdaptiveColor = tone(0xffffff, 0x062e2c);
    /// Text on a wash of the accent ([`ACCENT_TINT`]): darker than the accent
    /// itself, so an accent badge's 11 pt label clears 4.5:1 (5.4:1 on a
    /// card, 4.9:1 on the page; the accent itself is 4.3/3.9).
    pub const ACCENT_FOREGROUND: AdaptiveColor = tone(0x0a6d6a, 0x8ee6df);
    /// A wash of the accent behind text.
    pub const ACCENT_TINT: AdaptiveColor = tint(0x0c7d791a, 0x35cfc624);
    /// Destructive actions.
    ///
    /// Light `#bd3037` (was `#d7373f`): 5.8:1 white on it and 4.9:1 as a
    /// danger badge's label on its wash (was 4.0:1). Dark raises `#ff6363` to
    /// `#ff7373` (4.8:1 on a danger wash, was 4.4:1).
    pub const DANGER: AdaptiveColor = tone(0xbd3037, 0xff7373);
    /// Destructive actions under the pointer.
    pub const DANGER_HOVER: AdaptiveColor = tone(0xa82a31, 0xff8f8f);
    /// Text on a filled danger button.
    pub const ON_DANGER: AdaptiveColor = tone(0xffffff, 0x2a0a0c);
    /// A wash of danger behind text.
    pub const DANGER_TINT: AdaptiveColor = tint(0xbd30371a, 0xff737324);
    /// Successful, completed, online.
    pub const SUCCESS: AdaptiveColor = tone(0x17743f, 0x3fd68f);
    /// A wash of success behind text.
    pub const SUCCESS_TINT: AdaptiveColor = tint(0x17743f1a, 0x3fd68f24);
    /// Something the user should notice.
    pub const WARNING: AdaptiveColor = tone(0x945800, 0xffb547);
    /// A wash of warning behind text.
    pub const WARNING_TINT: AdaptiveColor = tint(0x9458001c, 0xffb54724);
    /// Information.
    pub const INFO: AdaptiveColor = tone(0x3d63d1, 0x7ea8ff);
    /// A wash of information behind text.
    pub const INFO_TINT: AdaptiveColor = tint(0x3d63d11a, 0x7ea8ff24);
    /// The focus ring around a control (3 pt).
    ///
    /// Nearly opaque: a 38 % accent glow is 1.57:1 against the light card,
    /// below the 3:1 a focus indicator needs ([`super::focus_ring`]). 95 %
    /// light / 80 % dark gives 3.1–3.6:1 (light) and 5.7–6.2:1 (dark) on
    /// every surface a control sits on.
    pub const RING: AdaptiveColor = tint(0x0c7d79f2, 0x35cfc6cc);
    /// The dimming layer under a sheet.
    pub const SCRIM: AdaptiveColor = tint(0x1415184d, 0x00000073);
    /// The brand mark.
    pub const MARK: AdaptiveColor = tone(0x0c7d79, 0x35cfc6);
}

/// Spacing steps (points), a 4 pt grid.
pub mod space {
    /// 4.
    pub const XS: f32 = 4.0;
    /// 8.
    pub const SM: f32 = 8.0;
    /// 12.
    pub const MD: f32 = 12.0;
    /// 16.
    pub const LG: f32 = 16.0;
    /// 24.
    pub const XL: f32 = 24.0;
    /// The screen gutter.
    pub const GUTTER: f32 = 28.0;
}

/// Corner radii, concentric with the surfaces they sit in.
pub mod radius {
    /// A control (button, field, row).
    pub const CONTROL: f32 = 9.0;
    /// A card.
    pub const CARD: f32 = 14.0;
    /// A sheet.
    pub const SHEET: f32 = 18.0;
    /// A capsule or a circle.
    pub const PILL: f32 = 999.0;
    /// A small chip inside a row.
    pub const CHIP: f32 = 7.0;
    /// A device tile.
    pub const TILE: f32 = 10.0;
}

/// Type scale (points). System font throughout; body text is 13.
pub mod text_size {
    /// 11 — captions, counts.
    pub const CAPTION: f32 = 11.0;
    /// 12 — secondary text.
    pub const SMALL: f32 = 12.0;
    /// 13 — body.
    pub const BODY: f32 = 13.0;
    /// 15 — emphasis.
    pub const EMPHASIS: f32 = 15.0;
    /// 17 — screen titles.
    pub const TITLE: f32 = 17.0;
    /// 22 — display.
    pub const DISPLAY: f32 = 22.0;
}

/// The fixed sizes of the shell and its controls.
pub mod control {
    /// A regular control: 32 pt.
    pub const HEIGHT: f32 = 32.0;
    /// A small control: 26 pt.
    pub const SMALL_HEIGHT: f32 = 26.0;
    /// An icon-only control: 28 pt square.
    pub const ICON: f32 = 28.0;
    /// The custom title bar's height.
    pub const TITLE_BAR: f32 = 50.0;
    /// The sidebar's width.
    pub const SIDEBAR: f32 = 252.0;
    /// The incoming-request sheet's width.
    pub const SHEET_INCOMING: f32 = 448.0;
    /// The verification sheet's width.
    pub const SHEET_VERIFY: f32 = 448.0;
    /// The PIN sheet's width.
    pub const SHEET_PIN: f32 = 380.0;
    /// The settings sheet's width (and the detail sheet's).
    pub const SHEET_WIDE: f32 = 560.0;
    /// A device chooser popover's width.
    pub const POPOVER: f32 = 280.0;
    /// The settings alias field's width.
    pub const FIELD_ALIAS: f32 = 220.0;
    /// The settings PIN field's width.
    pub const FIELD_PIN: f32 = 160.0;
    /// The widest a toast grows before its text wraps.
    pub const TOAST_MAX_W: f32 = 420.0;
    /// A device row in the sidebar.
    pub const DEVICE_ROW: f32 = 34.0;
    /// A device row in the chooser popover.
    pub const DEVICE_POPOVER_ROW: f32 = 36.0;
    /// A file row inside a sheet (30 pt).
    pub const FILE_ROW: f32 = 30.0;
    /// A transfer row in the history and recent lists.
    pub const TRANSFER_ROW: f32 = 56.0;
    /// The bottom transfer card.
    pub const TRANSFER_CARD: f32 = 64.0;
    /// The Receive radar's outer circle.
    pub const RADAR: f32 = 148.0;
    /// The Receive radar's inner ring.
    pub const RADAR_RING: f32 = 112.0;
    /// The device tile at the radar's center.
    pub const RADAR_TILE: f32 = 64.0;
}

/// The tile radius of a device tile drawn at `size`.
///
/// Device tiles are authored at 44 pt ([`radius::TILE`] is the 44 pt tile's
/// radius); smaller tiles in rows and the chip scale the radius with them so
/// the corner reads the same. Named so the scaling is a documented rule
/// rather than an implicit ratio at each call site.
pub fn tile_radius(size: f32) -> f32 {
    radius::TILE * (size / 44.0)
}

/// Motion: short, explaining a change. Durations are milliseconds.
pub mod motion {
    use nsh_core::element::Spring;

    /// Hover and focus: 140 ms ease-out.
    pub const HOVER_MS: f32 = 140.0;
    /// A selection moving: 160 ms ease-out.
    pub const SELECT_MS: f32 = 160.0;
    /// A sheet coming in: 220 ms.
    pub const SHEET_IN_MS: f32 = 220.0;
    /// The scrim under a sheet.
    pub const SCRIM_MS: f32 = 160.0;
    /// A progress value moving to its target: 250 ms linear.
    pub const PROGRESS_MS: f32 = 250.0;
    /// A toast appearing.
    pub const TOAST_MS: f32 = 200.0;

    /// The spring of a knob or an indicator settling.
    pub const SETTLE: Spring = Spring {
        stiffness: 320.0,
        damping: 30.0,
        mass: 1.0,
        velocity: None,
    };
}

/// The one shadow under a card: soft, tight, and effectively invisible in
/// dark appearance (cards there are separated by their fill and border).
///
/// ```ignore
/// view().shadow(card_shadow()).dark(|s| s.shadow_none())
/// ```
pub fn card_shadow() -> [BoxShadow; 1] {
    [BoxShadow {
        offset_x: px(0.0),
        offset_y: px(1.0),
        blur: px(3.0),
        spread: px(0.0),
        color: rgba(0x1415180f).to_packed(),
        inset: false,
    }]
}

/// A floating surface's shadow (a sheet, a popover): deeper, for stacking.
pub fn floating_shadow() -> [BoxShadow; 2] {
    [
        BoxShadow {
            offset_x: px(0.0),
            offset_y: px(4.0),
            blur: px(12.0),
            spread: px(0.0),
            color: rgba(0x14151814).to_packed(),
            inset: false,
        },
        BoxShadow {
            offset_x: px(0.0),
            offset_y: px(18.0),
            blur: px(48.0),
            spread: px(-12.0),
            color: rgba(0x14151838).to_packed(),
            inset: false,
        },
    ]
}

/// A control's focus ring: a 3 pt accent glow, drawn outside the control
/// (DESIGN.md's visual language, "Focus | 3 pt `ring` glow").
pub fn focus_ring(style: StyleRefinement) -> StyleRefinement {
    style.ring(px(3.0)).ring_color(colors::RING)
}

/// A transition duration in milliseconds that collapses to zero when the
/// user asked for reduced motion.
///
/// Transitions do not follow [`reduce_motion`] themselves (only motions do),
/// so every `.transition_*()` in the design system
/// takes its `.duration(..)` from here: the binding re-reads the system
/// setting, and with it on, a hover or selection change jumps instead of
/// animating.
pub fn motion_ms(ms: f32) -> impl Fn() -> f32 {
    move || if reduce_motion() { 0.0 } else { ms }
}

/// The app's appearance: follow the system or force one.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Appearance {
    /// Follow the system.
    #[default]
    System,
    /// Always light.
    Light,
    /// Always dark.
    Dark,
}

impl Appearance {
    /// The i18n key of its label.
    pub fn label_key(self) -> &'static str {
        match self {
            Appearance::System => "appearance.system",
            Appearance::Light => "appearance.light",
            Appearance::Dark => "appearance.dark",
        }
    }

    /// Its index in [`Appearance::ALL`], for a segmented control.
    pub fn index(self) -> usize {
        match self {
            Appearance::System => 0,
            Appearance::Light => 1,
            Appearance::Dark => 2,
        }
    }

    /// Every appearance, in segmented-control order.
    pub const ALL: [Appearance; 3] = [Appearance::System, Appearance::Light, Appearance::Dark];
}

impl From<Appearance> for nsh_core::platform::Appearance {
    fn from(appearance: Appearance) -> Self {
        match appearance {
            Appearance::System => nsh_core::platform::Appearance::System,
            Appearance::Light => nsh_core::platform::Appearance::Light,
            Appearance::Dark => nsh_core::platform::Appearance::Dark,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every color the design system reads has the matching token in
    /// `theme.json`, and every radius matches its `radius-*` token. Both
    /// files are the palette: changing one without the other would drift a
    /// host theme, and the constants would silently win.
    #[test]
    fn palette_matches_theme_json() {
        let theme = nsh_base::Theme::from_json(include_str!("../../theme.json"))
            .expect("theme.json parses");
        let pairs: [(&str, AdaptiveColor); 34] = [
            ("background", colors::BACKGROUND),
            ("sidebar", colors::SIDEBAR),
            ("card", colors::SURFACE),
            ("inset", colors::INSET),
            ("raised", colors::RAISED),
            ("field", colors::FIELD),
            ("sidebar-accent", colors::SIDEBAR_ACCENT),
            ("line", colors::LINE),
            ("border", colors::BORDER),
            ("hover", colors::HOVER),
            ("track", colors::TRACK),
            ("knob", colors::KNOB),
            ("foreground", colors::TEXT),
            ("muted-foreground", colors::MUTED),
            ("faint", colors::FAINT),
            ("primary", colors::ACCENT),
            ("accent-hover", colors::ACCENT_HOVER),
            ("accent-active", colors::ACCENT_ACTIVE),
            ("on-accent", colors::ON_ACCENT),
            ("accent-foreground", colors::ACCENT_FOREGROUND),
            ("brand-tint", colors::ACCENT_TINT),
            ("destructive", colors::DANGER),
            ("danger-hover", colors::DANGER_HOVER),
            ("on-danger", colors::ON_DANGER),
            ("destructive-tint", colors::DANGER_TINT),
            ("success", colors::SUCCESS),
            ("success-tint", colors::SUCCESS_TINT),
            ("warning", colors::WARNING),
            ("warning-tint", colors::WARNING_TINT),
            ("info", colors::INFO),
            ("info-tint", colors::INFO_TINT),
            ("ring", colors::RING),
            ("scrim", colors::SCRIM),
            ("mark", colors::MARK),
        ];
        for (token, expected) in pairs {
            let actual = theme
                .color(token)
                .unwrap_or_else(|| panic!("theme.json has no `{token}` token"));
            assert_close(token, actual, expected);
        }
        for (token, expected) in [
            ("radius-control", radius::CONTROL),
            ("radius-card", radius::CARD),
            ("radius-sheet", radius::SHEET),
            ("radius-chip", radius::CHIP),
            ("radius-tile", radius::TILE),
        ] {
            assert_eq!(theme.number(token), Some(expected), "theme.json `{token}`");
        }
    }

    /// Compares two appearances channel by channel. Alpha is looser: a
    /// constant writes an 8-bit hex alpha (`0x1a`), the theme file a decimal
    /// one (`0.10`).
    fn assert_close(token: &str, actual: AdaptiveColor, expected: AdaptiveColor) {
        for (appearance, found, wanted) in [
            ("light", actual.light, expected.light),
            ("dark", actual.dark, expected.dark),
        ] {
            let close = (found.h - wanted.h).abs() < 1.0e-4
                && (found.s - wanted.s).abs() < 1.0e-4
                && (found.l - wanted.l).abs() < 1.0e-4
                && (found.a - wanted.a).abs() < 1.0e-2;
            assert!(
                close,
                "`{token}` {appearance}: theme.json {found:?} does not match the constant {wanted:?}"
            );
        }
    }
}
