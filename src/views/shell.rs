//! The application shell: the hidden title bar, the 252 pt sidebar (nearby
//! devices, destinations, settings), the content pane and the overlays.
//!
//! The shell is the one thing the root component mounts. It never renders a
//! screen's internals: it mounts `views::send` / `views::receive` /
//! `views::history` in the content pane and the overlay functions
//! (`incoming_sheet`, `pin_sheet`, `settings_sheet`, `toast`,
//! `transfer_card`) around them.
//!
//! Layout (1200 × 800, and fluid down to 1120 × 760):
//!
//! ```text
//! ┌──────────────────────────┬────────────────────────────────┐
//! │ ◉ ◉ ◉  LocalSend          │ 50 pt drag strip                │
//! ├──────────────────────────┤────────────────────────────────┤
//! │ Nearby devices            │                                │
//! │  ▢ MacBook Pro        ●  │  screen (send / receive /       │
//! │  ▢ iPhone 15 Pro      ●  │  history)                       │
//! │                           │                                │
//! │  Send  Receive  History   │  transfer card (when active)    │
//! │                           │                                │
//! │ ⚙ Settings      ⟨alias⟩   │                                │
//! └──────────────────────────┴────────────────────────────────┘
//! ```

use nsh_core::desktop::TitleBarStyle;
use nsh_core::element::AccessibilityState;
use nsh_core::element::desktop::{set_title_bar, title_bar};
use nsh_core::prelude::*;
use std::time::Duration;

use crate::model::{Session, View};
use crate::net::Peer;
use crate::ui::icons::{IconName, icon};
use crate::ui::theme::{
    Appearance, colors, control, floating_shadow, focus_ring, motion as timing, motion_ms, radius,
    space, text_size, theme,
};
use crate::ui::{PeerRowKind, icon_button, peer_row, spinner};

/// The sheet the shell is showing over the content pane, if any.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum Sheet {
    /// None.
    #[default]
    None,
    /// An incoming transfer request.
    Incoming,
    /// A peer asked for this device's PIN.
    Pin,
    /// Settings.
    Settings,
    /// The combined fingerprint of this device and a peer (LocalSend's
    /// "Verify" page), opened from a device row or an incoming request.
    Verify,
}

/// Dismisses `sheet` if it can be dismissed (an incoming request waits for
/// Accept or Decline, so the scrim and the close button do nothing there).
fn dismiss_sheet(session: &Session, sheet: Sheet) {
    match sheet {
        Sheet::Settings => session.open_settings(false),
        Sheet::Pin => session.dismiss_pin(),
        Sheet::Verify => session.close_verify(),
        Sheet::Incoming | Sheet::None => {}
    }
}

/// The sheet covering the window, in the order it stacks: an incoming
/// request, then the PIN prompt, then settings.
///
/// Escape and the scrim both dismiss through this one answer, so a key press
/// can never close a sheet the user cannot see.
fn current_sheet(session: &Session) -> Sheet {
    if session.verify_peer.get().is_some() {
        Sheet::Verify
    } else if session.read_transfers(|state| state.incoming.is_some()) {
        Sheet::Incoming
    } else if session.pin_request.get().is_some() {
        Sheet::Pin
    } else if session.settings_open.get() {
        Sheet::Settings
    } else {
        Sheet::None
    }
}

/// The whole app: sidebar, content pane, overlays.
pub fn shell(session: Session, _appearance: Appearance) -> impl IntoElement {
    // The window: a hidden title bar with the traffic lights vertically
    // centered in a 50 pt strip. Called once, at the root.
    set_title_bar(TitleBarStyle::Hidden, Some(px(control::TITLE_BAR)));

    // The sheet covering the window, in stacking order: one memo drives the
    // overlay, Escape, and the accessibility isolation of the background.
    let sheet = computed({
        let session = session.clone();
        move || current_sheet(&session)
    });
    {
        let session = session.clone();
        on_key_with(
            "escape",
            KeyOptions {
                // A sheet's first control (the settings alias field, the PIN
                // input) takes focus when the sheet opens; `NotEditing` would
                // make Escape dead exactly then. DESIGN.md: "Escape/close
                // dismisses", so the binding fires wherever focus is.
                when: nsh_core::keymap::When::Always,
                ..KeyOptions::default()
            },
            // The same priority the overlay stacks in: Escape closes the PIN
            // over settings, never the hidden settings behind the PIN.
            move |_| dismiss_sheet(&session, current_sheet(&session)),
        );
    }
    // Where the settings sheet hands the keyboard focus back on close.
    let settings_trigger = element_ref();

    view()
        .flex_1()
        .flex_row()
        .bg(move || theme().background)
        .text_color(move || theme().foreground)
        .child(sidebar(session.clone(), sheet, settings_trigger))
        .child(content_pane(session, sheet, settings_trigger))
}

// --- sidebar -------------------------------------------------------------

/// The sidebar: the title bar's drag region with the app mark, the nearby
/// device list, the destinations and the settings row.
fn sidebar(session: Session, sheet: Memo<Sheet>, settings_trigger: ElementRef) -> impl IntoElement {
    view()
        .w(px(control::SIDEBAR))
        .h_full()
        .flex_shrink_0()
        .flex_col()
        .bg(move || theme().sidebar)
        .border_r_1()
        .border_color(move || theme().sidebar_border)
        // A sheet is modal: the sidebar behind it leaves the accessibility
        // tree (and its rows leave the Tab order, see the rows below).
        .accessibility_elements_hidden(move || sheet.get() != Sheet::None)
        .child(sidebar_title(session.clone()))
        .child(nearby_section(session.clone(), sheet))
        .child(nav_section(session.clone(), sheet))
        .child(settings_row(session, sheet, settings_trigger))
}

/// The top 50 pt: the drag region, padded past the traffic lights.
fn sidebar_title(session: Session) -> impl IntoElement {
    view()
        .h(px(control::TITLE_BAR))
        .flex_shrink_0()
        .flex_row()
        .items_center()
        .gap(px(space::SM))
        .pl(move || title_bar().left + px(space::SM))
        .pr(px(space::MD))
        .window_drag(true)
        .child(icon(IconName::LocalSend).size(19.0).color(colors::MARK))
        .child(
            text(move || session.text("app.name"))
                .role(AccessibilityRole::Header)
                .number_of_lines(1)
                .text_size(px(text_size::BODY))
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(colors::TEXT),
        )
}

/// "Nearby devices" and the list under it.
fn nearby_section(session: Session, sheet: Memo<Sheet>) -> impl IntoElement {
    let header = session.clone();
    let empty = session.clone();
    // The device set and the selection live in the peers domain: read them
    // through its revision so a transfer's progress tick (up to ~20/s) does
    // not clone every peer to re-diff a list that did not change.
    let selected = computed({
        let session = session.clone();
        move || session.read_peers(|state| state.selected.clone())
    });
    // Favorites come from the settings domain, one memo for the whole list:
    // a row's star binding updates on a favorite change, not on every
    // transfer tick.
    let favorites = computed({
        let session = session.clone();
        move || session.read_settings(|state| state.settings.favorites.clone())
    });
    let rows = session.clone();
    let row_session = session.clone();
    let refresh_label = session.clone();
    let refresh_enabled = session.clone();
    let refresh_action = session.clone();
    view()
        .flex_1()
        .flex_col()
        .min_h(px(0.0))
        .child(
            view()
                .px(px(space::MD + 2.0))
                .pt(px(space::SM))
                .pb(px(6.0))
                .flex_row()
                .items_center()
                .justify_between()
                .child(
                    text(move || header.text("nav.nearby"))
                        .text_size(px(text_size::CAPTION))
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(colors::FAINT),
                )
                .child(
                    icon_button(IconName::Refresh)
                        .label(move || refresh_label.text("device.refresh"))
                        .disabled(move || {
                            sheet.get() != Sheet::None || !refresh_enabled.can_refresh_discovery()
                        })
                        .on_press(move || refresh_action.refresh_discovery()),
                ),
        )
        .child(
            scroll().flex_1().scrollbar(false).child(
                view()
                    .px(px(space::SM))
                    .pb(px(space::MD))
                    .gap(px(1.0))
                    .child(each(
                        move || rows.read_peers(|state| state.devices.clone()),
                        |peer| peer.fingerprint.clone(),
                        move |peer, _index| {
                            device_row(row_session.clone(), peer, selected, favorites, sheet)
                        },
                    ))
                    .child(searching_hint(session.clone()))
                    .child(empty_state(empty)),
            ),
        )
}

/// A row in the nearby list: the shared [`PeerRow`] with the favorite toggle
/// and the sidebar's selection state.
fn device_row(
    session: Session,
    peer: ReadSignal<Peer>,
    selected: Memo<Option<String>>,
    favorites: Memo<Vec<String>>,
    sheet: Memo<Sheet>,
) -> impl IntoElement {
    let fingerprint = peer.get_untracked().fingerprint;
    let is_selected = {
        let fingerprint = fingerprint.clone();
        move || selected.get().as_deref() == Some(fingerprint.as_str())
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
    let press = {
        let session = session.clone();
        let fingerprint = fingerprint.clone();
        move || session.select(Some(fingerprint.clone()))
    };
    let verify_label = {
        let session = session.clone();
        move || session.text("verify.action")
    };
    let verify_session = session.clone();
    let verify_peer = peer;
    peer_row(peer, PeerRowKind::Sidebar)
        .selected(is_selected)
        // Behind a modal sheet no background row is a Tab stop.
        .focusable(move || sheet.get() == Sheet::None)
        .verify(verify_label, move || {
            verify_session.open_verify(verify_peer.get_untracked())
        })
        .favorite(is_favorite, favorite_label, favorite_action)
        .on_press(press)
}

/// Brief search feedback, then a static empty state while discovery continues.
fn empty_state(session: Session) -> AnyElement {
    let empty = computed({
        let session = session.clone();
        move || session.read_peers(|state| state.devices.is_empty())
    });
    let discovering = computed({
        let session = session.clone();
        move || session.discovery_searching()
    });
    dynamic(move || {
        if !empty.get() {
            return view().into_element();
        }
        let message = session.clone();
        let label_discovering = discovering;
        let hint = session.clone();
        view()
            .min_h(px(34.0))
            .flex_row()
            .items_center()
            .gap(px(space::SM))
            .pl(px(space::SM + 2.0))
            .pr(px(space::SM))
            // Background discovery must not keep the foreground spinner alive.
            .child(if discovering.get() {
                spinner().size(12.0).into_element()
            } else {
                icon(IconName::Search)
                    .size(14.0)
                    .color(colors::FAINT)
                    .into_element()
            })
            .child(
                view()
                    .flex_1()
                    .min_w(px(0.0))
                    .flex_col()
                    .gap(px(space::XS))
                    .child(
                        text(move || {
                            if label_discovering.get() {
                                message.text("device.searching")
                            } else {
                                message.text("device.no_devices")
                            }
                        })
                        .number_of_lines(2)
                        .text_size(px(text_size::SMALL))
                        .text_color(colors::MUTED),
                    )
                    .children(
                        (!discovering.get() && session.read_peers(|state| state.discovering)).then(
                            || {
                                text(move || hint.text("device.empty_hint"))
                                    .number_of_lines(2)
                                    .text_size(px(text_size::CAPTION))
                                    .text_color(colors::FAINT)
                            },
                        ),
                    ),
            )
            .into_element()
    })
}

/// Brief feedback for an explicit refresh with devices already present.
fn searching_hint(session: Session) -> AnyElement {
    let searching = computed({
        let session = session.clone();
        move || {
            session.discovery_searching() && session.read_peers(|state| !state.devices.is_empty())
        }
    });
    dynamic(move || {
        if !searching.get() {
            return view().into_element();
        }
        let message = session.clone();
        view()
            .h(px(28.0))
            .flex_row()
            .items_center()
            .gap(px(space::SM))
            .pl(px(space::SM + 2.0))
            .pr(px(space::SM))
            .child(spinner().size(10.0))
            .child(
                text(move || message.text("device.searching"))
                    .number_of_lines(1)
                    .truncate()
                    .text_size(px(text_size::CAPTION))
                    .text_color(colors::FAINT),
            )
            .into_element()
    })
}

/// The three destinations.
fn nav_section(session: Session, sheet: Memo<Sheet>) -> impl IntoElement {
    let current = computed({
        let session = session.clone();
        move || session.read(|state| state.view)
    });
    view()
        .flex_col()
        .flex_shrink_0()
        .gap(px(1.0))
        .px(px(space::SM))
        .pt(px(space::SM))
        .pb(px(space::XS))
        .child(
            view()
                .h(px(1.0))
                .mx(px(space::XS))
                .mb(px(space::SM))
                .bg(move || theme().sidebar_border),
        )
        .child(nav_row(
            session.clone(),
            View::Send,
            IconName::Send,
            current,
            sheet,
        ))
        .child(nav_row(
            session.clone(),
            View::Receive,
            IconName::Receive,
            current,
            sheet,
        ))
        .child(nav_row(
            session,
            View::History,
            IconName::History,
            current,
            sheet,
        ))
}

/// One destination: a row with a pill when selected.
fn nav_row(
    session: Session,
    target: View,
    glyph: IconName,
    current: Memo<View>,
    sheet: Memo<Sheet>,
) -> impl IntoElement {
    // The closure captures only `Copy` handles, so every use below shares it.
    let selected = move || current.get() == target;
    let label = session.clone();
    let text_session = session.clone();
    let press = session;
    let key = view_key(target);
    view()
        .h(px(control::HEIGHT))
        .flex_shrink_0()
        .flex_row()
        .items_center()
        .gap(px(9.0))
        .px(px(space::SM))
        .rounded(px(radius::CHIP))
        .cursor_pointer()
        .role(AccessibilityRole::Button)
        .label(move || label.text(key))
        .accessibility_state(move || AccessibilityState {
            selected: selected(),
            ..AccessibilityState::default()
        })
        .focusable(move || sheet.get() == Sheet::None)
        .window_drag(false)
        .style_when(selected, |s| s.bg(colors::SIDEBAR_ACCENT))
        .hover(|s| s.bg(colors::HOVER))
        .active(|s| s.bg(colors::TRACK))
        .focus(focus_ring)
        .transition_colors()
        .duration(motion_ms(timing::HOVER_MS))
        .ease_out()
        .on_press(move || press.set_view(target))
        .child(icon(glyph).size(17.0).color(move || {
            if selected() {
                colors::ACCENT
            } else {
                colors::MUTED
            }
        }))
        .child(
            text(move || text_session.text(key))
                .flex_1()
                .number_of_lines(1)
                .text_size(px(text_size::BODY))
                .style_when(selected, |s| s.font_weight(FontWeight::SEMIBOLD))
                .text_color(move || {
                    if selected() {
                        colors::TEXT
                    } else {
                        colors::MUTED
                    }
                }),
        )
}

/// A destination arriving: a quick fade (160 ms), so switching panes reads
/// as a change rather than a jump.
fn screen_fade() -> MotionSpec {
    let mut timing = KeyframeTiming::new(timing::SELECT_MS);
    timing.easing = Easing::EaseOut;
    MotionSpec::new(timing).values(MotionChannel::Opacity, [0.0, 1.0])
}

/// A sheet's entrance: 220 ms, fading in and rising from 97 % (DESIGN.md's
/// motion table). Motions follow `reduce_motion()` themselves.
fn sheet_in() -> MotionSpec {
    let mut timing = KeyframeTiming::new(timing::SHEET_IN_MS);
    timing.easing = Easing::EaseOut;
    MotionSpec::new(timing)
        .values(MotionChannel::Opacity, [0.0, 1.0])
        .values(MotionChannel::Scale, [0.97, 1.0])
}

/// The scrim's fade: 160 ms.
fn scrim_fade() -> MotionSpec {
    let mut timing = KeyframeTiming::new(timing::SCRIM_MS);
    timing.easing = Easing::EaseOut;
    MotionSpec::new(timing).values(MotionChannel::Opacity, [0.0, 1.0])
}

/// A toast arriving: 200 ms, fading in and rising a few points.
fn toast_in() -> MotionSpec {
    let mut timing = KeyframeTiming::new(timing::TOAST_MS);
    timing.easing = Easing::EaseOut;
    MotionSpec::new(timing)
        .values(MotionChannel::Opacity, [0.0, 1.0])
        .values(MotionChannel::TranslateY, [6.0, 0.0])
}

/// The tallest a sheet may be at the current window height.
///
/// This reads the reactive `window()`, so it belongs in a style binding: a
/// value read while a sheet is built would make resizing re-mount the sheet
/// (losing its scroll position and focus) instead of re-laying it out.
fn sheet_max_height() -> f32 {
    (window().height.0 - 140.0).clamp(280.0, 680.0)
}

/// The i18n key of a destination.
fn view_key(view: View) -> &'static str {
    match view {
        View::Send => "nav.send",
        View::Receive => "nav.receive",
        View::History => "nav.history",
    }
}

/// The bottom row: settings and this device's alias.
fn settings_row(
    session: Session,
    sheet: Memo<Sheet>,
    settings_trigger: ElementRef,
) -> impl IntoElement {
    let alias = computed({
        let session = session.clone();
        move || {
            session
                .identity
                .get()
                .map(|peer| SharedString::from(peer.alias))
                .unwrap_or_else(|| {
                    session.read_settings(|state| SharedString::from(state.settings.alias.clone()))
                })
        }
    });
    let label = session.clone();
    let text_session = session.clone();
    let press = session;
    view()
        .flex_shrink_0()
        .px(px(space::SM))
        .pb(px(space::SM))
        .border_t_1()
        .border_t_color(move || theme().sidebar_border)
        .child(
            view()
                .element_ref(&settings_trigger)
                .h(px(control::HEIGHT))
                .flex_row()
                .items_center()
                .gap(px(9.0))
                .px(px(space::SM))
                .rounded(px(radius::CHIP))
                .cursor_pointer()
                .role(AccessibilityRole::Button)
                .label(move || label.text("nav.settings"))
                .focusable(move || sheet.get() == Sheet::None)
                .window_drag(false)
                .hover(|s| s.bg(colors::HOVER))
                .active(|s| s.bg(colors::TRACK))
                .focus(focus_ring)
                .transition_colors()
                .duration(motion_ms(timing::HOVER_MS))
                .ease_out()
                .on_press(move || press.open_settings(true))
                .child(icon(IconName::Settings).size(16.0).color(colors::MUTED))
                .child(
                    text(move || text_session.text("nav.settings"))
                        .flex_1()
                        .number_of_lines(1)
                        .text_size(px(text_size::BODY))
                        .text_color(colors::TEXT),
                )
                .child(
                    text(alias)
                        .max_w(px(96.0))
                        .number_of_lines(1)
                        .truncate()
                        .text_size(px(text_size::CAPTION))
                        .text_color(colors::FAINT),
                ),
        )
}

// --- content pane --------------------------------------------------------

/// The pane right of the sidebar: its 50 pt drag strip, the mounted screen,
/// the transfer card and the overlays.
fn content_pane(
    session: Session,
    sheet: Memo<Sheet>,
    settings_trigger: ElementRef,
) -> impl IntoElement {
    view()
        .flex_1()
        .min_w(px(0.0))
        .relative()
        .flex_col()
        .bg(move || theme().background)
        .child(
            view()
                .h(px(control::TITLE_BAR))
                .flex_shrink_0()
                .window_drag(true),
        )
        .child(
            // The pane's body is the modal background: hidden from assistive
            // technologies while a sheet covers it. The overlays are siblings
            // of this view, not children, so they stay visible.
            view()
                .relative()
                .flex_1()
                .min_h(px(0.0))
                .flex_col()
                .accessibility_elements_hidden(move || sheet.get() != Sheet::None)
                .child(screen(session.clone()))
                .child(transfer_card(session.clone())),
        )
        .children(overlays(session, sheet, settings_trigger))
}

/// The screen for the current destination. It remounts only when the
/// destination changes, so screens read the rest of their state in bindings.
fn screen(session: Session) -> impl IntoElement {
    // A memo, not a signal + effect: the destination is derived state, and a
    // memo recomputes without a second reactive pass.
    let current = computed({
        let session = session.clone();
        move || session.read(|state| state.view)
    });
    let entering = screen_fade();
    let app = session;
    dynamic(move || {
        let content = match current.get() {
            View::Send => super::send::send(app.clone()).into_element(),
            View::Receive => super::receive::receive(app.clone()).into_element(),
            View::History => super::history::history(app.clone()).into_element(),
        };
        let fade = motion().entering(entering.clone());
        view()
            .flex_1()
            .min_h(px(0.0))
            .bind_motion(&fade)
            .child(content)
            .into_element()
    })
}

/// The bottom status card: mounted while a transfer runs, and for a moment
/// after one finishes so its "Saved to …" state can be read. The card itself
/// returns an empty view when it has nothing to show.
fn transfer_card(session: Session) -> impl IntoElement {
    // Re-evaluate when the state changes and, for the finished card's
    // eight-second grace period, once a second: the tick is what makes the
    // memo recompute while time passes without a state change.
    let tick = signal(0_u64);
    set_interval(Duration::from_secs(1), move || {
        tick.update(|tick| *tick += 1)
    });
    let visible = computed({
        let session = session.clone();
        move || {
            tick.get();
            super::transfer::card_visible_now(&session)
        }
    });
    let app = session;
    dynamic(move || {
        if !visible.get() {
            return view().into_element();
        }
        view()
            .flex_shrink_0()
            .px(px(space::GUTTER))
            .pb(px(space::GUTTER))
            .child(super::transfer::transfer_card(app.clone()))
            .into_element()
    })
}

// --- overlays ------------------------------------------------------------

/// The modal sheet (incoming request, PIN, settings) and the toast, stacked
/// over the content pane's body.
fn overlays(session: Session, sheet: Memo<Sheet>, settings_trigger: ElementRef) -> [AnyElement; 2] {
    [
        sheet_overlay(session.clone(), sheet, settings_trigger),
        toast_overlay(session),
    ]
}

/// The dimmed scrim and the sheet card for the sheet that is open.
fn sheet_overlay(session: Session, sheet: Memo<Sheet>, settings_trigger: ElementRef) -> AnyElement {
    let scrim_fade = scrim_fade();
    let card_in = sheet_in();
    dynamic(move || {
        let which = sheet.get();
        if which == Sheet::None {
            return view().into_element();
        }
        let width = match which {
            Sheet::Incoming => control::SHEET_INCOMING,
            Sheet::Verify => control::SHEET_VERIFY,
            Sheet::Pin => control::SHEET_PIN,
            Sheet::Settings => control::SHEET_WIDE,
            Sheet::None => unreachable!(),
        };
        let content = match which {
            Sheet::Incoming => super::incoming::incoming_sheet(session.clone()).into_element(),
            Sheet::Verify => super::verify::verify_sheet(session.clone()).into_element(),
            Sheet::Pin => super::pin::pin_sheet(session.clone()).into_element(),
            Sheet::Settings => {
                super::settings::settings_sheet(session.clone(), settings_trigger).into_element()
            }
            Sheet::None => unreachable!(),
        };
        let scrim = motion().entering(scrim_fade.clone());
        let card = motion().entering(card_in.clone());
        let close_label = session.clone();
        // Settings has more content than fits: a definite height is what lets
        // its scroll view lay out (a flex child of a content-sized card
        // collapses to 0 height on macOS). The request sheets size to their
        // content.
        let scrolls = which == Sheet::Settings;
        let show_close = matches!(which, Sheet::Pin | Sheet::Verify);
        let scrim_press = {
            let session = session.clone();
            move || dismiss_sheet(&session, which)
        };
        let close = {
            let session = session.clone();
            move || dismiss_sheet(&session, which)
        };
        view()
            .absolute()
            .inset_0()
            .z_index(30)
            .flex_col()
            .items_center()
            .justify_center()
            .child(
                view()
                    .absolute()
                    .inset_0()
                    .bg(colors::SCRIM)
                    .bind_motion(&scrim)
                    .on_press(scrim_press),
            )
            .child(
                view()
                    .relative()
                    .w(px(width))
                    // A binding, not a value captured here: resizing the
                    // window re-lays the sheet out instead of re-mounting it
                    // (which would drop its scroll position and focus).
                    .when(scrolls, |v| v.h(move || px(sheet_max_height())))
                    .when(!scrolls, |v| v.max_h(move || px(sheet_max_height())))
                    .flex_col()
                    .rounded(px(radius::SHEET))
                    .bg(colors::SURFACE)
                    .border_1()
                    .border_color(colors::BORDER)
                    .shadow(floating_shadow())
                    .overflow_hidden()
                    // A press on the card's own background stays here instead
                    // of reaching the scrim and closing the sheet (the detail
                    // sheet does the same).
                    .on_press(move || {})
                    .bind_motion(&card)
                    .child(if scrolls {
                        let title = session.clone();
                        let header_close_label = session.clone();
                        let header_close = session.clone();
                        view()
                            .flex_1()
                            .min_h(px(0.0))
                            .flex_col()
                            .child(
                                view()
                                    .test_id("settings-header")
                                    .flex_shrink_0()
                                    .flex_row()
                                    .items_center()
                                    .justify_between()
                                    .px(px(space::XL))
                                    .py(px(space::MD))
                                    .bg(colors::SURFACE)
                                    .border_b_1()
                                    .border_color(colors::BORDER)
                                    .child(
                                        text(move || title.text("settings.title"))
                                            .text_size(px(text_size::TITLE))
                                            .font_weight(FontWeight::SEMIBOLD),
                                    )
                                    .child(
                                        icon_button(IconName::Close)
                                            .label(move || header_close_label.text("common.close"))
                                            .color(colors::TEXT)
                                            .on_press(move || header_close.open_settings(false)),
                                    ),
                            )
                            .child(
                                scroll()
                                    .test_id("settings-scroll")
                                    .flex_1()
                                    .min_h(px(0.0))
                                    .overflow_hidden()
                                    .child(view().p(px(space::XL)).child(content)),
                            )
                            .into_element()
                    } else {
                        view().p(px(space::XL)).child(content).into_element()
                    })
                    .children(show_close.then(|| {
                        view()
                            .absolute()
                            .top(px(space::MD))
                            .right(px(space::MD))
                            .child(
                                icon_button(IconName::Close)
                                    .label(close_label.text("common.close"))
                                    .color(colors::TEXT)
                                    .on_press(close),
                            )
                    })),
            )
            .into_element()
    })
}

/// The transient toast, floating over the bottom of the content pane.
fn toast_overlay(session: Session) -> AnyElement {
    let rise = toast_in();
    // Whether the transfer card occupies the bottom of the pane: a memo, so
    // the toast's lift follows a card appearing or leaving instead of being
    // frozen when the toast was built.
    let lifted = computed({
        let session = session.clone();
        move || super::transfer::card_visible_now(&session)
    });
    dynamic(move || {
        if session.toast.get().is_none() {
            return view().into_element();
        }
        let rise = motion().entering(rise.clone());
        view()
            .absolute()
            .left_0()
            .right_0()
            .bottom(move || {
                if lifted.get() {
                    px(space::XL + 92.0)
                } else {
                    px(space::XL)
                }
            })
            .z_index(40)
            .flex_col()
            .items_center()
            .child(
                view()
                    .bind_motion(&rise)
                    .child(super::toast::toast(session.clone())),
            )
            .into_element()
    })
}
