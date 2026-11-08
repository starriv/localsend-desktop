//! Windows notification-area icon. A dedicated native thread owns its hidden
//! window, icon and popup menu. Callbacks send actions to the engine without
//! waiting for it; the main nsh window can be hidden without stopping its
//! surface or the network. The icon lives for the process, across root remounts.

use super::status_item::TrayAction;
use crate::i18n::{self, Language};
use crate::model::Session;
use nsh_core::prelude::*;
use nsh_platform_windows::windows::{
    Win32::{
        Foundation::{HINSTANCE, HWND, LPARAM, LRESULT, POINT, WPARAM},
        System::LibraryLoader::GetModuleHandleW,
        UI::{
            Shell::{
                NIF_ICON, NIF_MESSAGE, NIF_SHOWTIP, NIF_TIP, NIM_ADD, NIM_DELETE, NIM_SETVERSION,
                NIN_SELECT, NINF_KEY, NOTIFYICON_VERSION_4, NOTIFYICONDATAW, NOTIFYICONDATAW_0,
                Shell_NotifyIconW,
            },
            WindowsAndMessaging::{
                AppendMenuW, CreatePopupMenu, CreateWindowExW, DefWindowProcW, DestroyMenu,
                DestroyWindow, DispatchMessageW, GetCursorPos, GetMessageW, IDI_APPLICATION,
                LoadIconW, MF_STRING, MSG, PostMessageW, PostQuitMessage, RegisterClassW,
                RegisterWindowMessageW, SetForegroundWindow, TPM_RETURNCMD, TPM_RIGHTBUTTON,
                TrackPopupMenu, TranslateMessage, WINDOW_EX_STYLE, WINDOW_STYLE, WM_APP, WM_CLOSE,
                WM_CONTEXTMENU, WM_DESTROY, WM_NULL, WNDCLASSW,
            },
        },
    },
    core::{PCWSTR, Result, w},
};
use std::sync::{
    Mutex, OnceLock,
    atomic::{AtomicBool, AtomicU32, AtomicUsize, Ordering},
};

const CALLBACK: u32 = WM_APP + 0x4c53;
const HIDE_WINDOW: u32 = CALLBACK + 1;
// shellapi.h defines this expression as a macro, absent from windows-rs.
const NIN_KEYSELECT: u32 = NIN_SELECT | NINF_KEY;
const OPEN: usize = 1;
const QUIT: usize = 2;
static STARTED: OnceLock<()> = OnceLock::new();
static READY: AtomicBool = AtomicBool::new(false);
static WINDOW: AtomicUsize = AtomicUsize::new(0);
static TASKBAR_CREATED: AtomicU32 = AtomicU32::new(0);
static ACTIONS: Mutex<Option<async_channel::Sender<TrayAction>>> = Mutex::new(None);
static LANGUAGE: Mutex<Language> = Mutex::new(Language::En);

pub(super) fn available() -> bool {
    READY.load(Ordering::Acquire)
}

/// Queue the close without capturing tray readiness on the engine thread.
/// The tray thread checks readiness when it handles this message, and emits
/// hide/show requests in the same order as its Explorer recovery handling.
pub(super) fn hide_window(main: HWND) -> bool {
    let tray = HWND(WINDOW.load(Ordering::Acquire) as *mut _);
    if tray.is_invalid() || main.is_invalid() {
        return false;
    }
    // SAFETY: no pointers to borrowed data cross threads; WPARAM carries
    // only our process-lifetime main window handle.
    unsafe { PostMessageW(Some(tray), HIDE_WINDOW, WPARAM(main.0 as usize), LPARAM(0)).is_ok() }
}

pub(super) fn request(action: TrayAction) {
    if let Some(sender) = ACTIONS
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .as_ref()
    {
        let _ = sender.try_send(action);
    }
}

pub(super) fn install(session: Session) {
    if !is_desktop() || std::env::var_os("LOCALSEND_OFFLINE").is_some() {
        return;
    }
    let (sender, receiver) = async_channel::unbounded();
    // Reconnect the process-lifetime icon to the current root's session.
    *ACTIONS
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(sender);
    let language = session.language;
    effect(move || {
        *LANGUAGE
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = language.get();
    });
    session.clone().task_detached(async move {
        while let Ok(action) = receiver.recv().await {
            super::status_item::perform(&session, action);
        }
    });
    STARTED.get_or_init(|| {
        if let Err(error) = std::thread::Builder::new().name("windows-tray".into()).spawn(|| {
            if let Err(error) = run() {
                tracing::warn!(%error, "Cannot start the Windows tray; close will minimize instead");
            }
        }) {
            tracing::warn!(%error, "Cannot start the Windows tray thread");
        }
    });
}

pub(super) fn shutdown() {
    let hwnd = HWND(WINDOW.swap(0, Ordering::AcqRel) as *mut _);
    READY.store(false, Ordering::Release);
    if !hwnd.is_invalid() {
        // SAFETY: the tray thread owns the window and destroys it on WM_CLOSE.
        let _ = unsafe { PostMessageW(Some(hwnd), WM_CLOSE, WPARAM(0), LPARAM(0)) };
    }
}

fn run() -> Result<()> {
    // SAFETY: this thread owns the hidden top-level window and its message
    // loop. All callbacks and class strings have process lifetime. A top-level
    // window (rather than HWND_MESSAGE) also receives TaskbarCreated broadcasts.
    unsafe {
        let instance = HINSTANCE(GetModuleHandleW(None)?.0);
        let class = w!("LocalSendTrayWindow");
        if RegisterClassW(&WNDCLASSW {
            lpfnWndProc: Some(window_proc),
            hInstance: instance,
            lpszClassName: class,
            ..Default::default()
        }) == 0
        {
            return Err(nsh_platform_windows::windows::core::Error::from_thread());
        }
        let hwnd = CreateWindowExW(
            WINDOW_EX_STYLE::default(),
            class,
            w!("LocalSend"),
            WINDOW_STYLE::default(),
            0,
            0,
            0,
            0,
            None,
            None,
            Some(instance),
            None,
        )?;
        WINDOW.store(hwnd.0 as usize, Ordering::Release);
        TASKBAR_CREATED.store(
            RegisterWindowMessageW(w!("TaskbarCreated")),
            Ordering::Release,
        );
        add_icon(hwnd);
        let mut message = MSG::default();
        loop {
            let result = GetMessageW(&mut message, None, 0, 0).0;
            if result <= 0 {
                if result == -1 {
                    tracing::warn!("Windows tray message loop failed");
                    let _ = DestroyWindow(hwnd);
                }
                break;
            }
            let _ = TranslateMessage(&message);
            DispatchMessageW(&message);
        }
        WINDOW.store(0, Ordering::Release);
    }
    Ok(())
}

fn icon_data(hwnd: HWND) -> NOTIFYICONDATAW {
    NOTIFYICONDATAW {
        cbSize: std::mem::size_of::<NOTIFYICONDATAW>() as u32,
        hWnd: hwnd,
        uID: 1,
        ..Default::default()
    }
}

fn add_icon(hwnd: HWND) {
    let mut data = icon_data(hwnd);
    data.uFlags = NIF_ICON | NIF_MESSAGE | NIF_TIP | NIF_SHOWTIP;
    data.uCallbackMessage = CALLBACK;
    for (slot, ch) in data.szTip.iter_mut().zip("LocalSend".encode_utf16()) {
        *slot = ch;
    }
    // SAFETY: nsh packages the app icon at resource ID 1. LoadIconW returns
    // shared icons, which must not be destroyed by this code. The default icon
    // is a fallback for cargo run binaries without embedded resources.
    unsafe {
        let instance = GetModuleHandleW(None)
            .ok()
            .map(|module| HINSTANCE(module.0));
        data.hIcon = LoadIconW(instance, PCWSTR(1 as _))
            .or_else(|_| LoadIconW(None, IDI_APPLICATION))
            .unwrap_or_default();
        let added = Shell_NotifyIconW(NIM_ADD, &data).as_bool();
        if added {
            data.Anonymous = NOTIFYICONDATAW_0 {
                uVersion: NOTIFYICON_VERSION_4,
            };
            let configured = Shell_NotifyIconW(NIM_SETVERSION, &data).as_bool();
            READY.store(configured, Ordering::Release);
            if !configured {
                // Do not hide the app behind an icon whose callback protocol
                // we cannot decode.
                let _ = Shell_NotifyIconW(NIM_DELETE, &data);
                tracing::warn!(
                    "Cannot configure the Windows tray icon; close will minimize instead"
                );
            }
        } else {
            READY.store(false, Ordering::Release);
            tracing::warn!("Cannot add the Windows tray icon; close will minimize instead");
        }
    }
}

unsafe extern "system" fn window_proc(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    let taskbar_created = TASKBAR_CREATED.load(Ordering::Acquire);
    if taskbar_created != 0 && message == taskbar_created {
        // Explorer restarted: recreate the icon before another close can hide
        // the window. If that fails, restore a taskbar entry for recovery.
        READY.store(false, Ordering::Release);
        add_icon(hwnd);
        if !available() {
            super::windows_close::show();
        }
        return LRESULT(0);
    }
    match message {
        HIDE_WINDOW => {
            // This runs on the same thread that changes READY and restores
            // the window after a failed icon recreation. Neither operation
            // can overtake the readiness check and enqueue a stale hide.
            super::windows_close::hide_window(HWND(wparam.0 as *mut _), available());
            LRESULT(0)
        }
        CALLBACK => {
            // Version 4 packs the event into LOWORD(lParam).
            match lparam.0 as u32 & 0xffff {
                NIN_SELECT | NIN_KEYSELECT => {
                    super::windows_close::show();
                    request(TrayAction::Open(None));
                }
                WM_CONTEXTMENU => popup(hwnd),
                _ => {}
            }
            LRESULT(0)
        }
        WM_DESTROY => {
            READY.store(false, Ordering::Release);
            // SAFETY: this is our icon and our native message loop.
            unsafe {
                let _ = Shell_NotifyIconW(NIM_DELETE, &icon_data(hwnd));
                PostQuitMessage(0);
            }
            LRESULT(0)
        }
        // SAFETY: pass all unhandled messages through, including WM_CLOSE,
        // whose default handler destroys this thread's hidden window.
        _ => unsafe { DefWindowProcW(hwnd, message, wparam, lparam) },
    }
}

fn popup(hwnd: HWND) {
    let language = *LANGUAGE
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let open: Vec<u16> = i18n::text(language, "tray.open")
        .encode_utf16()
        .chain([0])
        .collect();
    let quit: Vec<u16> = i18n::text(language, "tray.quit")
        .encode_utf16()
        .chain([0])
        .collect();
    // SAFETY: the menu belongs to this thread; labels live through the modal
    // menu loop, and it is destroyed after selection. Foreground + WM_NULL is
    // the documented notification-area popup dismissal pattern.
    unsafe {
        let Ok(menu) = CreatePopupMenu() else { return };
        let built = AppendMenuW(menu, MF_STRING, OPEN, PCWSTR(open.as_ptr()))
            .and_then(|()| AppendMenuW(menu, MF_STRING, QUIT, PCWSTR(quit.as_ptr())));
        if built.is_ok() {
            let mut point = POINT::default();
            let _ = GetCursorPos(&mut point);
            let _ = SetForegroundWindow(hwnd);
            let selected = TrackPopupMenu(
                menu,
                TPM_RETURNCMD | TPM_RIGHTBUTTON,
                point.x,
                point.y,
                None,
                hwnd,
                None,
            )
            .0 as usize;
            let _ = PostMessageW(Some(hwnd), WM_NULL, WPARAM(0), LPARAM(0));
            match selected {
                OPEN => {
                    super::windows_close::show();
                    request(TrayAction::Open(None));
                }
                QUIT => request(TrayAction::Quit),
                _ => {}
            }
        }
        let _ = DestroyMenu(menu);
    }
}

/// Runs in the native close regression's UI thread with its live main window.
/// No Explorer dependency: simulate icon recreation failure on either side
/// of a close posted by another thread, then dispatch the real tray handler.
#[cfg(test)]
pub(super) fn test_close_recovery_orderings(main: HWND) {
    use super::windows_close::wait_for_window;
    use nsh_platform_windows::windows::Win32::UI::WindowsAndMessaging::{
        IsIconic, IsWindowVisible, PM_REMOVE, PeekMessageW, WM_QUIT,
    };
    // SAFETY: the caller owns main on this thread; this test creates and
    // destroys its tray window here and dispatches only its own messages.
    unsafe {
        let class = w!("LocalSendTrayRaceTest");
        assert_ne!(
            RegisterClassW(&WNDCLASSW {
                lpfnWndProc: Some(window_proc),
                lpszClassName: class,
                ..Default::default()
            }),
            0
        );
        let tray = CreateWindowExW(
            WINDOW_EX_STYLE::default(),
            class,
            w!(""),
            WINDOW_STYLE::default(),
            0,
            0,
            0,
            0,
            None,
            None,
            None,
            None,
        )
        .unwrap();
        WINDOW.store(tray.0 as usize, Ordering::Release);
        READY.store(true, Ordering::Release);
        let main_bits = main.0 as usize;
        std::thread::spawn(move || {
            assert!(hide_window(HWND(main_bits as *mut _)));
        })
        .join()
        .unwrap();

        // Failure/recovery overtakes a queued close. The close must evaluate
        // current readiness, rather than hide using its earlier true value.
        READY.store(false, Ordering::Release);
        super::windows_close::show();
        let mut message = MSG::default();
        assert!(
            PeekMessageW(
                &mut message,
                Some(tray),
                HIDE_WINDOW,
                HIDE_WINDOW,
                PM_REMOVE
            )
            .as_bool()
        );
        DispatchMessageW(&message);
        wait_for_window(|| IsIconic(main).as_bool());
        assert!(
            IsWindowVisible(main).as_bool(),
            "late close stranded the app"
        );
        super::windows_close::show();

        wait_for_window(|| !IsIconic(main).as_bool());

        // Reverse ordering: the close hides first, then recovery restores it.
        READY.store(true, Ordering::Release);
        assert!(hide_window(main));
        assert!(
            PeekMessageW(
                &mut message,
                Some(tray),
                HIDE_WINDOW,
                HIDE_WINDOW,
                PM_REMOVE
            )
            .as_bool()
        );
        DispatchMessageW(&message);
        wait_for_window(|| !IsWindowVisible(main).as_bool());
        READY.store(false, Ordering::Release);
        super::windows_close::show();
        wait_for_window(|| IsWindowVisible(main).as_bool());

        WINDOW.store(0, Ordering::Release);
        DestroyWindow(tray).unwrap();
        // This tray's production handler posts quit for its loop. Consume
        // that test-owned message before returning to the main-window test.
        let _ = PeekMessageW(&mut message, None, WM_QUIT, WM_QUIT, PM_REMOVE);
    }
}
