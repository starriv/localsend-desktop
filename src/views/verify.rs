//! The verification sheet: LocalSend's "Verify" page.
//!
//! Both devices show the same combined fingerprint — their two certificate
//! fingerprints sorted and concatenated — either as the 16 Material icons the
//! official clients render or as the text itself, so the two screens can be
//! compared. The shell owns the sheet chrome (scrim, card, close button): this
//! module returns only the content, and nothing while no peer is being
//! verified.

use crate::model::Session;
use crate::model::verify::{combined_fingerprint, fingerprint_icons};
use crate::net::Peer;
use crate::ui::material_icons::material_icon;
use crate::ui::{ButtonKind, button, colors, device_tile, radius, segmented, space, text_size};
use nsh_core::prelude::*;

/// Which representation the sheet shows, as on the official Verify page.
#[derive(Clone, Copy, Default, PartialEq, Eq)]
enum Mode {
    #[default]
    Icons,
    Text,
}

/// Survives closing the sheet: reopening keeps the last chosen view.
fn mode() -> Signal<Mode> {
    global::<Mode>()
}

/// How many icons a row holds (the official page uses four).
const ICONS_PER_ROW: usize = 4;

pub fn verify_sheet(session: Session) -> impl IntoElement {
    let check = session.clone();
    let build = session.clone();
    when(
        move || check.verify_peer.get().is_some(),
        move || {
            let peer = untracked(|| build.verify_peer.get());
            match peer {
                Some(peer) => content(build.clone(), peer).into_element(),
                None => view().into_element(),
            }
        },
    )
}

fn content(session: Session, peer: Peer) -> impl IntoElement {
    let own = untracked(|| session.identity.get());
    let combined = own
        .as_ref()
        .map(|me| combined_fingerprint(&me.fingerprint, &peer.fingerprint));
    let alias = peer.alias.clone();
    let tile = peer.clone();
    let title = session.clone();
    let question = session.clone();
    let hint = session.clone();
    let unavailable = session.clone();
    let close = session.clone();

    let close_anchor = element_ref();
    effect(move || close_anchor.focus());
    let mode_state = mode();
    let options = vec![session.text("verify.icons"), session.text("verify.text")];
    let selected = computed(move || if mode_state.get() == Mode::Text { 1 } else { 0 });
    let toggle = mode_state;

    view()
        .test_id("verify-sheet")
        .w_full()
        .flex_col()
        // MD rather than LG: keeps the card inside `sheet_max_height` at the
        // minimum window size, close button included.
        .gap(px(space::MD))
        .child(
            text(move || title.text("verify.title"))
                .text_size(px(text_size::TITLE))
                .font_weight(FontWeight::SEMIBOLD),
        )
        .child(
            view()
                .flex_row()
                .items_center()
                .gap(px(space::MD))
                .child(device_tile(move || tile.clone()).size(36.0))
                .child(
                    text(alias)
                        .flex_1()
                        .number_of_lines(1)
                        .truncate()
                        .text_size(px(text_size::BODY))
                        .font_weight(FontWeight::MEDIUM),
                ),
        )
        .child(
            view().w_full().flex_row().justify_center().child(
                view()
                    .w(px(200.0))
                    .child(
                        segmented(options)
                            .selected(selected)
                            .on_change(move |index| {
                                toggle.set(if index == 0 { Mode::Icons } else { Mode::Text });
                            }),
                    ),
            ),
        )
        .child(dynamic({
            let unavailable = unavailable.clone();
            move || {
                let Some(combined) = combined.as_deref() else {
                    let label = unavailable.clone();
                    return text(move || label.text("verify.unavailable"))
                        .test_id("verify-unavailable")
                        .text_size(px(text_size::SMALL))
                        .text_color(colors::MUTED)
                        .into_element();
                };
                match mode_state.get() {
                    Mode::Icons => icons(combined),
                    Mode::Text => text_fingerprint(combined),
                }
            }
        }))
        .child(
            view().w_full().flex_row().justify_center().child(
                text(move || question.text("verify.question"))
                    .text_size(px(text_size::BODY))
                    .text_color(colors::TEXT),
            ),
        )
        .child(
            view().w_full().flex_row().justify_center().child(
                text(move || hint.text("verify.hint"))
                    .text_size(px(text_size::CAPTION))
                    .text_color(colors::FAINT),
            ),
        )
        .child(
            view().flex_row().items_center().justify_end().child(
                button(session.text("common.close"))
                    .kind(ButtonKind::Secondary)
                    .element_ref(close_anchor)
                    .on_press(move || close.close_verify()),
            ),
        )
}

/// The 16 icons in a centred 4×4 grid, as the official page draws them.
fn icons(combined: &str) -> AnyElement {
    let glyphs = fingerprint_icons(combined);
    let rows: Vec<AnyElement> = glyphs
        .chunks(ICONS_PER_ROW)
        .map(|row| {
            view()
                .flex_row()
                .justify_center()
                .gap(px(space::LG))
                .children(row.iter().map(|codepoint| {
                    view()
                        .w(px(44.0))
                        .h(px(44.0))
                        .flex_row()
                        .items_center()
                        .justify_center()
                        .child(material_icon(*codepoint, 30.0))
                }))
                .into_element()
        })
        .collect();
    view()
        .test_id("verify-icons")
        .w_full()
        .flex_col()
        .items_center()
        // The glyphs are private-use codepoints: screen readers cannot compare
        // icons, so keep them out of the accessibility tree.
        .accessibility_elements_hidden(|| true)
        .gap(px(space::XS))
        .children(rows)
        .into_element()
}

/// The combined fingerprint text, split into readable lines; the characters
/// are exactly the ones the other device shows.
fn text_fingerprint(combined: &str) -> AnyElement {
    let lines: Vec<String> = combined
        .as_bytes()
        .chunks(32)
        .map(|chunk| String::from_utf8_lossy(chunk).into_owned())
        .collect();
    view()
        .test_id("verify-fingerprint")
        .w_full()
        .p(px(space::MD))
        .rounded(px(radius::CONTROL))
        .border_1()
        .border_color(colors::BORDER)
        .bg(colors::FIELD)
        .flex_col()
        .gap(px(2.0))
        .children(lines.into_iter().map(|line| {
            text(SharedString::from(line))
                .text_size(px(text_size::SMALL))
                .text_color(colors::TEXT)
        }))
        .into_element()
}
