//! The History screen: finished and running transfers grouped by day, with
//! per-row actions and a destructive clear action behind a confirmation.
//!
//! The list is an `each` over a derived, keyed row model. The model is built
//! from the transfers domain revision only, and behind a cheap signature
//! cache: a running transfer bumps that revision up to ~20×/s (one progress
//! event per 50 ms) but can never change a finished row, so the 100-row
//! derivation only reruns when a row's displayed fields change.
//!
//! Rows also do not read the session in their body: the actions (Open / Show
//! in Finder / Copy text) resolve their path or text when pressed. A
//! body-level `session.read(..)` would make every visible row rebuild on
//! every progress event.

use crate::i18n::Language;
use crate::model::{AppState, Session, Transfer, TransferStatus};
use crate::net::Direction;
use crate::ui::format::bytes as fmt_bytes;
use crate::ui::format::day_label_in;
use crate::ui::format::local_day;
use crate::ui::{
    BadgeTone, DayLabel, IconName, badge, clock, colors, control, icon, icon_button, radius, space,
    text_size,
};
use nsh_core::prelude::*;
use std::cell::RefCell;
use std::rc::Rc;
use std::time::Duration;

use super::send::count_text;

/// The History destination.
pub fn history(session: Session) -> impl IntoElement {
    let title = session.clone();
    let clear = session.clone();
    let has_rows = session.clone();
    let list = session.clone();
    // Day headings ("Today"/"Yesterday") depend on the wall clock, not only
    // on the transfers: a minute tick recomputes them after midnight.
    let today = signal(local_day());
    set_interval(Duration::from_secs(60), move || today.set(local_day()));
    view()
        .test_id("history")
        .flex_1()
        .min_w(px(0.0))
        .flex_col()
        .p(px(space::GUTTER))
        .gap(px(space::MD))
        .text_color(colors::TEXT)
        .child(
            view()
                .flex_row()
                .items_center()
                .gap(px(space::SM))
                .child(
                    text(move || title.text("history.title"))
                        .text_size(px(text_size::DISPLAY))
                        .font_weight(FontWeight::SEMIBOLD),
                )
                .child(view().flex_1())
                .child(
                    icon_button(IconName::Trash)
                        .label(session.text("history.clear"))
                        .color(colors::DANGER)
                        .disabled(move || {
                            !has_rows.read_transfers(|state| {
                                state.transfers.iter().any(|t| !t.is_active())
                            })
                        })
                        .on_press(move || confirm_clear(&clear)),
                ),
        )
        .child(
            when(move || !has_finished(&list), {
                let session = session.clone();
                move || empty_state(session.clone())
            })
            .otherwise({
                let session = session.clone();
                move || grouped_list(session.clone(), today)
            }),
        )
}

fn empty_state(session: Session) -> impl IntoElement {
    let title = session.clone();
    let hint = session.clone();
    view()
        .test_id("history-empty")
        .flex_1()
        .items_center()
        .justify_center()
        .gap(px(space::SM))
        .child(
            view()
                .size(px(44.0))
                .items_center()
                .justify_center()
                .rounded(px(radius::TILE))
                .bg(colors::INSET)
                .child(icon(IconName::History).size(22.0).color(colors::MUTED)),
        )
        .child(
            text(move || title.text("history.empty"))
                .text_size(px(text_size::TITLE))
                .font_weight(FontWeight::SEMIBOLD),
        )
        .child(
            text(move || hint.text("history.empty_hint"))
                .text_size(px(text_size::SMALL))
                .text_color(colors::MUTED),
        )
}

/// Asks before deleting every finished transfer, then clears them.
fn confirm_clear(session: &Session) {
    let options = nsh_base::alert_dialog()
        .title(session.text("history.clear_confirm"))
        .description(session.text("history.clear_confirm_hint"))
        .cancel("cancel", session.text("common.cancel"))
        .destructive("clear", session.text("history.clear"));
    let this = (*session).clone();
    let task = this.clone();
    this.task(async move {
        if let Ok(result) = options.show().await
            && result.action.as_deref() == Some("clear")
        {
            task.clear_history();
        }
    });
}

// ---------------------------------------------------------------------------
// Grouped rows
// ---------------------------------------------------------------------------

/// One line of the list: a day heading or a transfer.
///
/// The key is stored with the row, so re-deriving the list does not build a
/// `String` per row for `each`'s key lookup.
#[derive(Clone, PartialEq)]
struct Entry {
    key: SharedString,
    kind: EntryKind,
}

#[derive(Clone, PartialEq)]
enum EntryKind {
    Day { label: SharedString },
    Transfer(RowData),
}

fn entry_key(entry: &Entry) -> SharedString {
    entry.key.clone()
}

/// The fields a transfer row shows, copied out of a [`Transfer`] once.
///
/// A full `Transfer` carries every offered file, saved path and text block;
/// cloning it per row per change is what made the list expensive. Paths and
/// text are resolved from the state when an action is pressed.
#[derive(Clone, PartialEq)]
pub(crate) struct RowData {
    pub id: SharedString,
    pub alias: SharedString,
    pub direction: Direction,
    pub status: TransferStatus,
    pub files: usize,
    pub total: u64,
    pub started_at: u64,
    /// A file was saved, so Open can show it.
    pub openable: bool,
    /// There is received text or a file list to copy.
    pub copyable: bool,
}

impl RowData {
    pub(crate) fn from_transfer(transfer: &Transfer) -> Self {
        Self {
            id: transfer.id.clone().into(),
            alias: transfer.peer.alias.clone().into(),
            direction: transfer.direction,
            status: transfer.status,
            files: transfer.file_count(),
            total: transfer.total,
            started_at: transfer.started_at,
            openable: !transfer.saved_paths.is_empty(),
            copyable: !transfer.files.is_empty() || !transfer.texts.is_empty(),
        }
    }
}

/// The last derived list and the state it came from: `(language, local day,
/// signature)`. Seeing the same signature again reuses the rows instead of
/// re-deriving them (progress updates change neither the signature nor the
/// rows), while a day change still refreshes the "Today"/"Yesterday" labels.
type EntryCache = Rc<RefCell<Option<(Language, i32, u64, Rc<Vec<Entry>>)>>>;

fn has_finished(session: &Session) -> bool {
    session.read_transfers(|state| state.transfers.iter().any(|transfer| !transfer.is_active()))
}

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

/// A cheap fingerprint of what the finished rows display. Uses the transfers
/// domain revision for scheduling and only re-derives when this changes.
fn history_signature(state: &AppState) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for transfer in state
        .transfers
        .iter()
        .filter(|transfer| !transfer.is_active())
    {
        mix_str(&mut hash, &transfer.id);
        mix_str(&mut hash, &transfer.peer.alias);
        mix(
            &mut hash,
            match transfer.direction {
                Direction::Sending => 1,
                Direction::Receiving => 2,
            },
        );
        mix(
            &mut hash,
            match transfer.status {
                TransferStatus::Active => 0,
                TransferStatus::Completed => 3,
                TransferStatus::Cancelled => 4,
                TransferStatus::Failed => 5,
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

/// The derived list behind a pointer comparison: a memo over it notifies
/// only when the cache produced a new list, so a progress tick neither clones
/// the rows nor walks them to compare.
#[derive(Clone)]
struct EntryList(Rc<Vec<Entry>>);

impl PartialEq for EntryList {
    fn eq(&self, other: &Self) -> bool {
        Rc::ptr_eq(&self.0, &other.0)
    }
}

/// The rows for the current state. A signature hit returns the cached `Rc`
/// itself: handing out the handle is free, and the memo above it can tell a
/// hit from a rebuild without cloning anything.
fn entries(session: &Session, cache: &EntryCache) -> EntryList {
    let language = session.language.get();
    let day = local_day();
    let signature = session.read_transfers(history_signature);
    if let Some((cached_language, cached_day, cached_signature, entries)) = cache.borrow().as_ref()
        && *cached_language == language
        && *cached_day == day
        && *cached_signature == signature
    {
        return EntryList(entries.clone());
    }
    let entries = Rc::new(session.read_transfers(|state| build_entries(state, language)));
    cache.replace(Some((language, day, signature, entries.clone())));
    EntryList(entries)
}

fn build_entries(state: &AppState, language: Language) -> Vec<Entry> {
    let mut entries = Vec::with_capacity(state.transfers.len() + 2);
    let mut last_day: Option<String> = None;
    // Running transfers belong to the transfer card, not to history.
    for transfer in state
        .transfers
        .iter()
        .filter(|transfer| !transfer.is_active())
    {
        let day = day_label_in(transfer.started_at, language);
        let key = match &day {
            DayLabel::Today => "today".to_string(),
            DayLabel::Yesterday => "yesterday".to_string(),
            DayLabel::Date(date) => date.to_string(),
        };
        if last_day.as_deref() != Some(key.as_str()) {
            let label = day.label_in(language);
            entries.push(Entry {
                key: format!("day:{key}").into(),
                kind: EntryKind::Day { label },
            });
            last_day = Some(key);
        }
        entries.push(Entry {
            key: format!("transfer:{}", transfer.id).into(),
            kind: EntryKind::Transfer(RowData::from_transfer(transfer)),
        });
    }
    entries
}

fn grouped_list(session: Session, today: Signal<i32>) -> impl IntoElement {
    let rows = session.clone();
    let cache: EntryCache = Rc::new(RefCell::new(None));
    // One memo for the list: the `each` below only re-runs when the cached
    // rows actually change, so a progress tick does not clone the 100 rows.
    // The day signal is read here so the midnight tick reaches the cache key.
    let list = computed(move || {
        let _ = today.get();
        entries(&rows, &cache)
    });
    let each_list = list;
    view()
        .flex_1()
        .min_h(px(0.0))
        .child(scroll().flex_1().scrollbar(false).child(
            view().flex_col().gap(px(space::XS)).child(each(
                move || (*each_list.get().0).clone(),
                entry_key,
                move |entry, _index| match entry.get().kind {
                    EntryKind::Day { label } => day_heading(label).into_element(),
                    EntryKind::Transfer(row) => {
                        transfer_row(session.clone(), row, true).into_element()
                    }
                },
            )),
        ))
}

fn day_heading(label: SharedString) -> impl IntoElement {
    text(label)
        .text_size(px(text_size::CAPTION))
        .font_weight(FontWeight::SEMIBOLD)
        .text_color(colors::MUTED)
        .uppercase()
        .px(px(space::MD))
        .pt(px(space::SM))
        .pb(px(space::XS))
}

// ---------------------------------------------------------------------------
// One transfer row (also used by the Receive screen's recent list)
// ---------------------------------------------------------------------------

/// A 56 pt transfer row: direction glyph, peer, file summary, status and time.
///
/// `actions` shows the hover actions (Open / Show in Finder / Copy text); the
/// Receive screen passes `false` for its quiet recent list.
pub(crate) fn transfer_row(session: Session, row: RowData, actions: bool) -> impl IntoElement {
    let received = row.direction == Direction::Receiving;
    let status = row.status;
    let files = row.files;
    let total = row.total;
    let started = row.started_at;
    let alias = row.alias;
    let id = row.id;
    let openable = row.openable;
    let copyable = row.copyable;

    let count = session.clone();

    view()
        .test_id("history-row")
        .group_named("history-row")
        .h(px(control::TRANSFER_ROW))
        .flex_row()
        .items_center()
        .gap(px(space::MD))
        .px(px(space::MD))
        .rounded(px(radius::CONTROL))
        .border_1()
        .border_color(colors::BORDER)
        .bg(colors::SURFACE)
        .shadow(crate::ui::card_shadow())
        .dark(|style| style.shadow_none())
        .hover(|style| style.bg(colors::HOVER))
        .child(
            view()
                .size(px(30.0))
                .flex_shrink_0()
                .items_center()
                .justify_center()
                .rounded_full()
                .bg(if received {
                    colors::SUCCESS_TINT
                } else {
                    colors::ACCENT_TINT
                })
                .child(
                    icon(if received {
                        IconName::ArrowDown
                    } else {
                        IconName::ArrowUp
                    })
                    .size(15.0)
                    .color(if received {
                        colors::SUCCESS
                    } else {
                        colors::ACCENT
                    }),
                ),
        )
        .child(
            view()
                .flex_col()
                .flex_1()
                .min_w(px(0.0))
                .gap(px(2.0))
                .child(
                    text(alias)
                        .text_size(px(text_size::BODY))
                        .font_weight(FontWeight::SEMIBOLD)
                        .truncate(),
                )
                .child(
                    text(move || count_text(&count, "transfer.file_one", "transfer.files", files))
                        .text_size(px(text_size::CAPTION))
                        .text_color(colors::MUTED)
                        .truncate()
                        .child(" · ")
                        .child(text(fmt_bytes(total))),
                ),
        )
        .child(status_badge(session.clone(), status))
        .child(
            text(clock(started))
                .text_size(px(text_size::CAPTION))
                .text_color(colors::FAINT)
                .tabular_nums(),
        )
        .child(when(actions, {
            let open = session.clone();
            let reveal = session.clone();
            let copy = session.clone();
            let labels = session.clone();
            move || {
                let (open, reveal, copy) = (open.clone(), reveal.clone(), copy.clone());
                let labels = labels.clone();
                let row_id = id.clone();
                view()
                    .flex_row()
                    .items_center()
                    .gap(px(2.0))
                    .opacity(0.0)
                    .group_hover_named("history-row", |style| style.opacity(1.0))
                    // Keyboard users tab through the same three buttons: make
                    // the group visible when the focus is inside it, not only
                    // on pointer hover.
                    .focus_within(|style| style.opacity(1.0))
                    .transition_opacity()
                    .duration(120.0)
                    .child(when(openable, {
                        let open = open.clone();
                        let id = row_id.clone();
                        let label = labels.text("transfer.open");
                        move || {
                            let open = open.clone();
                            let id = id.clone();
                            icon_button(IconName::ExternalLink)
                                .label(label.clone())
                                .on_press(move || open_saved(&open, &id))
                        }
                    }))
                    .child({
                        let reveal = reveal.clone();
                        let id = row_id.clone();
                        let label = labels.text("transfer.reveal");
                        icon_button(IconName::Folder)
                            .label(label)
                            .on_press(move || reveal_transfer(&reveal, &id))
                    })
                    .child(when(copyable, {
                        let copy = copy.clone();
                        let id = row_id.clone();
                        let label = labels.text("transfer.copy_text");
                        move || {
                            let copy = copy.clone();
                            let id = id.clone();
                            icon_button(IconName::Copy)
                                .label(label.clone())
                                .on_press(move || copy_transfer(&copy, &id))
                        }
                    }))
            }
        }))
}

/// The saved path of the transfer `id`, if it has one.
fn saved_path(session: &Session, id: &str) -> Option<std::path::PathBuf> {
    session
        .state
        .borrow()
        .transfer(id)
        .and_then(|transfer| transfer.saved_paths.first().cloned())
}

fn open_saved(session: &Session, id: &str) {
    if let Some(path) = saved_path(session, id) {
        session.open_file(path);
    }
}

fn reveal_transfer(session: &Session, id: &str) {
    // Finished transfers reveal their file, else the download folder, which
    // is read when pressed so a row's body stays free of session reads.
    let path = saved_path(session, id)
        .unwrap_or_else(|| session.state.borrow().settings.downloads.clone());
    session.reveal_file(path);
}

fn copy_transfer(session: &Session, id: &str) {
    let text = {
        let state = session.state.borrow();
        let Some(transfer) = state.transfer(id) else {
            return;
        };
        if transfer.texts.is_empty() {
            transfer
                .files
                .iter()
                .map(|file| file.name.as_str())
                .collect::<Vec<_>>()
                .join("\n")
        } else {
            transfer.texts.join("\n")
        }
    };
    if !text.is_empty() {
        session.copy_text(text);
    }
}

/// A small pill for a transfer's state.
pub(crate) fn status_badge(session: Session, status: TransferStatus) -> impl IntoElement {
    let (key, tone) = match status {
        TransferStatus::Active => ("status.transferring", BadgeTone::Accent),
        TransferStatus::Completed => ("status.completed", BadgeTone::Success),
        TransferStatus::Cancelled => ("status.cancelled", BadgeTone::Neutral),
        TransferStatus::Failed => ("status.failed", BadgeTone::Danger),
    };
    badge(move || session.text(key)).tone(tone)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::TransferStatus;
    use crate::net::{DeviceType, Direction, OfferedFile, Peer};

    fn transfer(
        id: &str,
        direction: Direction,
        status: TransferStatus,
        files: usize,
        saved: bool,
    ) -> Transfer {
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
            direction,
            files: (0..files)
                .map(|index| OfferedFile {
                    name: format!("file-{index}.bin"),
                    size: 10,
                    mime: "application/octet-stream".into(),
                    saved_to: None,
                })
                .collect(),
            total: 10 * files as u64,
            transferred: 10 * files as u64,
            status,
            error: None,
            saved_paths: saved
                .then(|| "/tmp/file-0.bin".into())
                .into_iter()
                .collect(),
            texts: Vec::new(),
            started_at: 1,
            finished_at: Some(2),
            omitted_files: 0,
        }
    }

    #[test]
    fn row_data_copies_the_displayed_fields() {
        let row = RowData::from_transfer(&transfer(
            "t1",
            Direction::Receiving,
            TransferStatus::Completed,
            3,
            true,
        ));
        assert_eq!(row.id.as_str(), "t1");
        assert_eq!(row.alias.as_str(), "Peer");
        assert_eq!(row.files, 3);
        assert_eq!(row.total, 30);
        assert!(row.openable);
        assert!(row.copyable);
        assert_eq!(row.status, TransferStatus::Completed);
    }

    #[test]
    fn signature_ignores_progress_of_a_running_transfer() {
        let mut state = AppState::default();
        let mut running = transfer(
            "active",
            Direction::Receiving,
            TransferStatus::Active,
            2,
            false,
        );
        let done = transfer(
            "done",
            Direction::Sending,
            TransferStatus::Completed,
            1,
            true,
        );
        state.transfers = vec![running.clone(), done];
        let before = history_signature(&state);
        // A progress event changes only transferred/total; finished rows do
        // not move, so the derived list need not be rebuilt.
        running.transferred = 5;
        running.total = 20;
        state.transfers[0] = running;
        assert_eq!(history_signature(&state), before);
        // A finished row's status change does change the signature.
        state.transfers[1].status = TransferStatus::Failed;
        assert_ne!(history_signature(&state), before);
    }

    #[test]
    fn entries_group_finished_transfers_by_day() {
        let mut state = AppState::default();
        let mut first = transfer("a", Direction::Sending, TransferStatus::Completed, 1, true);
        let mut second = transfer(
            "b",
            Direction::Receiving,
            TransferStatus::Completed,
            2,
            false,
        );
        // Two days: `build_entries` uses the local clock, so put them far
        // apart instead of depending on the current time of day.
        first.started_at = 0;
        second.started_at = crate::net::now_ms();
        state.transfers = vec![second, first];
        let entries = build_entries(&state, Language::En);
        let days = entries
            .iter()
            .filter(|entry| matches!(entry.kind, EntryKind::Day { .. }))
            .count();
        assert_eq!(days, 2);
        assert_eq!(entries.len(), 4);
        assert_eq!(entries[0].key.as_str(), "day:today");
        assert_eq!(entries[1].key.as_str(), "transfer:b");
        assert_eq!(entries[2].key.as_str(), "day:Jan 1");
        assert_eq!(entries[3].key.as_str(), "transfer:a");
    }

    #[test]
    fn keys_are_unique_per_row() {
        let state = AppState {
            transfers: vec![
                transfer("a", Direction::Sending, TransferStatus::Completed, 1, true),
                transfer("b", Direction::Sending, TransferStatus::Completed, 1, true),
            ],
            ..AppState::default()
        };
        let entries = build_entries(&state, Language::En);
        let transfer_keys: Vec<_> = entries
            .iter()
            .filter(|entry| matches!(entry.kind, EntryKind::Transfer(_)))
            .map(|entry| entry.key.as_str().to_owned())
            .collect();
        assert_eq!(transfer_keys, vec!["transfer:a", "transfer:b"]);
    }
}
