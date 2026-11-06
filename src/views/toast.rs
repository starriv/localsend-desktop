//! The transient toast at the bottom of the window.
//!
//! The session owns the timeout; the shell mounts this above the content's
//! bottom edge and fades it in. This module returns the capsule itself, and
//! nothing when there is no toast.

use crate::model::Session;
use crate::ui::{IconName, colors, control, floating_shadow, icon, space, text_size};
use nsh_core::prelude::*;

pub fn toast(session: Session) -> impl IntoElement {
    let check = session.clone();
    when(
        move || check.toast.get().is_some(),
        move || capsule(session.clone()),
    )
}

fn capsule(session: Session) -> impl IntoElement {
    let tint = session.clone();
    let glyph = session.clone();
    let background = session.clone();
    let text_session = session.clone();
    let text_color = session.clone();
    view()
        .test_id("toast")
        .flex_row()
        .items_center()
        .gap(px(space::SM))
        .max_w(px(control::TOAST_MAX_W))
        .px(px(space::LG))
        .py(px(space::SM + space::XS))
        .rounded_full()
        .border_1()
        .border_color(move || {
            if tint.toast.get().is_some_and(|toast| toast.error) {
                colors::DANGER.opacity(0.35)
            } else {
                colors::BORDER
            }
        })
        .bg(move || {
            if background.toast.get().is_some_and(|toast| toast.error) {
                colors::DANGER_TINT
            } else {
                colors::SURFACE
            }
        })
        .shadow(floating_shadow())
        .role(AccessibilityRole::Alert)
        .child(dynamic(move || {
            // A toast is replaced in place when another message arrives: the
            // capsule is not rebuilt, so the glyph has to read the signal.
            let error = glyph.toast.get().is_some_and(|toast| toast.error);
            icon(if error {
                IconName::Warning
            } else {
                IconName::Check
            })
            .size(15.0)
            .color(if error {
                colors::DANGER
            } else {
                colors::SUCCESS
            })
            .into_element()
        }))
        .child(
            text(move || {
                text_session
                    .toast
                    .get()
                    .map(|toast| toast.text)
                    .unwrap_or_default()
            })
            // Network errors can carry a host, a URL and an OS message: cap
            // the capsule (three lines) instead of letting it grow past the
            // narrowest window.
            .number_of_lines(3)
            .truncate()
            .text_size(px(text_size::BODY))
            .text_color(move || {
                if text_color.toast.get().is_some_and(|toast| toast.error) {
                    colors::DANGER
                } else {
                    colors::TEXT
                }
            }),
        )
}
