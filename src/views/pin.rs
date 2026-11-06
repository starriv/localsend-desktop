//! The PIN prompt: the peer's device asked for this device's receive PIN
//! before it will accept the upload.
//!
//! The shell owns the sheet chrome (380 pt card, close button, scrim): this
//! module returns only the content, and nothing while no PIN is pending.

use crate::model::Session;
use crate::model::session::PinRequest;
use crate::ui::{ButtonKind, IconName, button, colors, icon, radius, space, text_size};
use nsh_core::element::input;
use nsh_core::prelude::*;

use super::send::fill;

#[derive(Clone, Default)]
struct PinUi {
    /// The request the draft belongs to; a different request starts empty.
    request: Option<String>,
    /// What the user typed.
    value: String,
}

fn ui() -> Signal<PinUi> {
    global::<PinUi>()
}

pub fn pin_sheet(session: Session) -> impl IntoElement {
    let check = session.clone();
    let build = session.clone();
    when(
        move || check.pin_request.get().is_some(),
        move || {
            let request = untracked(|| build.pin_request.get());
            match request {
                Some(request) => content(build.clone(), request).into_element(),
                None => view().into_element(),
            }
        },
    )
}

fn content(session: Session, request: PinRequest) -> impl IntoElement {
    let ui = ui();
    let title = session.clone();
    let hint = session.clone();
    let peer = session.clone();
    let alias = request.peer.alias.clone();
    let submit = session.clone();
    let cancel = session.clone();
    let submit_enter = session.clone();
    let request_id = request.id.clone();
    let value_request = request.id.clone();
    let change_request = request.id.clone();
    view()
        .test_id("pin-sheet")
        .w_full()
        .flex_col()
        .gap(px(space::LG))
        .child(
            view()
                .flex_row()
                .items_center()
                .gap(px(space::SM))
                .pr(px(32.0))
                .child(
                    text(move || title.text("pin.title"))
                        .text_size(px(text_size::TITLE))
                        .font_weight(FontWeight::SEMIBOLD),
                ),
        )
        .child(
            view()
                .flex_row()
                .items_center()
                .gap(px(space::SM))
                .child(icon(IconName::Lock).size(15.0).color(colors::MUTED))
                .child(
                    text(fill(&peer.text("receive.from"), &[("name", alias)]))
                        .text_size(px(text_size::BODY))
                        .font_weight(FontWeight::MEDIUM)
                        .truncate(),
                ),
        )
        .child(
            text(move || hint.text("pin.hint"))
                .text_size(px(text_size::SMALL))
                .text_color(colors::MUTED),
        )
        .child(
            input()
                .test_id("pin-input")
                .value(move || {
                    let draft = ui.get();
                    if draft.request.as_deref() == Some(value_request.as_str()) {
                        draft.value
                    } else {
                        String::new()
                    }
                })
                .on_change(move |text: &str| {
                    ui.update(|state| {
                        state.request = Some(change_request.clone());
                        state.value = text.to_owned();
                    })
                })
                .on_submit(move |text: &str| submit_pin(&submit_enter, ui, text))
                .placeholder(session.text("pin.placeholder").to_string())
                .auto_focus(true)
                .max_length(12)
                .w_full()
                .h(px(36.0))
                .px(px(space::SM))
                .rounded(px(radius::CONTROL))
                .bg(colors::FIELD)
                .border_1()
                .border_color(colors::BORDER)
                .text_size(px(text_size::EMPHASIS))
                .into_element(),
        )
        .child(
            view()
                .flex_row()
                .items_center()
                .justify_end()
                .gap(px(space::SM))
                .child(
                    button(session.text("pin.cancel"))
                        .kind(ButtonKind::Secondary)
                        .on_press(move || {
                            ui.update(|state| {
                                state.request = None;
                                state.value.clear();
                            });
                            cancel.dismiss_pin();
                        }),
                )
                .child(
                    button(session.text("pin.confirm"))
                        .kind(ButtonKind::Primary)
                        .disabled(move || {
                            let draft = ui.get();
                            draft.request.as_deref() != Some(request_id.as_str())
                                || draft.value.trim().is_empty()
                        })
                        .on_press(move || {
                            let value = ui.get_untracked().value;
                            submit_pin(&submit, ui, &value);
                        }),
                ),
        )
}

fn submit_pin(session: &Session, ui: Signal<PinUi>, value: &str) {
    let value = value.trim().to_string();
    if value.is_empty() {
        return;
    }
    ui.update(|state| {
        state.request = None;
        state.value.clear();
    });
    session.submit_pin(value);
}
