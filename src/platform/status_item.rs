//! The menu bar (status bar) item: LocalSend stays reachable after its window
//! closes.
//!
//! The design follows the CloudShuttle status menu: a summary line (nearby
//! devices), a status line (ready, a pending request or the transfers in
//! flight with their progress), the most recent transfers, "Open LocalSend"
//! and Quit (⌘Q). While the window is hidden the app leaves the Dock (AppKit's
//! accessory policy), so the menu bar icon is how one gets back to it; opening
//! the window brings the Dock icon back.
//!
//! nsh 0.4 has no status item API, so the macOS part talks to AppKit directly
//! (`objc2`). It never calls into nsh from an AppKit callback: a menu click
//! only puts a [`TrayAction`] into a channel, and a session task performs it
//! on the engine thread, through the same [`Session`] commands the views use.
//! What the menu shows is computed by the pure [`menu_rows`], which the
//! tests cover; the AppKit side only renders rows.
//!
//! Other platforms: [`install`] does nothing (nsh 0.4 offers no tray there).

use crate::i18n::{self, Language, fill};
use crate::model::{AppState, Session, Transfer, TransferStatus, View};
use crate::net::Direction;
use crate::ui::format;
use crate::ui::icons::IconName;

/// Transfers in flight listed one by one; more are summarized in one row.
const ACTIVE_ROWS: usize = 3;

/// Finished transfers listed under "Recent transfers".
const RECENT_ROWS: usize = 5;

/// Longest device name shown in a row, in display columns (the menu would grow
/// as wide as the longest peer-chosen alias otherwise).
const NAME_COLUMNS: usize = 32;

/// What a menu row does when it is clicked.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TrayAction {
    /// Show the window, optionally on a given screen.
    Open(Option<View>),
    /// Quit (with the usual confirmation while a transfer runs).
    Quit,
}

/// The icon of a row.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TrayIcon {
    Devices,
    Ready,
    Offline,
    Request,
    Sending,
    Receiving,
    Recent,
    Failed,
    Cancelled,
    Open,
}

impl TrayIcon {
    /// The app's own glyph for this icon (`ui::icons`), so the menu and the
    /// window share one icon set. SF Symbols are not used: AppKit draws no
    /// image for them in this menu on current macOS.
    pub fn glyph(self) -> IconName {
        match self {
            TrayIcon::Devices => IconName::Laptop,
            TrayIcon::Ready => IconName::Check,
            TrayIcon::Offline | TrayIcon::Failed => IconName::Warning,
            TrayIcon::Request => IconName::Receive,
            TrayIcon::Sending => IconName::ArrowUp,
            TrayIcon::Receiving => IconName::ArrowDown,
            TrayIcon::Recent => IconName::History,
            TrayIcon::Cancelled => IconName::Cancel,
            TrayIcon::Open => IconName::Send,
        }
    }
}

/// One row of the status menu.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TrayRow {
    /// A line of text. Rows without an action are shown dimmed, as headings
    /// and details are.
    Item {
        /// Identifies the row across updates, so a progress change only
        /// retitles the open menu instead of rebuilding it.
        key: String,
        label: String,
        icon: Option<TrayIcon>,
        action: Option<TrayAction>,
    },
    Separator,
}

impl TrayRow {
    fn item(
        key: impl Into<String>,
        label: impl Into<String>,
        icon: Option<TrayIcon>,
        action: Option<TrayAction>,
    ) -> Self {
        TrayRow::Item {
            key: key.into(),
            label: label.into(),
            icon,
            action,
        }
    }

    /// The row's identity (separators are told apart by position).
    pub fn key(&self) -> &str {
        match self {
            TrayRow::Item { key, .. } => key,
            TrayRow::Separator => "-",
        }
    }
}

/// Everything the status menu shows, in order.
///
/// `online` is whether the network service is running; `language` picks the
/// strings. Pure, so the tests check the menu without AppKit.
pub fn menu_rows(state: &AppState, online: bool, language: Language) -> Vec<TrayRow> {
    let text = |key: &str| i18n::text(language, key);
    let mut rows = Vec::new();

    let nearby = state
        .devices
        .iter()
        .filter(|peer| !peer.is_self && peer.address.is_some())
        .count();
    let devices = match nearby {
        0 => text("tray.devices_none").to_string(),
        1 => text("tray.devices_one").to_string(),
        count => fill(text("tray.devices"), &[("count", count.to_string())]),
    };
    rows.push(TrayRow::item(
        "devices",
        devices,
        Some(TrayIcon::Devices),
        None,
    ));

    let active: Vec<&Transfer> = state.transfers.iter().filter(|t| t.is_active()).collect();
    if !online {
        rows.push(TrayRow::item(
            "status",
            text("tray.offline"),
            Some(TrayIcon::Offline),
            None,
        ));
    } else if let Some(request) = &state.incoming {
        // The request sheet is in the window: the row opens it.
        rows.push(TrayRow::item(
            "request",
            fill(
                text("tray.request"),
                &[("name", short_name(&request.peer.alias))],
            ),
            Some(TrayIcon::Request),
            Some(TrayAction::Open(None)),
        ));
    } else if active.is_empty() {
        let key = if state.settings.discoverable {
            "tray.ready"
        } else {
            "tray.hidden"
        };
        rows.push(TrayRow::item(
            "status",
            text(key),
            Some(TrayIcon::Ready),
            None,
        ));
    }
    for transfer in active.iter().take(ACTIVE_ROWS) {
        let percent = format!("{}%", (transfer.progress() * 100.0).round() as u32);
        rows.push(TrayRow::item(
            format!("active/{}", transfer.id),
            transfer_line(transfer, &percent, language),
            Some(direction_icon(transfer.direction)),
            Some(TrayAction::Open(None)),
        ));
        rows.push(TrayRow::item(
            format!("progress/{}", transfer.id),
            format!(
                "{} / {}",
                format::bytes(transfer.transferred),
                format::bytes(transfer.total)
            ),
            None,
            None,
        ));
    }
    if active.len() > ACTIVE_ROWS {
        rows.push(TrayRow::item(
            "more",
            fill(
                text("tray.more"),
                &[("count", (active.len() - ACTIVE_ROWS).to_string())],
            ),
            None,
            Some(TrayAction::Open(None)),
        ));
    }

    rows.push(TrayRow::Separator);
    rows.push(TrayRow::item(
        "recent",
        text("tray.recent"),
        Some(TrayIcon::Recent),
        None,
    ));
    let recent: Vec<&Transfer> = state
        .transfers
        .iter()
        .filter(|transfer| !transfer.is_active())
        .take(RECENT_ROWS)
        .collect();
    if recent.is_empty() {
        rows.push(TrayRow::item(
            "no-recent",
            text("tray.recent_empty"),
            None,
            None,
        ));
    }
    for transfer in recent {
        let status = text(match transfer.status {
            TransferStatus::Completed => "status.completed",
            TransferStatus::Cancelled => "status.cancelled",
            TransferStatus::Failed => "status.failed",
            TransferStatus::Active => "status.transferring",
        });
        let icon = match transfer.status {
            TransferStatus::Failed => TrayIcon::Failed,
            TransferStatus::Cancelled => TrayIcon::Cancelled,
            _ => direction_icon(transfer.direction),
        };
        rows.push(TrayRow::item(
            format!("recent/{}", transfer.id),
            transfer_line(transfer, status, language),
            Some(icon),
            Some(TrayAction::Open(Some(View::History))),
        ));
        let at = transfer.finished_at.unwrap_or(transfer.started_at);
        let day = format::day_label_in(at, language).label_in(language);
        rows.push(TrayRow::item(
            format!("detail/{}", transfer.id),
            format!(
                "{day} {} · {}",
                format::clock(at),
                format::bytes(transfer.total)
            ),
            None,
            None,
        ));
    }

    rows.push(TrayRow::Separator);
    rows.push(TrayRow::item(
        "open",
        text("tray.open"),
        Some(TrayIcon::Open),
        Some(TrayAction::Open(None)),
    ));
    rows.push(TrayRow::Separator);
    rows.push(TrayRow::item(
        "quit",
        text("tray.quit"),
        None,
        Some(TrayAction::Quit),
    ));
    rows
}

/// "To {name} — {status}" / "From {name} — {status}".
fn transfer_line(transfer: &Transfer, status: &str, language: Language) -> String {
    let key = match transfer.direction {
        Direction::Sending => "tray.to",
        Direction::Receiving => "tray.from",
    };
    fill(
        i18n::text(language, key),
        &[
            ("name", short_name(&transfer.peer.alias)),
            ("status", status.to_string()),
        ],
    )
}

fn direction_icon(direction: Direction) -> TrayIcon {
    match direction {
        Direction::Sending => TrayIcon::Sending,
        Direction::Receiving => TrayIcon::Receiving,
    }
}

/// A peer-chosen alias, single-spaced, without control characters and cut to
/// [`NAME_COLUMNS`] display columns.
///
/// The alias comes from the other device: newlines and other control
/// characters would break the row's height or hide part of its title, so they
/// are spaced out here. Wide (East Asian and emoji) characters take two
/// columns, so a CJK name is cut at about half the characters of an ASCII one.
pub(crate) fn short_name(alias: &str) -> String {
    let clean: String = alias
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect();
    let name = clean.split_whitespace().collect::<Vec<_>>().join(" ");
    if display_columns(&name) <= NAME_COLUMNS {
        return name;
    }
    // Leave the last column for the ellipsis.
    let mut cut = String::new();
    let mut columns = 0;
    for c in name.chars() {
        let width = char_columns(c);
        if columns + width > NAME_COLUMNS - 1 {
            break;
        }
        cut.push(c);
        columns += width;
    }
    cut.push('…');
    cut
}

/// Columns a character takes in a menu row: East Asian wide and emoji
/// characters take two, everything else one.
///
/// A conservative approximation of the Unicode East Asian Width data (the app
/// has no width-table dependency); it only has to keep a peer-chosen name from
/// making the menu wider than [`NAME_COLUMNS`] suggests.
fn char_columns(c: char) -> usize {
    match c as u32 {
        0x1100..=0x115F        // Hangul Jamo
        | 0x2E80..=0x303E      // CJK radicals, Kangxi, CJK symbols
        | 0x3041..=0x33FF      // Hiragana, Katakana, CJK compatibility
        | 0x3400..=0x4DBF      // CJK extension A
        | 0x4E00..=0x9FFF      // CJK unified ideographs
        | 0xA000..=0xA4CF      // Yi
        | 0xAC00..=0xD7A3      // Hangul syllables
        | 0xF900..=0xFAFF      // CJK compatibility ideographs
        | 0xFE30..=0xFE4F      // CJK compatibility forms
        | 0xFF00..=0xFF60      // Fullwidth forms
        | 0xFFE0..=0xFFE6      // Fullwidth signs
        | 0x1F300..=0x1FAFF    // Emoji
        | 0x20000..=0x3FFFD    // CJK extensions B+
        => 2,
        _ => 1,
    }
}

/// The display columns `text` takes in a menu row.
fn display_columns(text: &str) -> usize {
    text.chars().map(char_columns).sum()
}

/// Performs a menu action through the session, on the UI thread.
fn perform(session: &Session, action: TrayAction) {
    match action {
        TrayAction::Open(view) => {
            if let Some(view) = view {
                session.set_view(view);
            }
            show_window();
        }
        TrayAction::Quit => {
            // The "a transfer is running" confirmation is drawn in the window:
            // bring it up first, or the question would be invisible.
            if session.has_active_transfers() {
                show_window();
            }
            super::quit::quit(session);
        }
    }
}

/// Performs `action` as if its menu row had been clicked: for callers on the
/// main thread outside the menu (a notification click). Does nothing before
/// [`install`] ran (or where it does nothing).
pub fn request(action: TrayAction) {
    #[cfg(target_os = "macos")]
    macos::request(action);
    #[cfg(not(target_os = "macos"))]
    let _ = action;
}

/// Brings the window (and the Dock icon) back and focuses the app.
pub fn show_window() {
    #[cfg(target_os = "macos")]
    macos::bring_to_front();
    super::quit::show();
}

/// Installs the status item and keeps its menu current.
///
/// Must run inside a component body (the root component calls it through
/// [`crate::platform::install`]): it creates an effect and a task.
pub fn install(session: Session) {
    #[cfg(target_os = "macos")]
    macos::install(session);
    #[cfg(not(target_os = "macos"))]
    let _ = session;
}

#[cfg(target_os = "macos")]
mod macos {
    //! Threads: nsh runs components, effects and session tasks on its engine
    //! thread, while AppKit objects belong to the main thread. The engine side
    //! computes the rows (plain data) and posts them to the main queue; the
    //! main side owns the status item and renders. A click goes back through
    //! a channel to a session task on the engine thread.

    use super::{TrayAction, TrayRow, menu_rows, perform};
    use crate::model::Session;
    use crate::ui::icons::{IconName, Pen};
    use block2::RcBlock;
    use dispatch2::DispatchQueue;
    use nsh_core::prelude::*;
    use objc2::rc::Retained;
    use objc2::runtime::{AnyClass, AnyObject, Bool, Imp, NSObjectProtocol, ProtocolObject, Sel};
    use objc2::{MainThreadMarker, MainThreadOnly, define_class, msg_send, sel};
    use objc2_app_kit::{
        NSApplication, NSApplicationActivationPolicy, NSBezierPath, NSColor, NSImage,
        NSLineCapStyle, NSLineJoinStyle, NSMenu, NSMenuDelegate, NSMenuItem, NSStatusBar,
        NSStatusItem, NSVariableStatusItemLength, NSWindow,
        NSWindowDidChangeOcclusionStateNotification,
    };
    use objc2_foundation::{
        NSNotification, NSNotificationCenter, NSObject, NSPoint, NSRect, NSSize, NSString,
    };
    use std::cell::RefCell;
    use std::sync::OnceLock;
    use std::sync::atomic::{AtomicBool, Ordering};

    /// The "keep running in the background" setting, mirrored for the window's
    /// close button, whose AppKit callback runs on the main thread and must not
    /// call into nsh. Kept current by an effect in [`install`].
    static RUN_IN_BACKGROUND: AtomicBool = AtomicBool::new(true);

    /// Where menu clicks go (main thread); a session task on the engine
    /// thread receives and performs them.
    static ACTIONS: OnceLock<async_channel::Sender<TrayAction>> = OnceLock::new();

    thread_local! {
        /// The status item. Only ever touched on the main thread.
        static TRAY: RefCell<Option<Tray>> = const { RefCell::new(None) };
    }

    define_class!(
        /// Receives the status menu's clicks and the window visibility changes.
        #[unsafe(super(NSObject))]
        #[thread_kind = MainThreadOnly]
        #[name = "LocalSendStatusTarget"]
        struct Target;

        unsafe impl NSObjectProtocol for Target {}

        unsafe impl NSMenuDelegate for Target {
            /// Re-checks the Dock policy whenever the menu opens. The occlusion
            /// notification is the only other trigger, and it is AppKit's call
            /// when (or whether) it fires for a closed window; reconciling here
            /// means a missed notification cannot leave the app in the wrong
            /// activation policy.
            #[unsafe(method(menuWillOpen:))]
            fn menu_will_open(&self, _menu: &NSMenu) {
                if let Some(mtm) = MainThreadMarker::new() {
                    follow_window(mtm);
                }
            }
        }

        impl Target {
            #[unsafe(method(menuAction:))]
            fn menu_action(&self, sender: &NSMenuItem) {
                let Ok(index) = usize::try_from(sender.tag()) else {
                    return;
                };
                let action = TRAY.with(|tray| {
                    tray.borrow()
                        .as_ref()
                        .and_then(|tray| tray.actions.get(index).copied())
                });
                let Some(action) = action else { return };
                if matches!(action, TrayAction::Open(_))
                    && let Some(mtm) = MainThreadMarker::new()
                {
                    // Back in the Dock and frontmost before the window shows.
                    activate(mtm);
                }
                if let Some(sender) = ACTIONS.get() {
                    let _ = sender.try_send(action);
                }
            }

            #[unsafe(method(windowsChanged:))]
            fn windows_changed(&self, _notification: &NSNotification) {
                if let Some(mtm) = MainThreadMarker::new() {
                    follow_window(mtm);
                }
            }
        }
    );

    /// The status item, its menu and what the menu currently shows.
    struct Tray {
        /// Owned so it stays in the menu bar: AppKit removes a status item
        /// once its last reference goes.
        _item: Retained<NSStatusItem>,
        menu: Retained<NSMenu>,
        target: Retained<Target>,
        rows: Vec<TrayRow>,
        items: Vec<Retained<NSMenuItem>>,
        /// The action of each clickable item, by its tag.
        actions: Vec<TrayAction>,
        /// Menu glyph images (16 pt) by icon: a rebuilt menu reuses them, and
        /// AppKit draws the template lazily per backing scale.
        images: Vec<(IconName, Retained<NSImage>)>,
    }

    /// Engine thread: wires the click channel, creates the item on the main
    /// thread and keeps posting the rows to it.
    pub fn install(session: Session) {
        if !is_desktop() {
            return;
        }
        // Headless UI tests run the real app in-process, and the AppKit objects
        // below belong to the main run loop, which a test host does not
        // necessarily run. `nsh test` sets this same test switch that
        // `Session::start` uses to stay off the network.
        if std::env::var_os("LOCALSEND_OFFLINE").is_some() {
            return;
        }
        let (sender, receiver) = async_channel::unbounded();
        if ACTIONS.set(sender).is_err() {
            return; // installed already
        }
        let performer = session.clone();
        session.task_detached(async move {
            while let Ok(action) = receiver.recv().await {
                perform(&performer, action);
            }
        });

        // Settings domain only: progress ticks must not re-run this.
        let background = session.clone();
        effect(move || {
            let on = background.read_settings(|state| state.settings.run_in_background);
            RUN_IN_BACKGROUND.store(on, Ordering::Relaxed);
        });

        DispatchQueue::main().exec_async(|| {
            if let Some(mtm) = MainThreadMarker::new() {
                create(mtm);
            }
        });

        // Only a change crosses threads: a progress tick that rounds to the
        // same percent posts nothing. The dedupe cannot lose the first batch:
        // `create` is enqueued on the main queue before this effect first runs,
        // so a posted batch is applied to an installed `Tray` (or superseded by
        // one that was).
        let last: RefCell<Option<Vec<TrayRow>>> = RefCell::new(None);
        effect(move || {
            let language = session.language.get();
            let online = session.handle.get().is_some();
            let rows = session.read(|state| menu_rows(state, online, language));
            if last.borrow().as_ref() == Some(&rows) {
                return;
            }
            *last.borrow_mut() = Some(rows.clone());
            DispatchQueue::main().exec_async(move || {
                let Some(mtm) = MainThreadMarker::new() else {
                    return;
                };
                TRAY.with(|tray| {
                    if let Some(tray) = tray.borrow_mut().as_mut() {
                        tray.show(rows, mtm);
                    }
                });
            });
        });
    }

    /// Main thread: queues `action` for the session, bringing the app back to
    /// the Dock first when it opens the window (as a menu click does).
    pub fn request(action: TrayAction) {
        if matches!(action, TrayAction::Open(_))
            && let Some(mtm) = MainThreadMarker::new()
        {
            activate(mtm);
        }
        if let Some(sender) = ACTIONS.get() {
            let _ = sender.try_send(action);
        }
    }

    /// Main thread: the status item, its menu and the window observer.
    fn create(mtm: MainThreadMarker) {
        let target: Retained<Target> = unsafe { msg_send![Target::alloc(mtm), init] };
        let item = NSStatusBar::systemStatusBar().statusItemWithLength(NSVariableStatusItemLength);
        let menu = NSMenu::new(mtm);
        // `Target` owns no AppKit object itself; the menu's delegate is weak
        // and `Tray` below keeps the target alive for the process.
        menu.setDelegate(Some(ProtocolObject::from_ref(&*target)));
        // Rows without an action stay dimmed; AppKit would otherwise enable
        // every item that has a target.
        menu.setAutoenablesItems(false);
        item.setMenu(Some(&menu));
        if let Some(button) = item.button(mtm) {
            // The app mark, as the sidebar draws it; 18 pt fits the menu bar.
            button.setImage(Some(&glyph_image(IconName::Send, 18.0)));
            button.setToolTip(Some(&NSString::from_str("LocalSend")));
        }
        // Hiding the window (closing it keeps the app alive) leaves the Dock;
        // showing it comes back. Occlusion changes cover order-in, order-out,
        // minimize and restore.
        unsafe {
            NSNotificationCenter::defaultCenter().addObserver_selector_name_object(
                as_object(&target),
                sel!(windowsChanged:),
                Some(NSWindowDidChangeOcclusionStateNotification),
                None,
            );
        }
        let tray = Tray {
            _item: item,
            menu,
            target,
            rows: Vec::new(),
            items: Vec::new(),
            actions: Vec::new(),
            images: Vec::new(),
        };
        tracing::debug!("installed the menu bar status item");
        TRAY.with(|current| *current.borrow_mut() = Some(tray));
        take_over_close_button();
    }

    /// Makes the window's close button (and ⌘W) follow the "keep running in
    /// the background" setting.
    ///
    /// nsh's macOS runner only hides the window on close when
    /// `AppOptions::keep_alive_on_close` is set, and the runner that
    /// `nsh run`/`nsh package` generate builds its options itself, without
    /// it: there, closing the window would end the process — no menu bar
    /// item, no transfers, and no session flush. nsh 0.4 has no API for this,
    /// so the `windowShouldClose:` of its window delegate (`NshAppDelegate`,
    /// named in `nsh-platform-macos`'s `app.rs`) is replaced:
    ///
    /// - setting on: the window is ordered out and stays alive (the menu bar
    ///   item and the Dock reopen it), as `keep_alive_on_close` would do;
    /// - setting off: the close is refused and a quit is requested instead,
    ///   so it goes through [`super::super::quit::quit`] (the "transfer in
    ///   progress" confirmation and [`Session::shutdown`]).
    ///
    /// The app's own quit path (`quit_app`) closes the window without asking
    /// the delegate, so it is not affected.
    fn take_over_close_button() {
        let Some(class) = AnyClass::get(c"NshAppDelegate") else {
            tracing::warn!("nsh's window delegate class is missing: the close button quits");
            return;
        };
        let Some(method) = class.instance_method(sel!(windowShouldClose:)) else {
            tracing::warn!("nsh's window delegate has no windowShouldClose:");
            return;
        };
        type ShouldClose = unsafe extern "C-unwind" fn(*mut AnyObject, Sel, *mut NSWindow) -> Bool;
        let replacement: ShouldClose = window_should_close;
        // SAFETY: `windowShouldClose:` takes the window and returns `BOOL`;
        // `ShouldClose` is that signature (`Bool` is `BOOL` on every
        // architecture), and the replacement accepts every input the original
        // did (a null window included).
        unsafe {
            method.set_implementation(std::mem::transmute::<ShouldClose, Imp>(replacement));
        }
        tracing::debug!("the window's close button follows the background setting");
    }

    /// The window delegate's `windowShouldClose:` (see
    /// [`take_over_close_button`]). Always answers NO: either the window is
    /// hidden instead, or the session decides how to quit.
    unsafe extern "C-unwind" fn window_should_close(
        _this: *mut AnyObject,
        _cmd: Sel,
        window: *mut NSWindow,
    ) -> Bool {
        if RUN_IN_BACKGROUND.load(Ordering::Relaxed) {
            // SAFETY: AppKit passes the window being closed, alive for the
            // duration of the call.
            if let Some(window) = unsafe { window.as_ref() } {
                window.orderOut(None);
            }
        } else if let Some(sender) = ACTIONS.get() {
            let _ = sender.try_send(TrayAction::Quit);
        }
        Bool::NO
    }

    impl Tray {
        /// The menu image for `glyph`: created once per rebuild sequence.
        fn image(&mut self, glyph: IconName) -> Retained<NSImage> {
            if let Some((_, image)) = self.images.iter().find(|(name, _)| *name == glyph) {
                return image.clone();
            }
            let image = glyph_image(glyph, 16.0);
            self.images.push((glyph, image.clone()));
            image
        }

        fn show(&mut self, rows: Vec<TrayRow>, mtm: MainThreadMarker) {
            if rows == self.rows {
                return;
            }
            let same_shape = rows.len() == self.rows.len()
                && rows.iter().zip(&self.rows).all(|(a, b)| a.key() == b.key());
            if same_shape {
                // Progress ticks only change titles: retitle in place, so an
                // open menu keeps its highlighted row.
                for ((row, old), item) in rows.iter().zip(&self.rows).zip(&self.items) {
                    if let (
                        TrayRow::Item { label, .. },
                        TrayRow::Item {
                            label: old_label, ..
                        },
                    ) = (row, old)
                        && label != old_label
                    {
                        item.setTitle(&NSString::from_str(label));
                    }
                }
                self.rows = rows;
                return;
            }
            self.rebuild(rows, mtm);
        }

        fn rebuild(&mut self, rows: Vec<TrayRow>, mtm: MainThreadMarker) {
            self.menu.removeAllItems();
            self.items.clear();
            self.actions.clear();
            for row in &rows {
                let item = match row {
                    TrayRow::Separator => NSMenuItem::separatorItem(mtm),
                    TrayRow::Item {
                        label,
                        icon,
                        action,
                        ..
                    } => {
                        let key_equivalent = if *action == Some(TrayAction::Quit) {
                            "q"
                        } else {
                            ""
                        };
                        let item = unsafe {
                            NSMenuItem::initWithTitle_action_keyEquivalent(
                                NSMenuItem::alloc(mtm),
                                &NSString::from_str(label),
                                action.map(|_| sel!(menuAction:)),
                                &NSString::from_str(key_equivalent),
                            )
                        };
                        if let Some(icon) = icon {
                            let image = self.image(icon.glyph());
                            item.setImage(Some(&image));
                            show_image(&item);
                        }
                        match action {
                            Some(action) => {
                                item.setTag(self.actions.len() as isize);
                                self.actions.push(*action);
                                unsafe {
                                    item.setTarget(Some(as_object(&self.target)));
                                }
                                item.setEnabled(true);
                            }
                            None => item.setEnabled(false),
                        }
                        item
                    }
                };
                self.menu.addItem(&item);
                self.items.push(item);
            }
            self.rows = rows;
        }
    }

    fn as_object(target: &Target) -> &AnyObject {
        target.as_ref()
    }

    /// Asks AppKit to draw the item's image.
    ///
    /// Apps linked against the macOS 27 SDK get
    /// `NSMenuItem.preferredImageVisibility` = Automatic, and AppKit then
    /// hides menu item images (no icons at all, SF Symbols or not). `Visible`
    /// restores them. Older systems have no such property and always draw the
    /// image; `objc2-app-kit` 0.3 predates it, hence the checked message.
    fn show_image(item: &NSMenuItem) {
        // `NSMenuItemImageVisibilityVisible` from the macOS 27 SDK:
        // `<AppKit/NSMenuItem.h>` declares Automatic 0, Visible 1, Hidden 2.
        const VISIBLE: isize = 1;
        // The selector returns `BOOL`, not a Rust `bool`: the two differ on
        // x86_64, and `Bool` is the ABI-correct type on every architecture.
        let responds: Bool =
            unsafe { msg_send![item, respondsToSelector: sel!(setPreferredImageVisibility:)] };
        if responds.as_bool() {
            let _: () = unsafe { msg_send![item, setPreferredImageVisibility: VISIBLE] };
        }
    }

    /// A template image of one of the app's glyphs, `points` wide and high.
    ///
    /// Drawn on demand by AppKit (the handler runs for each backing scale),
    /// so it is sharp on every display; as a template, the menu tints it for
    /// light, dark and highlighted rows.
    fn glyph_image(name: IconName, points: f64) -> Retained<NSImage> {
        let handler = RcBlock::new(move |rect: NSRect| -> Bool {
            // AppKit hands the rectangle to draw into: the image size at the
            // current backing scale, or a smaller one when it scales the icon.
            // Draw the 20 × 20 glyph into that rect rather than assuming the
            // image size, or a scaled-down draw would be clipped.
            let scale = if rect.size.width > 0.0 {
                rect.size.width / 20.0
            } else {
                points / 20.0
            };
            name.draw(BezierPen::at(scale, rect.origin));
            Bool::YES
        });
        // Flipped: y grows downward, as in the glyphs' 20 × 20 box.
        let image = NSImage::imageWithSize_flipped_drawingHandler(
            NSSize::new(points, points),
            true,
            &handler,
        );
        image.setTemplate(true);
        image
    }

    /// [`Pen`] over `NSBezierPath`, scaled from the 20 × 20 glyph box.
    struct BezierPen {
        path: Retained<NSBezierPath>,
        scale: f64,
        /// Where the glyph box's origin sits in AppKit's drawing rect.
        origin: NSPoint,
    }

    impl BezierPen {
        fn at(scale: f64, origin: NSPoint) -> Self {
            Self {
                path: NSBezierPath::bezierPath(),
                scale,
                origin,
            }
        }

        fn point(&self, x: f32, y: f32) -> NSPoint {
            NSPoint::new(
                self.origin.x + f64::from(x) * self.scale,
                self.origin.y + f64::from(y) * self.scale,
            )
        }
    }

    impl Pen for BezierPen {
        fn begin(self) -> Self {
            let path = NSBezierPath::bezierPath();
            path.setLineWidth(1.6 * self.scale);
            path.setLineCapStyle(NSLineCapStyle::Round);
            path.setLineJoinStyle(NSLineJoinStyle::Round);
            Self { path, ..self }
        }

        fn move_to(self, x: f32, y: f32) -> Self {
            self.path.moveToPoint(self.point(x, y));
            self
        }

        fn line_to(self, x: f32, y: f32) -> Self {
            self.path.lineToPoint(self.point(x, y));
            self
        }

        fn quad_to(self, cx: f32, cy: f32, x: f32, y: f32) -> Self {
            // The cubic with the same curve: controls 2/3 of the way to the
            // quadratic control point.
            let start = self.path.currentPoint();
            let control = self.point(cx, cy);
            let end = self.point(x, y);
            let c1 = NSPoint::new(
                start.x + 2.0 / 3.0 * (control.x - start.x),
                start.y + 2.0 / 3.0 * (control.y - start.y),
            );
            let c2 = NSPoint::new(
                end.x + 2.0 / 3.0 * (control.x - end.x),
                end.y + 2.0 / 3.0 * (control.y - end.y),
            );
            self.path
                .curveToPoint_controlPoint1_controlPoint2(end, c1, c2);
            self
        }

        fn close(self) -> Self {
            self.path.closePath();
            self
        }

        fn arc(self, x: f32, y: f32, r: f32, start: f32, end: f32) -> Self {
            // Angles grow toward +y; in the flipped image that is clockwise
            // on screen, as on the canvas. AppKit's `clockwise: false` means
            // "angles increase".
            self.path
                .appendBezierPathWithArcWithCenter_radius_startAngle_endAngle_clockwise(
                    self.point(x, y),
                    f64::from(r) * self.scale,
                    f64::from(start).to_degrees(),
                    f64::from(end).to_degrees(),
                    false,
                );
            self
        }

        fn round_rect(self, x: f32, y: f32, w: f32, h: f32, r: f32) -> Self {
            let origin = self.point(x, y);
            let size = NSSize::new(f64::from(w) * self.scale, f64::from(h) * self.scale);
            let radius = f64::from(r) * self.scale;
            self.path.appendBezierPathWithRoundedRect_xRadius_yRadius(
                NSRect::new(origin, size),
                radius,
                radius,
            );
            self
        }

        fn ellipse(self, cx: f32, cy: f32, rx: f32, ry: f32) -> Self {
            let origin = self.point(cx - rx, cy - ry);
            let size = NSSize::new(
                f64::from(rx * 2.0) * self.scale,
                f64::from(ry * 2.0) * self.scale,
            );
            self.path
                .appendBezierPathWithOvalInRect(NSRect::new(origin, size));
            self
        }

        fn stroke(self) -> Self {
            // Template images use only the alpha; any opaque color works.
            NSColor::blackColor().setStroke();
            self.path.stroke();
            self
        }

        fn fill(self) -> Self {
            NSColor::blackColor().setFill();
            self.path.fill();
            self
        }
    }

    /// Whether the app's own window is on screen or minimized to the Dock.
    /// The status bar's window and panels cannot become main, so they do not
    /// count.
    fn window_shown(app: &NSApplication) -> bool {
        app.windows().iter().any(|window| {
            window.canBecomeMainWindow() && (window.isVisible() || window.isMiniaturized())
        })
    }

    /// Leaves the Dock while the window is closed and returns when it shows.
    fn follow_window(mtm: MainThreadMarker) {
        let app = NSApplication::sharedApplication(mtm);
        // Hiding the app (⌘H) hides every window, but the Dock icon is how one
        // unhides it again: leave the policy alone for that case.
        if app.isHidden() {
            return;
        }
        let policy = if window_shown(&app) {
            NSApplicationActivationPolicy::Regular
        } else {
            NSApplicationActivationPolicy::Accessory
        };
        if app.activationPolicy() != policy {
            app.setActivationPolicy(policy);
        }
    }

    /// Back in the Dock and frontmost, so a window shown next comes forward.
    fn activate(mtm: MainThreadMarker) {
        let app = NSApplication::sharedApplication(mtm);
        if app.activationPolicy() != NSApplicationActivationPolicy::Regular {
            app.setActivationPolicy(NSApplicationActivationPolicy::Regular);
        }
        app.activate();
    }

    /// [`activate`] from any thread (the engine thread's quit path).
    pub fn bring_to_front() {
        DispatchQueue::main().exec_async(|| {
            if let Some(mtm) = MainThreadMarker::new() {
                activate(mtm);
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::IncomingRequest;
    use crate::net::{DeviceType, OfferedFile, Peer, PeerAddress, Protocol};

    fn peer(alias: &str, online: bool) -> Peer {
        Peer {
            fingerprint: alias.to_uppercase(),
            alias: alias.to_string(),
            device_model: String::new(),
            address: online.then(|| PeerAddress {
                host: "192.168.1.2".to_string(),
                port: 53317,
                protocol: Protocol::Https,
            }),
            device_type: DeviceType::Mobile,
            is_self: false,
        }
    }

    fn transfer(id: &str, status: TransferStatus, direction: Direction) -> Transfer {
        Transfer {
            id: id.to_string(),
            peer: peer("iPhone 15 Pro", true),
            direction,
            files: Vec::new(),
            total: 6_000_000,
            transferred: 2_400_000,
            status,
            error: None,
            saved_paths: Vec::new(),
            texts: Vec::new(),
            started_at: 1_700_000_000_000,
            finished_at: None,
            omitted_files: 0,
        }
    }

    fn labels(rows: &[TrayRow]) -> Vec<String> {
        rows.iter()
            .map(|row| match row {
                TrayRow::Item { label, .. } => label.clone(),
                TrayRow::Separator => "—".to_string(),
            })
            .collect()
    }

    fn action_of(rows: &[TrayRow], key: &str) -> Option<TrayAction> {
        rows.iter().find_map(|row| match row {
            TrayRow::Item { key: k, action, .. } if k == key => *action,
            _ => None,
        })
    }

    #[test]
    fn an_idle_menu_counts_online_devices_and_ends_with_open_and_quit() {
        let state = AppState {
            devices: vec![peer("Mac", true), peer("TV", false), peer("Phone", true)],
            ..AppState::default()
        };
        let rows = menu_rows(&state, true, Language::En);
        let labels = labels(&rows);
        assert_eq!(
            labels[0], "2 nearby devices",
            "offline devices do not count"
        );
        assert_eq!(labels[1], "Ready to receive");
        assert!(labels.contains(&"No recent transfers".to_string()));
        assert_eq!(labels[labels.len() - 3], "Open LocalSend");
        assert_eq!(action_of(&rows, "open"), Some(TrayAction::Open(None)));
        assert_eq!(action_of(&rows, "quit"), Some(TrayAction::Quit));
        assert_eq!(
            action_of(&rows, "devices"),
            None,
            "the summary is not clickable"
        );
    }

    #[test]
    fn the_status_line_follows_network_requests_and_transfers() {
        let mut state = AppState::default();
        assert_eq!(
            labels(&menu_rows(&state, false, Language::Zh))[1],
            "局域网服务未运行"
        );

        state.incoming = Some(IncomingRequest {
            id: "r".to_string(),
            peer: peer("iPhone 15 Pro", true),
            files: vec![OfferedFile {
                name: "a.jpg".to_string(),
                size: 1,
                mime: "image/jpeg".to_string(),
                saved_to: None,
            }],
            total: 1,
            message: None,
        });
        let rows = menu_rows(&state, true, Language::Zh);
        assert_eq!(labels(&rows)[1], "iPhone 15 Pro 想给你发送文件");
        assert_eq!(action_of(&rows, "request"), Some(TrayAction::Open(None)));

        state.incoming = None;
        state
            .transfers
            .push(transfer("a", TransferStatus::Active, Direction::Sending));
        let rows = menu_rows(&state, true, Language::Zh);
        let labels = labels(&rows);
        assert_eq!(labels[1], "发送到 iPhone 15 Pro — 40%");
        assert_eq!(
            labels[2],
            format!(
                "{} / {}",
                format::bytes(2_400_000),
                format::bytes(6_000_000)
            )
        );
    }

    #[test]
    fn recent_transfers_open_the_history_and_progress_keeps_the_row_keys() {
        let mut state = AppState::default();
        let mut done = transfer("done", TransferStatus::Completed, Direction::Receiving);
        done.finished_at = Some(1_700_000_100_000);
        state.transfers = vec![
            transfer("run", TransferStatus::Active, Direction::Sending),
            done,
        ];
        let before = menu_rows(&state, true, Language::En);
        assert!(labels(&before).contains(&"From iPhone 15 Pro — Completed".to_string()));
        assert_eq!(
            action_of(&before, "recent/done"),
            Some(TrayAction::Open(Some(View::History)))
        );

        state.transfers[0].transferred = 5_000_000;
        let after = menu_rows(&state, true, Language::En);
        let keys = |rows: &[TrayRow]| {
            rows.iter()
                .map(|row| row.key().to_string())
                .collect::<Vec<_>>()
        };
        assert_eq!(keys(&before), keys(&after), "progress only retitles rows");
        assert_ne!(labels(&before), labels(&after));
    }

    #[test]
    fn long_aliases_are_cut() {
        let name = short_name(&"x".repeat(80));
        assert_eq!(name.chars().count(), NAME_COLUMNS);
        assert!(name.ends_with('…'));
        assert_eq!(short_name("  Living   Room  TV "), "Living Room TV");
    }

    #[test]
    fn wide_names_are_cut_by_display_columns() {
        // Each 中 takes two columns, so only 15 of them fit before the ellipsis.
        let wide = short_name(&"中".repeat(40));
        assert!(display_columns(&wide) <= NAME_COLUMNS);
        assert!(wide.ends_with('…'));
        assert_eq!(wide.chars().count(), 16, "{wide:?}");
    }

    #[test]
    fn a_name_at_the_limit_is_kept_and_control_characters_are_spaced() {
        let exact = "y".repeat(NAME_COLUMNS);
        assert_eq!(short_name(&exact), exact, "the limit itself is not cut");
        assert_eq!(short_name("Bad\u{7}Name\u{1f}"), "Bad Name");
    }

    #[test]
    fn hidden_discoverability_and_an_empty_recent_list_say_so() {
        let mut state = AppState::default();
        let rows = menu_rows(&state, true, Language::En);
        assert_eq!(labels(&rows)[0], "No devices nearby");
        assert_eq!(labels(&rows)[1], "Ready to receive");

        state.settings.discoverable = false;
        let labels = labels(&menu_rows(&state, true, Language::En));
        assert_eq!(labels[1], "Ready · hidden from other devices");
        assert!(
            labels.contains(&"No recent transfers".to_string()),
            "the recent section has its own empty state: {labels:?}"
        );
    }

    #[test]
    fn transfers_beyond_the_listed_rows_are_summarized() {
        let state = AppState {
            transfers: (0..ACTIVE_ROWS + 2)
                .map(|index| {
                    transfer(
                        &format!("run-{index}"),
                        TransferStatus::Active,
                        Direction::Receiving,
                    )
                })
                .collect(),
            ..AppState::default()
        };
        let rows = menu_rows(&state, true, Language::En);
        let labels = labels(&rows);
        assert_eq!(
            labels.iter().filter(|label| label.ends_with('%')).count(),
            ACTIVE_ROWS,
            "only the first transfers get a row each"
        );
        assert!(
            labels.contains(&"2 more transfers".to_string()),
            "the rest are summarized in one row: {labels:?}"
        );
        assert_eq!(action_of(&rows, "more"), Some(TrayAction::Open(None)));
    }
}
