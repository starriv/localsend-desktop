//! Routes the native close button and Alt+F4 through the session's quit
//! flow. nsh 0.4 has no close-request callback: a thread-local CBT hook
//! vetoes destruction of our main window until that flow allows it.
//!
//! The hook only sends a message to the engine; it must never wait for the
//! engine, which may itself be waiting for the Windows UI thread.

use crate::model::Session;
use nsh_core::prelude::*;
use nsh_platform_windows::windows::{
    Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM},
    Win32::UI::WindowsAndMessaging::{
        CallNextHookEx, EnumWindows, GetClassNameW, GetWindowThreadProcessId, HCBT_DESTROYWND,
        HHOOK, SetWindowsHookExW, UnhookWindowsHookEx, WH_CBT,
    },
    core::BOOL,
};
use std::sync::{
    OnceLock,
    atomic::{AtomicBool, AtomicUsize, Ordering},
};

static REQUESTS: OnceLock<async_channel::Sender<()>> = OnceLock::new();
static WINDOW: AtomicUsize = AtomicUsize::new(0);
static ALLOW_CLOSE: AtomicBool = AtomicBool::new(false);

/// Called only after state has been flushed and any update installer started.
pub(super) fn allow_close() {
    ALLOW_CLOSE.store(true, Ordering::Release);
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
    if REQUESTS.set(sender).is_err() {
        return;
    }
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
            super::quit::quit(&session);
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
        && let Some(sender) = REQUESTS.get()
    {
        match sender.try_send(()) {
            // A queued quit already owns this close request too.
            Ok(()) | Err(async_channel::TrySendError::Full(())) => return LRESULT(1),
            Err(async_channel::TrySendError::Closed(())) => {}
        }
    }
    // SAFETY: forward every unhandled event, especially negative hook codes.
    unsafe { CallNextHookEx(None, code, wparam, lparam) }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nsh_platform_windows::windows::{
        Win32::UI::WindowsAndMessaging::{
            CreateWindowExW, DefWindowProcW, DestroyWindow, IsWindow, RegisterClassW, SC_CLOSE,
            SendMessageW, WINDOW_EX_STYLE, WINDOW_STYLE, WM_CLOSE, WM_SYSCOMMAND, WNDCLASSW,
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
                WINDOW_STYLE::default(),
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
            REQUESTS.set(sender).unwrap();
            WINDOW.store(hwnd.0 as usize, Ordering::Release);
            ALLOW_CLOSE.store(false, Ordering::Release);
            let thread = GetWindowThreadProcessId(hwnd, None);
            let hook = SetWindowsHookExW(WH_CBT, Some(before_destroy), None, thread).unwrap();

            SendMessageW(hwnd, WM_CLOSE, None, None);
            assert!(IsWindow(Some(hwnd)).as_bool());
            receiver
                .try_recv()
                .expect("native close should request a session quit");
            // The same path works after the user cancels the first quit.
            SendMessageW(hwnd, WM_SYSCOMMAND, Some(WPARAM(SC_CLOSE as usize)), None);
            assert!(IsWindow(Some(hwnd)).as_bool());
            receiver.try_recv().unwrap();

            allow_close();
            DestroyWindow(hwnd).unwrap();
            assert!(!IsWindow(Some(hwnd)).as_bool());
            assert!(receiver.try_recv().is_err());
            UnhookWindowsHookEx(hook).unwrap();
        }
    }
}
