//! The bottom transfer card and its detail sheet.
//!
//! The card is mounted by the shell inside the content pane and returns an
//! empty view when nothing is running (or recently finished). Clicking it
//! opens the detail sheet, an overlay owned here; the shell only mounts
//! [`transfer_card`].
//!
//! Progress events arrive up to ~20×/s. Everything the card shows is therefore
//! derived **once per transfer change** into a [`CardSnapshot`] memo, so one
//! event formats the bytes, speed and ETA once instead of once per binding.
//! The detail sheet derives its rows behind a signature cache: file rows and
//! saved paths do not change while bytes are moving, so a progress tick reuses
//! the previous rows instead of rebuilding them.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use std::time::{Duration, Instant};

use crate::i18n::Language;
use crate::model::{AppState, Session, Transfer, TransferStatus};
use crate::net::{Direction, Peer, now_ms};
use crate::ui::{
    ButtonKind, IconName, button, bytes, colors, control, device_tile, eta, file_row,
    floating_shadow, icon, icon_button, progress_bar, radius, space, speed, text_size,
};
use nsh_core::element::AdaptiveColor;
use nsh_core::prelude::*;

use super::history::status_badge;
use super::send::{count_text, fill};

/// File rows one transfer shows in the detail sheet. The sheet is not
/// virtualized, and a transfer may carry 10,000 files: the rest is summarized
/// in one "N more files" row (the folder is one click away).
const SHEET_FILES_SHOWN: usize = 100;

/// How long a finished transfer stays in the card after it completes.
const COMPLETED_CARD_MS: u64 = 8_000;

#[derive(Clone, Default)]
struct TransferUi {
    /// The detail sheet is open.
    open: bool,
    /// When the sheet opened, for the guard below.
    opened_at: u64,
    /// A finished transfer the user dismissed from the card.
    dismissed: Option<String>,
}

/// How long after opening a sheet a press is ignored as closing it.
///
/// The click that opens the sheet can be delivered twice (once to the card,
/// once, after the overlay mounted, to its scrim): a trace of the interaction
/// shows a second `Press` on the freshly mounted scrim, which closed the sheet
/// in the same frame. A press this soon after opening is that replay, not a
/// user dismissing the sheet.
const OPEN_PRESS_GUARD_MS: u64 = 250;

/// Whether a press on the scrim should dismiss the sheet.
fn dismissable(ui: &TransferUi) -> bool {
    now_ms().saturating_sub(ui.opened_at) > OPEN_PRESS_GUARD_MS
}

fn ui() -> Signal<TransferUi> {
    global::<TransferUi>()
}

/// Whether a finished transfer is still inside the card's grace period.
fn recent(transfer: &Transfer) -> bool {
    transfer
        .finished_at
        .is_some_and(|finished| now_ms().saturating_sub(finished) < COMPLETED_CARD_MS)
}

/// The transfers the card and sheet show: the active ones, else the most
/// recent finished one for a few seconds.
///
/// Returns ids only, so a visibility check never clones a transfer.
fn shown_ids(session: &Session, dismissed: Option<&str>) -> Vec<SharedString> {
    session.read_transfers(|state| {
        let active: Vec<SharedString> = state
            .active_transfers()
            .map(|transfer| SharedString::from(transfer.id.clone()))
            .collect();
        if !active.is_empty() {
            return active;
        }
        state
            .transfers
            .iter()
            .find(|transfer| recent(transfer) && dismissed != Some(transfer.id.as_str()))
            .map(|transfer| vec![SharedString::from(transfer.id.clone())])
            .unwrap_or_default()
    })
}

fn card_visible(session: &Session, dismissed: Option<&str>) -> bool {
    !shown_ids(session, dismissed).is_empty()
}

/// Whether the shell should reserve the bottom of the pane for the card:
/// active transfers, or a finished one still inside its grace period that the
/// user has not dismissed.
///
/// The shell's toast lift reads this same answer, so dismissing the card
/// drops the toast at once instead of leaving it raised until the grace
/// period ends (the text-visible card and the reserved space agree).
pub(crate) fn card_visible_now(session: &Session) -> bool {
    let ui = ui();
    card_visible(session, ui.get().dismissed.as_deref())
}

/// The card shown while transfers run; an empty view when there is none.
pub fn transfer_card(session: Session) -> impl IntoElement {
    let ui = ui();
    let card_when = session.clone();
    let card_body = session.clone();
    let sheet_when = session.clone();
    let sheet_body = session.clone();
    view()
        .child(when(
            move || card_visible(&card_when, ui.get().dismissed.as_deref()),
            move || card(card_body.clone()),
        ))
        .child(when(
            move || ui.get().open && !shown_ids(&sheet_when, None).is_empty(),
            move || transfer_sheet(sheet_body.clone()),
        ))
}

// ---------------------------------------------------------------------------
// The card snapshot
// ---------------------------------------------------------------------------

/// Everything the card shows, formatted once per transfer change.
#[derive(Clone, PartialEq)]
struct CardSnapshot {
    peer: Peer,
    active: bool,
    /// "Send to <name>" / "From <name>" / "N active transfers".
    label: SharedString,
    /// `2.3 MB / 6.2 MB`.
    progress_text: SharedString,
    /// `24 MB/s`, or an em dash before the first bytes.
    speed_text: SharedString,
    /// `00:42`, or an em dash when unknown.
    eta_text: SharedString,
    progress: f32,
    status: TransferStatus,
    /// For a completed transfer: the folder the files were saved to.
    saved_dir: Option<SharedString>,
    /// For a failed transfer: what the network reported.
    error: Option<SharedString>,
    openable: bool,
}

type CardMemo = Memo<Rc<CardSnapshot>>;

fn read_card(session: &Session) -> CardSnapshot {
    session.read_transfers(|state| {
        let active: Vec<&Transfer> = state.active_transfers().collect();
        let shown: Vec<&Transfer> = if active.is_empty() {
            state
                .transfers
                .iter()
                .find(|transfer| recent(transfer))
                .into_iter()
                .collect()
        } else {
            active
        };
        let first = shown.first().copied();
        let active_now = first.is_some_and(|transfer| transfer.is_active());
        let peer = first
            .map(|transfer| transfer.peer.clone())
            .unwrap_or_else(fallback_peer);

        let label = if shown.len() > 1 {
            session.text("transfer.active")
        } else {
            match first {
                Some(transfer) => match transfer.direction {
                    Direction::Sending => fill(
                        &session.text("send.send_to"),
                        &[("name", transfer.peer.alias.clone())],
                    ),
                    Direction::Receiving => fill(
                        &session.text("receive.from"),
                        &[("name", transfer.peer.alias.clone())],
                    ),
                },
                None => SharedString::default(),
            }
        };

        let (transferred, total) =
            shown
                .iter()
                .fold((0_u64, 0_u64), |(transferred, total), transfer| {
                    (transferred + transfer.transferred, total + transfer.total)
                });
        let progress_text =
            SharedString::from(format!("{} / {}", bytes(transferred), bytes(total)));
        let progress = if total == 0 {
            0.0
        } else {
            (transferred as f32 / total as f32).clamp(0.0, 1.0)
        };

        let rate: f64 = shown
            .iter()
            .filter_map(|transfer| average_speed(transfer))
            .sum();
        let speed_text = if rate <= 0.0 {
            // No bytes have moved yet: an em dash reads better than "0 B/s".
            SharedString::new_static("—")
        } else {
            speed(rate).into()
        };
        let remaining: Option<f64> = shown
            .iter()
            .filter_map(|transfer| remaining_seconds(transfer))
            .max_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        let eta_text = remaining
            .map(|seconds| SharedString::from(eta(seconds)))
            .unwrap_or_else(|| SharedString::new_static("—"));

        let status = first
            .map(|transfer| transfer.status)
            .unwrap_or(TransferStatus::Completed);
        let saved_dir = first.and_then(|transfer| {
            if transfer.status != TransferStatus::Completed {
                return None;
            }
            transfer
                .saved_paths
                .first()
                .and_then(|path| path.parent())
                .map(|parent| parent.display().to_string())
                .or_else(|| {
                    Some(
                        session
                            .state
                            .borrow()
                            .settings
                            .downloads
                            .display()
                            .to_string(),
                    )
                })
                .map(SharedString::from)
        });
        let error = first
            .filter(|transfer| transfer.status == TransferStatus::Failed)
            .and_then(|transfer| transfer.error.clone())
            .map(SharedString::from);
        let openable = first.is_some_and(|transfer| !transfer.saved_paths.is_empty());

        // Transfers leave the list eventually; keep the monotonic clocks in
        // step with it instead of growing for the life of the process.
        CLOCKS.with(|clocks| {
            let mut clocks = clocks.borrow_mut();
            if clocks.len() > 64 {
                clocks.retain(|id, _| state.transfers.iter().any(|transfer| &transfer.id == id));
            }
        });

        CardSnapshot {
            peer,
            active: active_now,
            label,
            progress_text,
            speed_text,
            eta_text,
            progress,
            status,
            saved_dir,
            error,
            openable,
        }
    })
}

// ---------------------------------------------------------------------------
// Card
// ---------------------------------------------------------------------------

fn card(session: Session) -> impl IntoElement {
    let ui = ui();
    let snapshot_session = session.clone();
    let snap: CardMemo = computed(move || Rc::new(read_card(&snapshot_session)));
    let tile = snap;
    let label = snap;
    let content = snap;
    let complete = snap;
    let cancel_cond = snap;
    let verify_cond = snap;
    let verify_peer = snap;
    let verify_session = session.clone();
    let content_session = session.clone();
    let cancel_session = session.clone();
    let dismiss_session = session.clone();
    view()
        .test_id("transfer-card")
        .h(px(control::TRANSFER_CARD))
        .flex_row()
        .items_center()
        .gap(px(space::MD))
        .px(px(space::MD + space::XS))
        .rounded(px(radius::CARD))
        .border_1()
        .border_color(colors::BORDER)
        .bg(colors::SURFACE)
        .shadow(floating_shadow())
        .cursor_pointer()
        .role(AccessibilityRole::Button)
        .focusable(true)
        .label(move || label.get().label.clone())
        .hover(|style| style.bg(colors::HOVER))
        .focus(crate::ui::focus_ring)
        .on_press(move || {
            ui.update(|state| {
                state.open = true;
                state.opened_at = now_ms();
            })
        })
        .child(device_tile(move || tile.get().peer.clone()).size(36.0))
        .child(
            when(move || content.get().active, {
                let snap = content;
                move || active_content(snap)
            })
            .otherwise({
                let session = content_session.clone();
                move || completed_content(session.clone(), complete)
            }),
        )
        .child(when(move || verify_cond.get().active, {
            let verify = verify_session.clone();
            let peer = verify_peer;
            move || {
                let verify = verify.clone();
                let peer = peer;
                icon_button(IconName::Verified)
                    .label(verify.text("verify.action"))
                    .on_press(move || verify.open_verify(peer.get().peer.clone()))
            }
        }))
        .child(
            when(move || cancel_cond.get().active, {
                let label = cancel_session.clone();
                let action = cancel_session.clone();
                move || {
                    let action = action.clone();
                    icon_button(IconName::Cancel)
                        .label(label.text("transfer.cancel"))
                        .on_press(move || cancel_shown(&action))
                }
            })
            .otherwise({
                let dismiss = dismiss_session.clone();
                move || {
                    let dismiss = dismiss.clone();
                    icon_button(IconName::Close)
                        .label(dismiss.text("common.dismiss"))
                        .on_press(move || {
                            let id = shown_ids(&dismiss, None).into_iter().next();
                            ui.update(|state| state.dismissed = id.map(|id| id.to_string()));
                        })
                }
            }),
        )
        .child(icon(IconName::ChevronRight).size(16.0).color(colors::FAINT))
}

fn active_content(snap: CardMemo) -> impl IntoElement {
    let label = snap;
    let stats = snap;
    let speed = snap;
    let eta = snap;
    let bar = snap;
    view()
        .flex_1()
        .min_w(px(0.0))
        .flex_col()
        .gap(px(6.0))
        .child(
            view()
                .flex_row()
                .items_center()
                .gap(px(space::SM))
                .child(
                    text(move || label.get().label.clone())
                        .text_size(px(text_size::BODY))
                        .font_weight(FontWeight::SEMIBOLD)
                        .truncate(),
                )
                .child(view().flex_1())
                .child(
                    text(move || stats.get().progress_text.clone())
                        .text_size(px(text_size::CAPTION))
                        .text_color(colors::MUTED)
                        .tabular_nums(),
                )
                .child(
                    text(move || speed.get().speed_text.clone())
                        .text_size(px(text_size::CAPTION))
                        .text_color(colors::MUTED)
                        .tabular_nums(),
                )
                .child(
                    text(move || eta.get().eta_text.clone())
                        .text_size(px(text_size::CAPTION))
                        .text_color(colors::FAINT)
                        .tabular_nums(),
                ),
        )
        .child(progress_bar(move || bar.get().progress))
}

fn completed_content(session: Session, snap: CardMemo) -> impl IntoElement {
    let look_session = snap;
    let label = snap;
    let subtitle = snap;
    let open_cond = snap;
    let open = session.clone();
    let label_session = session.clone();
    let subtitle_session = session.clone();
    view()
        .flex_1()
        .min_w(px(0.0))
        .flex_row()
        .items_center()
        .gap(px(space::SM))
        .child(
            view()
                .size(px(24.0))
                .flex_shrink_0()
                .items_center()
                .justify_center()
                .rounded_full()
                .bg(move || finished(&look_session).tint)
                .child(dynamic(move || {
                    let look = finished(&look_session);
                    icon(look.glyph).size(14.0).color(look.ink).into_element()
                })),
        )
        .child(
            view()
                .flex_col()
                .flex_1()
                .min_w(px(0.0))
                .gap(px(2.0))
                .child(
                    text(move || label_session.text(label_key(&label)))
                        .text_size(px(text_size::BODY))
                        .font_weight(FontWeight::SEMIBOLD),
                )
                .child(
                    text(move || finished_subtitle(&subtitle_session, &subtitle))
                        .text_size(px(text_size::CAPTION))
                        .text_color(colors::MUTED)
                        .number_of_lines(1)
                        .text_overflow(nsh_core::EllipsizeMode::Middle)
                        .truncate(),
                ),
        )
        .child(when(
            move || {
                let snapshot = open_cond.get();
                snapshot.status == TransferStatus::Completed && snapshot.openable
            },
            move || {
                let label = open.clone();
                let open = open.clone();
                button(move || label.text("transfer.open"))
                    .kind(ButtonKind::Secondary)
                    .small()
                    .on_press(move || open_latest(&open))
            },
        ))
}

/// The look of a finished transfer: only Completed is a success; Cancelled
/// and Failed get their own glyph, tint and label.
struct FinishedLook {
    glyph: IconName,
    tint: AdaptiveColor,
    ink: AdaptiveColor,
    label_key: &'static str,
}

fn finished_look(status: TransferStatus) -> FinishedLook {
    match status {
        TransferStatus::Completed => FinishedLook {
            glyph: IconName::Check,
            tint: colors::SUCCESS_TINT,
            ink: colors::SUCCESS,
            label_key: "transfer.completed",
        },
        TransferStatus::Cancelled => FinishedLook {
            glyph: IconName::Cancel,
            tint: colors::INSET,
            ink: colors::MUTED,
            label_key: "transfer.cancelled",
        },
        TransferStatus::Failed => FinishedLook {
            glyph: IconName::Warning,
            tint: colors::DANGER_TINT,
            ink: colors::DANGER,
            label_key: "transfer.failed",
        },
        TransferStatus::Active => FinishedLook {
            glyph: IconName::ArrowUp,
            tint: colors::ACCENT_TINT,
            ink: colors::ACCENT,
            label_key: "transfer.sending",
        },
    }
}

fn finished(snap: &CardMemo) -> FinishedLook {
    finished_look(snap.get().status)
}

fn label_key(snap: &CardMemo) -> &'static str {
    finished(snap).label_key
}

fn finished_subtitle(session: &Session, snap: &CardMemo) -> SharedString {
    let snapshot = snap.get();
    match snapshot.status {
        TransferStatus::Completed => snapshot
            .saved_dir
            .as_ref()
            .map(|dir| {
                fill(
                    &session.text("transfer.completed_to"),
                    &[("dir", dir.to_string())],
                )
            })
            .unwrap_or_default(),
        TransferStatus::Failed => snapshot.error.clone().unwrap_or_default(),
        _ => snapshot.label.clone(),
    }
}

// ---------------------------------------------------------------------------
// Card values
// ---------------------------------------------------------------------------

/// A transfer's monotonic clock: the bytes it had moved when the card first
/// saw it, the bytes it has moved since, the last sample, and the time during
/// which bytes actually moved.
struct Clock {
    initial: u64,
    seen: u64,
    last: Instant,
    active: Duration,
}

thread_local! {
    /// One [`Clock`] per transfer shown in the card. Monotonic (`Instant`),
    /// so a wall-clock change cannot make a speed negative or an ETA jump;
    /// the active time excludes stalls (waiting for a PIN or a decision), so
    /// idle time does not inflate the ETA.
    static CLOCKS: RefCell<HashMap<String, Clock>> = RefCell::new(HashMap::new());
}

/// The average speed of `transfer` in bytes per second, measured on the
/// monotonic clock; `None` until enough movement has happened to estimate
/// one (zero or negative speeds would only render as a nonsense ETA).
fn average_speed(transfer: &Transfer) -> Option<f64> {
    let now = Instant::now();
    let (moved, active) = CLOCKS.with(|clocks| {
        let mut clocks = clocks.borrow_mut();
        let clock = clocks.entry(transfer.id.clone()).or_insert(Clock {
            initial: transfer.transferred,
            seen: transfer.transferred,
            last: now,
            active: Duration::ZERO,
        });
        if transfer.transferred > clock.seen {
            clock.active += now.saturating_duration_since(clock.last);
            clock.seen = transfer.transferred;
        }
        clock.last = now;
        (clock.seen.saturating_sub(clock.initial), clock.active)
    });
    // Quantized to 250 ms: the design's motion table asks for speed numbers to
    // update at most 4×/s, which also keeps the text stable between ticks.
    let elapsed = (active.as_millis() / 250 * 250) as f64 / 1000.0;
    if elapsed < 0.5 || moved == 0 {
        return None;
    }
    Some(moved as f64 / elapsed)
}

fn remaining_seconds(transfer: &Transfer) -> Option<f64> {
    let rate = average_speed(transfer)?;
    if transfer.transferred >= transfer.total {
        return None;
    }
    Some((transfer.total - transfer.transferred) as f64 / rate)
}

fn fallback_peer() -> Peer {
    Peer {
        fingerprint: String::new(),
        alias: String::new(),
        device_model: String::new(),
        address: None,
        device_type: crate::net::DeviceType::Unknown,
        is_self: false,
    }
}

fn cancel_shown(session: &Session) {
    let active: Vec<String> = session.read_transfers(|state| {
        state
            .active_transfers()
            .map(|transfer| transfer.id.clone())
            .collect()
    });
    for id in active {
        session.cancel(&id);
    }
}

fn open_latest(session: &Session) {
    let path = session.read_transfers(|state| {
        state
            .active_transfers()
            .next()
            .or_else(|| state.transfers.iter().find(|transfer| recent(transfer)))
            .and_then(|transfer| transfer.saved_paths.first().cloned())
    });
    match path {
        Some(path) => session.open_file(path),
        None => session.open_downloads(),
    }
}

// ---------------------------------------------------------------------------
// Detail sheet
// ---------------------------------------------------------------------------

/// One file row of the detail sheet.
#[derive(Clone, PartialEq)]
struct FileRow {
    name: SharedString,
    size: u64,
    saved: bool,
}

/// One transfer of the detail sheet, derived from the transfers domain.
#[derive(Clone, PartialEq)]
struct SectionData {
    id: SharedString,
    direction: Direction,
    status: TransferStatus,
    label: SharedString,
    /// The peer this transfer is with, for its verification page.
    peer: Peer,
    files: Vec<FileRow>,
    /// Files beyond [`SHEET_FILES_SHOWN`] (summarized, not rendered).
    more_files: usize,
    texts: Vec<SharedString>,
    saved_dir: Option<SharedString>,
    openable: bool,
    error: Option<SharedString>,
}

/// The last derived section list and the state it came from:
/// `(language, signature)`.
type SectionCache = Rc<RefCell<Option<(Language, u64, Rc<Vec<SectionData>>)>>>;

fn mix(hash: &mut u64, value: u64) {
    *hash ^= value;
    *hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
}

/// A fingerprint of what the detail sheet displays. Progress alone never
/// changes it, so the (potentially long) file rows are derived once.
fn sheet_signature(state: &AppState) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for transfer in state.transfers.iter().take(8) {
        // The id's bytes, not its length: every UUID has the same length, so
        // eight look-alike transfers (one photo sent eight times) used to keep
        // the signature and show the previous transfer's rows.
        for byte in transfer.id.bytes() {
            mix(&mut hash, u64::from(byte));
        }
        mix(
            &mut hash,
            match transfer.status {
                TransferStatus::Active => 1,
                TransferStatus::Completed => 2,
                TransferStatus::Cancelled => 3,
                TransferStatus::Failed => 4,
            },
        );
        mix(&mut hash, transfer.file_count() as u64);
        // The cached rows also show the peer's alias (and the section carries
        // it for the verify action), so a rename must rebuild them.
        for byte in transfer.peer.fingerprint.bytes() {
            mix(&mut hash, u64::from(byte));
        }
        for byte in transfer.peer.alias.bytes() {
            mix(&mut hash, u64::from(byte));
        }
        mix(&mut hash, 0xff);
        // Only the rendered rows matter; walking all 10,000 files per
        // progress tick was wasted work.
        for file in transfer.files.iter().take(SHEET_FILES_SHOWN) {
            mix(&mut hash, u64::from(file.saved_to.is_some()));
        }
        mix(&mut hash, transfer.saved_paths.len() as u64);
        mix(&mut hash, transfer.texts.len() as u64);
    }
    hash
}

/// The derived section list behind a pointer comparison: a memo over it
/// notifies only when the cache actually produced a new list, so a progress
/// tick neither clones the sections nor walks them to compare.
#[derive(Clone)]
struct SectionList(Rc<Vec<SectionData>>);

impl PartialEq for SectionList {
    fn eq(&self, other: &Self) -> bool {
        Rc::ptr_eq(&self.0, &other.0)
    }
}

/// The section list for the current state. A signature hit returns the
/// cached `Rc` itself: handing out the handle is free, and the memo above it
/// can tell a hit from a rebuild without cloning anything.
fn sections(session: &Session, cache: &SectionCache) -> SectionList {
    let language = session.language.get();
    let signature = session.read_transfers(sheet_signature);
    if let Some((cached_language, cached_signature, sections)) = cache.borrow().as_ref()
        && *cached_language == language
        && *cached_signature == signature
    {
        return SectionList(sections.clone());
    }
    let sections = Rc::new(session.read_transfers(|state| build_sections(session, state)));
    cache.replace(Some((language, signature, sections.clone())));
    SectionList(sections)
}

fn build_sections(session: &Session, state: &AppState) -> Vec<SectionData> {
    let active: Vec<&Transfer> = state.active_transfers().collect();
    let shown: Vec<&Transfer> = if active.is_empty() {
        state
            .transfers
            .iter()
            .find(|transfer| recent(transfer))
            .into_iter()
            .collect()
    } else {
        active
    };
    let mut out = Vec::with_capacity(shown.len());
    for transfer in shown {
        let label = match transfer.direction {
            Direction::Sending => fill(
                &session.text("send.send_to"),
                &[("name", transfer.peer.alias.clone())],
            ),
            Direction::Receiving => fill(
                &session.text("receive.from"),
                &[("name", transfer.peer.alias.clone())],
            ),
        };
        let files = transfer
            .files
            .iter()
            .take(SHEET_FILES_SHOWN)
            .map(|file| FileRow {
                name: SharedString::from(file.name.clone()),
                size: file.size,
                saved: file.saved_to.is_some(),
            })
            .collect();
        let saved_dir = transfer
            .saved_paths
            .first()
            .and_then(|path| path.parent())
            .map(|parent| SharedString::from(parent.display().to_string()));
        out.push(SectionData {
            id: SharedString::from(transfer.id.clone()),
            direction: transfer.direction,
            status: transfer.status,
            label,
            peer: transfer.peer.clone(),
            files,
            more_files: transfer.file_count().saturating_sub(SHEET_FILES_SHOWN),
            texts: transfer
                .texts
                .iter()
                .map(|text| SharedString::from(text.clone()))
                .collect(),
            saved_dir,
            openable: !transfer.saved_paths.is_empty(),
            error: transfer.error.clone().map(SharedString::from),
        });
    }
    out
}

/// The sheet's height: a header plus the rows it lists, so one file does not
/// leave a tall empty card.
fn sheet_height(sections: &[SectionData]) -> f32 {
    let mut height: f32 = 96.0;
    for section in sections {
        height += 48.0 + section.files.len() as f32 * 30.0;
        if section.more_files > 0 {
            height += 30.0;
        }
        height += section.texts.len() as f32 * 34.0;
        if section.status != TransferStatus::Active {
            height += 36.0;
        }
    }
    height.clamp(240.0, (window().height.0 - 160.0).clamp(240.0, 560.0))
}

fn transfer_sheet(session: Session) -> impl IntoElement {
    let ui = ui();
    let cache: SectionCache = Rc::new(RefCell::new(None));
    // One memo for the sections: the height binding and the list both read
    // it, and both only re-run when the cached list changes (not per tick).
    let sections_memo = computed({
        let session = session.clone();
        move || sections(&session, &cache)
    });
    let height_sections = sections_memo;
    let row_sections = sections_memo;
    let title = session.clone();
    let close_button = session.clone();
    overlay()
        .inset_0()
        .bg(colors::SCRIM)
        .items_center()
        .justify_center()
        .on_press(move || {
            ui.update(|state| {
                if dismissable(state) {
                    state.open = false;
                }
            })
        })
        .on_cancel(move || ui.update(|state| state.open = false))
        .child(
            view()
                .test_id("transfer-sheet")
                .w(px(control::SHEET_WIDE))
                .h(move || px(sheet_height(height_sections.get().0.as_slice())))
                .flex_col()
                .rounded(px(radius::SHEET))
                .border_1()
                .border_color(colors::BORDER)
                .bg(colors::SURFACE)
                .shadow(floating_shadow())
                // Presses on the sheet's own background stay here instead of
                // reaching the scrim (buttons inside are closer targets).
                .on_press(move || {})
                .child(
                    view()
                        .flex_row()
                        .items_center()
                        .gap(px(space::SM))
                        .p(px(space::LG))
                        .pb(px(space::SM))
                        .child(
                            text(move || title.text("transfer.details"))
                                .text_size(px(text_size::TITLE))
                                .font_weight(FontWeight::SEMIBOLD),
                        )
                        .child(view().flex_1())
                        .child(
                            icon_button(IconName::Close)
                                .label(close_button.text("common.close"))
                                .on_press(move || ui.update(|state| state.open = false)),
                        ),
                )
                .child(
                    scroll().flex_1().scrollbar(false).child(
                        view()
                            .flex_col()
                            .gap(px(space::SM))
                            .px(px(space::LG))
                            .pb(px(space::LG))
                            .child(each(
                                move || (*row_sections.get().0).clone(),
                                |section: &SectionData| section.id.clone(),
                                move |section, _index| {
                                    transfer_section(session.clone(), section.get())
                                },
                            )),
                    ),
                ),
        )
}

fn transfer_section(session: Session, data: SectionData) -> impl IntoElement {
    let ui = ui();
    let badge_session = session.clone();
    let verify = session.clone();
    let verify_peer = data.peer.clone();
    let cancel = session.clone();
    let reveal = session.clone();
    let reveal_label = session.clone();
    let failed = session.clone();
    let completed = session.clone();
    let open = session.clone();
    let label = data.label;
    let direction = data.direction;
    let status = data.status;
    let files = data.files;
    let more_files = data.more_files;
    let more_label = session.clone();
    let texts = data.texts;
    let saved_dir = data.saved_dir;
    let openable = data.openable;
    let error = data.error;
    let id = data.id;
    let cancel_id = id.clone();
    let open_id = id.clone();

    view()
        .test_id("transfer-section")
        .flex_col()
        .gap(px(space::XS))
        .p(px(space::SM))
        .rounded(px(radius::CONTROL))
        .border_1()
        .border_color(colors::BORDER)
        .child(
            view()
                .flex_row()
                .items_center()
                .gap(px(space::SM))
                .child(
                    icon(if direction == Direction::Receiving {
                        IconName::ArrowDown
                    } else {
                        IconName::ArrowUp
                    })
                    .size(15.0)
                    .color(if direction == Direction::Receiving {
                        colors::SUCCESS
                    } else {
                        colors::ACCENT
                    }),
                )
                .child(
                    text(label)
                        .text_size(px(text_size::BODY))
                        .font_weight(FontWeight::SEMIBOLD)
                        .truncate(),
                )
                .child(view().flex_1())
                .child(status_badge(badge_session, status))
                // The phone's Verify page is what the transfer waits on, so
                // the action lives here too, not only next to the device.
                .child(
                    button(session.text("verify.action"))
                        .kind(ButtonKind::Ghost)
                        .icon(IconName::Verified)
                        .on_press(move || {
                            // The sheet would sit behind this modal one.
                            ui.update(|state| state.open = false);
                            verify.open_verify(verify_peer.clone());
                        }),
                )
                .child(when(status == TransferStatus::Active, {
                    let cancel = cancel.clone();
                    move || {
                        let cancel = cancel.clone();
                        let cancel_id = cancel_id.clone();
                        icon_button(IconName::Cancel)
                            .label(cancel.text("transfer.cancel"))
                            .on_press(move || cancel.cancel(&cancel_id))
                    }
                })),
        )
        .children(files.iter().map(|file| {
            let saved = file.saved;
            let reveal = reveal.clone();
            let reveal_label = reveal_label.clone();
            let reveal_id = id.clone();
            let row = file_row(IconName::File, file.name.clone(), file.size);
            if saved {
                row.reveal(
                    move || reveal_label.text("transfer.reveal"),
                    move || reveal_transfer_file(&reveal, &reveal_id),
                )
            } else {
                row
            }
            .into_element()
        }))
        .child(when(more_files > 0, move || {
            text(count_text(
                &more_label,
                "receive.more_one",
                "receive.more",
                more_files,
            ))
            .text_size(px(text_size::CAPTION))
            .text_color(colors::MUTED)
            .px(px(space::SM))
            .py(px(space::XS))
        }))
        .children(texts.into_iter().map(text_row_preview))
        .child(when(status == TransferStatus::Failed, move || {
            let failed = failed.clone();
            view()
                .flex_col()
                .gap(px(2.0))
                .px(px(space::SM))
                .py(px(space::XS))
                .child(
                    text(failed.text("transfer.failed"))
                        .text_size(px(text_size::CAPTION))
                        .text_color(colors::DANGER),
                )
                .child(
                    text(error.clone().unwrap_or_default())
                        .text_size(px(text_size::CAPTION))
                        .text_color(colors::MUTED)
                        .number_of_lines(2)
                        .truncate(),
                )
        }))
        .child(when(
            status == TransferStatus::Completed && openable,
            move || {
                let completed = completed.clone();
                let open = open.clone();
                let open_id = open_id.clone();
                let dir = saved_dir.clone().unwrap_or_default();
                view()
                    .flex_row()
                    .items_center()
                    .gap(px(space::SM))
                    .child(
                        text(fill(
                            &completed.text("transfer.completed_to"),
                            &[("dir", dir.to_string())],
                        ))
                        .text_size(px(text_size::CAPTION))
                        .text_color(colors::MUTED)
                        .number_of_lines(1)
                        .text_overflow(nsh_core::EllipsizeMode::Middle)
                        .flex_1()
                        .min_w(px(0.0)),
                    )
                    .child(
                        button(move || completed.text("transfer.open"))
                            .kind(ButtonKind::Secondary)
                            .small()
                            .on_press(move || open_saved(&open, &open_id)),
                    )
            },
        ))
}

/// The saved path of transfer `id`, read when the press happens (a row's body
/// must not read the session, or every row rebuilds on every progress event).
fn saved_path(session: &Session, id: &str) -> Option<std::path::PathBuf> {
    session
        .state
        .borrow()
        .transfer(id)
        .and_then(|transfer| transfer.saved_paths.first().cloned())
}

fn reveal_transfer_file(session: &Session, id: &str) {
    if let Some(path) = saved_path(session, id) {
        session.reveal_file(path);
    }
}

fn open_saved(session: &Session, id: &str) {
    if let Some(path) = saved_path(session, id) {
        session.open_file(path);
    }
}

fn text_row_preview(body: SharedString) -> impl IntoElement {
    view()
        .flex_row()
        .items_start()
        .gap(px(space::SM))
        .px(px(space::SM))
        .py(px(space::XS))
        .child(icon(IconName::Text).size(15.0).color(colors::MUTED))
        .child(
            text(body)
                .text_size(px(text_size::SMALL))
                .text_color(colors::MUTED)
                .number_of_lines(2)
                .truncate(),
        )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn running(transferred: u64, total: u64, started_at: u64) -> Transfer {
        Transfer {
            id: "id".into(),
            peer: fallback_peer(),
            direction: Direction::Sending,
            files: Vec::new(),
            total,
            transferred,
            status: TransferStatus::Active,
            error: None,
            saved_paths: Vec::new(),
            texts: Vec::new(),
            started_at,
            finished_at: None,
            omitted_files: 0,
        }
    }

    #[test]
    fn only_a_completed_transfer_reads_as_success() {
        assert_eq!(
            finished_look(TransferStatus::Completed).label_key,
            "transfer.completed"
        );
        assert_eq!(
            finished_look(TransferStatus::Cancelled).label_key,
            "transfer.cancelled"
        );
        assert_eq!(
            finished_look(TransferStatus::Failed).label_key,
            "transfer.failed"
        );
        assert_ne!(
            finished_look(TransferStatus::Cancelled).tint,
            finished_look(TransferStatus::Completed).tint
        );
    }

    #[test]
    fn remaining_time_is_unknown_without_movement_or_bytes_left() {
        // No total: no division by zero, no ETA.
        assert_eq!(remaining_seconds(&running(0, 0, 0)), None);
        // Already done: no time left even with an old start.
        assert_eq!(remaining_seconds(&running(10, 10, 0)), None);
        // Nothing moved and started just now: no rate estimate yet.
        assert_eq!(remaining_seconds(&running(0, 10, now_ms())), None);
        // A wall clock behind the transfer's start cannot produce a negative
        // or absurd ETA: the rate comes from the monotonic clock and stays
        // unknown until bytes actually move.
        assert_eq!(
            remaining_seconds(&running(1, 10_000_000_000, now_ms() + 3_600_000)),
            None
        );
    }

    #[test]
    fn speed_is_measured_on_a_monotonic_clock() {
        let mut transfer = running(0, 10_000, now_ms());
        assert_eq!(remaining_seconds(&transfer), None, "no movement yet");
        std::thread::sleep(Duration::from_millis(600));
        transfer.transferred = 1_000;
        let eta = remaining_seconds(&transfer).expect("movement gives a rate");
        assert!(eta.is_finite() && eta > 0.0, "eta {eta}");
        // Waiting (no new bytes) adds no active time: the estimate must not
        // drift upward just because the card kept ticking.
        std::thread::sleep(Duration::from_millis(300));
        let unchanged = remaining_seconds(&transfer).expect("still a rate");
        assert!(unchanged <= eta + 1.0, "{unchanged} vs {eta}");
    }

    #[test]
    fn progress_clamps_and_zero_total_is_zero() {
        assert_eq!(crate::model::Transfer::progress(&running(0, 0, 0)), 0.0);
        assert_eq!(crate::model::Transfer::progress(&running(5, 10, 0)), 0.5);
        // A bogus event reporting more than the total cannot overflow the bar.
        assert_eq!(crate::model::Transfer::progress(&running(15, 10, 0)), 1.0);
    }
}
