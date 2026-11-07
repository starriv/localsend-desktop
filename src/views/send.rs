//! The Send screen: drop zone, staged items, text composer, device selection
//! and the send action.
//!
//! The whole pane is the drop target: `file:` URLs are decoded here and handed
//! to [`Session::add_paths`], which expands folders into their files.
//!
//! UI-local state (drag highlight, the device popover, the text composer) is
//! kept in an app-global store: screen functions are plain functions that the
//! shell re-runs only when the destination changes, so they must not own
//! retained signals themselves.

use crate::model::{ComposerItem, Session};
use crate::net::now_ms;
use crate::ui::{
    ButtonKind, IconName, PeerRowKind, button, bytes, card_shadow, colors, control, device_tile,
    floating_shadow, focus_ring, icon, icon_button, motion as timing, motion_ms, peer_row, radius,
    space, text_size,
};
use nsh_core::element::input;
use nsh_core::prelude::*;
use std::path::PathBuf;

// ---------------------------------------------------------------------------
// UI-local state
// ---------------------------------------------------------------------------

/// Send-screen state that survives the content pane's re-runs.
#[derive(Clone, Default)]
struct SendUi {
    /// Files are being dragged over the pane.
    dragging: bool,
    /// The device chooser popover is open.
    devices_open: bool,
    /// When it opened, for the closing guard below.
    devices_opened_at: u64,
    /// The text composer is open.
    text_open: bool,
    /// What the user typed into the text composer.
    text_draft: String,
}

fn ui() -> Signal<SendUi> {
    global::<SendUi>()
}

// ---------------------------------------------------------------------------
// Screen
// ---------------------------------------------------------------------------

/// The Send destination.
pub fn send(session: Session) -> impl IntoElement {
    // The shell mounts a screen in a `dynamic` that tracks only the
    // destination. Reading the language here subscribes that dynamic to the
    // language signal, so switching language remounts the screen and the
    // labels built once inside its `when`/`each` branches (buttons, hints,
    // row captions) are translated too.
    let _ = session.language.get();
    let ui = ui();
    // The store outlives the screen: without this, coming back to Send would
    // remount a device popover (or a drag highlight) the user never opened.
    let cleanup_ui = ui;
    cleanup(move || {
        cleanup_ui.update(|state| {
            state.dragging = false;
            state.devices_open = false;
        });
    });
    let drop = session.clone();
    let header_when = session.clone();
    let header_body = session.clone();
    let empty_when = session.clone();
    let empty_body = session.clone();
    let staged_body = session.clone();
    let composer_body = session.clone();
    let footer_when = session.clone();
    let footer_body = session.clone();
    let flight_session = session.clone();
    let drag_cond = session.clone();
    let drag_overlay = session.clone();

    view()
        .test_id("send")
        .relative()
        .flex_1()
        .min_w(px(0.0))
        .flex_col()
        .p(px(space::GUTTER))
        .gap(px(space::LG))
        .text_color(colors::TEXT)
        .on_file_drag_enter(move || ui.update(|state| state.dragging = true))
        .on_file_drag_leave(move || ui.update(|state| state.dragging = false))
        .on_file_drop(move |event: &FileDropEvent| {
            ui.update(|state| state.dragging = false);
            let paths: Vec<PathBuf> = event
                .uris
                .iter()
                .filter_map(|uri| file_url_to_path(uri))
                .collect();
            if paths.is_empty() {
                // A drop that carried nothing usable: log what the system sent,
                // so a URL form the decoder does not know is diagnosable (the
                // toast only says that nothing was added).
                tracing::warn!(uris = ?event.uris, "the drop carried no local file path");
                drop.fail(drop.text("send.nothing_added"));
            } else {
                drop.add_paths(paths);
            }
        })
        // A staged list gets a small header with the add/clear actions; the
        // empty state is just the drop zone.
        .child(when(
            move || !header_when.read_composer(|state| state.composer.is_empty()),
            move || header(header_body.clone()),
        ))
        .child(
            when(
                move || empty_when.read_composer(|state| state.composer.is_empty()),
                move || drop_zone(empty_body.clone()),
            )
            .otherwise(move || staged_list(staged_body.clone())),
        )
        .child(when(
            move || ui.get().text_open,
            move || composer_card(composer_body.clone()),
        ))
        .child(when(
            move || !footer_when.read_composer(|state| state.composer.is_empty()),
            move || footer(footer_body.clone()),
        ))
        // One paper plane per started send: `dynamic` remakes the element when
        // the epoch changes, so its entering motion plays again; it ends at
        // opacity 0, inert, until the next send.
        .child(dynamic({
            let session = flight_session;
            move || {
                if session.flight_epoch.get() == 0 {
                    view().into_element()
                } else {
                    flight().into_element()
                }
            }
        }))
        // Drag highlight over a non-empty pane (the empty state highlights its
        // own dashed zone instead).
        .child(when(
            move || {
                ui.get().dragging && !drag_cond.read_composer(|state| state.composer.is_empty())
            },
            move || {
                let overlay_session = drag_overlay.clone();
                view()
                    .absolute()
                    .inset(px(space::MD))
                    .items_center()
                    .justify_center()
                    .rounded(px(radius::CARD))
                    .border_2()
                    .border_dashed()
                    .border_color(colors::ACCENT)
                    .bg(colors::ACCENT_TINT)
                    .pointer_events(PointerEvents::None)
                    .child(
                        text(move || overlay_session.text("send.drop_title"))
                            .text_size(px(text_size::TITLE))
                            .font_weight(FontWeight::SEMIBOLD)
                            // Text on the accent wash: ACCENT itself is 3.98–4.35:1
                            // there in light appearance.
                            .text_color(colors::ACCENT_FOREGROUND),
                    )
            },
        ))
}

/// The heading shown once something is staged: title, hint, add/clear.
fn header(session: Session) -> impl IntoElement {
    let add = session.clone();
    let clear = session.clone();
    let paste_files = session.clone();
    let title = session.clone();
    let hint = session.clone();
    view()
        .flex_row()
        .items_center()
        .gap(px(space::SM))
        .child(
            view()
                .flex_col()
                .gap(px(2.0))
                .child(
                    text(move || title.text("send.title"))
                        .text_size(px(text_size::DISPLAY))
                        .font_weight(FontWeight::SEMIBOLD),
                )
                .child(
                    text(move || hint.text("send.subtitle"))
                        .text_size(px(text_size::SMALL))
                        .text_color(colors::MUTED),
                ),
        )
        .child(view().flex_1())
        .child(
            button(session.text("send.pick_files"))
                .kind(ButtonKind::Secondary)
                .on_press(move || add.pick_files()),
        )
        .child(
            button(session.text("send.paste_files"))
                .kind(ButtonKind::Ghost)
                .icon(IconName::Copy)
                .on_press(move || paste_files.paste_clipboard_files()),
        )
        .child(
            button(session.text("send.clear"))
                .kind(ButtonKind::Ghost)
                .on_press(move || clear.clear_composer()),
        )
}

// ---------------------------------------------------------------------------
// Empty state
// ---------------------------------------------------------------------------

fn drop_zone(session: Session) -> impl IntoElement {
    let ui = ui();
    let enter = motion().entering(drop_in());
    let pick_files = session.clone();
    let pick_folder = session.clone();
    let paste_files = session.clone();
    let title = session.clone();
    let hint = session.clone();
    view()
        .test_id("send-drop-zone")
        .flex_1()
        .min_h(px(0.0))
        .items_center()
        .justify_center()
        .gap(px(space::MD))
        .rounded(px(radius::CARD))
        .border_1()
        .border_dashed()
        .border_color(move || {
            if ui.get().dragging {
                colors::ACCENT
            } else {
                colors::BORDER
            }
        })
        .bg(move || {
            if ui.get().dragging {
                colors::ACCENT_TINT
            } else {
                colors::SURFACE
            }
        })
        .shadow(card_shadow())
        .dark(|style| style.shadow_none())
        .transition_colors()
        .duration(motion_ms(timing::HOVER_MS))
        .ease_out()
        .child(
            view()
                .size(px(48.0))
                .items_center()
                .justify_center()
                .rounded(px(radius::TILE))
                .bg(colors::ACCENT_TINT)
                .child(icon(IconName::Upload).size(24.0).color(colors::ACCENT)),
        )
        .child(
            text(move || title.text("send.drop_title"))
                .text_size(px(text_size::DISPLAY))
                .font_weight(FontWeight::SEMIBOLD),
        )
        .child(
            text(move || hint.text("send.drop_hint"))
                .text_size(px(text_size::SMALL))
                .text_color(colors::MUTED),
        )
        .child(
            view()
                .flex_row()
                .items_center()
                .gap(px(space::SM))
                .mt(px(space::MD))
                .child(
                    // One clear primary action; the rest stays quiet.
                    button(session.text("send.pick_files"))
                        .kind(ButtonKind::Primary)
                        .on_press(move || pick_files.pick_files()),
                )
                .child(
                    button(session.text("send.pick_folder"))
                        .kind(ButtonKind::Secondary)
                        .on_press(move || pick_folder.pick_folder()),
                )
                .child(
                    button(session.text("send.add_text"))
                        .kind(ButtonKind::Ghost)
                        .icon(IconName::Text)
                        .on_press(move || ui.update(|state| state.text_open = true)),
                )
                .child(
                    button(session.text("send.paste_files"))
                        .kind(ButtonKind::Ghost)
                        .icon(IconName::Copy)
                        .on_press(move || paste_files.paste_clipboard_files()),
                ),
        )
        .bind_motion(&enter)
}

/// The empty state's arrival: a short fade with a 6 pt rise.
fn drop_in() -> MotionSpec {
    let mut timing = KeyframeTiming::new(220.0);
    timing.easing = Easing::EaseOut;
    MotionSpec::new(timing)
        .values(MotionChannel::Opacity, [0.0, 1.0])
        .values(MotionChannel::TranslateY, [6.0, 0.0])
}

// ---------------------------------------------------------------------------
// Send flight
// ---------------------------------------------------------------------------

/// The paper plane: it takes off straight up from the Send button and fades
/// before the top of the pane.
fn flight_spec() -> MotionSpec {
    let mut timing = KeyframeTiming::new(1000.0);
    timing.easing = Easing::EaseOut;
    MotionSpec::new(timing)
        .values(MotionChannel::Opacity, [0.0, 1.0, 1.0, 0.0])
        // Straight up: a little horizontal drift keeps it from looking rigid.
        .values(MotionChannel::TranslateX, [0.0, 2.0, 6.0, 10.0])
        .values(MotionChannel::TranslateY, [0.0, -80.0, -230.0, -420.0])
        .values(MotionChannel::Scale, [0.7, 1.0, 1.1, 0.9])
        // The glyph points up-right; tilting it up makes it climb nose-first.
        .values(MotionChannel::Rotate, [0.0, -10.0, -22.0, -34.0])
}

fn flight() -> impl IntoElement {
    let spec = flight_spec();
    let motion = motion().entering(spec);
    view()
        .absolute()
        // Above the transfer card (64 pt + margins) that appears at the pane's
        // bottom when the send starts; it would otherwise cover the take-off.
        .right(px(108.0))
        .bottom(px(120.0))
        .pointer_events(PointerEvents::None)
        // Decoration: screen readers must not announce the flight.
        .accessibility_elements_hidden(|| true)
        .child(icon(IconName::Send).size(36.0).color(colors::ACCENT))
        .bind_motion(&motion)
}

// ---------------------------------------------------------------------------
// Text composer
// ---------------------------------------------------------------------------

fn composer_card(session: Session) -> impl IntoElement {
    let ui = ui();
    let paste = session.clone();
    let submit = session.clone();
    let label = session.clone();
    let add = session.clone();
    view()
        .test_id("send-text-composer")
        .flex_col()
        .gap(px(space::SM))
        .p(px(space::MD))
        .rounded(px(radius::CARD))
        .border_1()
        .border_color(colors::BORDER)
        .bg(colors::SURFACE)
        .child(
            text(move || label.text("send.add_text"))
                .text_size(px(text_size::CAPTION))
                .text_color(colors::MUTED),
        )
        .child(
            input()
                .test_id("send-text-input")
                .value(move || ui.get().text_draft.clone())
                .on_change(move |text: &str| ui.update(|state| state.text_draft = text.to_owned()))
                .multiline(true)
                .placeholder(session.text("send.text_placeholder").to_string())
                .h(px(64.0))
                .p(px(space::SM))
                .rounded(px(radius::CONTROL))
                .bg(colors::FIELD)
                .border_1()
                .border_color(colors::BORDER)
                .text_size(px(text_size::BODY))
                .on_submit(move |_: &str| add_draft(&submit, ui)),
        )
        .child(
            view()
                .flex_row()
                .items_center()
                .gap(px(space::SM))
                .child(
                    button(session.text("menu.paste"))
                        .kind(ButtonKind::Ghost)
                        .on_press(move || paste.paste_clipboard()),
                )
                .child(view().flex_1())
                .child(
                    button(session.text("common.cancel"))
                        .kind(ButtonKind::Ghost)
                        .on_press(move || {
                            ui.update(|state| {
                                state.text_draft.clear();
                                state.text_open = false;
                            });
                        }),
                )
                .child(
                    // The screen's primary action is Send (or Choose files
                    // on the empty state); the draft's Add stays secondary so
                    // there is never a second filled button on screen.
                    button(session.text("send.add"))
                        .kind(ButtonKind::Secondary)
                        .disabled(move || ui.get().text_draft.trim().is_empty())
                        .on_press(move || add_draft(&add, ui)),
                ),
        )
}

fn add_draft(session: &Session, ui: Signal<SendUi>) {
    let draft = ui.get_untracked().text_draft.trim().to_string();
    if draft.is_empty() {
        return;
    }
    session.add_text(draft);
    ui.update(|state| {
        state.text_draft.clear();
        state.text_open = false;
    });
}

// ---------------------------------------------------------------------------
// Staged items
// ---------------------------------------------------------------------------

/// Staged rows rendered at once. The list is not virtualized and a dropped
/// folder may hold up to 10,000 files; the rest is summarized in one row (the
/// header still shows the full count and size).
const STAGED_ROWS_SHOWN: usize = 200;

fn staged_list(session: Session) -> impl IntoElement {
    let rows = session.clone();
    let more_when = session.clone();
    let more_text = session.clone();
    let more_count = move |session: &Session| {
        session.read_composer(|state| state.composer.len().saturating_sub(STAGED_ROWS_SHOWN))
    };
    view()
        .flex_1()
        .min_h(px(0.0))
        .flex_col()
        .gap(px(space::SM))
        .child(drop_strip(session.clone()))
        .child(
            scroll().flex_1().scrollbar(false).child(
                view()
                    .flex_col()
                    .gap(px(space::SM))
                    .child(each(
                        move || {
                            rows.read_composer(|state| {
                                state
                                    .composer
                                    .iter()
                                    .take(STAGED_ROWS_SHOWN)
                                    .cloned()
                                    .enumerate()
                                    .collect::<Vec<(usize, ComposerItem)>>()
                            })
                        },
                        composer_key,
                        move |row, _index| staged_row(session.clone(), row),
                    ))
                    .child(when(
                        move || more_count(&more_when) > 0,
                        move || {
                            let label = more_text.clone();
                            text(move || {
                                count_text(
                                    &label,
                                    "receive.more_one",
                                    "receive.more",
                                    more_count(&label),
                                )
                            })
                            .text_size(px(text_size::CAPTION))
                            .text_color(colors::MUTED)
                            .px(px(space::SM))
                        },
                    )),
            ),
        )
}

/// A compact, always-visible drop target once something is staged: the whole
/// pane still accepts drops, this makes that visible and clickable.
fn drop_strip(session: Session) -> impl IntoElement {
    let this = session.clone();
    let label = session.clone();
    let pick = session.clone();
    view()
        .h(px(44.0))
        .flex_row()
        .items_center()
        .justify_center()
        .gap(px(space::SM))
        .rounded(px(radius::CONTROL))
        .border_1()
        .border_dashed()
        .border_color(move || {
            if ui().get().dragging {
                colors::ACCENT
            } else {
                colors::BORDER
            }
        })
        .cursor_pointer()
        .focusable(true)
        .role(AccessibilityRole::Button)
        .label(move || label.text("send.drop_title"))
        .hover(|style| style.bg(colors::HOVER))
        .active(|style| style.opacity(0.8))
        .focus(focus_ring)
        .on_press(move || this.pick_files())
        .child(icon(IconName::Plus).size(15.0).color(colors::MUTED))
        .child(
            text(move || pick.text("send.drop_title"))
                .text_size(px(text_size::SMALL))
                .text_color(colors::MUTED),
        )
}

/// A key unique per staged row.
///
/// A file's path identifies it (staged duplicates are already skipped). A text
/// block has no identity, so its position keeps two identical texts distinct:
/// duplicate keys make `each`'s reconciliation build a fresh row for the
/// duplicate on every update and leak the row it replaced.
fn composer_key(row: &(usize, ComposerItem)) -> SharedString {
    match &row.1 {
        ComposerItem::File { path, .. } => SharedString::from(format!("file:{}", path.display())),
        ComposerItem::Text(_) => SharedString::from(format!("text:{}", row.0)),
    }
}

fn staged_row(session: Session, row: ReadSignal<(usize, ComposerItem)>) -> impl IntoElement {
    let remove = session.clone();
    let is_text = row.get_untracked().1.is_text();
    let inner = match row.get_untracked().1 {
        ComposerItem::File { .. } => file_row(row).into_element(),
        ComposerItem::Text(_) => text_row(session.clone(), row).into_element(),
    };
    view()
        .flex_row()
        .items_center()
        .gap(px(space::MD))
        .p(px(space::SM))
        .rounded(px(radius::CONTROL))
        .border_1()
        .border_color(colors::BORDER)
        .bg(colors::SURFACE)
        .hover(|style| style.bg(colors::HOVER))
        .child(
            view()
                .flex_row()
                .items_center()
                .gap(px(space::MD))
                .flex_1()
                .min_w(px(0.0))
                .child(inner),
        )
        .child(
            icon_button(IconName::Close)
                .label(session.text("send.remove"))
                .on_press(move || remove.remove_item(row.get_untracked().0)),
        )
        .when(is_text, |element| element.min_h(px(72.0)))
}

fn file_row(row: ReadSignal<(usize, ComposerItem)>) -> impl IntoElement {
    let icon_name = row
        .get_untracked()
        .1
        .as_file_icon()
        .unwrap_or(IconName::File);
    view()
        .flex_row()
        .items_center()
        .gap(px(space::MD))
        .flex_1()
        .min_w(px(0.0))
        .h(px(36.0))
        .child(glyph_tile(icon_name))
        .child(
            view()
                .flex_col()
                .flex_1()
                .min_w(px(0.0))
                .gap(px(2.0))
                .child(
                    text(move || row.with(|row| row.1.name()))
                        .text_size(px(text_size::BODY))
                        .font_weight(FontWeight::MEDIUM)
                        .truncate(),
                )
                .child(
                    text(move || row.with(|row| bytes(row.1.size())))
                        .text_size(px(text_size::CAPTION))
                        .text_color(colors::MUTED),
                ),
        )
}

fn text_row(session: Session, row: ReadSignal<(usize, ComposerItem)>) -> impl IntoElement {
    view()
        .flex_col()
        .flex_1()
        .min_w(px(0.0))
        .gap(px(2.0))
        .child(
            text(move || session.text("send.text_preview"))
                .text_size(px(text_size::CAPTION))
                .text_color(colors::MUTED),
        )
        .child(
            text(move || {
                row.with(|row| match &row.1 {
                    ComposerItem::Text(text) => text.clone(),
                    ComposerItem::File { name, .. } => name.clone(),
                })
            })
            .text_size(px(text_size::SMALL))
            .number_of_lines(3)
            .truncate(),
        )
}

fn glyph_tile(name: IconName) -> impl IntoElement {
    view()
        .size(px(32.0))
        .flex_shrink_0()
        .items_center()
        .justify_center()
        .rounded(px(radius::CONTROL))
        .bg(colors::INSET)
        .child(icon(name).size(18.0).color(colors::MUTED))
}

// ---------------------------------------------------------------------------
// Footer: counts, device chip, send
// ---------------------------------------------------------------------------

fn footer(session: Session) -> impl IntoElement {
    let ui = ui();
    let chip_session = session.clone();
    let send_action = session.clone();
    let can_send = session.clone();
    let counts = session.clone();
    let total = session.clone();
    let anchor = element_ref();
    view()
        .test_id("send-footer")
        .flex_row()
        .items_center()
        .gap(px(space::MD))
        .child(
            view()
                .flex_col()
                .gap(px(2.0))
                .child(
                    text(move || {
                        count_text(
                            &counts,
                            "send.item_one",
                            "send.items_count",
                            counts.read_composer(|state| state.composer.len()),
                        )
                    })
                    .text_size(px(text_size::BODY))
                    .font_weight(FontWeight::MEDIUM),
                )
                .child(
                    text(move || {
                        fill(
                            &total.text("send.total_size"),
                            &[(
                                "size",
                                bytes(total.read_composer(|state| state.total_composer_size())),
                            )],
                        )
                    })
                    .text_size(px(text_size::CAPTION))
                    .text_color(colors::MUTED),
                ),
        )
        .child(view().flex_1())
        .child(device_chip(chip_session, anchor))
        .child(
            button(session.text("send.send"))
                .kind(ButtonKind::Primary)
                .icon(IconName::Send)
                .disabled(move || !can_send.read(|state| state.can_send()))
                .on_press(move || send_action.send()),
        )
        .child(when(
            move || ui.get().devices_open,
            move || device_popover(session.clone(), anchor),
        ))
}

fn device_chip(session: Session, anchor: ElementRef) -> impl IntoElement {
    let ui = ui();
    // One memo for the selected peer: the chip's parts read it instead of
    // re-running a `dynamic` branch on every peers revision (a discovery
    // announcement must not rebuild the chip; a selection change only
    // retargets its bindings).
    let selected = computed({
        let session = session.clone();
        move || session.read_peers(|state| state.selected_peer().cloned())
    });
    let has_peer = selected;
    let tile_peer = selected;
    let alias = selected;
    let style_peer = selected;
    let choose = session.clone();
    view()
        .element_ref(&anchor)
        .test_id("send-device-chip")
        .h(px(control::HEIGHT))
        .px(px(space::SM))
        .flex_row()
        .items_center()
        .gap(px(space::SM))
        .rounded(px(radius::CONTROL))
        .border_1()
        .border_color(colors::BORDER)
        .bg(colors::SURFACE)
        .cursor_pointer()
        .focusable(true)
        .role(AccessibilityRole::Button)
        .hover(|style| style.bg(colors::HOVER))
        .active(|style| style.opacity(0.8))
        .focus(focus_ring)
        .on_press(move || {
            ui.update(|state| {
                state.devices_open = !state.devices_open;
                if state.devices_open {
                    state.devices_opened_at = now_ms();
                }
            })
        })
        .child(dynamic(move || {
            if has_peer.get().is_some() {
                device_tile(move || tile_peer.get().unwrap_or_else(fallback_peer))
                    .size(22.0)
                    .into_element()
            } else {
                icon(IconName::Users)
                    .size(16.0)
                    .color(colors::MUTED)
                    .into_element()
            }
        }))
        .child(
            text(move || match alias.get() {
                Some(peer) => SharedString::from(peer.alias),
                None => choose.text("send.choose_device"),
            })
            .text_size(px(text_size::BODY))
            .font_weight(FontWeight::MEDIUM)
            .text_color(move || {
                if style_peer.get().is_some() {
                    colors::TEXT
                } else {
                    colors::MUTED
                }
            }),
        )
        .child(icon(IconName::ChevronDown).size(14.0).color(colors::MUTED))
}

/// A placeholder peer for the chip while nothing is selected.
fn fallback_peer() -> crate::net::Peer {
    crate::net::Peer {
        fingerprint: String::new(),
        alias: String::new(),
        device_model: String::new(),
        address: None,
        device_type: crate::net::DeviceType::Unknown,
        is_self: false,
    }
}

/// How long after opening the popover a press is ignored as closing it.
///
/// The click that opens the popover can arrive again on the overlay's
/// press-outside listener right after it mounts (a trace shows the mounted
/// popover being torn down by the same interaction); ignore that replay so the
/// popover stays open until the user actually presses elsewhere.
const POPOVER_PRESS_GUARD_MS: u64 = 250;

fn popover_dismissable(ui: &SendUi) -> bool {
    now_ms().saturating_sub(ui.devices_opened_at) > POPOVER_PRESS_GUARD_MS
}

fn device_popover(session: Session, anchor: ElementRef) -> impl IntoElement {
    let ui = ui();
    let rows = session.clone();
    let empty = session.clone();
    let heading = session.clone();
    // One memo for the favorite fingerprints: rows read it in their star
    // binding instead of cloning the settings list each.
    let favorites = computed({
        let session = session.clone();
        move || session.read_settings(|state| state.settings.favorites.clone())
    });
    overlay()
        .anchor(&anchor)
        .placement(Placement::Top)
        .align(Align::End)
        .offset(px(space::SM))
        .on_press_outside(move |_: &Point<Points>| {
            ui.update(|state| {
                if popover_dismissable(state) {
                    state.devices_open = false;
                }
            })
        })
        .on_cancel(move || ui.update(|state| state.devices_open = false))
        .child(
            view()
                .test_id("send-devices-popover")
                .w(px(control::POPOVER))
                .p(px(space::SM))
                .flex_col()
                .gap(px(space::XS))
                .rounded(px(radius::CARD))
                .border_1()
                .border_color(colors::BORDER)
                .bg(colors::SURFACE)
                .shadow(floating_shadow())
                .child(
                    text(move || heading.text("nav.nearby"))
                        .text_size(px(text_size::CAPTION))
                        .text_color(colors::MUTED)
                        .px(px(space::SM))
                        .py(px(space::XS)),
                )
                .child(each(
                    move || online_devices(&rows),
                    |peer: &crate::net::Peer| peer.fingerprint.clone(),
                    move |peer, _index| device_popover_row(session.clone(), peer, favorites),
                ))
                .child(
                    text(move || {
                        if online_devices(&empty).is_empty() {
                            if empty.discovery_searching() {
                                empty.text("device.searching")
                            } else {
                                empty.text("device.no_devices")
                            }
                        } else {
                            SharedString::default()
                        }
                    })
                    .text_size(px(text_size::SMALL))
                    .text_color(colors::MUTED)
                    .px(px(space::SM))
                    .py(px(space::SM)),
                ),
        )
}

/// A chooser row: the shared [`PeerRow`] with the online subtitle, the
/// selection check and the favorite toggle.
fn device_popover_row(
    session: Session,
    peer: ReadSignal<crate::net::Peer>,
    favorites: Memo<Vec<String>>,
) -> impl IntoElement {
    let fingerprint = peer.get_untracked().fingerprint;
    // The selected state is a memo, not a one-time read: a peer dropping out
    // (which clears the selection) updates the check while the popover is up.
    let selected = {
        let session = session.clone();
        let fingerprint = fingerprint.clone();
        computed(move || {
            session.read_peers(|state| state.selected.as_deref() == Some(fingerprint.as_str()))
        })
    };
    let is_favorite = {
        let fingerprint = fingerprint.clone();
        move || favorites.get().iter().any(|known| known == &fingerprint)
    };
    let label_session = session.clone();
    let favorite_label = {
        let fingerprint = fingerprint.clone();
        move || {
            label_session.text(
                if favorites.get().iter().any(|known| known == &fingerprint) {
                    "device.remove_favorite"
                } else {
                    "device.add_favorite"
                },
            )
        }
    };
    let favorite_action = {
        let session = session.clone();
        let fingerprint = fingerprint.clone();
        move || session.toggle_favorite(&fingerprint)
    };
    let online = {
        let session = session.clone();
        move || session.text("device.online")
    };
    let choose = {
        let session = session.clone();
        move || {
            let fingerprint = peer.get_untracked().fingerprint;
            session.select(Some(fingerprint));
            ui().update(|state| state.devices_open = false);
        }
    };
    peer_row(peer, PeerRowKind::Chooser)
        .selected(selected)
        .subtitle(online)
        .favorite(is_favorite, favorite_label, favorite_action)
        .on_press(choose)
}

fn online_devices(session: &Session) -> Vec<crate::net::Peer> {
    session.read_peers(|state| {
        state
            .devices
            .iter()
            .filter(|peer| !peer.is_self && peer.address.is_some())
            .cloned()
            .collect()
    })
}

/// A count label with a singular form: `one_key` when `count` is one, else
/// the plural template with `{count}`.
///
/// Both keys are expected in every language: the i18n tables are tested to
/// have exactly the same keys, so a singular form is never missing from one
/// language only. (A missing key would show the `<missing>` sentinel and fail
/// the i18n test, not silently fall back at runtime.)
pub(crate) fn count_text(
    session: &Session,
    one_key: &'static str,
    many_key: &'static str,
    count: usize,
) -> SharedString {
    if count == 1 {
        return session.text(one_key);
    }
    fill(&session.text(many_key), &[("count", count.to_string())])
}

/// Fills `{name}` in a translated template without hardcoding the copy.
///
/// [`i18n::fill`](crate::i18n::fill) does the substitution, so the views and
/// the macOS menu bar cannot drift apart; this wrapper only keeps the
/// [`SharedString`] call sites unchanged.
pub(crate) fn fill(template: &str, values: &[(&str, String)]) -> SharedString {
    crate::i18n::fill(template, values).into()
}

impl ComposerItem {
    /// The glyph a staged file shows (folders are expanded before staging).
    fn as_file_icon(&self) -> Option<IconName> {
        match self {
            ComposerItem::File { mime, is_dir, .. } => Some(if *is_dir {
                IconName::Folder
            } else if mime.starts_with("image/") {
                IconName::Image
            } else if mime.starts_with("text/") {
                IconName::Text
            } else {
                IconName::File
            }),
            ComposerItem::Text(_) => Some(IconName::Text),
        }
    }
}

// ---------------------------------------------------------------------------
// Drop helpers
// ---------------------------------------------------------------------------

/// Decodes a `file:` URL dropped by the system into a path.
///
/// `uris` are percent-encoded `file:` URLs (`file:///Users/me/a%20b.txt`), and
/// a folder is just another path: [`Session::add_paths`] expands it. A Finder
/// drag can instead carry a file reference URL (`file:///.file/id=…`), which
/// names no path until AppKit resolves it; see [`crate::platform::urls`].
pub(crate) fn file_url_to_path(url: &str) -> Option<PathBuf> {
    if let Some(path) = crate::platform::urls::resolved_reference_path(url) {
        return Some(path);
    }
    let rest = url.strip_prefix("file://")?;
    // `file://localhost/path` and `file:///path` are the same file.
    let rest = rest.strip_prefix("localhost").unwrap_or(rest);
    let decoded = percent_decode(rest)?;
    if decoded.is_empty() {
        return None;
    }
    Some(PathBuf::from(decoded))
}

fn percent_decode(input: &str) -> Option<String> {
    let bytes = input.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' {
            let hex = bytes.get(index + 1..index + 3)?;
            let raw = std::str::from_utf8(hex).ok()?;
            let byte = u8::from_str_radix(raw, 16).ok()?;
            out.push(byte);
            index += 3;
        } else {
            out.push(bytes[index]);
            index += 1;
        }
    }
    String::from_utf8(out).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_dropped_file_url_is_percent_decoded() {
        assert_eq!(
            file_url_to_path("file:///Users/me/a%20b.txt"),
            Some(PathBuf::from("/Users/me/a b.txt"))
        );
        assert_eq!(
            file_url_to_path("file://localhost/Users/me/caf%C3%A9.png"),
            Some(PathBuf::from("/Users/me/café.png"))
        );
        // Not a file URL, or an invalid escape: nothing is staged.
        assert_eq!(file_url_to_path("https://example.com/a"), None);
        assert_eq!(file_url_to_path("file:///bad%zz"), None);
    }

    /// Finder puts file reference URLs (`file:///.file/id=…`) on a drag
    /// pasteboard: they name no path until AppKit resolves them, which a plain
    /// `file:` decode cannot do. This is the drop that used to stage nothing.
    #[cfg(target_os = "macos")]
    #[test]
    fn a_finder_reference_url_is_resolved() {
        use objc2_foundation::{NSString, NSURL};

        let directory =
            std::env::temp_dir().join(format!("localsend-drop-ref-{}", std::process::id()));
        std::fs::create_dir_all(&directory).expect("temp directory");
        let file = directory.join("dropped.txt");
        std::fs::write(&file, b"x").expect("temp file");

        let path_url =
            NSURL::fileURLWithPath(&NSString::from_str(file.to_str().expect("utf-8 path")));
        let reference = path_url.fileReferenceURL().expect("reference URL");
        let uri = reference
            .absoluteString()
            .expect("absolute string")
            .to_string();

        let resolved = file_url_to_path(&uri).and_then(|path| std::fs::canonicalize(path).ok());
        assert_eq!(resolved, std::fs::canonicalize(&file).ok());

        std::fs::remove_dir_all(&directory).ok();
    }

    #[test]
    fn identical_text_rows_get_distinct_each_keys() {
        // Two identical text blocks have no identity of their own: the position
        // keeps them apart, so `each` never reconciles one row onto two items
        // (which would leak the replaced row and rebuild it on every update).
        let first = (0_usize, ComposerItem::Text("same".into()));
        let second = (1_usize, ComposerItem::Text("same".into()));
        assert_ne!(composer_key(&first), composer_key(&second));
    }

    #[test]
    fn a_file_row_is_keyed_by_its_path() {
        let row = |index: usize| {
            (
                index,
                ComposerItem::File {
                    path: PathBuf::from("/tmp/report.pdf"),
                    name: "report.pdf".into(),
                    size: 10,
                    mime: "application/pdf".into(),
                    is_dir: false,
                },
            )
        };
        // The same file is staged once (deduplicated), but its key must not
        // change when rows shift.
        assert_eq!(composer_key(&row(0)), composer_key(&row(7)));
    }

    #[test]
    fn every_count_form_exists_in_every_language() {
        // The helper picks `one_key` for a count of one, so both forms must
        // exist in every language (the i18n test enforces table parity).
        for language in crate::i18n::Language::ALL {
            for key in ["transfer.file_one", "transfer.files", "send.item_one"] {
                assert!(
                    crate::i18n::has_key(language, key),
                    "{key} missing in {language:?}"
                );
            }
        }
    }
}
