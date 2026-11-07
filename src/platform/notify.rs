//! System notifications for incoming requests and received transfers.
//!
//! LocalSend usually lives in the menu bar with its window closed, so a
//! request that only shows inside the window is easy to miss. This module
//! posts a notification when:
//!
//! - a request arrives (one notification at a time, replaced by the next
//!   request and withdrawn once the request is accepted, declined or gone).
//!   A message shows its text right away; files show their name or count;
//! - a receive completes (also with quick-save, which never shows a
//!   request): the message text, or what was saved. A message whose request
//!   was already notified is not notified twice.
//!
//! The sender's name is the title, as in Messages. Clicking a notification
//! opens the window (on History for a completed receive). While the window is
//! visible and LocalSend is the active app, nothing is presented: the request
//! sheet and the toasts already show it.
//!
//! What to post is decided by the pure [`Planner`] (covered by the tests),
//! from the session's state on the engine thread. nsh 0.4 has no notification
//! API, so on macOS the result goes to `UNUserNotificationCenter` directly
//! (on the main thread, like [`super::status_item`]). That needs a real app
//! bundle: `cargo run` / `nsh run` binaries post nothing. Other platforms:
//! [`install`] does nothing yet.
//!
//! macOS asks for permission once. Someone who declines (or turns LocalSend
//! off later) is never asked again, so [`install`] keeps
//! [`Session::notification_access`] current — at launch, when the settings
//! sheet opens and whenever LocalSend becomes the active app (coming back
//! from System Settings) — and the settings sheet offers
//! [`open_system_settings`].

use crate::i18n::{self, Language, fill};
use crate::model::{AppState, IncomingRequest, Session, Transfer, TransferStatus, View};
use crate::net::Direction;
use std::collections::HashSet;

/// Routes permission results to the current session, including after a root
/// remount. Replacing the sender disconnects the previous session's task.
#[cfg(any(target_os = "macos", test))]
#[derive(Default)]
struct AccessRelay(
    std::sync::Mutex<Option<async_channel::Sender<crate::model::NotificationAccess>>>,
);

#[cfg(any(target_os = "macos", test))]
impl AccessRelay {
    const fn new() -> Self {
        Self(std::sync::Mutex::new(None))
    }

    fn subscribe(&self) -> async_channel::Receiver<crate::model::NotificationAccess> {
        let (sender, receiver) = async_channel::unbounded();
        *self
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(sender);
        receiver
    }

    fn publish(&self, access: crate::model::NotificationAccess) {
        if let Some(sender) = self
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .as_ref()
        {
            let _ = sender.try_send(access);
        }
    }
}

/// The identifier of the request notification (one at a time).
const REQUEST_ID: &str = "localsend.request";

/// Prefix of a completed receive's notification identifier.
const RECEIVED_PREFIX: &str = "localsend.received.";

/// Longest message excerpt in a notification body, in characters (the
/// notification itself shows a few lines).
const MESSAGE_CHARS: usize = 240;

/// Whether this build can show system notifications.
pub const fn available() -> bool {
    cfg!(target_os = "macos")
}

/// Opens LocalSend's page in the system's notification settings (macOS);
/// does nothing elsewhere.
pub fn open_system_settings() {
    #[cfg(target_os = "macos")]
    macos::open_system_settings();
}

/// A notification to post.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Note {
    /// Stable per event: posting the same id again replaces the notification.
    pub id: String,
    pub title: String,
    pub body: String,
}

/// What changes in the notification center.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Change {
    Show(Note),
    /// Withdraws a delivered notification that no longer applies.
    Remove(String),
}

/// Decides which notifications the state calls for, remembering what it
/// already posted.
#[derive(Debug, Default)]
pub struct Planner {
    /// Whether the first state was seen (its finished transfers are history,
    /// not news).
    started: bool,
    /// The request the posted request notification is about.
    request: Option<String>,
    /// Message requests already notified with their text.
    messages: HashSet<String>,
    /// The latest message may have been accepted before `Started` arrives.
    /// Keep at most this one untracked request across that gap; a declined
    /// request is evicted by the next request instead of accumulating.
    awaiting_start: Option<String>,
    /// Completed receives already seen.
    received: HashSet<String>,
}

impl Planner {
    /// The changes since the last call.
    pub fn plan(&mut self, state: &AppState, language: Language) -> Vec<Change> {
        let mut changes = Vec::new();

        let request = state.incoming.as_ref();
        let request_id = request.map(|request| request.id.clone());
        if request_id != self.request {
            if self.request.is_some() {
                changes.push(Change::Remove(REQUEST_ID.to_owned()));
            }
            if let Some(request) = request {
                self.awaiting_start = request.message.as_ref().map(|_| request.id.clone());
                if request.message.is_some() {
                    self.messages.insert(request.id.clone());
                }
                changes.push(Change::Show(request_note(request, language)));
            }
            self.request = request_id;
        }

        let completed = state
            .transfers
            .iter()
            .filter(|transfer| received(transfer))
            .collect::<Vec<_>>();
        if self.started {
            for transfer in &completed {
                if self.received.contains(&transfer.id) {
                    continue;
                }
                let message_only = !transfer.texts.is_empty() && transfer.saved_paths.is_empty();
                if self.messages.remove(&transfer.id) && message_only {
                    // Its text was in the request notification already.
                    continue;
                }
                if let Some(note) = received_note(transfer, language) {
                    changes.push(Change::Show(note));
                }
            }
        }
        self.received = completed
            .iter()
            .map(|transfer| transfer.id.clone())
            .collect();
        self.messages.retain(|id| {
            if let Some(transfer) = state.transfers.iter().find(|transfer| &transfer.id == id) {
                transfer.direction == Direction::Receiving && transfer.is_active()
            } else {
                self.awaiting_start.as_ref() == Some(id)
            }
        });
        if self
            .awaiting_start
            .as_ref()
            .is_some_and(|id| state.transfers.iter().any(|transfer| &transfer.id == id))
        {
            self.awaiting_start = None;
        }
        self.started = true;
        changes
    }
}

/// A receive that finished successfully.
fn received(transfer: &Transfer) -> bool {
    transfer.direction == Direction::Receiving && transfer.status == TransferStatus::Completed
}

fn request_note(request: &IncomingRequest, language: Language) -> Note {
    let text = |key: &str| i18n::text(language, key);
    let body = match (&request.message, request.files.as_slice()) {
        (Some(message), _) => fill(
            text("notify.request_message"),
            &[("text", excerpt(message))],
        ),
        (None, [file]) => fill(text("notify.request_file"), &[("file", file.name.clone())]),
        (None, files) => fill(
            text("notify.request_files"),
            &[("count", files.len().to_string())],
        ),
    };
    Note {
        id: REQUEST_ID.to_owned(),
        title: super::status_item::short_name(&request.peer.alias),
        body,
    }
}

fn received_note(transfer: &Transfer, language: Language) -> Option<Note> {
    let text = |key: &str| i18n::text(language, key);
    let saved = transfer.saved_paths.len();
    let body = if saved == 0 {
        // A message: show it.
        let message = transfer.texts.join("\n");
        if message.trim().is_empty() {
            return None;
        }
        excerpt(&message)
    } else if saved == 1 {
        let name = transfer.saved_paths[0]
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default();
        fill(text("notify.received_file"), &[("file", name)])
    } else {
        fill(
            text("notify.received_files"),
            &[("count", saved.to_string())],
        )
    };
    Some(Note {
        id: format!("{RECEIVED_PREFIX}{}", transfer.id),
        title: super::status_item::short_name(&transfer.peer.alias),
        body,
    })
}

/// `text` cut to [`MESSAGE_CHARS`] characters, with an ellipsis when cut.
fn excerpt(text: &str) -> String {
    let text = text.trim();
    if text.chars().count() <= MESSAGE_CHARS {
        return text.to_owned();
    }
    let mut cut: String = text.chars().take(MESSAGE_CHARS - 1).collect();
    cut.push('…');
    cut
}

/// The screen a notification opens.
fn destination(id: &str) -> Option<View> {
    id.starts_with(RECEIVED_PREFIX).then_some(View::History)
}

/// Asks for permission and keeps posting what the session's state calls for.
///
/// Must run inside a component body (the root component calls it through
/// [`crate::platform::install`]): it creates an effect.
pub fn install(session: Session) {
    #[cfg(target_os = "macos")]
    macos::install(session);
    #[cfg(not(target_os = "macos"))]
    let _ = session;
}

#[cfg(target_os = "macos")]
mod macos {
    //! `UNUserNotificationCenter`, through the Objective-C runtime (the app
    //! has no typed binding for the UserNotifications framework).

    use super::{AccessRelay, Change, Planner, destination};
    use crate::model::{NotificationAccess, Session};
    use crate::platform::status_item::{self, TrayAction};
    use block2::{Block, RcBlock};
    use dispatch2::DispatchQueue;
    use nsh_core::prelude::*;
    use objc2::rc::Retained;
    use objc2::runtime::{AnyClass, AnyObject, Bool, NSObjectProtocol};
    use objc2::{AnyThread, MainThreadMarker, define_class, msg_send};
    use objc2_app_kit::NSApplication;
    use objc2_foundation::{NSArray, NSObject, NSString};
    use std::cell::RefCell;
    use std::sync::atomic::{AtomicBool, Ordering};

    // Loads the framework, so its classes exist at run time.
    #[link(name = "UserNotifications", kind = "framework")]
    unsafe extern "C" {}

    /// `UNAuthorizationOptionSound | UNAuthorizationOptionAlert`.
    const AUTHORIZATION: usize = (1 << 1) | (1 << 2);

    /// `UNNotificationPresentationOptionSound | …List | …Banner`.
    const PRESENT: usize = (1 << 1) | (1 << 3) | (1 << 4);

    /// The notification center can be used (set on the main thread once the
    /// app bundle was checked).
    static ENABLED: AtomicBool = AtomicBool::new(false);

    /// Where the permission checks go (any thread); a session task on the
    /// engine thread puts them into [`Session::notification_access`].
    static ACCESS: AccessRelay = AccessRelay::new();

    /// LocalSend's bundle identifier when the main bundle has none (the
    /// packaged app's, from `nsh.json`).
    const BUNDLE_ID: &str = "org.localsend.desktop";

    define_class!(
        /// Decides whether a notification shows while LocalSend is the active
        /// app, and opens the window when one is clicked. AppKit may call it
        /// off the main thread.
        #[unsafe(super(NSObject))]
        #[name = "LocalSendNotificationDelegate"]
        struct Delegate;

        unsafe impl NSObjectProtocol for Delegate {}

        impl Delegate {
            #[unsafe(method(userNotificationCenter:willPresentNotification:withCompletionHandler:))]
            fn will_present(
                &self,
                _center: &AnyObject,
                _notification: &AnyObject,
                handler: &Block<dyn Fn(usize)>,
            ) {
                let visible = on_main(window_visible);
                handler.call((if visible { 0 } else { PRESENT },));
            }

            #[unsafe(method(userNotificationCenter:didReceiveNotificationResponse:withCompletionHandler:))]
            fn did_receive(
                &self,
                _center: &AnyObject,
                response: &AnyObject,
                handler: &Block<dyn Fn()>,
            ) {
                let id = identifier(response);
                let view = destination(&id);
                on_main(move || status_item::request(TrayAction::Open(view)));
                handler.call(());
            }
        }
    );

    /// Engine thread: sets up the center on the main thread and keeps
    /// posting the planner's changes to it.
    pub fn install(session: Session) {
        if !is_desktop() || std::env::var_os("LOCALSEND_OFFLINE").is_some() {
            return;
        }
        let receiver = ACCESS.subscribe();
        let access = session.notification_access;
        session.task_detached(async move {
            while let Ok(value) = receiver.recv().await {
                if access.get_untracked() != value {
                    access.set(value);
                }
            }
        });
        DispatchQueue::main().exec_async(|| {
            if MainThreadMarker::new().is_some() {
                set_up();
            }
        });
        // Opening the settings sheet checks again: the row there must not
        // show a stale state.
        let settings_open = session.settings_open;
        effect(move || {
            if settings_open.get() {
                DispatchQueue::main().exec_async(refresh);
            }
        });
        let planner = RefCell::new(Planner::default());
        effect(move || {
            let language = session.language.get();
            let changes = session.read(|state| planner.borrow_mut().plan(state, language));
            if changes.is_empty() {
                return;
            }
            DispatchQueue::main().exec_async(move || apply(changes));
        });
    }

    /// Main thread: checks that this process is an app bundle (the center
    /// throws otherwise), installs the delegate and asks for permission.
    fn set_up() {
        // The center and observer live for the process; a new session only
        // needs a fresh permission result, not another delegate or observer.
        if ENABLED.load(Ordering::Acquire) {
            refresh();
            return;
        }
        if !in_app_bundle() {
            tracing::info!("Not running from an app bundle: no system notifications");
            return;
        }
        let Some(center) = center() else {
            tracing::warn!("UNUserNotificationCenter is unavailable");
            return;
        };
        let delegate: Retained<Delegate> = unsafe { msg_send![Delegate::alloc(), init] };
        let _: () = unsafe { msg_send![&center, setDelegate: &*delegate] };
        // The center holds its delegate weakly; this one lives as long as the
        // process.
        std::mem::forget(delegate);
        // Publish readiness before requesting authorization: its completion
        // may run immediately on another thread when already decided.
        ENABLED.store(true, Ordering::Release);
        observe_activation();
        let done = RcBlock::new(|granted: Bool, error: *mut AnyObject| {
            // SAFETY: the handler's error is an `NSError` or nil.
            let reason = unsafe { error.as_ref() }.map(describe).unwrap_or_default();
            tracing::info!(granted = granted.as_bool(), %reason, "Notification permission");
            refresh();
        });
        let _: () = unsafe {
            msg_send![
                &center,
                requestAuthorizationWithOptions: AUTHORIZATION,
                completionHandler: &*done
            ]
        };
        refresh();
    }

    /// Main thread: checks the permission again whenever LocalSend becomes
    /// the active app — the way back from System Settings.
    fn observe_activation() {
        let Some(class) = AnyClass::get(c"NSNotificationCenter") else {
            return;
        };
        let name = NSString::from_str("NSApplicationDidBecomeActiveNotification");
        let block = RcBlock::new(|_notification: *mut AnyObject| refresh());
        unsafe {
            let center: Retained<AnyObject> = msg_send![class, defaultCenter];
            let token: Option<Retained<AnyObject>> = msg_send![
                &center,
                addObserverForName: &*name,
                object: None::<&AnyObject>,
                queue: None::<&AnyObject>,
                usingBlock: &*block
            ];
            // Observes for the life of the process.
            std::mem::forget(token);
        }
    }

    /// Any thread: reads the current permission and reports it to the
    /// session ([`ACCESS`]).
    fn refresh() {
        if !ENABLED.load(Ordering::Acquire) {
            return;
        }
        let Some(center) = center() else { return };
        let done = RcBlock::new(|settings: *mut AnyObject| {
            // SAFETY: the handler's argument is a `UNNotificationSettings`.
            let Some(settings) = (unsafe { settings.as_ref() }) else {
                return;
            };
            let status: isize = unsafe { msg_send![settings, authorizationStatus] };
            let access = access_for(status);
            ACCESS.publish(access);
        });
        let _: () =
            unsafe { msg_send![&center, getNotificationSettingsWithCompletionHandler: &*done] };
    }

    /// `UNAuthorizationStatus`: 0 not determined, 1 denied, 2 authorized,
    /// 3 provisional, 4 ephemeral.
    pub(super) fn access_for(status: isize) -> NotificationAccess {
        match status {
            1 => NotificationAccess::Denied,
            2..=4 => NotificationAccess::Allowed,
            _ => NotificationAccess::Unknown,
        }
    }

    /// Opens `x-apple.systempreferences:…Notifications-Settings…?id=<bundle>`.
    pub fn open_system_settings() {
        let id = bundle_id().unwrap_or_else(|| BUNDLE_ID.to_owned());
        let url = system_settings_url(&id);
        // Wait and reap off the engine thread. A successful spawn alone
        // does not mean Launch Services accepted the URL.
        if let Err(error) = std::thread::Builder::new()
            .name("notification-settings".to_owned())
            .spawn(move || {
                let result = run_opener(std::process::Command::new("/usr/bin/open").arg(url));
                if let Err(error) = result {
                    tracing::warn!(%error, "Cannot open the notification settings");
                }
            })
        {
            tracing::warn!(%error, "Cannot open the notification settings");
        }
    }

    pub(super) fn system_settings_url(id: &str) -> String {
        format!("x-apple.systempreferences:com.apple.Notifications-Settings.extension?id={id}")
    }

    pub(super) fn run_opener(command: &mut std::process::Command) -> anyhow::Result<()> {
        let output = command.output()?;
        anyhow::ensure!(
            output.status.success(),
            "System Settings opener exited with {}: {}",
            output.status,
            String::from_utf8_lossy(&output.stderr).trim()
        );
        Ok(())
    }

    /// The main bundle's identifier.
    fn bundle_id() -> Option<String> {
        let class = AnyClass::get(c"NSBundle")?;
        unsafe {
            let bundle: Option<Retained<AnyObject>> = msg_send![class, mainBundle];
            let id: Option<Retained<NSString>> = msg_send![&bundle?, bundleIdentifier];
            id.map(|id| id.to_string())
        }
    }

    /// Main thread: posts and withdraws notifications.
    fn apply(changes: Vec<Change>) {
        if !ENABLED.load(Ordering::Relaxed) {
            return;
        }
        let Some(center) = center() else { return };
        for change in changes {
            match change {
                Change::Show(note) => post(&center, &note.id, &note.title, &note.body),
                Change::Remove(id) => {
                    let ids = NSArray::from_retained_slice(&[NSString::from_str(&id)]);
                    let _: () = unsafe {
                        msg_send![&center, removeDeliveredNotificationsWithIdentifiers: &*ids]
                    };
                    let _: () = unsafe {
                        msg_send![&center, removePendingNotificationRequestsWithIdentifiers: &*ids]
                    };
                }
            }
        }
    }

    fn post(center: &AnyObject, id: &str, title: &str, body: &str) {
        let (Some(content_class), Some(sound_class), Some(request_class)) = (
            AnyClass::get(c"UNMutableNotificationContent"),
            AnyClass::get(c"UNNotificationSound"),
            AnyClass::get(c"UNNotificationRequest"),
        ) else {
            return;
        };
        unsafe {
            let content: Retained<AnyObject> = msg_send![content_class, new];
            let _: () = msg_send![&content, setTitle: &*NSString::from_str(title)];
            let _: () = msg_send![&content, setBody: &*NSString::from_str(body)];
            let sound: Option<Retained<AnyObject>> = msg_send![sound_class, defaultSound];
            let _: () = msg_send![&content, setSound: sound.as_deref()];
            let request: Retained<AnyObject> = msg_send![
                request_class,
                requestWithIdentifier: &*NSString::from_str(id),
                content: &*content,
                trigger: None::<&AnyObject>
            ];
            let done = RcBlock::new(|error: *mut AnyObject| {
                // The handler's error is an `NSError` or nil (inside the
                // enclosing `unsafe` block).
                if let Some(error) = error.as_ref() {
                    tracing::warn!(reason = %describe(error), "A notification was not delivered");
                }
            });
            let _: () = msg_send![
                center,
                addNotificationRequest: &*request,
                withCompletionHandler: &*done
            ];
        }
    }

    fn center() -> Option<Retained<AnyObject>> {
        let class = AnyClass::get(c"UNUserNotificationCenter")?;
        unsafe { msg_send![class, currentNotificationCenter] }
    }

    /// `[[NSBundle mainBundle] bundleIdentifier]` exists and the bundle is a
    /// `.app` (the notification center refuses anything else, by throwing).
    fn in_app_bundle() -> bool {
        let Some(bundle_class) = AnyClass::get(c"NSBundle") else {
            return false;
        };
        unsafe {
            let bundle: Option<Retained<AnyObject>> = msg_send![bundle_class, mainBundle];
            let Some(bundle) = bundle else { return false };
            let id: Option<Retained<NSString>> = msg_send![&bundle, bundleIdentifier];
            let path: Option<Retained<NSString>> = msg_send![&bundle, bundlePath];
            id.is_some() && path.is_some_and(|path| path.to_string().ends_with(".app"))
        }
    }

    /// An `NSError`'s `localizedDescription`.
    fn describe(error: &AnyObject) -> String {
        let text: Option<Retained<NSString>> = unsafe { msg_send![error, localizedDescription] };
        text.map(|text| text.to_string()).unwrap_or_default()
    }

    /// The notification's request identifier from a response.
    fn identifier(response: &AnyObject) -> String {
        unsafe {
            let notification: Option<Retained<AnyObject>> = msg_send![response, notification];
            let Some(notification) = notification else {
                return String::new();
            };
            let request: Option<Retained<AnyObject>> = msg_send![&notification, request];
            let Some(request) = request else {
                return String::new();
            };
            let id: Option<Retained<NSString>> = msg_send![&request, identifier];
            id.map(|id| id.to_string()).unwrap_or_default()
        }
    }

    /// Main thread: a window of the app is on screen (not hidden, not
    /// minimized).
    fn window_visible() -> bool {
        let Some(mtm) = MainThreadMarker::new() else {
            return false;
        };
        NSApplication::sharedApplication(mtm)
            .windows()
            .iter()
            .any(|window| {
                window.canBecomeMainWindow() && window.isVisible() && !window.isMiniaturized()
            })
    }

    /// Runs `f` on the main thread and returns its result (directly when
    /// already there).
    fn on_main<R: Send>(f: impl FnOnce() -> R + Send) -> R {
        if MainThreadMarker::new().is_some() {
            return f();
        }
        let mut result = None;
        let slot = &mut result;
        DispatchQueue::main().exec_sync(move || *slot = Some(f()));
        result.expect("exec_sync runs the block before returning")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::net::{DeviceType, OfferedFile, Peer};
    use std::path::PathBuf;

    fn peer(alias: &str) -> Peer {
        Peer {
            fingerprint: alias.to_uppercase(),
            alias: alias.to_string(),
            device_model: String::new(),
            address: None,
            device_type: DeviceType::Mobile,
            is_self: false,
        }
    }

    fn file(name: &str) -> OfferedFile {
        OfferedFile {
            name: name.to_string(),
            size: 1,
            mime: "application/octet-stream".to_string(),
            saved_to: None,
        }
    }

    fn request(id: &str, files: Vec<OfferedFile>, message: Option<&str>) -> IncomingRequest {
        IncomingRequest {
            id: id.to_string(),
            peer: peer("炽热的木瓜"),
            files,
            total: 1,
            message: message.map(str::to_string),
        }
    }

    fn received(id: &str, saved: &[&str], texts: &[&str]) -> Transfer {
        Transfer {
            id: id.to_string(),
            peer: peer("炽热的木瓜"),
            direction: Direction::Receiving,
            files: saved.iter().map(|name| file(name)).collect(),
            total: 1,
            transferred: 1,
            status: TransferStatus::Completed,
            error: None,
            saved_paths: saved
                .iter()
                .map(|name| PathBuf::from("/Users/me/Downloads").join(name))
                .collect(),
            texts: texts.iter().map(|text| text.to_string()).collect(),
            started_at: 0,
            finished_at: Some(1),
            omitted_files: 0,
        }
    }

    fn shown(changes: &[Change]) -> Vec<(String, String, String)> {
        changes
            .iter()
            .filter_map(|change| match change {
                Change::Show(note) => {
                    Some((note.id.clone(), note.title.clone(), note.body.clone()))
                }
                Change::Remove(_) => None,
            })
            .collect()
    }

    #[test]
    fn history_at_launch_is_not_announced() {
        let mut state = AppState::default();
        state.transfers.push(received("old", &["a.jpg"], &[]));
        let mut planner = Planner::default();
        assert!(planner.plan(&state, Language::Zh).is_empty());
        assert!(planner.plan(&state, Language::Zh).is_empty());
    }

    #[test]
    fn a_request_is_shown_once_and_withdrawn_when_answered() {
        let mut state = AppState::default();
        let mut planner = Planner::default();
        planner.plan(&state, Language::Zh);

        state.incoming = Some(request("r1", vec![file("a.jpg"), file("b.jpg")], None));
        assert_eq!(
            shown(&planner.plan(&state, Language::Zh)),
            [(
                REQUEST_ID.to_string(),
                "炽热的木瓜".to_string(),
                "想给你发送 2 个文件".to_string()
            )]
        );
        // Progress ticks and other re-runs post nothing new.
        assert!(planner.plan(&state, Language::Zh).is_empty());

        state.incoming = None;
        assert_eq!(
            planner.plan(&state, Language::Zh),
            [Change::Remove(REQUEST_ID.to_string())]
        );
    }

    #[test]
    fn a_single_file_and_a_message_are_named() {
        let mut planner = Planner::default();
        let mut state = AppState {
            incoming: Some(request("r1", vec![file("报告.pdf")], None)),
            ..AppState::default()
        };
        assert_eq!(
            shown(&planner.plan(&state, Language::Zh))[0].2,
            "想给你发送「报告.pdf」"
        );
        state.incoming = Some(request("r2", vec![file("Message.txt")], Some("晚上吃什么")));
        let changes = planner.plan(&state, Language::En);
        assert_eq!(changes[0], Change::Remove(REQUEST_ID.to_string()));
        assert_eq!(shown(&changes)[0].2, "Sent you a message: 晚上吃什么");
    }

    #[test]
    fn a_completed_receive_is_announced_once() {
        let mut planner = Planner::default();
        let mut state = AppState::default();
        planner.plan(&state, Language::Zh);

        state
            .transfers
            .push(received("t1", &["a.jpg", "b.jpg", "c.jpg"], &[]));
        assert_eq!(
            shown(&planner.plan(&state, Language::Zh)),
            [(
                "localsend.received.t1".to_string(),
                "炽热的木瓜".to_string(),
                "发来了 3 个文件".to_string()
            )]
        );
        assert!(planner.plan(&state, Language::Zh).is_empty());

        state.transfers.push(received("t2", &["照片.heic"], &[]));
        assert_eq!(
            shown(&planner.plan(&state, Language::Zh))[0].2,
            "发来了「照片.heic」"
        );
    }

    #[test]
    fn a_quick_saved_message_shows_its_text_but_a_notified_one_does_not_repeat() {
        let mut planner = Planner::default();
        let mut state = AppState::default();
        planner.plan(&state, Language::Zh);

        // Quick-save: no request, the text arrives with the completed receive.
        state
            .transfers
            .push(received("quick", &[], &["  hello\nworld  "]));
        assert_eq!(
            shown(&planner.plan(&state, Language::Zh))[0].2,
            "hello\nworld"
        );

        // A message request already showed the text.
        state.incoming = Some(request("asked", vec![file("Message.txt")], Some("hi")));
        planner.plan(&state, Language::Zh);
        state.incoming = None;
        state.transfers.push(received("asked", &[], &["hi"]));
        assert_eq!(
            planner.plan(&state, Language::Zh),
            [Change::Remove(REQUEST_ID.to_string())]
        );
    }

    #[test]
    fn declined_message_requests_do_not_accumulate() {
        let mut planner = Planner::default();
        let mut state = AppState::default();
        planner.plan(&state, Language::En);
        for number in 0..100 {
            let id = format!("declined-{number}");
            state.incoming = Some(request(&id, vec![file("Message.txt")], Some("hi")));
            planner.plan(&state, Language::En);
            state.incoming = None;
            planner.plan(&state, Language::En);
            // No Started event distinguishes a decline from an acceptance
            // still in the actor's queue. At most one such ID is retained.
            assert_eq!(planner.messages.len(), 1);
        }
        state.incoming = Some(request("files", vec![file("a.jpg")], None));
        planner.plan(&state, Language::En);
        assert!(planner.messages.is_empty());
    }

    #[test]
    fn accepted_messages_stay_deduplicated_across_the_start_event_gap() {
        let mut planner = Planner::default();
        let mut state = AppState::default();
        planner.plan(&state, Language::En);
        state.incoming = Some(request("accepted", vec![file("Message.txt")], Some("hi")));
        planner.plan(&state, Language::En);
        // decide_inner clears incoming before the actor emits Started.
        state.incoming = None;
        planner.plan(&state, Language::En);
        let mut transfer = received("accepted", &[], &[]);
        transfer.status = TransferStatus::Active;
        state.transfers.push(transfer);
        planner.plan(&state, Language::En);
        assert!(planner.awaiting_start.is_none());
        state.transfers[0].status = TransferStatus::Completed;
        state.transfers[0].texts.push("hi".to_owned());
        assert!(shown(&planner.plan(&state, Language::En)).is_empty());
        assert!(planner.messages.is_empty());
    }

    #[test]
    fn unsuccessful_message_transfers_release_the_deduplication_record() {
        for status in [TransferStatus::Failed, TransferStatus::Cancelled] {
            let mut planner = Planner::default();
            let mut state = AppState {
                incoming: Some(request("stopped", vec![file("Message.txt")], Some("hi"))),
                ..AppState::default()
            };
            planner.plan(&state, Language::En);
            state.incoming = None;
            let mut transfer = received("stopped", &[], &[]);
            transfer.status = status;
            state.transfers.push(transfer);
            planner.plan(&state, Language::En);
            assert!(planner.messages.is_empty());
            assert!(planner.awaiting_start.is_none());
        }
    }

    #[test]
    fn failed_sends_and_receives_are_not_announced() {
        let mut planner = Planner::default();
        let mut state = AppState::default();
        planner.plan(&state, Language::Zh);
        let mut sent = received("s", &["a.jpg"], &[]);
        sent.direction = Direction::Sending;
        let mut failed = received("f", &["a.jpg"], &[]);
        failed.status = TransferStatus::Failed;
        state.transfers.extend([sent, failed]);
        assert!(planner.plan(&state, Language::Zh).is_empty());
    }

    #[test]
    fn long_messages_are_cut() {
        let long = "字".repeat(MESSAGE_CHARS + 10);
        let cut = excerpt(&long);
        assert_eq!(cut.chars().count(), MESSAGE_CHARS);
        assert!(cut.ends_with('…'));
        assert_eq!(excerpt(" short "), "short");
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn the_system_permission_maps_to_access() {
        use crate::model::NotificationAccess;
        assert_eq!(macos::access_for(0), NotificationAccess::Unknown);
        assert_eq!(macos::access_for(1), NotificationAccess::Denied);
        for allowed in 2..=4 {
            assert_eq!(macos::access_for(allowed), NotificationAccess::Allowed);
        }
        assert_eq!(macos::access_for(9), NotificationAccess::Unknown);
    }

    #[test]
    fn permission_results_follow_the_current_session() {
        use crate::model::NotificationAccess::{Allowed, Denied};
        let relay = AccessRelay::new();
        let first = relay.subscribe();
        relay.publish(Denied);
        assert_eq!(first.try_recv().unwrap(), Denied);
        let second = relay.subscribe();
        assert!(first.is_closed());
        relay.publish(Allowed);
        assert_eq!(second.try_recv().unwrap(), Allowed);
        assert!(first.try_recv().is_err());
        drop(second);
        // Late platform callbacks after teardown must be harmless, and a
        // subsequent mount must still be able to subscribe.
        relay.publish(Denied);
        let third = relay.subscribe();
        relay.publish(Denied);
        assert_eq!(third.try_recv().unwrap(), Denied);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn settings_opener_targets_the_app_and_reports_failed_exit() {
        use std::process::Command;
        assert_eq!(
            macos::system_settings_url("org.localsend.desktop"),
            "x-apple.systempreferences:com.apple.Notifications-Settings.extension?id=org.localsend.desktop"
        );
        macos::run_opener(&mut Command::new("/usr/bin/true")).unwrap();
        assert!(macos::run_opener(&mut Command::new("/usr/bin/false")).is_err());
        assert!(macos::run_opener(&mut Command::new("/missing/notification-opener")).is_err());
    }

    #[test]
    fn a_received_notification_opens_history() {
        assert_eq!(destination("localsend.received.t1"), Some(View::History));
        assert_eq!(destination(REQUEST_ID), None);
    }
}
