//! Routes the native close button and Alt+F4 through the session's close
//! policy. With background mode enabled the window is hidden, leaving the
//! tray and network alive. Only an explicit quit allows destruction.
//! nsh 0.4 has no close-request callback, so a thread-local CBT hook vetoes
//! destruction of our main window until the quit flow allows it.
//!
//! The hook only sends a message to the engine; it must never wait for the
//! engine, which may itself be waiting for the Windows UI thread.

use crate::model::Session;
use nsh_core::prelude::*;
use nsh_platform_windows::windows::{
    Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM},
    Win32::UI::WindowsAndMessaging::{
        CallNextHookEx, EnumWindows, GetClassNameW, GetWindowThreadProcessId, HCBT_DESTROYWND,
        HHOOK, IsIconic, SW_HIDE, SW_MINIMIZE, SW_RESTORE, SW_SHOW, SetForegroundWindow,
        SetWindowsHookExW, ShowWindowAsync, UnhookWindowsHookEx, WH_CBT,
    },
    core::BOOL,
};
use std::sync::{
    Mutex,
    atomic::{AtomicBool, AtomicUsize, Ordering},
};

static REQUESTS: Mutex<Option<async_channel::Sender<()>>> = Mutex::new(None);
static WINDOW: AtomicUsize = AtomicUsize::new(0);
static ALLOW_CLOSE: AtomicBool = AtomicBool::new(false);

/// Called only after state has been flushed and any update installer started.
pub(super) fn allow_close() {
    super::windows_tray::shutdown();
    ALLOW_CLOSE.store(true, Ordering::Release);
}

/// Hide only when the tray is reachable; otherwise keep a taskbar entry so
/// a failed Explorer/tray startup cannot strand the running app.
pub(super) fn hide() {
    let hwnd = HWND(WINDOW.load(Ordering::Acquire) as *mut _);
    if !super::windows_tray::hide_window(hwnd) {
        // No tray thread yet (or it has stopped): never hide without a
        // reachable icon. This fallback can only minimize, so it is safe
        // even if Explorer is concurrently recovering.
        hide_window(hwnd, false);
    }
}

/// Called on the tray thread, which also orders Explorer recovery requests.
pub(super) fn hide_window(hwnd: HWND, tray_available: bool) {
    if !hwnd.is_invalid() {
        let command = if tray_available { SW_HIDE } else { SW_MINIMIZE };
        // SAFETY: the handle is our main window. The async API marshals to
        // its UI thread without waiting for an engine/UI round trip.
        let _ = unsafe { ShowWindowAsync(hwnd, command) };
    }
}

pub(super) fn show() {
    let hwnd = HWND(WINDOW.load(Ordering::Acquire) as *mut _);
    if !hwnd.is_invalid() {
        // SAFETY: these APIs accept cross-thread window handles. Restore a
        // minimized window, but preserve the size of a merely hidden one.
        unsafe {
            let command = if IsIconic(hwnd).as_bool() {
                SW_RESTORE
            } else {
                SW_SHOW
            };
            let _ = ShowWindowAsync(hwnd, command);
            let _ = SetForegroundWindow(hwnd);
        }
    }
}

pub(super) fn install(session: Session) {
    if !is_desktop() || std::env::var_os("LOCALSEND_OFFLINE").is_some() {
        return;
    }
    let mut hwnd = HWND::default();
    // SAFETY: enumeration is synchronous; the callback only writes this live
    // stack slot, and filters to our process's nsh top-level window.
    let _ = unsafe { EnumWindows(Some(find_window), LPARAM((&mut hwnd as *mut HWND) as isize)) };
    if hwnd.is_invalid() {
        tracing::warn!("Cannot find the main window: native close cannot flush the session");
        return;
    }
    let (sender, receiver) = async_channel::bounded(1);
    *REQUESTS
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(sender);
    WINDOW.store(hwnd.0 as usize, Ordering::Release);
    // SAFETY: hwnd belongs to this process. The callback has the HOOKPROC
    // ABI and process lifetime; the hook targets only the window's UI thread,
    // so no DLL or global system hook is needed.
    let hook = unsafe {
        let thread = GetWindowThreadProcessId(hwnd, None);
        SetWindowsHookExW(WH_CBT, Some(before_destroy), None, thread)
    };
    let hook = match hook {
        Ok(hook) => hook.0 as usize,
        Err(error) => {
            tracing::warn!(%error, "Cannot intercept the native close button");
            return;
        }
    };
    cleanup(move || {
        // SAFETY: this is our hook handle. Windows also releases thread hooks
        // at thread exit; no callback owns engine-thread state.
        let _ = unsafe { UnhookWindowsHookEx(HHOOK(hook as *mut _)) };
    });
    session.clone().task_detached(async move {
        while receiver.recv().await.is_ok() {
            super::quit::close(&session);
        }
    });
}

unsafe extern "system" fn find_window(hwnd: HWND, slot: LPARAM) -> BOOL {
    let mut pid = 0;
    // SAFETY: the OS supplied hwnd and pid is a valid output slot.
    unsafe { GetWindowThreadProcessId(hwnd, Some(&mut pid)) };
    if pid == std::process::id() {
        let mut class = [0_u16; 64];
        // SAFETY: class is a writable buffer for the window's class name.
        let len = unsafe { GetClassNameW(hwnd, &mut class) } as usize;
        if class[..len].iter().copied().eq("NshWindow".encode_utf16()) {
            // SAFETY: install passed a valid HWND slot for this synchronous
            // enumeration; it remains alive until EnumWindows returns.
            unsafe { *(slot.0 as *mut HWND) = hwnd };
            return BOOL(0);
        }
    }
    BOOL(1)
}

unsafe extern "system" fn before_destroy(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if code == HCBT_DESTROYWND as i32
        && wparam.0 == WINDOW.load(Ordering::Acquire)
        && !ALLOW_CLOSE.load(Ordering::Acquire)
    {
        let requests = REQUESTS
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        // A queued close already owns this request too.
        if matches!(
            requests.as_ref().map(|sender| sender.try_send(())),
            Some(Ok(()) | Err(async_channel::TrySendError::Full(())))
        ) {
            return LRESULT(1);
        }
    }
    // SAFETY: forward every unhandled event, especially negative hook codes.
    unsafe { CallNextHookEx(None, code, wparam, lparam) }
}

#[cfg(test)]
pub(super) fn wait_for_window(mut condition: impl FnMut() -> bool) {
    use nsh_platform_windows::windows::Win32::UI::WindowsAndMessaging::{
        DispatchMessageW, MSG, PM_REMOVE, PeekMessageW, TranslateMessage,
    };
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
    while !condition() {
        assert!(
            std::time::Instant::now() < deadline,
            "window state did not change"
        );
        // SAFETY: the test owns this thread's windows and services their
        // asynchronous ShowWindow requests as the real UI loop would.
        unsafe {
            let mut message = MSG::default();
            while PeekMessageW(&mut message, None, 0, 0, PM_REMOVE).as_bool() {
                let _ = TranslateMessage(&message);
                DispatchMessageW(&message);
            }
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nsh_platform_windows::windows::{
        Win32::UI::WindowsAndMessaging::{
            CreateWindowExW, DefWindowProcW, DestroyWindow, IsWindow, IsWindowVisible,
            RegisterClassW, SC_CLOSE, SW_SHOW, SendMessageW, ShowWindow, WINDOW_EX_STYLE, WM_CLOSE,
            WM_SYSCOMMAND, WNDCLASSW, WS_OVERLAPPEDWINDOW,
        },
        core::w,
    };

    unsafe extern "system" fn window_proc(
        hwnd: HWND,
        message: u32,
        wparam: WPARAM,
        lparam: LPARAM,
    ) -> LRESULT {
        // SAFETY: the window manager supplied the arguments unchanged.
        unsafe { DefWindowProcW(hwnd, message, wparam, lparam) }
    }

    /// Exercises a real UI-thread hook without starting the app or installer.
    #[test]
    fn native_destruction_waits_for_the_quit_flow() {
        // SAFETY: this thread owns this hidden test window and its hook. All
        // callbacks have process lifetime and no borrowed callback state.
        unsafe {
            let class = w!("LocalSendCloseHookTest");
            assert_ne!(
                RegisterClassW(&WNDCLASSW {
                    lpfnWndProc: Some(window_proc),
                    lpszClassName: class,
                    ..Default::default()
                }),
                0
            );
            let hwnd = CreateWindowExW(
                WINDOW_EX_STYLE::default(),
                class,
                w!(""),
                WS_OVERLAPPEDWINDOW,
                0,
                0,
                100,
                100,
                None,
                None,
                None,
                None,
            )
            .unwrap();
            let (sender, receiver) = async_channel::bounded(1);
            *REQUESTS.lock().unwrap() = Some(sender);
            WINDOW.store(hwnd.0 as usize, Ordering::Release);
            ALLOW_CLOSE.store(false, Ordering::Release);
            let thread = GetWindowThreadProcessId(hwnd, None);
            let hook = SetWindowsHookExW(WH_CBT, Some(before_destroy), None, thread).unwrap();

            let _ = ShowWindow(hwnd, SW_SHOW);
            SendMessageW(hwnd, WM_CLOSE, None, None);
            assert!(IsWindow(Some(hwnd)).as_bool());
            receiver
                .try_recv()
                .expect("native close should follow the session close policy");
            // Background close hides the window without destroying the nsh
            // surface, which would stop the network and end the process.
            hide_window(hwnd, true);
            wait_for_window(|| !IsWindowVisible(hwnd).as_bool());
            assert!(IsWindow(Some(hwnd)).as_bool());
            show();
            wait_for_window(|| IsWindowVisible(hwnd).as_bool());

            // The window can be closed and reopened repeatedly (also Alt+F4).
            SendMessageW(hwnd, WM_SYSCOMMAND, Some(WPARAM(SC_CLOSE as usize)), None);
            assert!(IsWindow(Some(hwnd)).as_bool());
            receiver.try_recv().unwrap();

            // Without a working tray, keep a taskbar entry for recovery.
            hide_window(hwnd, false);
            wait_for_window(|| IsIconic(hwnd).as_bool());
            assert!(IsWindowVisible(hwnd).as_bool());
            show();
            wait_for_window(|| !IsIconic(hwnd).as_bool());

            super::super::windows_tray::test_close_recovery_orderings(hwnd);

            allow_close();
            DestroyWindow(hwnd).unwrap();
            assert!(!IsWindow(Some(hwnd)).as_bool());
            assert!(receiver.try_recv().is_err());
            UnhookWindowsHookEx(hook).unwrap();
        }
    }
}
