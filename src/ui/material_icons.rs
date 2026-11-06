//! The bundled Material Icons font, for LocalSend's fingerprint icon view.
//!
//! The official app draws the verification alphabet with Flutter's built-in
//! Material icons (`Icons.*` in `app/lib/util/fingerprint_alphabet.dart`). The
//! same font ships here (Apache-2.0, taken from the Flutter SDK) so the 16
//! icons can be compared visually with the other device; the codepoints live
//! in `model::verify::ICON_ALPHABET`.

use nsh_core::element::FontAsset;
use nsh_core::prelude::*;

use super::colors;

/// `assets/MaterialIcons-Regular.otf`, registered from the crate's assets
/// before the first frame (see `native` in `lib.rs`).
pub const MATERIAL_ICONS: FontAsset = asset!("MaterialIcons-Regular.otf");

/// One Material glyph, drawn from the bundled font at `codepoint`.
pub fn material_icon(codepoint: u32, size: f32) -> impl IntoElement {
    let glyph = char::from_u32(codepoint)
        .map(|glyph| SharedString::from(glyph.to_string()))
        .unwrap_or_default();
    text(glyph)
        .font_family(MATERIAL_ICONS.family)
        .text_size(px(size))
        .text_color(colors::TEXT)
}
