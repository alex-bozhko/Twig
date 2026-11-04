//! Command-line path and single-instance handling: a second launch hands its path to the
//! running window (WM_COPYDATA) and exits, so "open folder" requests become new tabs.

use crate::util::{clean_path_input, wide};
use std::cell::RefCell;
use std::ffi::c_void;
use windows::core::PCWSTR;
use windows::Win32::Foundation::*;
use windows::Win32::System::DataExchange::COPYDATASTRUCT;
use windows::Win32::System::Threading::CreateMutexW;
use windows::Win32::UI::WindowsAndMessaging::*;

/// Tags our WM_COPYDATA payloads ("TWIG").
const MAGIC: usize = 0x5457_4947;

#[cfg(debug_assertions)]
const MUTEX: &str = "Local\\Twig.SingleInstance.Debug";
#[cfg(not(debug_assertions))]
const MUTEX: &str = "Local\\Twig.SingleInstance";

thread_local! {
    static PENDING: RefCell<Vec<String>> = const { RefCell::new(Vec::new()) };
}

/// The path passed on the command line, if any. Arguments are rejoined so an unquoted path with
/// spaces still works; stray quotes are dropped (`"C:\"` arrives as `C:"`).
pub fn path_from_args<I: IntoIterator<Item = String>>(args: I) -> Option<String> {
    let joined = args.into_iter().collect::<Vec<_>>().join(" ");
    let path = clean_path_input(&joined);
    (!path.is_empty()).then_some(path)
}

/// Claims the single-instance mutex. Returns false if another Twig already holds it.
pub fn claim() -> bool {
    let name = wide(MUTEX);
    unsafe {
        match CreateMutexW(None, false, PCWSTR(name.as_ptr())) {
            // The handle is intentionally kept open for the life of the process.
            Ok(_) => GetLastError() != ERROR_ALREADY_EXISTS,
            Err(_) => true,
        }
    }
}

/// Sends `path` (or just an activation request) to the running instance. Returns false if no
/// window answered, in which case the caller should start normally.
pub fn forward(path: Option<&str>) -> bool {
    let target = (0..50).find_map(|i| {
        if i > 0 {
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
        unsafe { FindWindowW(crate::mainwin::CLASS, PCWSTR::null()) }.ok().filter(|h| !h.0.is_null())
    });
    let Some(hwnd) = target else { return false };
    let text: Vec<u16> = path.unwrap_or("").encode_utf16().collect();
    let cds = COPYDATASTRUCT { dwData: MAGIC, cbData: (text.len() * 2) as u32, lpData: text.as_ptr() as *mut c_void };
    unsafe {
        let mut pid = 0u32;
        GetWindowThreadProcessId(hwnd, Some(&mut pid));
        let _ = AllowSetForegroundWindow(pid);
        let mut result = 0usize;
        let ok = SendMessageTimeoutW(hwnd, WM_COPYDATA, WPARAM(0), LPARAM(&cds as *const _ as isize), SMTO_ABORTIFHUNG, 5000, Some(&mut result));
        ok.0 != 0 && result == 1
    }
}

/// Lets a non-elevated launch reach an elevated window.
pub fn allow_copydata(hwnd: HWND) {
    unsafe {
        let _ = ChangeWindowMessageFilterEx(hwnd, WM_COPYDATA, MSGFLT_ALLOW, None);
    }
}

/// Handles WM_COPYDATA on the main window: queues the path and posts WM_OPEN_PATH.
/// Returns false for payloads that aren't ours.
pub fn receive(hwnd: HWND, lp: LPARAM) -> bool {
    let cds = unsafe { &*(lp.0 as *const COPYDATASTRUCT) };
    if cds.dwData != MAGIC {
        return false;
    }
    if cds.cbData >= 2 && !cds.lpData.is_null() {
        let units = unsafe { std::slice::from_raw_parts(cds.lpData as *const u16, cds.cbData as usize / 2) };
        let path = String::from_utf16_lossy(units);
        PENDING.with(|p| p.borrow_mut().push(path));
    }
    crate::win::post(hwnd, crate::win::WM_OPEN_PATH, 0, 0);
    true
}

pub fn take_pending() -> Vec<String> {
    PENDING.with(|p| std::mem::take(&mut *p.borrow_mut()))
}

pub fn bring_to_front(hwnd: HWND) {
    unsafe {
        if IsIconic(hwnd).as_bool() {
            let _ = ShowWindow(hwnd, SW_RESTORE);
        }
        let _ = SetForegroundWindow(hwnd);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(a: &[&str]) -> Option<String> {
        path_from_args(a.iter().map(|s| s.to_string()))
    }

    #[test]
    fn path_from_args_cleans_and_joins() {
        assert_eq!(args(&[]), None);
        assert_eq!(args(&["  "]), None);
        assert_eq!(args(&["C:\""]), Some("C:".into()));
        assert_eq!(args(&["C:\\Program", "Files"]), Some("C:\\Program Files".into()));
        assert_eq!(args(&["\"C:\\Users\\me\""]), Some("C:\\Users\\me".into()));
    }
}
