//! The Receive screen: this device's identity and receiving options, then the
//! recent incoming transfers or the quiet waiting state.
//!
//! Everything here reads the session in bindings: the shell mounts the screen
//! once per destination change, so only reactive reads update it. Settings
//! rows read the settings domain (they change rarely); the recent list reads
//! the transfers domain through the cheap [`RowData`] row model.

use std::cell::RefCell;
use std::rc::Rc;

use crate::model::{AppState, Session, TransferStatus};
use crate::net::{DeviceType, Direction};
use crate::ui::{
    ButtonKind, IconName, button, card_shadow, colors, control, device_tile, icon, radius, space,
    text_size, tile_radius, toggle,
};
use nsh_core::prelude::*;

use super::history::{RowData, transfer_row};

/// The Receive destination.
pub fn receive(session: Session) -> impl IntoElement {
    let waiting_when = session.clone();
    let waiting_body = session.clone();
    let recent_body = session.clone();
    view()
        .test_id("receive")
        .flex_1()
        .min_w(px(0.0))
        .flex_col()
        .p(px(space::GUTTER))
        .gap(px(space::LG))
        .text_color(colors::TEXT)
        .child(identity_card(session.clone()))
        .child(
            when(
                move || !has_receiving(&waiting_when),
                move || waiting(waiting_body.clone()),
            )
            .otherwise(move || recent(recent_body.clone())),
        )
}

fn has_receiving(session: &Session) -> bool {
    session.read_transfers(|state| {
        state
            .transfers
            .iter()
            .any(|transfer| transfer.direction == Direction::Receiving)
    })
}

/// The last derived recent list and the transfers signature it came from.
type RecentCache = Rc<RefCell<Option<(u64, Rc<Vec<RowData>>)>>>;

/// FNV-1a mixing, so the signature is one `u64` and needs no allocation.
fn mix(hash: &mut u64, value: u64) {
    *hash ^= value;
    *hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
}

fn mix_str(hash: &mut u64, text: &str) {
    for byte in text.as_bytes() {
        mix(hash, u64::from(*byte));
    }
    mix(hash, 0xff);
}

/// A fingerprint of what the recent rows display. A progress tick changes
/// only `transferred`, so the eight rows are derived once instead of per tick.
fn recent_signature(state: &AppState) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for transfer in state
        .transfers
        .iter()
        .filter(|transfer| transfer.direction == Direction::Receiving)
        .take(8)
    {
        mix_str(&mut hash, &transfer.id);
        mix_str(&mut hash, &transfer.peer.alias);
        mix(
            &mut hash,
            match transfer.status {
                TransferStatus::Active => 1,
                TransferStatus::Completed => 2,
                TransferStatus::Cancelled => 3,
                TransferStatus::Failed => 4,
            },
        );
        mix(&mut hash, transfer.files.len() as u64);
        mix(&mut hash, transfer.total);
        mix(&mut hash, transfer.started_at);
        mix(&mut hash, transfer.saved_paths.len() as u64);
        mix(&mut hash, transfer.texts.len() as u64);
    }
    hash
}

/// The row list behind a pointer comparison: a memo over it notifies only
/// when the cache produced a new list, not on every progress tick.
#[derive(Clone)]
struct RowList(Rc<Vec<RowData>>);

impl PartialEq for RowList {
    fn eq(&self, other: &Self) -> bool {
        Rc::ptr_eq(&self.0, &other.0)
    }
}

fn recent_transfers(session: &Session, cache: &RecentCache) -> RowList {
    let signature = session.read_transfers(recent_signature);
    if let Some((cached, rows)) = cache.borrow().as_ref()
        && *cached == signature
    {
        return RowList(rows.clone());
    }
    let rows = Rc::new(session.read_transfers(|state| {
        state
            .transfers
            .iter()
            .filter(|transfer| transfer.direction == Direction::Receiving)
            .take(8)
            .map(RowData::from_transfer)
            .collect::<Vec<_>>()
    }));
    cache.replace(Some((signature, rows.clone())));
    RowList(rows)
}

// ---------------------------------------------------------------------------
// Identity card
// ---------------------------------------------------------------------------

fn identity_card(session: Session) -> impl IntoElement {
    let this = session.clone();
    let discoverable = session.clone();
    let discoverable_label = session.clone();
    let discoverable_change = session.clone();
    let header = session.clone();
    let alias = session.clone();
    let model = session.clone();
    let address = session.clone();
    let pin = session.clone();
    let downloads = session.clone();
    let quick_save = session.clone();
    view()
        .test_id("receive-identity")
        .flex_col()
        .rounded(px(radius::CARD))
        .border_1()
        .border_color(colors::BORDER)
        .bg(colors::SURFACE)
        .shadow(card_shadow())
        .dark(|style| style.shadow_none())
        .child(
            view()
                .flex_row()
                .items_center()
                .gap(px(space::LG))
                .p(px(space::XL - space::XS)) // 20 pt card padding
                .child(identity_tile(header, 44.0))
                .child(
                    view()
                        .flex_col()
                        .flex_1()
                        .min_w(px(0.0))
                        .gap(px(2.0))
                        .child(
                            text(move || alias_name(&alias))
                                .text_size(px(text_size::TITLE))
                                .font_weight(FontWeight::SEMIBOLD)
                                .truncate(),
                        )
                        .child(
                            text(move || model_name(&model))
                                .text_size(px(text_size::SMALL))
                                .text_color(colors::MUTED)
                                .truncate(),
                        )
                        .child(
                            text(move || address_line(&address))
                                .text_size(px(text_size::CAPTION))
                                .text_color(colors::FAINT)
                                .tabular_nums()
                                .truncate(),
                        ),
                )
                .child(
                    view()
                        .flex_col()
                        .items_end()
                        .gap(px(space::XS))
                        .child(
                            text(move || this.text("receive.discoverable"))
                                .text_size(px(text_size::CAPTION))
                                .text_color(colors::MUTED),
                        )
                        .child(
                            toggle(move || {
                                discoverable.read_settings(|state| state.settings.discoverable)
                            })
                            .label(discoverable_label.text("receive.discoverable"))
                            .on_change(move |value| {
                                discoverable_change
                                    .update_settings(|settings| settings.discoverable = value);
                            }),
                        ),
                ),
        )
        .child(setting_row(
            pin.text("receive.pin"),
            pin.text("receive.pin_hint"),
            {
                let pin = pin.clone();
                move || {
                    let value = pin.read_settings(|state| {
                        state
                            .settings
                            .pin
                            .clone()
                            .map(SharedString::from)
                            .unwrap_or_else(|| pin.text("receive.pin_unset"))
                    });
                    view()
                        .flex_row()
                        .items_center()
                        .gap(px(space::SM))
                        .child(
                            text(value)
                                .text_size(px(text_size::SMALL))
                                .text_color(colors::MUTED)
                                .tabular_nums(),
                        )
                        .child(
                            button(pin.text("receive.change"))
                                .kind(ButtonKind::Ghost)
                                .small()
                                .on_press({
                                    let pin = pin.clone();
                                    move || pin.open_settings(true)
                                }),
                        )
                        .into_element()
                }
            },
            false,
        ))
        .child(setting_row(
            downloads.text("receive.downloads"),
            SharedString::default(),
            {
                let downloads = downloads.clone();
                move || {
                    let path = downloads.read_settings(|state| state.settings.downloads.clone());
                    let display = path.display().to_string();
                    view()
                        .flex_row()
                        .items_center()
                        .gap(px(space::SM))
                        .child(
                            text(display)
                                .text_size(px(text_size::SMALL))
                                .text_color(colors::MUTED)
                                .number_of_lines(1)
                                .text_overflow(nsh_core::EllipsizeMode::Middle)
                                .max_w(px(280.0)),
                        )
                        .child(
                            button(downloads.text("receive.change"))
                                .kind(ButtonKind::Ghost)
                                .small()
                                .on_press({
                                    let downloads = downloads.clone();
                                    move || downloads.choose_downloads_dir()
                                }),
                        )
                        .into_element()
                }
            },
            true,
        ))
        .child(setting_row(
            quick_save.text("receive.quick_save"),
            quick_save.text("receive.quick_save_hint"),
            {
                let quick_save = quick_save.clone();
                move || {
                    let value = quick_save.clone();
                    let label = quick_save.clone();
                    let change = quick_save.clone();
                    toggle(move || value.read_settings(|state| state.settings.quick_save))
                        .label(label.text("receive.quick_save"))
                        .on_change(move |enabled| {
                            change.update_settings(|settings| settings.quick_save = enabled);
                        })
                        .into_element()
                }
            },
            true,
        ))
}

/// A labelled setting row: label and hint on the left, the control on the
/// right. `divided` draws the hairline above it.
///
/// The control is a `dynamic` region: the closures it takes read the session
/// (settings), and running them in the screen body would subscribe the whole
/// screen to the global revision — remounting the identity card, the radar and
/// the recent list on every transfer progress event.
fn setting_row(
    label: SharedString,
    hint: SharedString,
    control: impl Fn() -> AnyElement + 'static,
    divided: bool,
) -> impl IntoElement {
    view()
        .flex_row()
        .items_center()
        .gap(px(space::LG))
        .px(px(space::XL - space::XS))
        .py(px(space::MD + space::XS))
        .when(divided, |element| {
            element.border_t_1().border_color(colors::BORDER)
        })
        .child(
            view()
                .flex_col()
                .flex_1()
                .min_w(px(0.0))
                .gap(px(2.0))
                .child(
                    text(label)
                        .text_size(px(text_size::BODY))
                        .font_weight(FontWeight::MEDIUM),
                )
                .child(
                    text(hint)
                        .text_size(px(text_size::CAPTION))
                        .text_color(colors::MUTED),
                ),
        )
        .child(dynamic(control))
}

fn alias_name(session: &Session) -> SharedString {
    session.identity.get().map_or_else(
        || session.read_settings(|state| SharedString::from(state.settings.alias.clone())),
        |peer| SharedString::from(peer.alias),
    )
}

fn model_name(session: &Session) -> SharedString {
    session
        .identity
        .get()
        .map(|peer| SharedString::from(peer.device_model))
        .unwrap_or_else(|| session.text("device.this_mac"))
}

/// `LocalSend · v2.2 · 192.168.1.24:53317` (the host is omitted until the
/// network layer knows it).
fn address_line(session: &Session) -> SharedString {
    let app = session.text("app.name");
    let protocol = session.protocol_version();
    match session.identity.get().and_then(|peer| peer.address) {
        Some(address) => SharedString::from(format!(
            "{app} · v{protocol} · {}:{}",
            address.host, address.port
        )),
        // The network has not reported an address yet (or discovery is off):
        // show the port the app will listen on instead of an empty host.
        None => {
            let port = session.read_settings(|state| state.settings.port);
            let port_label = session.text("settings.port");
            SharedString::from(format!("{app} · v{protocol} · {port_label} {port}"))
        }
    }
}

// ---------------------------------------------------------------------------
// Recent incoming transfers / waiting state
// ---------------------------------------------------------------------------

fn recent(session: Session) -> impl IntoElement {
    let rows = session.clone();
    let title = session.clone();
    let cache: RecentCache = Rc::new(RefCell::new(None));
    let list = computed(move || recent_transfers(&rows, &cache));
    let each_list = list;
    view()
        .flex_1()
        .min_h(px(0.0))
        .flex_col()
        .gap(px(space::SM))
        .child(
            text(move || title.text("receive.incoming"))
                .text_size(px(text_size::CAPTION))
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(colors::MUTED)
                .uppercase()
                .px(px(space::XS)),
        )
        .child(scroll().flex_1().scrollbar(false).child(
            view().flex_col().gap(px(space::XS)).child(each(
                move || (*each_list.get().0).clone(),
                |row: &RowData| row.id.clone(),
                move |row, _index| transfer_row(session.clone(), row.get(), false),
            )),
        ))
}

fn waiting(session: Session) -> impl IntoElement {
    let title = session.clone();
    let hint = session.clone();
    view()
        .test_id("receive-waiting")
        .flex_1()
        .items_center()
        .justify_center()
        .gap(px(space::SM))
        .child(radar(session.clone()))
        .child(
            text(move || title.text("receive.waiting"))
                .text_size(px(text_size::EMPHASIS))
                .font_weight(FontWeight::MEDIUM),
        )
        .child(
            text(move || hint.text("receive.waiting_hint"))
                .text_size(px(text_size::SMALL))
                .text_color(colors::MUTED)
                .max_w(px(360.0))
                .text_center(),
        )
}

/// A quiet radar: two rings pulsing around the device tile (still when the
/// user asked for reduced motion).
fn radar(session: Session) -> impl IntoElement {
    let tile_session = session.clone();
    view()
        .size(px(control::RADAR))
        .items_center()
        .justify_center()
        .relative()
        .child(
            view()
                .absolute()
                .size(px(control::RADAR))
                .rounded_full()
                .border_1()
                .border_color(colors::ACCENT.opacity(0.18))
                // Reactive like the spinner: toggling the system setting
                // mid-wait stops or resumes the ping on the mounted view.
                .animate_ping()
                .style_when(reduce_motion, |element| element.animate_none()),
        )
        .child(
            view()
                .absolute()
                .size(px(control::RADAR_RING))
                .rounded_full()
                .border_1()
                .border_color(colors::ACCENT.opacity(0.28))
                .animate_ping()
                .style_when(reduce_motion, |element| element.animate_none()),
        )
        .child(identity_tile(tile_session, control::RADAR_TILE))
}

// ---------------------------------------------------------------------------
// The device tile
// ---------------------------------------------------------------------------

/// The tile for this device: the live identity peer, else the configured
/// device type (the network may not have started yet).
fn identity_tile(session: Session, size: f32) -> impl IntoElement {
    let identity = session.identity;
    dynamic(move || match identity.get() {
        Some(peer) => device_tile(move || peer.clone()).size(size).into_element(),
        None => {
            let device_type = session.read_settings(|state| state.settings.device_type);
            view()
                .size(px(size))
                .flex_shrink_0()
                .items_center()
                .justify_center()
                .rounded(px(tile_radius(size)))
                .bg(colors::ACCENT_TINT)
                .child(
                    icon(device_type_icon(device_type))
                        .size(size * 0.5)
                        .color(colors::ACCENT),
                )
                .into_element()
        }
    })
}

fn device_type_icon(device_type: DeviceType) -> IconName {
    match device_type {
        DeviceType::Desktop => IconName::Laptop,
        DeviceType::Mobile => IconName::Phone,
        DeviceType::Web | DeviceType::Headless | DeviceType::Server => IconName::Globe,
        DeviceType::Unknown => IconName::Laptop,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Transfer, TransferStatus};
    use crate::net::{OfferedFile, Peer};

    fn receiving(id: &str, status: TransferStatus, transferred: u64) -> Transfer {
        Transfer {
            id: id.into(),
            peer: Peer {
                fingerprint: "F".into(),
                alias: "Peer".into(),
                device_model: "Model".into(),
                address: None,
                device_type: DeviceType::Desktop,
                is_self: false,
            },
            direction: Direction::Receiving,
            files: vec![OfferedFile {
                name: "file.bin".into(),
                size: 10,
                mime: "application/octet-stream".into(),
                saved_to: None,
            }],
            total: 10,
            transferred,
            status,
            error: None,
            saved_paths: Vec::new(),
            texts: Vec::new(),
            started_at: 1,
            finished_at: None,
            omitted_files: 0,
        }
    }

    #[test]
    fn recent_signature_ignores_progress_and_sending_rows() {
        let mut state = AppState {
            transfers: vec![receiving("a", TransferStatus::Active, 0)],
            ..AppState::default()
        };
        let before = recent_signature(&state);
        // A progress event moves only `transferred`: the eight rows show no
        // field that changed, so the list must not be rebuilt.
        state.transfers[0].transferred = 5;
        assert_eq!(recent_signature(&state), before);
        // a finished receiving row changing status does change the rows.
        state.transfers[0].status = TransferStatus::Failed;
        assert_ne!(recent_signature(&state), before);
        // Sending transfers belong to the other list, so they never change it.
        let mut sent = AppState {
            transfers: state.transfers.clone(),
            ..AppState::default()
        };
        let mut outgoing = receiving("b", TransferStatus::Completed, 10);
        outgoing.direction = Direction::Sending;
        sent.transfers.push(outgoing);
        assert_eq!(recent_signature(&sent), recent_signature(&state));
    }
}
