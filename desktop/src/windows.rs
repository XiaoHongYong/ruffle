use std::ffi::OsString;
use std::os::windows::ffi::OsStringExt;
use std::path::PathBuf;
use std::sync::Mutex;

use windows_sys::Win32::Foundation::{HWND, TRUE};
use windows_sys::Win32::Storage::FileSystem::{FILE_TYPE_DISK, FILE_TYPE_PIPE, GetFileType};
use windows_sys::Win32::System::Com::{CoInitializeEx, COINIT_APARTMENTTHREADED};
use windows_sys::Win32::System::Console::{
    ATTACH_PARENT_PROCESS, AttachConsole, FreeConsole, GetStdHandle, STD_OUTPUT_HANDLE,
};
use windows_sys::Win32::System::Ole::RevokeDragDrop;
use windows_sys::Win32::UI::Shell::{DragAcceptFiles, DragFinish, DragQueryFileW, HDROP};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    ChangeWindowMessageFilterEx, MSGFLT_ALLOW, MSG, WM_COPYDATA,
};
use winit::event_loop::EventLoopProxy;
use winit::raw_window_handle::{HasWindowHandle, RawWindowHandle};
use winit::window::Window;

use crate::custom_event::RuffleEvent;

use std::ptr::NonNull;

/// WM_DROPFILES — not always re-exported depending on windows-sys feature set.
const WM_DROPFILES: u32 = 0x0233;
/// Undocumented but required alongside WM_DROPFILES for elevated drag-drop.
const WM_COPYGLOBALDATA: u32 = 0x0049;

static DROP_PROXY: Mutex<Option<EventLoopProxy<RuffleEvent>>> = Mutex::new(None);

/// Initialize the UI thread as a COM STA before winit / audio / GPU touch COM.
///
/// OleInitialize (used by winit for drag-drop) requires the apartment model;
/// initializing early avoids `RPC_E_CHANGED_MODE` from other crates.
pub(super) fn init_com_apartment() {
    // SAFETY: Called once on the UI thread before other COM users.
    // S_OK and S_FALSE (already initialized) are both acceptable.
    unsafe {
        let _ = CoInitializeEx(std::ptr::null(), COINIT_APARTMENTTHREADED as u32);
    }
}

pub(super) fn set_drop_proxy(proxy: EventLoopProxy<RuffleEvent>) {
    if let Ok(mut guard) = DROP_PROXY.lock() {
        *guard = Some(proxy);
    }
}

/// Switch from winit's OLE `IDropTarget` to classic `WM_DROPFILES`.
///
/// Winit's drop handler leaves `QueryInterface` unimplemented (can panic) and
/// OLE drops are often blocked for elevated processes on Windows 7. Classic
/// drag-drop plus message-filter exceptions is more reliable.
pub(super) fn enable_file_drops(window: &Window) {
    let Some(hwnd) = hwnd_from_window(window) else {
        tracing::warn!("Could not get HWND to enable file drag-drop");
        return;
    };

    // SAFETY: hwnd is a live window we own; filter APIs tolerate unknown messages.
    unsafe {
        ChangeWindowMessageFilterEx(hwnd, WM_DROPFILES, MSGFLT_ALLOW, std::ptr::null_mut());
        ChangeWindowMessageFilterEx(hwnd, WM_COPYDATA, MSGFLT_ALLOW, std::ptr::null_mut());
        ChangeWindowMessageFilterEx(hwnd, WM_COPYGLOBALDATA, MSGFLT_ALLOW, std::ptr::null_mut());

        // RegisterDragDrop takes precedence over DragAcceptFiles; revoke first.
        let _ = RevokeDragDrop(hwnd);
        DragAcceptFiles(hwnd, TRUE);
    }
}

/// Hook for `EventLoopBuilderExtWindows::with_msg_hook`.
/// Returns true when the message was handled (skip winit dispatch).
pub(super) fn handle_drop_message(msg: *const std::ffi::c_void) -> bool {
    // SAFETY: winit passes a pointer to its stack `MSG`.
    let msg = unsafe { &*(msg as *const MSG) };
    if msg.message != WM_DROPFILES {
        return false;
    }

    let hdrop = msg.wParam as HDROP;
    let paths = unsafe { collect_hdrop_paths(hdrop) };
    // SAFETY: We own this HDROP for the duration of WM_DROPFILES handling.
    unsafe { DragFinish(hdrop) };

    if let Ok(guard) = DROP_PROXY.lock()
        && let Some(proxy) = guard.as_ref()
        && let Some(path) = paths.into_iter().next()
    {
        let _ = proxy.send_event(RuffleEvent::OpenDroppedFile(path));
    }

    true
}

pub(super) fn hwnd_from_window(window: &Window) -> Option<HWND> {
    let handle = window.window_handle().ok()?;
    match handle.as_raw() {
        RawWindowHandle::Win32(win32) => Some(win32.hwnd.get() as HWND),
        _ => None,
    }
}

/// SAFETY: `hdrop` must be a valid `HDROP` from `WM_DROPFILES` / `CF_HDROP`.
unsafe fn collect_hdrop_paths(hdrop: HDROP) -> Vec<PathBuf> {
    let item_count = unsafe { DragQueryFileW(hdrop, 0xffffffff, std::ptr::null_mut(), 0) };
    let mut paths = Vec::with_capacity(item_count as usize);

    for i in 0..item_count {
        let character_count = unsafe { DragQueryFileW(hdrop, i, std::ptr::null_mut(), 0) as usize };
        let str_len = character_count + 1;
        let mut path_buf = Vec::with_capacity(str_len);
        unsafe {
            DragQueryFileW(hdrop, i, path_buf.as_mut_ptr(), str_len as u32);
            path_buf.set_len(str_len);
        }
        paths.push(OsString::from_wide(&path_buf[..character_count]).into());
    }

    paths
}

/// RAII guard for the attached parent console.
/// Frees the console on drop if it was successfully attached.
pub(super) struct Console {
    attached: bool,
}

impl Console {
    // When linked with the windows subsystem windows won't automatically attach
    // to the console of the parent process, so we do it explicitly. This fails
    // silently if the parent has no console.
    //
    // However, if stdout/stderr are already redirected (e.g., `ruffle.exe > file.txt`),
    // we should NOT attach to the console as that would bypass the redirection.
    // See: https://github.com/ruffle-rs/ruffle/issues/9145
    pub(super) fn attach() -> Self {
        // Check if stdout is already redirected to a file or pipe
        // SAFETY: STD_OUTPUT_HANDLE is a valid standard device constant.
        let stdout_handle = NonNull::new(unsafe { GetStdHandle(STD_OUTPUT_HANDLE) });

        let should_attach = stdout_handle.is_none_or(|mut handle| {
            // SAFETY: GetFileType accepts any handle value including INVALID_HANDLE_VALUE,
            // returning FILE_TYPE_UNKNOWN in that case.
            let file_type = unsafe { GetFileType(handle.as_mut()) };

            // If output is redirected to a file or pipe, don't attach to console
            // as that would bypass the redirection
            !matches!(file_type, FILE_TYPE_DISK | FILE_TYPE_PIPE)
        });

        let attached = if should_attach {
            // Otherwise, attach to parent console for interactive use
            // SAFETY: ATTACH_PARENT_PROCESS is a valid constant for AttachConsole.
            // This call fails silently if the parent has no console.
            (unsafe { AttachConsole(ATTACH_PARENT_PROCESS) }) != 0
        } else {
            false
        };

        Self { attached }
    }
}

impl Drop for Console {
    fn drop(&mut self) {
        if self.attached {
            // Without explicitly detaching, cmd won't redraw its prompt.
            // SAFETY: We only call FreeConsole if it was previously successfully attached.
            unsafe { FreeConsole() };
        }
    }
}
