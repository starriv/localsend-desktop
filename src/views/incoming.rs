//! The incoming request sheet: peer, size, the offered files and the
//! Accept / Decline decision.
//!
//! The shell owns the sheet chrome (dimmed scrim, 448 pt card, padding): this
//! module returns only the content, and nothing while no request is pending.

use crate::model::{IncomingRequest, Session};
use crate::ui::{
    ButtonKind, IconName, button, bytes, colors, device_tile, file_row, icon, radius, space,
    text_size,
};
use nsh_core::prelude::*;

use super::send::{count_text, fill};

/// How many file rows the sheet lists before summarizing the rest.
const MAX_FILES: usize = 6;

pub fn incoming_sheet(session: Session) -> impl IntoElement {
    let check = session.clone();
    let build = session.clone();
    when(
        move || check.read_transfers(|state| state.incoming.is_some()),
        move || {
            let incoming = untracked(|| build.read_transfers(|state| state.incoming.clone()));
            match incoming {
                Some(request) => content(build.clone(), request).into_element(),
                None => view().into_element(),
            }
        },
    )
}

fn content(session: Session, request: IncomingRequest) -> impl IntoElement {
    let title = session.clone();
    let peer_tile = request.peer.clone();
    let alias = request.peer.alias.clone();
    let files = request.files.clone();
    let total = request.total;
    let count = files.len();
    let quick_save = untracked(|| session.read_settings(|state| state.settings.quick_save));
    let hint = session.clone();
    let accept = session.clone();
    let decline = session.clone();
    // A sheet is modal: the keyboard starts on its primary action, so Enter
    // accepts and Screen Readers announce the decision first.
    let accept_anchor = element_ref();
    effect(move || accept_anchor.focus());
    let count_label = count_text(&session, "transfer.file_one", "transfer.files", count);
    let more = count.saturating_sub(MAX_FILES);
    let more_label = count_text(&session, "receive.more_one", "receive.more", more);
    let size_label = fill(&session.text("transfer.total"), &[("size", bytes(total))]);
    let mut rows: Vec<AnyElement> = Vec::new();
    for file in files.iter().take(MAX_FILES) {
        rows.push(
            file_row(
                IconName::File,
                SharedString::from(file.name.clone()),
                file.size,
            )
            .inset()
            .into_element(),
        );
    }
    if more > 0 {
        rows.push(
            view()
                .px(px(space::SM))
                .py(px(space::XS + 2.0))
                .bg(colors::INSET)
                .child(
                    text(more_label)
                        .text_size(px(text_size::CAPTION))
                        .text_color(colors::FAINT),
                )
                .into_element(),
        );
    }
    view()
        .test_id("incoming-sheet")
        .w_full()
        .flex_col()
        .gap(px(space::LG))
        .child(
            text(move || title.text("receive.incoming"))
                .text_size(px(text_size::TITLE))
                .font_weight(FontWeight::SEMIBOLD),
        )
        .child(
            view()
                .flex_row()
                .items_center()
                .gap(px(space::MD))
                .child(device_tile(move || peer_tile.clone()).size(44.0))
                .child(
                    view()
                        .flex_col()
                        .flex_1()
                        .min_w(px(0.0))
                        .gap(px(2.0))
                        .child(
                            text(alias)
                                .text_size(px(text_size::TITLE))
                                .font_weight(FontWeight::SEMIBOLD)
                                .truncate(),
                        )
                        .child(
                            text(count_label)
                                .text_size(px(text_size::SMALL))
                                .text_color(colors::MUTED)
                                .child(" · ")
                                .child(text(size_label)),
                        ),
                ),
        )
        .child(
            view()
                .flex_col()
                .gap(px(1.0))
                .rounded(px(radius::CONTROL))
                .overflow_hidden()
                .bg(colors::BORDER)
                .children(rows),
        )
        .child(when(quick_save, {
            let hint = hint.clone();
            move || {
                view()
                    .flex_row()
                    .items_center()
                    .gap(px(space::SM))
                    .child(icon(IconName::Bolt).size(14.0).color(colors::ACCENT))
                    .child(
                        text(hint.text("receive.quick_save"))
                            .text_size(px(text_size::CAPTION))
                            .text_color(colors::MUTED),
                    )
            }
        }))
        .child(
            view()
                .flex_row()
                .items_center()
                .justify_end()
                .gap(px(space::SM))
                .child(
                    button(session.text("receive.decline"))
                        .kind(ButtonKind::Secondary)
                        .on_press(move || decline.decide(false)),
                )
                .child(
                    button(session.text("receive.accept"))
                        .kind(ButtonKind::Primary)
                        .element_ref(accept_anchor)
                        .on_press(move || accept.decide(true)),
                ),
        )
}
