//! The UI-side session: the one place that owns [`AppState`], the signals the
//! views subscribe to, and the commands that reach the network actor.
//!
//! Created once by the root `App` component ([`Session::persistent`]); cloned
//! freely into views and key handlers. Everything here runs on the engine
//! thread; waiting happens in `spawn`, never in a handler.

use crate::i18n::{self, Language};
use crate::model::state::{HISTORY_LIMIT, HISTORY_SCHEMA, SETTINGS_SCHEMA, default_downloads};
use crate::model::updater::{UpdateStatus, Updater};
use crate::model::{AppState, ComposerItem, Settings, Transfer, TransferStatus, View};
use crate::net::{self, Handle, Node, NodeConfig, Peer, TransferEvent};
use crate::ui::theme::Appearance;
use nsh_core::prelude::*;
use nsh_core::storage::Storage;
use std::cell::{Cell, RefCell};
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::Duration;

const SETTINGS_KEY: &str = "settings";
const HISTORY_KEY: &str = "history";

/// A transient message shown at the bottom of the window.
#[derive(Clone, Debug, PartialEq)]
pub struct Toast {
    pub text: SharedString,
    pub error: bool,
}

/// A send waiting for the peer's PIN.
#[derive(Clone, Debug, PartialEq)]
pub struct PinRequest {
    pub id: String,
    pub peer: Peer,
}

/// Which part of [`AppState`] one event changes.
///
/// The single [`Session::revision`] stays the compatible "everything changed"
/// signal; these let hot bindings subscribe to one domain and skip the ~20
/// progress events/s that only change `transfers` (see
/// `docs/PERFORMANCE.md`). Bumped by the same code that
/// writes the state, never by the views.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Change {
    pub peers: bool,
    pub transfers: bool,
    pub composer: bool,
    pub settings: bool,
}

impl Change {
    pub const NONE: Self = Self {
        peers: false,
        transfers: false,
        composer: false,
        settings: false,
    };

    pub const fn peers() -> Self {
        Self {
            peers: true,
            ..Self::NONE
        }
    }

    pub const fn transfers() -> Self {
        Self {
            transfers: true,
            ..Self::NONE
        }
    }

    pub const fn composer() -> Self {
        Self {
            composer: true,
            ..Self::NONE
        }
    }

    pub const fn settings() -> Self {
        Self {
            settings: true,
            ..Self::NONE
        }
    }
}

#[derive(Clone)]
pub struct Session {
    /// The live state. Kept public for compatibility, but views should read it
    /// through [`Session::read`]/[`Session::read_*`] so they subscribe to the
    /// revision they depend on; borrowing it mutably bypasses the reactive
    /// contract (M-15). Do not write through it.
    #[doc(hidden)]
    pub state: Rc<RefCell<AppState>>,
    /// Bumped on every change; views read it to subscribe.
    pub revision: Signal<u64>,
    /// Per-domain revisions: a binding that only renders devices subscribes to
    /// this instead of [`Session::revision`] so transfer progress does not
    /// re-run it. Every bump also bumps [`Session::revision`], so `read()`
    /// stays correct for code that has not migrated.
    pub peers_revision: Signal<u64>,
    pub transfers_revision: Signal<u64>,
    pub composer_revision: Signal<u64>,
    pub settings_revision: Signal<u64>,
    pub identity: Signal<Option<Peer>>,
    pub handle: Signal<Option<Handle>>,
    pub language: Signal<Language>,
    pub appearance: Signal<Appearance>,
    pub toast: Signal<Option<Toast>>,
    pub pin_request: Signal<Option<PinRequest>>,
    /// Bumped when a send starts; the Send screen plays the paper plane once
    /// per value (a signal, not a timer, so the view owns the animation).
    pub flight_epoch: Signal<u64>,
    /// The device whose verification sheet (combined fingerprint) is open.
    pub verify_peer: Signal<Option<Peer>>,
    pub settings_open: Signal<bool>,
    /// A settings change needs a network restart but work is in flight; it is
    /// applied when the last transfer (or pending request) finishes. Views
    /// show "applies after the current transfer" while this is set (M-02).
    pub pending_restart: Signal<bool>,
    /// What the self-updater is doing (see `model::updater`).
    pub update_status: Signal<UpdateStatus>,
    /// The verified update waiting for "Restart to update".
    pub(crate) updater: Rc<RefCell<Updater>>,
    /// Bounded foreground feedback, independent of the background service.
    discovery_feedback: Signal<bool>,
    discovery_feedback_epoch: Rc<Cell<u64>>,
    owner: Owner,
    /// Guards the single network task.
    running: Rc<Cell<bool>>,
    /// Which network task is current; only it may publish `handle`/`identity`
    /// and clear `running`. `restart_network` bumps it, so the old task's
    /// teardown cannot clobber its successor.
    network_generation: Rc<Cell<u64>>,
    /// Invalidates the pending restart debounce (see `restart_network_soon`).
    restart_debounce: Rc<Cell<u64>>,
    /// Invalidates the pending toast-expiry timer (a newer toast must not be
    /// cleared by the previous one's timer).
    toast_epoch: Rc<Cell<u64>>,
    /// Failed `start_network` attempts since the last success, for the
    /// bounded automatic retry (M-03).
    network_start_attempts: Rc<Cell<u32>>,
    /// Set by [`Session::shutdown`]: a late retry timer must not start a node
    /// while the app is quitting.
    shutting_down: Rc<Cell<bool>>,
    /// Unit tests run without the engine's task executor (`spawn` panics
    /// there); they set this so the pure state transitions can be exercised.
    /// Always `false` in an app.
    #[cfg(test)]
    skip_tasks: Rc<Cell<bool>>,
}

impl Default for Session {
    fn default() -> Self {
        Self::new()
    }
}

impl Session {
    pub fn new() -> Self {
        Self::with_state(AppState::default())
    }

    fn with_state(state: AppState) -> Self {
        let language = state.settings.language;
        let appearance = state.settings.appearance;
        Self {
            state: Rc::new(RefCell::new(state)),
            revision: signal(0_u64),
            peers_revision: signal(0_u64),
            transfers_revision: signal(0_u64),
            composer_revision: signal(0_u64),
            settings_revision: signal(0_u64),
            identity: signal(None),
            handle: signal_with(None, SignalEquals::Never),
            language: signal(language),
            appearance: signal(appearance),
            toast: signal(None),
            pin_request: signal_with(None, SignalEquals::Never),
            flight_epoch: signal(0_u64),
            verify_peer: signal_with(None, SignalEquals::Never),
            settings_open: signal(false),
            pending_restart: signal(false),
            update_status: signal(UpdateStatus::Idle),
            updater: Rc::new(RefCell::new(Updater::default())),
            discovery_feedback: signal(false),
            discovery_feedback_epoch: Rc::new(Cell::new(0)),
            owner: Owner::current(),
            running: Rc::new(Cell::new(false)),
            network_generation: Rc::new(Cell::new(0)),
            restart_debounce: Rc::new(Cell::new(0)),
            toast_epoch: Rc::new(Cell::new(0)),
            network_start_attempts: Rc::new(Cell::new(0)),
            shutting_down: Rc::new(Cell::new(false)),
            #[cfg(test)]
            skip_tasks: Rc::new(Cell::new(false)),
        }
    }

    /// The app's session: settings and history restored, network started.
    ///
    /// Runs in the root component's body, before the first frame, so it does
    /// no I/O beyond the in-memory `Storage` map (history is capped at
    /// [`HISTORY_LIMIT`] entries). The
    /// state is built directly instead of `AppState::default()` followed by
    /// assignments, so the platform's default device name (a `scutil`
    /// subprocess on macOS) is not computed and thrown away.
    pub fn persistent() -> Self {
        let settings = load_settings();
        let history = load_history();
        let state = AppState {
            view: View::default(),
            discovering: false,
            devices: Vec::new(),
            selected: None,
            incoming: None,
            transfers: history,
            composer: Vec::new(),
            settings,
            pending_restart: false,
            error: None,
        };
        Self::with_state(state)
    }

    /// Start the network and the appearance. Idempotent.
    pub fn start(&self) {
        self.apply_color_scheme(self.appearance.get_untracked());
        if std::env::var_os("LOCALSEND_DEMO").is_some() {
            self.seed_demo();
            // Outside `seed_demo`: writing a signal can run its subscribers
            // synchronously, and one that reads `AppState` would collide with
            // the borrow the seeding holds.
            seed_demo_identity(self);
            return;
        }
        // Tests run the real app in-process: without this they would bind the
        // LocalSend port and write a device identity into the user's profile.
        if std::env::var_os("LOCALSEND_OFFLINE").is_some() {
            return;
        }
        self.start_network();
        self.start_update_checks();
    }

    pub fn task<T: 'static>(&self, future: impl Future<Output = T> + 'static) -> Spawned<T> {
        self.owner.run(|| spawn(future))
    }

    /// Starts a task whose result nobody needs (a dialog, an open request).
    ///
    /// The task still runs: dropping a [`Spawned`] handle does not cancel it.
    /// This is the supported fire-and-forget shape and keeps the
    /// `let _ = future` clippy lint out of the call sites.
    pub fn task_detached(&self, future: impl Future<Output = ()> + 'static) {
        drop(self.task(future));
    }

    /// Bumps the revision every view reads, plus the domain revision(s) that
    /// actually changed.
    pub fn changed(&self, change: Change) {
        if change.peers {
            self.peers_revision.update(|revision| *revision += 1);
        }
        if change.transfers {
            self.transfers_revision.update(|revision| *revision += 1);
        }
        if change.composer {
            self.composer_revision.update(|revision| *revision += 1);
        }
        if change.settings {
            self.settings_revision.update(|revision| *revision += 1);
        }
        self.revision.update(|revision| *revision += 1);
    }

    /// Bumps every revision (a `replace`-style state reset).
    fn changed_all(&self) {
        self.changed(Change {
            peers: true,
            transfers: true,
            composer: true,
            settings: true,
        });
    }

    /// Read the state, subscribing the caller to every change.
    ///
    /// Re-entrancy: the closure runs while a shared `RefCell` borrow of the
    /// state is held. Do not call another `Session` method that borrows it
    /// mutably from inside `f` (`update_settings`, `decide`, …): that panics
    /// with "already borrowed". Read everything you need, drop the borrow,
    /// then act.
    pub fn read<R>(&self, f: impl FnOnce(&AppState) -> R) -> R {
        self.revision.get();
        f(&self.state.borrow())
    }

    /// Read the state, subscribing to device changes only.
    pub fn read_peers<R>(&self, f: impl FnOnce(&AppState) -> R) -> R {
        self.peers_revision.get();
        f(&self.state.borrow())
    }

    /// Read the state, subscribing to transfer changes only.
    pub fn read_transfers<R>(&self, f: impl FnOnce(&AppState) -> R) -> R {
        self.transfers_revision.get();
        f(&self.state.borrow())
    }

    /// Read the state, subscribing to composer changes only.
    pub fn read_composer<R>(&self, f: impl FnOnce(&AppState) -> R) -> R {
        self.composer_revision.get();
        f(&self.state.borrow())
    }

    /// Read the state, subscribing to settings/language/appearance changes
    /// only.
    pub fn read_settings<R>(&self, f: impl FnOnce(&AppState) -> R) -> R {
        self.settings_revision.get();
        f(&self.state.borrow())
    }

    /// A translated string, re-read when the language changes.
    pub fn text(&self, key: &'static str) -> SharedString {
        let language = self.language.get();
        i18n::text(language, key).into()
    }

    pub fn toast(&self, text: impl Into<SharedString>) {
        self.toast.set(Some(Toast {
            text: text.into(),
            error: false,
        }));
        self.expire_toast();
    }

    pub fn fail(&self, text: impl Into<SharedString>) {
        self.toast.set(Some(Toast {
            text: text.into(),
            error: true,
        }));
        self.expire_toast();
    }

    /// Shows `key`'s localized message and keeps `error`'s (English)
    /// implementation detail in the log only.
    ///
    /// Network and platform errors are English strings; a Chinese UI must not
    /// show them raw (M-05). Every user-visible failure goes through an i18n
    /// key and logs the detail for debugging.
    pub fn fail_error(&self, key: &'static str, error: impl std::fmt::Display) {
        tracing::warn!(%error, key, "The command failed");
        self.fail(self.text(key));
    }

    pub fn discovery_searching(&self) -> bool {
        self.discovery_feedback.get() && self.read_peers(|state| state.discovering)
    }

    pub fn can_refresh_discovery(&self) -> bool {
        self.handle.get().is_some()
            && self.read_peers(|state| state.discovering)
            && !self.discovery_feedback.get()
    }

    pub fn refresh_discovery(&self) {
        if !self.can_refresh_discovery() {
            return;
        }
        if let Some(handle) = self.handle.get_untracked() {
            match handle.refresh_discovery() {
                Ok(()) => self.begin_discovery_feedback(),
                Err(error) => self.fail_error("error.network_lost", error),
            }
        }
    }

    fn end_discovery_feedback(&self) {
        self.discovery_feedback_epoch
            .set(self.discovery_feedback_epoch.get().wrapping_add(1));
        self.discovery_feedback.set(false);
    }

    fn begin_discovery_feedback(&self) {
        self.end_discovery_feedback();
        self.discovery_feedback.set(true);
        let epoch = self.discovery_feedback_epoch.get();
        #[cfg(test)]
        if self.skip_tasks.get() {
            return;
        }
        let session = self.clone();
        self.task(async move {
            timer(Duration::from_secs(4)).await;
            session.expire_discovery_feedback(epoch);
        });
    }

    fn expire_discovery_feedback(&self, epoch: u64) {
        if self.discovery_feedback_epoch.get() == epoch {
            self.discovery_feedback.set(false);
        }
    }

    fn expire_toast(&self) {
        #[cfg(test)]
        if self.skip_tasks.get() {
            return;
        }
        // Every toast gets its own expiry: a second toast shown within four
        // seconds must not be cleared early by the first one's timer.
        let epoch = self.toast_epoch.get().wrapping_add(1);
        self.toast_epoch.set(epoch);
        let toast = self.toast;
        let epochs = Rc::clone(&self.toast_epoch);
        self.task(async move {
            timer(Duration::from_secs(4)).await;
            if epochs.get() == epoch {
                toast.set(None);
            }
        });
    }

    /// Applies the window color scheme. Skipped in unit tests, which have no
    /// engine (the ambient command needs the task executor).
    fn apply_color_scheme(&self, appearance: Appearance) {
        #[cfg(test)]
        if self.skip_tasks.get() {
            return;
        }
        set_color_scheme(appearance.into());
    }

    // --- navigation -------------------------------------------------------

    pub fn set_view(&self, view: View) {
        if self.read(|state| state.view) == view {
            return;
        }
        self.state.borrow_mut().view = view;
        animate();
        self.changed(Change::NONE);
    }

    pub fn select(&self, fingerprint: Option<String>) {
        self.state.borrow_mut().selected = fingerprint;
        self.changed(Change::peers());
    }

    pub fn open_settings(&self, open: bool) {
        self.settings_open.set(open);
    }

    /// Opens the verification sheet for `peer`: the combined fingerprint both
    /// devices show under LocalSend's "Verify" page.
    pub fn open_verify(&self, peer: Peer) {
        self.verify_peer.set(Some(peer));
    }

    pub fn close_verify(&self) {
        self.verify_peer.set(None);
    }

    // --- composer ---------------------------------------------------------

    /// Add files and folders picked from the system dialog.
    pub fn pick_files(&self) {
        let picked = pick_paths(
            nsh_core::file_dialog::OpenDialog::files_and_folders()
                .multiple(true)
                .title(self.text("send.pick").to_string()),
        );
        let this = self.clone();
        self.task(async move {
            match picked.await {
                Ok(Some(paths)) => this.add_paths(paths),
                Ok(None) => {}
                Err(error) => this.fail_error("error.choose_failed", error),
            }
        });
    }

    /// Add a folder picked from the system dialog.
    pub fn pick_folder(&self) {
        let picked = pick_paths(
            nsh_core::file_dialog::OpenDialog::folders()
                .title(self.text("send.pick_folder").to_string()),
        );
        let this = self.clone();
        self.task(async move {
            match picked.await {
                Ok(Some(paths)) => this.add_paths(paths),
                Ok(None) => {}
                Err(error) => this.fail_error("error.choose_failed", error),
            }
        });
    }

    /// Add paths from a drop or the dialog, expanding directories.
    pub fn add_paths(&self, paths: Vec<PathBuf>) {
        let this = self.clone();
        self.task(async move {
            let collected = blocking(move || collect_paths(paths))
                .await
                .unwrap_or_default();
            let added = {
                let mut state = this.state.borrow_mut();
                stage_new_items(&mut state.composer, collected)
            };
            if added == 0 {
                this.fail(this.text("send.nothing_added"));
            } else {
                this.changed(Change::composer());
            }
        });
    }

    pub fn add_text(&self, text: String) {
        if text.trim().is_empty() {
            return;
        }
        self.state
            .borrow_mut()
            .composer
            .push(ComposerItem::Text(text));
        animate();
        self.changed(Change::composer());
    }

    pub fn paste_clipboard(&self) {
        let clipboard = clipboard_string();
        let this = self.clone();
        self.task(async move {
            let text = clipboard.await;
            if text.trim().is_empty() {
                this.fail(this.text("send.clipboard_empty"));
            } else {
                this.add_text(text);
            }
        });
    }

    /// Stage files copied in Finder.
    pub fn paste_clipboard_files(&self) {
        let this = self.clone();
        self.task(async move {
            let paths = crate::platform::clipboard::clipboard_files_as_paths().await;
            if paths.is_empty() {
                this.fail(this.text("send.clipboard_empty"));
            } else {
                this.add_paths(paths);
            }
        });
    }

    pub fn remove_item(&self, index: usize) {
        animate();
        let mut state = self.state.borrow_mut();
        if index < state.composer.len() {
            state.composer.remove(index);
        }
        drop(state);
        self.changed(Change::composer());
    }

    pub fn clear_composer(&self) {
        animate();
        self.state.borrow_mut().composer.clear();
        self.changed(Change::composer());
    }

    // --- transfers --------------------------------------------------------

    pub fn send(&self) {
        self.send_with_pin(None);
    }

    fn send_with_pin(&self, pin: Option<String>) {
        // Untracked: `send` runs in a handler, not in a binding.
        let (peer, items) = untracked(|| {
            let state = self.state.borrow();
            (state.selected_peer().cloned(), state.composer.clone())
        });
        let Some(peer) = peer else {
            self.fail(self.text("send.no_peer"));
            return;
        };
        if items.is_empty() {
            return;
        }
        // Receivers (this one included) refuse larger offers with a bare 403;
        // say it here, before anything is sent.
        if items.len() > net::receive::MAX_OFFER_FILES {
            self.fail(
                self.text("error.too_many_files")
                    .replace("{count}", &net::receive::MAX_OFFER_FILES.to_string()),
            );
            return;
        }
        // The actor fails a send to a peer without an address *before* it
        // emits `Started`, so clearing the composer here would lose the
        // staged items for nothing. Keep them and say why.
        if peer.address.is_none() {
            self.fail(self.text("error.peer_offline"));
            return;
        }
        let handle = self.handle.get_untracked();
        let Some(handle) = handle else {
            self.fail(self.text("send.offline"));
            return;
        };
        match handle.send(&peer, items, pin) {
            Ok(_id) => {
                self.clear_composer();
                // The paper plane flies off once per started send.
                self.flight_epoch
                    .set(self.flight_epoch.get().wrapping_add(1));
            }
            Err(error) => self.fail_error("error.send_failed", error),
        }
    }

    pub fn submit_pin(&self, pin: String) {
        let pin = pin.trim().to_string();
        if pin.is_empty() {
            return;
        }
        let Some(request) = self.pin_request.get_untracked() else {
            return;
        };
        self.pin_request.set(None);
        if let Some(handle) = self.handle.get_untracked()
            && let Err(error) = handle.submit_pin(&request.id, pin)
        {
            self.fail_error("error.pin_failed", error);
        }
    }

    pub fn dismiss_pin(&self) {
        if let Some(request) = self.pin_request.get_untracked()
            && let Some(handle) = self.handle.get_untracked()
        {
            let _ = handle.cancel(&request.id);
        }
        self.pin_request.set(None);
    }

    pub fn decide(&self, accept: bool) {
        self.decide_inner(accept, true);
    }

    /// Answers the pending request.
    ///
    /// `bump` is `false` for the auto-accept path inside [`Session::apply_event`]:
    /// that method publishes the event's own change once at the end, and a
    /// second bump here would re-run every transfers binding for one request
    /// (M-13).
    fn decide_inner(&self, accept: bool, bump: bool) {
        // Untracked: `decide` runs in a handler (or in the event task), never
        // in a binding.
        let id = untracked(|| {
            self.state
                .borrow()
                .incoming
                .as_ref()
                .map(|incoming| incoming.id.clone())
        });
        let Some(id) = id else { return };
        if let Some(handle) = self.handle.get_untracked()
            && let Err(error) = handle.decide(&id, accept)
        {
            self.fail_error("error.decide_failed", error);
        }
        self.state.borrow_mut().incoming = None;
        if bump {
            self.changed(Change::transfers());
        }
        self.maybe_apply_pending_restart();
    }

    pub fn cancel(&self, id: &str) {
        let Some(handle) = self.handle.get_untracked() else {
            // The network is gone (a failed start or a restart in progress):
            // no terminal event will arrive, so finish the row locally rather
            // than leaving a cancel button that does nothing.
            self.finish_local_cancel(id);
            return;
        };
        if let Err(error) = handle.cancel(id) {
            self.fail_error("error.cancel_failed", error);
        }
    }

    /// Marks a transfer as cancelled without the network actor.
    fn finish_local_cancel(&self, id: &str) {
        let mut changed = false;
        {
            let mut state = self.state.borrow_mut();
            if let Some(transfer) = state
                .transfers
                .iter_mut()
                .find(|transfer| transfer.id == id)
                && transfer.is_active()
            {
                transfer.status = TransferStatus::Cancelled;
                transfer.finished_at = Some(net::now_ms());
                changed = true;
            }
            if changed {
                state.sort_transfers();
            }
        }
        if changed {
            self.persist_history();
            self.changed(Change::transfers());
            self.maybe_apply_pending_restart();
        }
    }

    // --- files and history -----------------------------------------------

    /// Pick the folder received files are saved into.
    pub fn choose_downloads_dir(&self) {
        crate::platform::files::choose_downloads_dir(self);
    }

    pub fn open_downloads(&self) {
        crate::platform::files::open_downloads(self);
    }

    pub fn open_file(&self, path: PathBuf) {
        crate::platform::files::open(self, path);
    }

    pub fn reveal_file(&self, path: PathBuf) {
        crate::platform::files::reveal(self, path);
    }

    pub fn copy_text(&self, text: String) {
        set_clipboard(text);
        self.toast(self.text("common.copied"));
    }

    pub fn clear_history(&self) {
        animate();
        self.state
            .borrow_mut()
            .transfers
            .retain(|transfer| transfer.is_active());
        self.persist_history();
        self.changed(Change::transfers());
    }

    pub fn toggle_favorite(&self, fingerprint: &str) {
        let fingerprint = fingerprint.to_string();
        self.update_settings(|settings| {
            if let Some(index) = settings
                .favorites
                .iter()
                .position(|existing| *existing == fingerprint)
            {
                settings.favorites.remove(index);
            } else {
                settings.favorites.push(fingerprint);
            }
        });
    }

    /// The app version shown in About.
    pub fn version(&self) -> &'static str {
        env!("CARGO_PKG_VERSION")
    }

    /// The LocalSend protocol version this client speaks.
    pub fn protocol_version(&self) -> &'static str {
        net::PROTOCOL_VERSION
    }

    // --- settings ---------------------------------------------------------

    /// Applies `f` to the settings, validates the result, persists it and
    /// reacts when the change needs the network node to restart.
    ///
    /// Re-entrancy: `f` runs while the settings are mutably borrowed. Do not
    /// call another `Session` method (or read the state) from inside `f`:
    /// that panics with "already borrowed" (M-19). Change the fields you need
    /// and return; the session publishes and persists the result.
    ///
    /// A change the running node cannot pick up (alias, port, TLS, PIN,
    /// discovery, device type, receive folder) restarts the network: right
    /// away when it is idle, or once the last transfer/pending request
    /// finishes when work is in flight (M-02). A receive folder nothing can be
    /// written to is rejected and the previous one is kept (M-04).
    pub fn update_settings(&self, f: impl FnOnce(&mut Settings)) {
        let before = self.state.borrow().settings.clone();
        f(&mut self.state.borrow_mut().settings);

        // An unwritable folder makes the actor silently fall back to the app
        // data directory while the UI keeps promising the chosen one.
        let mut unwritable = false;
        let downloads = self.state.borrow().settings.downloads.clone();
        if downloads != before.downloads && !writable_downloads(&downloads) {
            self.state.borrow_mut().settings.downloads = before.downloads.clone();
            unwritable = true;
        }
        // Repairs values the UI does not guard (empty alias, port 0,
        // duplicates, …) so a non-UI caller cannot poison the settings.
        let repairs = self.state.borrow_mut().settings.sanitize();
        if !repairs.is_empty() {
            tracing::warn!(fields = ?repairs, "Repaired invalid settings");
        }

        let after = self.state.borrow().settings.clone();
        if after == before && !unwritable {
            return;
        }
        self.language.set(after.language);
        self.appearance.set(after.appearance);
        self.apply_color_scheme(after.appearance);
        // M-18: a failed write is reported below (so a deferral toast cannot
        // hide it), but a later update always tries again.
        let save_error = save_settings(&after).err();
        self.state.borrow_mut().sort_devices();
        if settings_need_restart(&before, &after) {
            // Toggles and the alias field can fire several changes in a row
            // (and an HTTPS/discoverable switch restarts the whole node);
            // coalesce them into one restart instead of a stop/start storm.
            if self.has_active_transfers() {
                // Never cancel running work just because a setting changed:
                // apply it once the transfers/pending request are done.
                self.set_pending_restart(true);
                self.toast(self.text("toast.settings_deferred"));
            } else {
                self.restart_network_soon();
            }
        }
        if let Some(error) = save_error {
            self.fail_error("error.settings_not_saved", error);
        }
        if unwritable {
            // The setting was not applied; show this last so it is the toast
            // the user actually sees.
            self.fail(self.text("error.no_writable"));
        }
        self.changed(Change {
            settings: true,
            // The device list order depends on the favorites.
            peers: before.favorites != after.favorites,
            transfers: false,
            composer: false,
        });
    }

    // --- network ----------------------------------------------------------

    /// Starts the network node. Idempotent; a no-op in demo/offline mode
    /// (L-01) and while a start is already in flight.
    pub fn start_network(&self) {
        if network_disabled() || self.shutting_down.get() {
            return;
        }
        #[cfg(test)]
        if self.skip_tasks.get() {
            return;
        }
        if self.running.replace(true) {
            return;
        }
        let generation = self.network_generation.get().wrapping_add(1);
        self.network_generation.set(generation);
        let config = NodeConfig::from_settings(&self.state.borrow().settings);
        let this = self.clone();
        self.task(async move {
            // Only the current network task may publish the handle/identity or
            // clear `running`; a superseded task's teardown must not clobber
            // its successor (restart_network starts the new node before the old
            // one has finished shutting down).
            let current = {
                let generations = Rc::clone(&this.network_generation);
                move || generations.get() == generation
            };
            match Node::start(config).await {
                Ok(node) => {
                    if !current() {
                        // A restart superseded this task while it was starting.
                        node.shutdown().await;
                        return;
                    }
                    // A working node clears the failure and the retry budget.
                    this.network_start_attempts.set(0);
                    this.state.borrow_mut().error = None;
                    this.identity.set(Some(node.device.clone()));
                    this.handle.set(Some(node.handle.clone()));
                    this.changed_all();
                    while let Ok(event) = node.events.recv().await {
                        if !current() {
                            break;
                        }
                        this.apply_event(event);
                    }
                    if current() {
                        // The node stopped without a restart being asked for
                        // (the actor only stops on command): no terminal
                        // events will arrive for the running transfers.
                        this.handle_network_lost("error.network_lost");
                    }
                    node.shutdown().await;
                }
                Err(error) => {
                    if current() {
                        tracing::warn!(%error, "Cannot start the local network service");
                        this.handle_network_lost("error.start_failed");
                    }
                }
            }
        });
    }

    /// Marks the network as gone, shows the localized error, finishes the
    /// transfers it can no longer report on and schedules a bounded retry
    /// (M-03). `key` is the i18n message for the failure.
    fn handle_network_lost(&self, key: &'static str) {
        self.end_discovery_feedback();
        let message = self.text(key).to_string();
        {
            let mut state = self.state.borrow_mut();
            // The peers are gone and discovery is not running: keeping
            // `discovering`/the stale list would claim a network that is not
            // there (M-03). History and settings are kept.
            state.network_stopped();
            state.error = Some(message.clone());
        }
        self.handle.set(None);
        self.identity.set(None);
        self.verify_peer.set(None);
        self.running.set(false);
        // The replacement node starts from the latest settings, so a deferred
        // settings restart must not fire again on top of it (M-02).
        self.set_pending_restart(false);
        self.finalize_network_transfers();
        self.fail(self.text(key));
        self.changed(Change::peers());
        self.schedule_start_retry();
    }

    /// Retries the start with a bounded backoff; a manual `retry_network`
    /// resets the budget (M-03).
    fn schedule_start_retry(&self) {
        let attempt = self.network_start_attempts.get().wrapping_add(1);
        self.network_start_attempts.set(attempt);
        let Some(delay) = retry_delay(attempt) else {
            tracing::warn!(
                attempt,
                "The local network service keeps failing; waiting for a manual restart"
            );
            return;
        };
        tracing::info!(attempt, ?delay, "Retrying the local network service");
        #[cfg(test)]
        if self.skip_tasks.get() {
            return;
        }
        let this = self.clone();
        self.task(async move {
            timer(delay).await;
            if !this.running.get() && !this.shutting_down.get() {
                this.start_network();
            }
        });
    }

    /// Restarts the network, resetting the automatic-retry budget. Used by
    /// the menu item and by the persistent-error UI (M-01/M-03).
    pub fn retry_network(&self) {
        self.network_start_attempts.set(0);
        self.restart_network();
    }

    /// Applies one network event and publishes the part of the state it
    /// changed (one revision bump per event, as before).
    fn apply_event(&self, event: TransferEvent) {
        match &event {
            TransferEvent::DiscoveryActive(true) if !self.state.borrow().discovering => {
                self.begin_discovery_feedback();
            }
            TransferEvent::DiscoveryActive(false) | TransferEvent::PeerFound(_) => {
                self.end_discovery_feedback();
            }
            _ => {}
        }

        if let TransferEvent::PinRequired { id, peer } = &event {
            self.pin_request.set(Some(PinRequest {
                id: id.clone(),
                peer: peer.clone(),
            }));
            return;
        }
        let change = change_of(&event);
        let terminal = terminal_id(&event).map(str::to_owned);
        let net_error = match &event {
            TransferEvent::NetError(message) => Some(message.clone()),
            _ => None,
        };
        if let TransferEvent::RequestRefused(reason) = &event {
            // Peer-triggered and expected: say why, do not claim the network
            // was lost (the old mapping turned "disk full" into "network lost").
            self.fail(self.text(refuse_reason_key(*reason)));
            return;
        }
        // The rule is deliberately evaluated *before* `apply`, from the live
        // setting, and the decision is sent *after* it: `apply` creates (or
        // replaces) `state.incoming`, so `decide` reads the id of the request
        // that just arrived and the request cannot stay pending in the UI.
        let auto_accept = auto_accept(&event, &self.state.borrow().settings);
        self.state.borrow_mut().apply(event);
        if auto_accept {
            // One bump for the whole request: `changed(change)` below
            // publishes it, so `decide` must not bump again (M-13).
            self.decide_inner(true, false);
        }
        if let Some(id) = terminal {
            // A terminal state for the transfer whose PIN is being requested
            // means the request can no longer be answered (M-10).
            if self
                .pin_request
                .get_untracked()
                .is_some_and(|pin| pin.id == id)
            {
                self.pin_request.set(None);
            }
            self.persist_history();
        }
        if let Some(message) = net_error {
            // M-01: network faults (listener/multicast/worker failure) must
            // reach the user, not just a field no view reads. The detail is
            // English implementation text: log it, show the localized line.
            tracing::warn!(%message, "The local network reported an error");
            self.fail(self.text("error.network_lost"));
        }
        self.maybe_apply_pending_restart();
        self.changed(change);
    }

    /// Restarts the network after a short quiet period (settings changes).
    fn restart_network_soon(&self) {
        // Bump first: a newer change invalidates this pending restart even
        // when the restart itself is skipped (demo/offline/tests).
        let epoch = self.restart_debounce.get().wrapping_add(1);
        self.restart_debounce.set(epoch);
        if network_disabled() {
            return;
        }
        #[cfg(test)]
        if self.skip_tasks.get() {
            return;
        }
        let this = self.clone();
        self.task(async move {
            timer(Duration::from_millis(400)).await;
            if this.restart_debounce.get() == epoch {
                this.restart_network();
            }
        });
    }

    /// Restarts the network immediately, finishing the transfers the old node
    /// was running. No-op in demo/offline mode (L-01).
    pub fn restart_network(&self) {
        if network_disabled() {
            return;
        }
        // Invalidate a pending debounced restart too: this one supersedes it.
        let epoch = self.restart_debounce.get().wrapping_add(1);
        self.restart_debounce.set(epoch);
        self.set_pending_restart(false);
        // A deliberate restart gets a fresh automatic-retry budget.
        self.network_start_attempts.set(0);
        // Bump the generation first: from here on, the old task's teardown is
        // stale and may not touch the handle/identity/running flag.
        self.network_generation
            .set(self.network_generation.get().wrapping_add(1));
        if let Some(handle) = self.handle.get_untracked() {
            let _ = handle.stop();
        }
        self.handle.set(None);
        self.identity.set(None);
        self.verify_peer.set(None);
        self.running.set(false);
        // The actor's shutdown emits no terminal events for running transfers
        // or a pending decision, so finish them here or the UI would show a
        // zombie transfer forever (and `can_send` would stay false).
        self.finalize_network_transfers();
        self.start_network();
    }

    /// Applies a deferred settings restart once no work is in flight (M-02).
    fn maybe_apply_pending_restart(&self) {
        let ready = self.state.borrow_mut().take_pending_restart();
        if ready {
            self.pending_restart.set(false);
            self.changed(Change::settings());
            self.restart_network_soon();
        }
    }

    fn set_pending_restart(&self, pending: bool) {
        self.state.borrow_mut().pending_restart = pending;
        self.pending_restart.set(pending);
    }

    /// Finishes every transfer that the network will not report on anymore,
    /// and clears the transient request state.
    fn finalize_network_transfers(&self) {
        let mut changed = false;
        {
            let mut state = self.state.borrow_mut();
            let now = net::now_ms();
            for transfer in state.transfers.iter_mut() {
                if transfer.is_active() {
                    transfer.status = TransferStatus::Cancelled;
                    transfer.finished_at = Some(now);
                    changed = true;
                }
            }
            if state.incoming.take().is_some() {
                changed = true;
            }
            if changed {
                state.sort_transfers();
            }
        }
        self.pin_request.set(None);
        if changed {
            self.persist_history();
            self.changed(Change::transfers());
        }
    }

    /// Flushes the session for the quit path: stops the network (the old
    /// task's teardown is invalidated by the generation bump), finishes the
    /// transfers it can no longer report on, and writes history and settings
    /// to disk. Idempotent, and safe when the network never started (M-20).
    pub fn shutdown(&self) {
        // First, so a retry timer that fires while flushing cannot start a node.
        self.shutting_down.set(true);
        self.restart_debounce
            .set(self.restart_debounce.get().wrapping_add(1));
        self.network_generation
            .set(self.network_generation.get().wrapping_add(1));
        if let Some(handle) = self.handle.get_untracked() {
            let _ = handle.stop();
        }
        self.handle.set(None);
        self.identity.set(None);
        self.verify_peer.set(None);
        self.running.set(false);
        self.set_pending_restart(false);
        self.finalize_network_transfers();
        self.persist_history();
        let settings = self.state.borrow().settings.clone();
        if let Err(error) = save_settings(&settings) {
            tracing::warn!(%error, "Cannot flush the settings on shutdown");
        }
    }

    /// Whether quitting would interrupt work: a running transfer or an
    /// incoming request waiting for a decision (INFRA-09's confirmation).
    pub fn has_active_transfers(&self) -> bool {
        untracked(|| self.state.borrow().has_active_work())
    }

    /// The last user-visible error, for a banner (M-01). The toast mirrors it.
    pub fn last_error(&self) -> Option<String> {
        untracked(|| self.state.borrow().error.clone())
    }

    /// Persists the finished transfers and keeps the in-memory list in sync
    /// with what actually fit into the storage quota (M-07).
    fn persist_history(&self) {
        let saved = {
            let state = self.state.borrow();
            save_history(&state.transfers)
        };
        let Some(kept) = saved else { return };
        let mut state = self.state.borrow_mut();
        let finished = state
            .transfers
            .iter()
            .filter(|transfer| !transfer.is_active())
            .count();
        if kept >= finished {
            return;
        }
        // Newest first (see `sort_transfers`): keep the newest `kept` finished
        // rows so a restart does not silently shrink the visible history.
        let mut remaining = kept;
        state.transfers.retain(|transfer| {
            if transfer.is_active() {
                return true;
            }
            let keep = remaining > 0;
            remaining = remaining.saturating_sub(1);
            keep
        });
        drop(state);
        self.changed(Change::transfers());
    }

    /// Fill the UI with sample data for design work and screenshots
    /// (`LOCALSEND_DEMO=1`; `=incoming` and `=pin` seed the sheets).
    ///
    /// `LOCALSEND_DEMO_PEERS=<n>` synthesizes `n` nearby devices instead of the
    /// default three (the first half online), for measuring long device lists.
    fn seed_demo(&self) {
        let now = net::now_ms();
        let mode = std::env::var("LOCALSEND_DEMO").unwrap_or_default();
        let peer_count = std::env::var("LOCALSEND_DEMO_PEERS")
            .ok()
            .and_then(|value| value.parse::<usize>().ok())
            .unwrap_or(3)
            .max(1);
        let mut state = self.state.borrow_mut();
        state.discovering = true;
        state.devices = demo_peers(peer_count);
        state.selected = state.devices.first().map(|peer| peer.fingerprint.clone());
        let macbook = state.devices[0].clone();
        let iphone = state
            .devices
            .get(1)
            .cloned()
            .unwrap_or_else(|| macbook.clone());
        state.transfers = vec![
            demo_transfer(
                "demo-active",
                iphone.clone(),
                crate::net::Direction::Receiving,
                "IMG_2041.HEIC",
                "image/heic",
                6_200_000,
                2_300_000,
                crate::model::TransferStatus::Active,
                now,
            ),
            demo_transfer(
                "demo-done-1",
                macbook.clone(),
                crate::net::Direction::Sending,
                "LocalSend-Q3-plan.pdf",
                "application/pdf",
                2_400_000,
                2_400_000,
                crate::model::TransferStatus::Completed,
                now - 3_600_000,
            ),
            demo_transfer(
                "demo-done-2",
                iphone.clone(),
                crate::net::Direction::Receiving,
                "holiday-2026.png",
                "image/png",
                8_100_000,
                8_100_000,
                crate::model::TransferStatus::Completed,
                now - 86_400_000 - 5_400_000,
            ),
            demo_transfer(
                "demo-cancelled",
                iphone,
                crate::net::Direction::Sending,
                "video.mov",
                "video/quicktime",
                120_000_000,
                31_000_000,
                crate::model::TransferStatus::Cancelled,
                now - 86_400_000 - 9_000_000,
            ),
        ];
        state.sort_transfers();
        state.composer = vec![
            ComposerItem::File {
                path: PathBuf::from("/tmp/LocalSend-Q3-plan.pdf"),
                name: "LocalSend-Q3-plan.pdf".into(),
                size: 2_400_000,
                mime: "application/pdf".into(),
                is_dir: false,
            },
            ComposerItem::File {
                path: PathBuf::from("/tmp/screenshot.png"),
                name: "screenshot.png".into(),
                size: 840_000,
                mime: "image/png".into(),
                is_dir: false,
            },
        ];
        if mode == "empty" {
            state.composer.clear();
            state.transfers.clear();
            state.incoming = None;
        } else if mode == "incoming" {
            state.composer.clear();
            state.transfers.retain(|transfer| !transfer.is_active());
            state.incoming = Some(crate::model::IncomingRequest {
                id: "demo-incoming".into(),
                peer: state
                    .devices
                    .get(1)
                    .cloned()
                    .unwrap_or_else(|| state.devices[0].clone()),
                files: (1..=7)
                    .map(|index| crate::net::OfferedFile {
                        name: format!("IMG_{:04}.HEIC", 2040 + index),
                        size: 6_200_000,
                        mime: "image/heic".into(),
                        saved_to: None,
                    })
                    .collect(),
                total: 43_400_000,
            });
        } else if mode == "pin" {
            let id = "demo-pin".to_string();
            let peer = state
                .devices
                .get(1)
                .cloned()
                .unwrap_or_else(|| state.devices[0].clone());
            drop(state);
            self.pin_request.set(Some(PinRequest { id, peer }));
            return;
        }
        state.error = None;
        drop(state);
        // `LOCALSEND_DEMO=progress` drives the active transfer forward so the
        // transfer card's per-event cost can be measured (and screenshotted)
        // without a network peer.
        if mode == "progress" {
            let this = self.clone();
            let _ticker = self.task(async move {
                loop {
                    timer(Duration::from_millis(50)).await;
                    let finished = {
                        let mut state = this.state.borrow_mut();
                        let Some(transfer) = state
                            .transfers
                            .iter_mut()
                            .find(|transfer| transfer.is_active())
                        else {
                            return;
                        };
                        let step = (transfer.total / 40).max(1);
                        transfer.transferred = (transfer.transferred + step).min(transfer.total);
                        if transfer.transferred >= transfer.total {
                            transfer.status = crate::model::TransferStatus::Completed;
                            transfer.finished_at = Some(crate::net::now_ms());
                            true
                        } else {
                            false
                        }
                    };
                    this.changed(Change::transfers());
                    if finished {
                        return;
                    }
                }
            });
        }
    }
}

// A demo fixture: one call site, every argument is a distinct field.
#[allow(clippy::too_many_arguments)]
fn demo_transfer(
    id: &str,
    peer: Peer,
    direction: crate::net::Direction,
    name: &str,
    mime: &str,
    total: u64,
    transferred: u64,
    status: crate::model::TransferStatus,
    started_at: u64,
) -> crate::model::Transfer {
    crate::model::Transfer {
        id: id.into(),
        peer,
        direction,
        files: vec![crate::net::OfferedFile {
            name: name.into(),
            size: total,
            mime: mime.into(),
            saved_to: None,
        }],
        total,
        transferred,
        status,
        error: None,
        saved_paths: Vec::new(),
        texts: Vec::new(),
        started_at,
        finished_at: (status != crate::model::TransferStatus::Active).then_some(started_at + 4_000),
        omitted_files: 0,
    }
}

/// The demo device list: the three designed examples, then synthetic rows.
fn demo_peers(count: usize) -> Vec<Peer> {
    // The first half is online, the rest are remembered-but-offline.
    let online = count.div_ceil(2);
    let mut peers = vec![
        demo_peer(
            "Peer MacBook Pro",
            "MacBook Pro",
            crate::net::DeviceType::Desktop,
            true,
        ),
        demo_peer(
            "iPhone 15 Pro",
            "iPhone 15 Pro",
            crate::net::DeviceType::Mobile,
            1 < online,
        ),
        demo_peer(
            "Living Room TV",
            "Android TV",
            crate::net::DeviceType::Headless,
            2 < online,
        ),
    ];
    let kinds = [
        crate::net::DeviceType::Desktop,
        crate::net::DeviceType::Mobile,
        crate::net::DeviceType::Headless,
        crate::net::DeviceType::Server,
        crate::net::DeviceType::Web,
    ];
    for index in peers.len()..count {
        let device_type = kinds[index % kinds.len()];
        peers.push(demo_peer(
            &format!("Demo Device {index}"),
            &format!("Demo {device_type:?}"),
            device_type,
            index < online,
        ));
    }
    peers.truncate(count);
    peers
}

/// A demo has no node, so it has no certificate to derive an identity from;
/// a stable fake one lets the verify sheet (which combines this device's
/// fingerprint with the peer's) open in design/screenshot runs too. Called
/// after [`Session::seed_demo`], so no `AppState` borrow is held when the
/// signal runs its subscribers.
fn seed_demo_identity(session: &Session) {
    let alias = session.state.borrow().settings.alias.clone();
    session.identity.set(Some(Peer {
        fingerprint: demo_self_fingerprint(),
        alias,
        device_model: "Mac".to_string(),
        address: None,
        device_type: crate::net::DeviceType::Desktop,
        is_self: true,
    }));
}

/// The demo self device's fingerprint: 64 uppercase hex, as a real
/// certificate hash would be.
fn demo_self_fingerprint() -> String {
    "9C4C0A5E".repeat(8)
}

fn demo_peer(alias: &str, model: &str, device_type: crate::net::DeviceType, online: bool) -> Peer {
    Peer {
        fingerprint: format!("DEMO{alias}"),
        alias: alias.to_string(),
        device_model: model.to_string(),
        address: online.then(|| crate::net::PeerAddress {
            host: "192.168.1.24".into(),
            port: 53317,
            protocol: crate::net::Protocol::Https,
        }),
        device_type,
        is_self: false,
    }
}

/// Expand directories into their files, and remember each file's metadata.
fn collect_paths(paths: Vec<PathBuf>) -> Vec<ComposerItem> {
    let mut items = Vec::new();
    let mut visited = HashSet::new();
    for path in paths {
        // A dropped folder keeps its own name and structure on the receiver
        // (`Album/2024/photo.jpg`), like the official clients send it: names
        // are relative to the folder's parent.
        let base = path.parent().map(Path::to_path_buf).unwrap_or_default();
        collect_one(&path, &base, &mut items, &mut visited);
    }
    items
}

/// The protocol name of `path` below `base`: its components joined with `/`
/// (never the platform separator), or the bare file name as a fallback.
fn wire_name(path: &Path, base: &Path) -> String {
    let relative = path.strip_prefix(base).unwrap_or(path);
    let segments: Vec<String> = relative
        .components()
        .filter_map(|component| match component {
            std::path::Component::Normal(segment) => Some(segment.to_string_lossy().into_owned()),
            _ => None,
        })
        .collect();
    if segments.is_empty() {
        path.file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default()
    } else {
        segments.join("/")
    }
}

fn collect_one(
    path: &Path,
    base: &Path,
    items: &mut Vec<ComposerItem>,
    visited: &mut HashSet<PathBuf>,
) {
    let Ok(metadata) = std::fs::metadata(path) else {
        return;
    };
    if metadata.is_dir() {
        // `metadata` follows a symlinked directory, so a link back up the tree
        // (`node_modules`-style setups do this) would recurse until the stack
        // overflows and kills the process. Visit every real directory once.
        let Ok(real) = std::fs::canonicalize(path) else {
            return;
        };
        if !visited.insert(real) {
            return;
        }
        let mut entries: Vec<_> = std::fs::read_dir(path)
            .into_iter()
            .flatten()
            .flatten()
            .map(|entry| entry.path())
            .filter(|path| {
                path.file_name()
                    .and_then(|name| name.to_str())
                    .is_none_or(|name| !name.starts_with('.'))
            })
            .collect();
        entries.sort();
        for entry in entries {
            collect_one(&entry, base, items, visited);
        }
        return;
    }
    let name = wire_name(path, base);
    if name.is_empty() {
        return;
    }
    items.push(ComposerItem::File {
        path: path.to_path_buf(),
        name,
        size: metadata.len(),
        mime: mime_guess::from_path(path)
            .first_or_octet_stream()
            .to_string(),
        is_dir: false,
    });
}

/// Appends the files of `collected` that are not staged yet; returns how many
/// were added. One hash set instead of a scan of the composer per item: a
/// 10,000-file folder used to cost ~50 million `PathBuf` clones here.
fn stage_new_items(composer: &mut Vec<ComposerItem>, collected: Vec<ComposerItem>) -> usize {
    let mut staged: HashSet<PathBuf> = composer
        .iter()
        .filter_map(|item| match item {
            ComposerItem::File { path, .. } => Some(path.clone()),
            ComposerItem::Text(_) => None,
        })
        .collect();
    let before = composer.len();
    for item in collected {
        if let ComposerItem::File { path, .. } = &item
            && !staged.insert(path.clone())
        {
            continue;
        }
        composer.push(item);
    }
    composer.len() - before
}

fn animate() {
    if reduce_motion() {
        return;
    }
    use nsh_core::{LayoutAnimation, LayoutCurve, LayoutTransition};
    animate_layout(LayoutAnimation::all(LayoutTransition::new(
        LayoutCurve::EaseOut,
        160.0,
    )));
}

fn load_settings() -> Settings {
    // After the app was renamed (bundle id and storage location changed), the
    // previous store still holds the user's preferences: use it once, then the
    // new store takes over (the defaults are persisted below).
    let mut migrated = false;
    let stored = Storage::get_item(SETTINGS_KEY).or_else(|| {
        let legacy = legacy_storage_value(SETTINGS_KEY);
        if legacy.is_some() {
            migrated = true;
        }
        legacy
    });
    let Some(json) = stored else {
        // First launch: remember the defaults (including the machine name and
        // the system locale) so later launches do not recompute them.
        let settings = first_run_settings();
        if let Err(error) = save_settings(&settings) {
            tracing::warn!(%error, "Cannot save the first-run settings");
        }
        return settings;
    };
    let Some((mut settings, mut changed)) = settings_from_json(&json) else {
        // A corrupt or foreign settings file: keep running with defaults
        // rather than failing to start, but say so (and keep the file — it is
        // the user's only copy of the preferences).
        return first_run_settings();
    };
    if ensure_writable_downloads(&mut settings) {
        changed = true;
    }
    if migrated {
        // Adopt the previous store's values into the new one so later launches
        // do not depend on the old app directory.
        tracing::info!("migrated settings from the previous app name");
        changed = true;
    }
    if changed {
        // Persist the repaired/migrated values, so a later launch starts from
        // a valid file instead of repairing it again.
        if let Err(error) = save_settings(&settings) {
            tracing::warn!(%error, "Cannot save the repaired settings");
        }
    }
    settings
}

/// Parses a stored settings value and repairs what can be repaired
/// (M-08/M-09).
///
/// Missing fields keep their serde defaults; a file from a newer schema is
/// still read best-effort (every field has a default, unknown fields are
/// ignored). Returns whether the value differs from what the file said, so the
/// caller can persist the repair. `None` means the file is not settings JSON
/// at all.
fn settings_from_json(json: &str) -> Option<(Settings, bool)> {
    let mut settings: Settings = match serde_json::from_str(json) {
        Ok(settings) => settings,
        Err(error) => {
            tracing::warn!(%error, "Cannot parse the saved settings; using defaults");
            return None;
        }
    };
    let mut changed = false;
    if settings.schema > SETTINGS_SCHEMA {
        tracing::warn!(
            schema = settings.schema,
            known = SETTINGS_SCHEMA,
            "The saved settings are from a newer version; reading them best-effort"
        );
    }
    if settings.schema != SETTINGS_SCHEMA {
        settings.schema = SETTINGS_SCHEMA;
        changed = true;
    }
    let repairs = settings.sanitize();
    if !repairs.is_empty() {
        tracing::warn!(fields = ?repairs, "Repaired invalid settings");
        changed = true;
    }
    Some((settings, changed))
}

/// Falls back to the platform's default download folder when the configured
/// one cannot be written (M-04/M-08). Returns whether it changed the setting.
fn ensure_writable_downloads(settings: &mut Settings) -> bool {
    let fallback = default_downloads();
    // Do not probe the platform default: it is the value a fresh install uses,
    // and touching `~/Downloads` on every launch can raise a macOS privacy
    // prompt (and logs a misleading warning when a sandbox blocks the probe).
    // The network actor still falls back if the default turns out unwritable.
    if settings.downloads == fallback {
        return false;
    }
    if writable_downloads(&settings.downloads) {
        return false;
    }
    tracing::warn!(
        downloads = %settings.downloads.display(),
        fallback = %fallback.display(),
        "The download folder is not writable; using the default"
    );
    settings.downloads = fallback;
    true
}

/// Whether a file can actually be created in `dir` (creating the directory
/// first when it does not exist).
///
/// The network actor falls back to `<app data>/Received` when it cannot write
/// the configured folder, and the UI would keep showing the old choice; the
/// model rejects it up front instead (M-04). One empty probe file is written
/// and removed; the call is already on the engine thread and a single probe is
/// cheaper than a `spawn_blocking` round trip.
fn writable_downloads(dir: &Path) -> bool {
    if dir.as_os_str().is_empty() || std::fs::create_dir_all(dir).is_err() {
        return false;
    }
    let probe = dir.join(format!(".localsend-write-probe-{}", std::process::id()));
    match std::fs::write(&probe, b"") {
        Ok(()) => {
            let _ = std::fs::remove_file(&probe);
            true
        }
        Err(_) => false,
    }
}

/// Settings for a fresh install: the UI language follows the system language
/// (LocalSend does the same), everything else is the app's default.
fn first_run_settings() -> Settings {
    let mut settings = Settings::default();
    if system_language().starts_with("zh") {
        settings.language = Language::Zh;
    }
    settings
}

/// The user's preferred language as a BCP 47 tag.
///
/// nsh's `locale()` on macOS is `NSLocale.currentLocale`, which follows the app
/// bundle's localizations (this app ships none), not the user's preferred
/// language list: read the system preference directly there and fall back to
/// nsh elsewhere.
fn system_language() -> String {
    #[cfg(target_os = "macos")]
    {
        if let Ok(output) = std::process::Command::new("defaults")
            .args(["read", "-g", "AppleLanguages"])
            .output()
        {
            let text = String::from_utf8_lossy(&output.stdout);
            if let Some(language) = text
                .lines()
                .map(str::trim)
                .find(|line| line.starts_with('"') && line.trim_matches(['"', ',']).len() >= 2)
            {
                return language.trim_matches(['"', ',']).to_ascii_lowercase();
            }
        }
        if let Ok(output) = std::process::Command::new("defaults")
            .args(["read", "-g", "AppleLocale"])
            .output()
        {
            let language = String::from_utf8_lossy(&output.stdout)
                .trim()
                .to_ascii_lowercase();
            if !language.is_empty() {
                return language;
            }
        }
    }
    locale().as_str().to_ascii_lowercase()
}

/// App ids this app used before, newest first. Their `storage.json` holds the
/// user's settings and history until the current store has its own.
const PREVIOUS_APP_IDS: [&str; 2] = ["org.localsend.client", "org.localsend.native"];

/// A value from the storage file of the app before it was renamed (see
/// [`PREVIOUS_APP_IDS`]), used once while the new store is still empty.
///
/// Settings and history live in nsh's `<data dir>/org.localsend.desktop/
/// storage.json`; the device identity now lives in the same app directory and
/// `net` migrates its own previous locations, so this is the only legacy store
/// that holds settings and history (M-17).
fn legacy_storage_value(key: &str) -> Option<String> {
    legacy_storage_value_in(&dirs::data_dir()?, key)
}

fn legacy_storage_value_in(data_dir: &std::path::Path, key: &str) -> Option<String> {
    PREVIOUS_APP_IDS.iter().find_map(|app_id| {
        let path = data_dir.join(app_id).join("storage.json");
        let text = std::fs::read_to_string(path).ok()?;
        let value: serde_json::Value = serde_json::from_str(&text).ok()?;
        value.get(key)?.as_str().map(str::to_owned)
    })
}

/// Persists the settings; a failure is returned so the caller can tell the
/// user, and never stops later saves from trying again (M-18).
fn save_settings(settings: &Settings) -> Result<(), String> {
    write_settings(settings, |json| {
        Storage::set_item(SETTINGS_KEY, json).map_err(|error| error.to_string())
    })
}

/// The settings write shared by the app and the tests: serialize once, write,
/// retry the write once (M-18).
fn write_settings(
    settings: &Settings,
    mut write: impl FnMut(String) -> Result<(), String>,
) -> Result<(), String> {
    let Some(json) = settings_json(settings) else {
        return Err("cannot serialize the settings".to_string());
    };
    match write(json.clone()) {
        Ok(()) => Ok(()),
        Err(first) => {
            tracing::warn!(%first, "Cannot save the settings; retrying once");
            write(json).map_err(|second| format!("{second} (first attempt: {first})"))
        }
    }
}

/// The settings as JSON with unrepresentable values repaired first (M-06):
/// serde serializes `PathBuf` through `str`, so a non-UTF-8 receive folder
/// would make every settings save fail for the rest of the process.
fn settings_json(settings: &Settings) -> Option<String> {
    let mut settings = settings.clone();
    let repairs = settings.sanitize();
    if !repairs.is_empty() {
        tracing::warn!(
            fields = ?repairs,
            "Repaired the settings before writing them"
        );
    }
    match serde_json::to_string(&settings) {
        Ok(json) => Some(json),
        Err(error) => {
            tracing::warn!(%error, "Cannot serialize the settings");
            None
        }
    }
}

fn load_history() -> Vec<Transfer> {
    let Some(json) = Storage::get_item(HISTORY_KEY).or_else(|| legacy_storage_value(HISTORY_KEY))
    else {
        return Vec::new();
    };
    history_from_json(&json, net::now_ms())
}

/// Parses the persisted history, tolerating the legacy bare array (schema 1)
/// and the versioned envelope (schema 2+).
///
/// One malformed record must not throw away the whole history: records are
/// parsed individually and the bad ones are skipped. A file from a newer
/// schema is read best-effort (M-09/MP-25).
fn history_from_json(json: &str, now: u64) -> Vec<Transfer> {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(json) else {
        tracing::warn!("Cannot parse the saved history; discarding it");
        return Vec::new();
    };
    let entries = match value {
        // Schema 1: the history was a bare array of transfers.
        serde_json::Value::Array(entries) => entries,
        serde_json::Value::Object(mut object) => {
            let schema = object
                .get("schema")
                .and_then(serde_json::Value::as_u64)
                .unwrap_or(1) as u32;
            if schema > HISTORY_SCHEMA {
                tracing::warn!(
                    schema,
                    known = HISTORY_SCHEMA,
                    "The saved history is from a newer version; reading it best-effort"
                );
            }
            match object.remove("entries") {
                Some(serde_json::Value::Array(entries)) => entries,
                _ => {
                    tracing::warn!("The saved history has no entries array; discarding it");
                    return Vec::new();
                }
            }
        }
        _ => {
            tracing::warn!("The saved history is neither an array nor an object; discarding it");
            return Vec::new();
        }
    };
    let mut skipped = 0usize;
    let mut history = Vec::with_capacity(entries.len());
    for entry in entries {
        match serde_json::from_value::<Transfer>(entry) {
            Ok(transfer) => history.push(transfer),
            Err(_) => skipped += 1,
        }
    }
    if skipped > 0 {
        tracing::warn!(
            skipped,
            kept = history.len(),
            "Skipped malformed history records"
        );
    }
    // A transfer that was active when the app quit is not running anymore:
    // showing it as active would leave a phantom transfer card forever.
    for transfer in &mut history {
        if transfer.is_active() {
            transfer.status = TransferStatus::Cancelled;
            transfer.finished_at = Some(now);
        }
    }
    history.sort_by_key(|transfer| std::cmp::Reverse(transfer.started_at));
    history.truncate(HISTORY_LIMIT);
    history
}

/// Whether every path in `transfer` can be represented in JSON.
///
/// serde serializes `Path`/`PathBuf` as a string and fails on a non-UTF-8
/// path (ext4 allows arbitrary bytes in file names). One such record must not
/// disable history persistence for the rest of the process (M-06).
fn transfer_paths_representable(transfer: &Transfer) -> bool {
    transfer
        .saved_paths
        .iter()
        .all(|path| path.to_str().is_some())
        && transfer.files.iter().all(|file| {
            file.saved_to
                .as_ref()
                .is_none_or(|path| path.to_str().is_some())
        })
}

/// The finished transfers serialized as JSON objects, once each, plus the
/// number of records skipped because of non-UTF-8 paths (M-06/M-07).
fn history_records(transfers: &[Transfer]) -> (Vec<String>, usize) {
    let mut records = Vec::with_capacity(transfers.len());
    let mut skipped = 0usize;
    for transfer in transfers.iter().filter(|transfer| !transfer.is_active()) {
        if !transfer_paths_representable(transfer) {
            skipped += 1;
            continue;
        }
        match serde_json::to_string(&transfer.compacted_for_history()) {
            Ok(record) => records.push(record),
            Err(error) => {
                tracing::warn!(%error, id = %transfer.id, "Cannot serialize a history record; skipping it");
                skipped += 1;
            }
        }
    }
    (records, skipped)
}

/// Builds the versioned history envelope from already-serialized records.
fn history_envelope(records: &[String], limit: usize) -> String {
    let mut json =
        String::with_capacity(32 + records.iter().take(limit).map(String::len).sum::<usize>());
    json.push_str("{\"schema\":");
    json.push_str(&HISTORY_SCHEMA.to_string());
    json.push_str(",\"entries\":[");
    for (index, record) in records.iter().take(limit).enumerate() {
        if index > 0 {
            json.push(',');
        }
        json.push_str(record);
    }
    json.push_str("]}");
    json
}

/// The JSON for the persisted history: the newest finished transfers, capped.
#[cfg(test)]
fn history_json(transfers: &[Transfer], limit: usize) -> Option<String> {
    let (mut records, _) = history_records(transfers);
    records.truncate(limit);
    Some(history_envelope(&records, limit))
}

/// Persist the finished transfers (newest first, capped) and report how many
/// actually fit; `None` means nothing was written.
///
/// History and settings share one 5 MiB Storage quota (nsh-core
/// `storage::QUOTA`), and a received message keeps its text body in the
/// history record (up to 64 KiB each, measured: 100 of them are ~6.5 MiB, so
/// the quota is reachable). A refused write keeps the previous value, so
/// without a retry history would stop updating *silently* for the rest of the
/// process. Halve the list until it fits, log what was dropped, and let the
/// caller truncate the in-memory list to the same size (M-07).
pub(crate) fn save_history(transfers: &[Transfer]) -> Option<usize> {
    let (mut records, skipped) = history_records(transfers);
    if skipped > 0 {
        tracing::warn!(
            skipped,
            kept = records.len(),
            "Skipped history records that JSON cannot represent (non-UTF-8 paths)"
        );
    }
    let finished = records.len().min(HISTORY_LIMIT);
    records.truncate(finished);
    let mut limit = finished;
    loop {
        let json = history_envelope(&records, limit);
        match Storage::set_item(HISTORY_KEY, json) {
            Ok(()) => {
                if limit < finished {
                    tracing::warn!(
                        kept = limit,
                        finished,
                        "The transfer history does not fit in the storage quota; kept the newest entries"
                    );
                }
                return Some(limit);
            }
            // Never halve down to an empty list: writing `[]` "succeeds" and
            // the caller would then truncate the in-memory history to nothing.
            // If not even the newest record fits, keep the previous value.
            Err(error) if limit > 1 => {
                tracing::warn!(%error, limit, "The transfer history is too large; retrying with fewer entries");
                limit /= 2;
            }
            Err(error) => {
                tracing::warn!(%error, limit, "Cannot save the transfer history; kept the previous one");
                return None;
            }
        }
    }
}

/// Whether this process must not start or restart the network (L-01).
///
/// The demo seeds its own peers and the in-process tests must not bind the
/// LocalSend port or write the user's device identity.
fn network_disabled() -> bool {
    std::env::var_os("LOCALSEND_DEMO").is_some() || std::env::var_os("LOCALSEND_OFFLINE").is_some()
}

/// The number of automatic start attempts before waiting for a manual restart.
const MAX_START_ATTEMPTS: u32 = 3;

/// The delay before retry number `failed_attempt` (1-based), or `None` when
/// the budget is spent (M-03).
fn retry_delay(failed_attempt: u32) -> Option<Duration> {
    match failed_attempt {
        1 => Some(Duration::from_secs(1)),
        2 => Some(Duration::from_secs(2)),
        _ => {
            debug_assert!(failed_attempt >= MAX_START_ATTEMPTS);
            None
        }
    }
}

/// The id whose terminal state ends the transfer (M-10/M-13).
fn terminal_id(event: &TransferEvent) -> Option<&str> {
    match event {
        TransferEvent::Completed { id, .. }
        | TransferEvent::Cancelled { id }
        | TransferEvent::Failed { id, .. } => Some(id),
        _ => None,
    }
}

/// Whether changing the settings requires restarting the network node: the
/// server's identity (alias, port, TLS, PIN), discovery, the device type and
/// the receive folder are all fixed when the actor starts (M-02).
fn settings_need_restart(before: &Settings, after: &Settings) -> bool {
    before.alias != after.alias
        || before.port != after.port
        || before.https != after.https
        || before.pin != after.pin
        || before.discoverable != after.discoverable
        || before.device_type != after.device_type
        || before.downloads != after.downloads
}

/// Which state domain one network event can change.
fn change_of(event: &TransferEvent) -> Change {
    match event {
        TransferEvent::DiscoveryActive(_)
        | TransferEvent::PeerFound(_)
        | TransferEvent::PeerLost(_) => Change::peers(),
        TransferEvent::IncomingRequest { .. }
        | TransferEvent::Started { .. }
        | TransferEvent::Accepted { .. }
        | TransferEvent::Progress { .. }
        | TransferEvent::FileDone { .. }
        | TransferEvent::TextReceived { .. }
        | TransferEvent::Completed { .. }
        | TransferEvent::Cancelled { .. }
        | TransferEvent::Failed { .. } => Change::transfers(),
        // Both surface through their own signal (`pin_request`, `toast`):
        // the global revision still bumps once per event in `apply_event`,
        // which is enough for `read()` subscribers (M-01).
        TransferEvent::PinRequired { .. }
        | TransferEvent::NetError(_)
        | TransferEvent::RequestRefused(_) => Change::NONE,
    }
}

/// The message key for a declined incoming request.
fn refuse_reason_key(reason: net::RefuseReason) -> &'static str {
    match reason {
        net::RefuseReason::Invalid => "error.request_invalid",
        net::RefuseReason::NoSpace => "error.request_no_space",
        net::RefuseReason::Unencrypted => "error.request_unencrypted",
    }
}

/// Whether an event should be accepted without asking the user.
///
/// Only an incoming request can be auto-accepted; a settings toggle while a
/// request is already pending must not accept it retroactively.
fn auto_accept(event: &TransferEvent, settings: &Settings) -> bool {
    let TransferEvent::IncomingRequest { peer, .. } = event else {
        return false;
    };
    settings.quick_save
        && (!settings.quick_save_favorites_only
            || settings
                .favorites
                .iter()
                .any(|favorite| favorite.eq_ignore_ascii_case(&peer.fingerprint)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::TransferStatus;
    use crate::net::{DeviceType, Direction, OfferedFile, PeerAddress, Protocol};

    fn peer() -> Peer {
        Peer {
            fingerprint: "A".to_string(),
            alias: "Peer".to_string(),
            device_model: "Mac".to_string(),
            address: Some(PeerAddress {
                host: "192.168.1.20".to_string(),
                port: 53317,
                protocol: Protocol::Https,
            }),
            device_type: DeviceType::Desktop,
            is_self: false,
        }
    }

    fn incoming() -> TransferEvent {
        TransferEvent::IncomingRequest {
            id: "session".to_string(),
            peer: peer(),
            files: vec![OfferedFile {
                name: "a.bin".to_string(),
                size: 10,
                mime: "application/octet-stream".to_string(),
                saved_to: None,
            }],
            total: 10,
        }
    }

    fn completed_outgoing() -> TransferEvent {
        TransferEvent::Completed {
            id: "send".to_string(),
            files: Vec::new(),
        }
    }

    #[test]
    fn quick_save_auto_accepts_only_requests() {
        let on = Settings {
            quick_save: true,
            ..Settings::default()
        };
        assert!(auto_accept(&incoming(), &on));
        assert!(!auto_accept(&incoming(), &Settings::default()));
        assert!(!auto_accept(&completed_outgoing(), &on));
        assert!(!auto_accept(&TransferEvent::PeerFound(peer()), &on));
    }

    /// "Favorites only" keeps strangers on the network behind the request
    /// sheet; a favorite (any fingerprint casing) is still saved directly.
    #[test]
    fn quick_save_can_be_limited_to_favorites() {
        let mut settings = Settings {
            quick_save: true,
            quick_save_favorites_only: true,
            ..Settings::default()
        };
        assert!(!auto_accept(&incoming(), &settings));
        settings
            .favorites
            .push(peer().fingerprint.to_ascii_lowercase());
        assert!(auto_accept(&incoming(), &settings));
    }

    /// The quick-save ordering contract: an outgoing transfer finishing while
    /// the request sheet is open must leave the request pending; the
    /// auto-accept decision then clears it (not the unrelated terminal event).
    #[test]
    fn outgoing_finish_does_not_swallow_a_pending_request() {
        let mut state = crate::model::AppState::default();
        state.apply(TransferEvent::Started {
            id: "send".to_string(),
            peer: peer(),
            direction: Direction::Sending,
            files: Vec::new(),
            total: 0,
        });
        state.apply(incoming());
        assert!(state.incoming.is_some());
        state.apply(completed_outgoing());
        assert!(
            state.incoming.is_some(),
            "the request must survive the send"
        );
        // What `Session::decide` does once it accepted the request.
        state.incoming = None;
        assert!(state.incoming.is_none());
    }

    /// The event -> domain map the revision split relies on.
    #[test]
    fn events_are_mapped_to_the_state_domain_they_change() {
        let peers = [
            TransferEvent::DiscoveryActive(true),
            TransferEvent::PeerFound(peer()),
            TransferEvent::PeerLost("A".to_string()),
        ];
        for event in peers {
            assert_eq!(change_of(&event), Change::peers(), "{event:?}");
        }
        let transfers = [
            incoming(),
            TransferEvent::Started {
                id: "t".to_string(),
                peer: peer(),
                direction: Direction::Receiving,
                files: Vec::new(),
                total: 0,
            },
            TransferEvent::Accepted {
                id: "t".to_string(),
            },
            TransferEvent::Progress {
                id: "t".to_string(),
                transferred: 1,
                total: Some(2),
            },
            TransferEvent::FileDone {
                id: "t".to_string(),
                file: OfferedFile {
                    name: "a.bin".to_string(),
                    size: 1,
                    mime: "text/plain".to_string(),
                    saved_to: None,
                },
            },
            TransferEvent::TextReceived {
                id: "t".to_string(),
                text: "hi".to_string(),
            },
            completed_outgoing(),
            TransferEvent::Cancelled {
                id: "t".to_string(),
            },
            TransferEvent::Failed {
                id: "t".to_string(),
                error: "no".to_string(),
            },
        ];
        for event in transfers {
            assert_eq!(change_of(&event), Change::transfers(), "{event:?}");
        }
        assert_eq!(
            change_of(&TransferEvent::NetError("x".to_string())),
            Change::NONE
        );
        assert_eq!(
            change_of(&TransferEvent::PinRequired {
                id: "t".to_string(),
                peer: peer(),
            }),
            Change::NONE
        );
    }

    fn history_transfer(id: &str, active: bool) -> Transfer {
        Transfer {
            id: id.to_string(),
            peer: peer(),
            direction: Direction::Receiving,
            files: Vec::new(),
            total: 0,
            transferred: 0,
            status: if active {
                TransferStatus::Active
            } else {
                TransferStatus::Completed
            },
            error: None,
            saved_paths: Vec::new(),
            texts: Vec::new(),
            started_at: 1,
            finished_at: None,
            omitted_files: 0,
        }
    }

    fn envelope_entries(json: &str) -> Vec<serde_json::Value> {
        let value: serde_json::Value = serde_json::from_str(json).unwrap();
        assert_eq!(value["schema"], serde_json::json!(HISTORY_SCHEMA));
        value["entries"].as_array().cloned().unwrap()
    }

    /// A dropped folder keeps its structure on the wire (`/`-joined, below
    /// the folder's parent); a dropped file keeps its bare name.
    #[test]
    fn collected_folders_keep_their_relative_names() {
        let root = tempfile::tempdir().unwrap();
        let album = root.path().join("Album");
        std::fs::create_dir_all(album.join("a")).unwrap();
        std::fs::create_dir_all(album.join("b")).unwrap();
        std::fs::write(album.join("a/x.jpg"), b"1").unwrap();
        std::fs::write(album.join("b/x.jpg"), b"2").unwrap();
        let single = root.path().join("single.txt");
        std::fs::write(&single, b"3").unwrap();

        let names: Vec<String> = collect_paths(vec![album, single])
            .into_iter()
            .map(|item| item.name())
            .collect();
        assert_eq!(names, ["Album/a/x.jpg", "Album/b/x.jpg", "single.txt"]);
    }

    #[test]
    fn staging_skips_paths_that_are_already_staged() {
        let file = |path: &str| ComposerItem::File {
            path: PathBuf::from(path),
            name: path.to_string(),
            size: 1,
            mime: "text/plain".to_string(),
            is_dir: false,
        };
        let mut composer = vec![file("/a"), ComposerItem::Text("hi".to_string())];
        let added = stage_new_items(&mut composer, vec![file("/a"), file("/b"), file("/b")]);
        assert_eq!(added, 1);
        assert_eq!(composer.len(), 3);
    }

    /// A 10,000-file receive is persisted as a bounded record that still
    /// knows its file count, so one big transfer cannot evict the history.
    #[test]
    fn history_records_are_compacted_but_keep_the_file_count() {
        let mut big = history_transfer("big", false);
        big.files = (0..10_000)
            .map(|index| OfferedFile {
                name: format!("folder/file-{index}.bin"),
                size: 1,
                mime: "application/octet-stream".to_string(),
                saved_to: Some(PathBuf::from(format!("/Downloads/folder/file-{index}.bin"))),
            })
            .collect();
        big.saved_paths = big
            .files
            .iter()
            .filter_map(|file| file.saved_to.clone())
            .collect();
        big.texts = vec!["x".repeat(200 * 1024)];

        let json = history_json(&[big.clone()], HISTORY_LIMIT).unwrap();
        assert!(json.len() < 128 * 1024, "record is {} bytes", json.len());
        let restored = history_from_json(&json, 2);
        assert_eq!(restored.len(), 1);
        assert_eq!(restored[0].file_count(), 10_000);
        assert_eq!(
            restored[0].files.len(),
            crate::model::state::HISTORY_FILES_KEPT
        );
        assert_eq!(
            restored[0].texts.iter().map(String::len).sum::<usize>(),
            crate::model::state::HISTORY_TEXT_BYTES
        );
    }

    #[test]
    fn history_json_keeps_only_finished_transfers_up_to_the_limit() {
        let transfers = [
            history_transfer("active", true),
            history_transfer("newest", false),
            history_transfer("older", false),
        ];
        let entries = envelope_entries(&history_json(&transfers, 1).unwrap());
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0]["id"], serde_json::json!("newest"));
        // Never the active one, whatever the limit.
        let entries = envelope_entries(&history_json(&transfers, 10).unwrap());
        assert_eq!(entries.len(), 2);
        assert!(entries.iter().all(|entry| entry["status"] != "Active"));
    }

    /// M-06: one record with a path JSON cannot represent must be skipped,
    /// not disable the whole history write.
    #[cfg(unix)]
    #[test]
    fn history_skips_records_with_non_utf8_paths() {
        use std::os::unix::ffi::OsStringExt;
        let mut bad = history_transfer("bad", false);
        bad.saved_paths = vec![PathBuf::from(std::ffi::OsString::from_vec(vec![
            b'/', b'x', 0xff,
        ]))];
        let good = history_transfer("good", false);
        let entries = envelope_entries(&history_json(&[bad, good], HISTORY_LIMIT).unwrap());
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0]["id"], serde_json::json!("good"));
    }

    #[test]
    fn previous_app_storage_is_read_once_for_migration() {
        let root = tempfile::tempdir().unwrap();
        let old = root.path().join("org.localsend.native");
        std::fs::create_dir_all(&old).unwrap();
        std::fs::write(
            old.join("storage.json"),
            r#"{"settings":"{\"language\":\"zh\"}","history":"[]"}"#,
        )
        .unwrap();
        assert_eq!(
            legacy_storage_value_in(root.path(), "settings").as_deref(),
            Some(r#"{"language":"zh"}"#)
        );
        assert_eq!(
            legacy_storage_value_in(root.path(), "history").as_deref(),
            Some("[]")
        );
        assert_eq!(legacy_storage_value_in(root.path(), "missing"), None);
        // The newest previous id wins over older ones.
        let newer = root.path().join("org.localsend.client");
        std::fs::create_dir_all(&newer).unwrap();
        std::fs::write(
            newer.join("storage.json"),
            r#"{"settings":"{\"language\":\"en\"}"}"#,
        )
        .unwrap();
        assert_eq!(
            legacy_storage_value_in(root.path(), "settings").as_deref(),
            Some(r#"{"language":"en"}"#)
        );
        assert_eq!(
            legacy_storage_value_in(root.path(), "history").as_deref(),
            Some("[]"),
            "a key the newer store lacks falls back to the older one"
        );
        assert_eq!(
            legacy_storage_value_in(&root.path().join("empty"), "settings"),
            None
        );
    }

    // --- headless session tests ------------------------------------------
    //
    // A `Session` needs a reactive runtime for its signals, and the engine's
    // task executor is not available in unit tests: `headless_session` sets
    // `skip_tasks` so the state transitions can be tested without spawning
    // (the engine path is covered by the nsh UI tests). Storage is the
    // in-memory backend of that runtime, so persistence is exercised for
    // real.

    /// Enter a fresh reactive runtime for one test (also isolates Storage).
    fn test_runtime() -> nsh_core::element::reactive::Runtime {
        nsh_core::element::reactive::Runtime::default()
    }

    #[test]
    fn foreground_search_expires_without_stopping_discovery() {
        let runtime = test_runtime();
        let _entered = runtime.enter();
        let session = headless_session(Settings::default());
        session.apply_event(TransferEvent::DiscoveryActive(true));
        assert!(session.discovery_searching());
        let epoch = session.discovery_feedback_epoch.get();
        session.expire_discovery_feedback(epoch);
        assert!(!session.discovery_searching());
        assert!(session.read_peers(|state| state.discovering));
        // Background notifications must not restart foreground feedback.
        session.apply_event(TransferEvent::DiscoveryActive(true));
        assert!(!session.discovery_searching());
    }

    #[test]
    fn finding_a_peer_ends_feedback_and_later_loss_stays_quiet() {
        let runtime = test_runtime();
        let _entered = runtime.enter();
        let session = headless_session(Settings::default());
        session.apply_event(TransferEvent::DiscoveryActive(true));
        let device = peer();
        session.apply_event(TransferEvent::PeerFound(device.clone()));
        assert!(!session.discovery_searching());
        session.apply_event(TransferEvent::PeerLost(device.fingerprint));
        assert!(!session.discovery_searching());
        assert!(session.read_peers(|state| state.discovering));
    }

    #[test]
    fn old_search_timer_cannot_hide_a_new_refresh() {
        let runtime = test_runtime();
        let _entered = runtime.enter();
        let session = headless_session(Settings::default());
        session.apply_event(TransferEvent::DiscoveryActive(true));
        let old_epoch = session.discovery_feedback_epoch.get();
        session.begin_discovery_feedback();
        session.expire_discovery_feedback(old_epoch);
        assert!(session.discovery_searching());
        session.apply_event(TransferEvent::DiscoveryActive(false));
        assert!(!session.discovery_searching());
        assert!(!session.can_refresh_discovery());
    }

    fn headless_session(settings: Settings) -> Session {
        let session = Session::with_state(crate::model::AppState {
            settings,
            ..crate::model::AppState::default()
        });
        session.skip_tasks.set(true);
        session
    }

    fn started_event(id: &str) -> TransferEvent {
        TransferEvent::Started {
            id: id.to_string(),
            peer: peer(),
            direction: Direction::Receiving,
            files: Vec::new(),
            total: 10,
        }
    }

    fn completed_event(id: &str) -> TransferEvent {
        TransferEvent::Completed {
            id: id.to_string(),
            files: Vec::new(),
        }
    }

    #[test]
    fn runtime_and_storage_work_headless() {
        let runtime = test_runtime();
        let _entered = runtime.enter();
        let session = headless_session(Settings::default());
        session.changed(Change::NONE);
        assert_eq!(session.revision.get_untracked(), 1);
        Storage::set_item("probe", "x").unwrap();
        assert_eq!(Storage::get_item("probe").as_deref(), Some("x"));
    }

    /// M-02: a settings change that needs a node restart must not cancel a
    /// running transfer; it is held back and applied exactly once when the
    /// transfer finishes.
    #[test]
    fn settings_change_defers_the_restart_while_a_transfer_runs() {
        let runtime = test_runtime();
        let _entered = runtime.enter();
        let session = headless_session(Settings::default());
        session.state.borrow_mut().apply(started_event("t"));
        let debounce = session.restart_debounce.get();
        let generation = session.network_generation.get();

        session.update_settings(|settings| settings.port = 53400);

        {
            let state = session.state.borrow();
            assert_eq!(
                state.transfer("t").unwrap().status,
                TransferStatus::Active,
                "the running transfer must not be cancelled"
            );
            assert!(state.pending_restart, "the restart is deferred");
        }
        assert!(session.pending_restart.get_untracked());
        assert_eq!(
            session.restart_debounce.get(),
            debounce,
            "no restart scheduled yet"
        );
        assert_eq!(session.network_generation.get(), generation);
        assert_eq!(
            session.toast.get_untracked().map(|toast| toast.error),
            Some(false),
            "the deferral is explained with a normal toast"
        );

        session.apply_event(completed_event("t"));

        assert!(!session.state.borrow().pending_restart);
        assert!(!session.pending_restart.get_untracked());
        assert_eq!(
            session.restart_debounce.get(),
            debounce + 1,
            "exactly one restart is scheduled once the work is done"
        );
    }

    /// M-02: with nothing in flight the restart keeps the existing 400 ms
    /// debounce and is coalesced.
    #[test]
    fn idle_settings_changes_keep_the_debounce() {
        let runtime = test_runtime();
        let _entered = runtime.enter();
        let session = headless_session(Settings::default());
        let debounce = session.restart_debounce.get();
        session.update_settings(|settings| settings.port = 53400);
        session.update_settings(|settings| settings.alias = "Renamed".to_string());
        assert!(!session.state.borrow().pending_restart);
        assert_eq!(
            session.restart_debounce.get(),
            debounce + 2,
            "each change invalidates the previous timer"
        );
        assert_eq!(
            session.network_generation.get(),
            0,
            "start_network is left to the debounce timer"
        );
    }

    /// One `(field name, mutation)` row of the restart-predicate matrix.
    type SettingMutation = (&'static str, Box<dyn Fn(&mut Settings)>);

    /// M-02: every node-bound field is in the restart predicate, and the
    /// purely local ones are not.
    #[test]
    fn settings_restart_predicate_covers_every_node_bound_field() {
        let base = Settings::default();
        let mutators: Vec<SettingMutation> = vec![
            ("alias", Box::new(|s: &mut Settings| s.alias.push('!'))),
            ("port", Box::new(|s: &mut Settings| s.port = 53400)),
            ("https", Box::new(|s: &mut Settings| s.https = !s.https)),
            (
                "pin",
                Box::new(|s: &mut Settings| s.pin = Some("1234".into())),
            ),
            (
                "discoverable",
                Box::new(|s: &mut Settings| s.discoverable = !s.discoverable),
            ),
            (
                "device_type",
                Box::new(|s: &mut Settings| s.device_type = DeviceType::Mobile),
            ),
            (
                "downloads",
                Box::new(|s: &mut Settings| s.downloads = PathBuf::from("/tmp/other")),
            ),
        ];
        for (field, mutate) in mutators {
            let mut after = base.clone();
            mutate(&mut after);
            assert!(
                settings_need_restart(&base, &after),
                "{field} must restart the node"
            );
        }
        let local: Vec<SettingMutation> = vec![
            (
                "language",
                Box::new(|s: &mut Settings| s.language = Language::Zh),
            ),
            (
                "appearance",
                Box::new(|s: &mut Settings| s.appearance = Appearance::Dark),
            ),
            (
                "quick_save",
                Box::new(|s: &mut Settings| s.quick_save = true),
            ),
            (
                "favorites",
                Box::new(|s: &mut Settings| s.favorites = vec!["A".into()]),
            ),
            (
                "run_in_background",
                Box::new(|s: &mut Settings| s.run_in_background = !s.run_in_background),
            ),
        ];
        for (field, mutate) in local {
            let mut after = base.clone();
            mutate(&mut after);
            assert!(
                !settings_need_restart(&base, &after),
                "{field} is local and must not restart the node"
            );
        }
    }

    /// M-04: an unwritable receive folder is rejected and the previous one is
    /// kept, with a localized error instead of a silent actor fallback.
    #[test]
    fn unwritable_downloads_are_rejected() {
        let runtime = test_runtime();
        let _entered = runtime.enter();
        let session = headless_session(Settings::default());
        let before = session.state.borrow().settings.downloads.clone();
        let root = tempfile::tempdir().unwrap();
        let file = root.path().join("not-a-directory");
        std::fs::write(&file, b"x").unwrap();

        session.update_settings(|settings| settings.downloads = file.clone());

        assert_eq!(session.state.borrow().settings.downloads, before);
        let toast = session.toast.get_untracked().unwrap();
        assert!(toast.error);
        assert_eq!(
            toast.text.to_string(),
            i18n::text(Language::En, "error.no_writable").to_string()
        );
    }

    #[test]
    fn writable_downloads_probes_the_directory() {
        let root = tempfile::tempdir().unwrap();
        assert!(writable_downloads(root.path()));
        assert!(writable_downloads(&root.path().join("new-folder")));
        let file = root.path().join("file");
        std::fs::write(&file, b"x").unwrap();
        assert!(!writable_downloads(&file));
        assert!(!writable_downloads(Path::new("")));
    }

    /// M-13: a quick-saved request publishes one transfers revision, not two.
    #[test]
    fn quick_save_accepts_with_a_single_transfers_bump() {
        let runtime = test_runtime();
        let _entered = runtime.enter();
        let settings = Settings {
            quick_save: true,
            ..Settings::default()
        };
        let session = headless_session(settings);
        let before = session.transfers_revision.get_untracked();

        session.apply_event(incoming());

        assert_eq!(
            session.transfers_revision.get_untracked() - before,
            1,
            "one request must bump the transfers revision once"
        );
        assert!(session.state.borrow().incoming.is_none());
    }

    /// M-10: a terminal state for the transfer whose PIN is being requested
    /// closes the PIN prompt (the actor can no longer answer it).
    #[test]
    fn terminal_event_clears_the_pin_request_for_its_transfer() {
        let runtime = test_runtime();
        let _entered = runtime.enter();
        let session = headless_session(Settings::default());
        session.state.borrow_mut().apply(started_event("t"));
        session.pin_request.set(Some(PinRequest {
            id: "t".to_string(),
            peer: peer(),
        }));

        session.apply_event(TransferEvent::Cancelled {
            id: "t".to_string(),
        });

        assert!(session.pin_request.get_untracked().is_none());
        assert_eq!(
            session.state.borrow().transfer("t").unwrap().status,
            TransferStatus::Cancelled
        );
    }

    /// The transient requests are per transfer: another id's terminal event
    /// must not close the prompt (same rule as the incoming request).
    #[test]
    fn unrelated_terminal_event_keeps_the_pin_request() {
        let runtime = test_runtime();
        let _entered = runtime.enter();
        let session = headless_session(Settings::default());
        session.state.borrow_mut().apply(started_event("t"));
        session.pin_request.set(Some(PinRequest {
            id: "t".to_string(),
            peer: peer(),
        }));
        session.apply_event(completed_event("other"));
        assert!(session.pin_request.get_untracked().is_some());
    }

    #[test]
    fn terminal_id_maps_terminal_events_only() {
        assert_eq!(terminal_id(&completed_event("a")), Some("a"));
        assert_eq!(
            terminal_id(&TransferEvent::Cancelled {
                id: "b".to_string()
            }),
            Some("b")
        );
        assert_eq!(
            terminal_id(&TransferEvent::Failed {
                id: "c".to_string(),
                error: String::new()
            }),
            Some("c")
        );
        assert_eq!(terminal_id(&started_event("d")), None);
        assert_eq!(terminal_id(&TransferEvent::NetError("x".into())), None);
    }

    /// M-01: `NetError` is surfaced as a red toast (and kept in
    /// `AppState::error` for a banner) instead of dying in an unread field.
    #[test]
    fn net_error_is_shown_as_an_error_toast() {
        let runtime = test_runtime();
        let _entered = runtime.enter();
        let session = headless_session(Settings::default());
        session.apply_event(TransferEvent::NetError("listener failed".to_string()));
        let toast = session.toast.get_untracked().unwrap();
        assert!(toast.error);
        assert_eq!(
            toast.text.to_string(),
            i18n::text(Language::En, "error.network_lost").to_string()
        );
        assert_eq!(
            session.last_error().as_deref(),
            Some("listener failed"),
            "the raw detail is kept for diagnostics"
        );
    }

    /// A declined incoming request names its reason; it must not read as a
    /// lost network (a full disk used to say "network lost").
    #[test]
    fn refused_requests_show_their_reason_not_a_network_error() {
        let runtime = test_runtime();
        let _entered = runtime.enter();
        let session = headless_session(Settings::default());
        session.apply_event(TransferEvent::RequestRefused(net::RefuseReason::NoSpace));
        let toast = session.toast.get_untracked().unwrap();
        assert!(toast.error);
        assert_eq!(
            toast.text.to_string(),
            i18n::text(Language::En, "error.request_no_space").to_string()
        );
        assert_eq!(session.last_error(), None, "not a network fault");
    }

    /// M-20: shutdown cancels what the network was running and flushes both
    /// stores; it is safe to call twice.
    #[test]
    fn shutdown_finalizes_work_and_flushes_the_stores() {
        let runtime = test_runtime();
        let _entered = runtime.enter();
        let session = headless_session(Settings::default());
        session.state.borrow_mut().apply(started_event("t"));
        session.pin_request.set(Some(PinRequest {
            id: "t".to_string(),
            peer: peer(),
        }));

        session.shutdown();
        session.shutdown();

        let state = session.state.borrow();
        assert_eq!(
            state.transfer("t").unwrap().status,
            TransferStatus::Cancelled
        );
        assert!(!state.pending_restart);
        drop(state);
        assert!(session.pin_request.get_untracked().is_none());
        let history = Storage::get_item(HISTORY_KEY).unwrap();
        assert_eq!(envelope_entries(&history).len(), 1);
        assert!(Storage::get_item(SETTINGS_KEY).is_some());
    }

    /// M-07: the quota retry shrinks the in-memory history to what actually
    /// fits on disk, so a restart does not silently drop entries.
    #[test]
    fn quota_retry_keeps_memory_and_disk_in_sync() {
        let runtime = test_runtime();
        let _entered = runtime.enter();
        let session = headless_session(Settings::default());
        {
            let mut state = session.state.borrow_mut();
            for index in 0..HISTORY_LIMIT {
                let mut transfer = history_transfer(&format!("t{index}"), false);
                // ~64 KiB of message body per record: 100 records exceed the
                // 5 MiB Storage quota, 50 fit.
                transfer.texts = vec!["x".repeat(64 * 1024)];
                transfer.started_at = (HISTORY_LIMIT - index) as u64;
                state.transfers.push(transfer);
            }
        }

        session.persist_history();

        let state = session.state.borrow();
        let finished = state
            .transfers
            .iter()
            .filter(|transfer| !transfer.is_active())
            .count();
        assert!(finished < HISTORY_LIMIT, "the history had to shrink");
        drop(state);
        let entries = envelope_entries(&Storage::get_item(HISTORY_KEY).unwrap());
        assert_eq!(
            entries.len(),
            finished,
            "memory and disk hold the same count"
        );
        assert_eq!(
            entries[0]["id"],
            serde_json::json!("t0"),
            "the newest entry is kept"
        );
    }

    /// M-06: settings with a path JSON cannot represent are repaired before
    /// serialization instead of failing every later save.
    #[cfg(unix)]
    #[test]
    fn settings_json_repairs_non_utf8_downloads() {
        use std::os::unix::ffi::OsStringExt;
        let settings = Settings {
            downloads: PathBuf::from(std::ffi::OsString::from_vec(vec![b'/', b'x', 0xff])),
            ..Settings::default()
        };
        let json = settings_json(&settings).unwrap();
        let parsed: Settings = serde_json::from_str(&json).unwrap();
        assert!(parsed.downloads.to_str().is_some());
        assert_eq!(parsed.downloads, default_downloads());
    }

    /// M-18: a failed settings write is retried once before giving up.
    #[test]
    fn settings_write_is_retried_once() {
        let attempts = std::cell::Cell::new(0);
        let result = write_settings(&Settings::default(), |_json| {
            attempts.set(attempts.get() + 1);
            if attempts.get() == 1 {
                Err("storage is full".to_string())
            } else {
                Ok(())
            }
        });
        assert!(result.is_ok());
        assert_eq!(attempts.get(), 2);

        let always = std::cell::Cell::new(0);
        let result = write_settings(&Settings::default(), |_json| {
            always.set(always.get() + 1);
            Err("storage is full".to_string())
        });
        assert!(result.is_err());
        assert_eq!(always.get(), 2, "one retry, then report");
    }

    /// M-08/M-09: a stored settings file is repaired on load, and a newer
    /// schema is read best-effort instead of resetting everything.
    #[test]
    fn settings_from_json_repairs_and_tolerates_newer_schemas() {
        let (settings, changed) = settings_from_json(
            r#"{"schema":99,"alias":"   ","port":0,"language":"zh","future":true}"#,
        )
        .unwrap();
        assert!(changed);
        assert_eq!(settings.schema, SETTINGS_SCHEMA);
        assert_eq!(settings.port, crate::net::DEFAULT_PORT);
        assert!(!settings.alias.trim().is_empty());
        assert_eq!(settings.language, Language::Zh);

        let (clean, changed) = settings_from_json(r#"{"language":"zh"}"#).unwrap();
        assert!(!changed, "a partial but valid file needs no repair");
        assert_eq!(clean.language, Language::Zh);
        assert_eq!(clean.schema, SETTINGS_SCHEMA);

        assert!(settings_from_json("{ not json").is_none());
    }

    #[test]
    fn ensure_writable_downloads_falls_back_to_the_default() {
        let root = tempfile::tempdir().unwrap();
        let file = root.path().join("not-a-directory");
        std::fs::write(&file, b"x").unwrap();
        let mut settings = Settings {
            downloads: file,
            ..Settings::default()
        };
        assert!(ensure_writable_downloads(&mut settings));
        assert_eq!(settings.downloads, default_downloads());

        let mut writable = Settings {
            downloads: root.path().to_path_buf(),
            ..Settings::default()
        };
        assert!(!ensure_writable_downloads(&mut writable));
        assert_eq!(writable.downloads, root.path());
    }

    /// M-18 round trip through the real Storage backend.
    #[test]
    fn settings_round_trip_through_storage() {
        let runtime = test_runtime();
        let _entered = runtime.enter();
        let settings = Settings {
            schema: SETTINGS_SCHEMA,
            alias: "Test Device".to_string(),
            port: 53400,
            language: Language::Zh,
            quick_save: true,
            ..Settings::default()
        };
        save_settings(&settings).unwrap();
        let loaded = load_settings();
        assert_eq!(loaded, settings);
    }

    #[test]
    fn corrupt_settings_keep_running_with_defaults_and_the_file() {
        let runtime = test_runtime();
        let _entered = runtime.enter();
        Storage::set_item(SETTINGS_KEY, "{ not json").unwrap();
        let loaded = load_settings();
        assert_eq!(loaded.port, crate::net::DEFAULT_PORT);
        assert_eq!(
            Storage::get_item(SETTINGS_KEY).as_deref(),
            Some("{ not json"),
            "the user's only copy is left in place"
        );
    }

    /// M-09/M-21: both history formats load, unknown schema is best-effort,
    /// malformed records are skipped and Active rows become Cancelled.
    #[test]
    fn history_loader_handles_both_schemas_and_recovers_records() {
        let finished = serde_json::to_string(&history_transfer("finished", false)).unwrap();
        let active = serde_json::to_string(&history_transfer("active", true)).unwrap();

        // Legacy (schema 1): a bare JSON array.
        let legacy = format!("[{finished},{active}]");
        let mut history = history_from_json(&legacy, 5_000);
        assert_eq!(history.len(), 2);
        assert_eq!(
            history.iter().find(|t| t.id == "active").unwrap().status,
            TransferStatus::Cancelled
        );
        assert_eq!(
            history
                .iter()
                .find(|t| t.id == "active")
                .unwrap()
                .finished_at,
            Some(5_000)
        );
        history.sort_by_key(|transfer| transfer.id.clone());
        assert_eq!(history[0].started_at, 1);

        // Versioned envelope.
        let envelope =
            format!(r#"{{"schema":{HISTORY_SCHEMA},"entries":[{finished},{{"malformed":true}}]}}"#);
        let history = history_from_json(&envelope, 6_000);
        assert_eq!(history.len(), 1, "the malformed record is skipped");
        assert_eq!(history[0].id, "finished");

        // A newer schema is read best-effort rather than discarded.
        let newer = format!(r#"{{"schema":99,"entries":[{finished}]}}"#);
        assert_eq!(history_from_json(&newer, 7_000).len(), 1);
        assert!(history_from_json("not json", 8_000).is_empty());
        assert!(history_from_json(r#"{"schema":2}"#, 8_000).is_empty());
    }

    #[test]
    fn history_round_trips_through_storage() {
        let runtime = test_runtime();
        let _entered = runtime.enter();
        let transfers = vec![history_transfer("a", false), history_transfer("b", false)];
        assert_eq!(save_history(&transfers), Some(2));
        let loaded = load_history();
        assert_eq!(loaded.len(), 2);
    }

    #[test]
    fn retry_delay_is_bounded() {
        assert_eq!(retry_delay(1), Some(Duration::from_secs(1)));
        assert_eq!(retry_delay(2), Some(Duration::from_secs(2)));
        assert_eq!(retry_delay(MAX_START_ATTEMPTS), None);
        assert_eq!(retry_delay(MAX_START_ATTEMPTS + 1), None);
    }

    /// L-01: demo/offline must not start or restart a node even when a caller
    /// asks. The env vars are process-wide, so this is the only test touching
    /// them (and it restores them).
    #[test]
    fn network_disabled_guards_start_and_restart() {
        struct Restore;
        impl Drop for Restore {
            fn drop(&mut self) {
                // SAFETY: this test is the only one mutating this variable,
                // and it restores the previous value before returning.
                unsafe { std::env::remove_var("LOCALSEND_OFFLINE") };
            }
        }
        let _restore = Restore;
        // SAFETY: see above.
        unsafe { std::env::set_var("LOCALSEND_OFFLINE", "1") };
        assert!(network_disabled());

        let runtime = test_runtime();
        let _entered = runtime.enter();
        let session = headless_session(Settings::default());
        session.start_network();
        assert!(!session.running.get(), "offline mode does not start a node");
        assert_eq!(session.network_generation.get(), 0);
        session.restart_network();
        assert_eq!(session.network_generation.get(), 0);
        session.retry_network();
        assert_eq!(session.network_generation.get(), 0);
        // `start()` also seeds the demo before checking offline; it must not
        // touch the network either.
        session.start();
        assert!(!session.running.get());
    }
}
