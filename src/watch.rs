//! Recursive directory change notifications (one thread per watched root).

use crate::util::{join_path, parent_path, wide};
use crate::win::{post_main, WM_WATCH};
use std::sync::{Mutex, OnceLock};
use windows::core::PCWSTR;
use windows::Win32::Foundation::*;
use windows::Win32::Storage::FileSystem::*;
use windows::Win32::System::Threading::*;
use windows::Win32::System::IO::*;

pub struct WatchEvent {
    pub id: u64,
    /// Directories whose listing changed.
    pub dirs: Vec<String>,
    /// Too many changes; refresh everything under the root.
    pub overflow: bool,
    /// The watch failed (root deleted, drive removed, ...).
    pub error: bool,
}

fn events() -> &'static Mutex<Vec<WatchEvent>> {
    static E: OnceLock<Mutex<Vec<WatchEvent>>> = OnceLock::new();
    E.get_or_init(|| Mutex::new(Vec::new()))
}

pub fn take_events() -> Vec<WatchEvent> {
    std::mem::take(&mut *events().lock().unwrap())
}

fn publish(ev: WatchEvent) {
    let mut q = events().lock().unwrap();
    let was_empty = q.is_empty();
    q.push(ev);
    drop(q);
    if was_empty {
        post_main(WM_WATCH);
    }
}

pub struct Watcher {
    pub id: u64,
    pub root: String,
    stop: isize,
}

impl Watcher {
    pub fn start(id: u64, root: &str) -> Option<Watcher> {
        let w = wide(root);
        let dir = unsafe {
            CreateFileW(
                PCWSTR(w.as_ptr()),
                FILE_LIST_DIRECTORY.0,
                FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
                None,
                OPEN_EXISTING,
                FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OVERLAPPED,
                None,
            )
        }
        .ok()?;
        let stop = unsafe { CreateEventW(None, true, false, None) }.ok()?;
        let (dir_i, stop_i) = (dir.0 as isize, stop.0 as isize);
        let root_s = root.to_string();
        std::thread::Builder::new()
            .name("watch".into())
            .spawn(move || run(id, root_s, HANDLE(dir_i as _), HANDLE(stop_i as _)))
            .ok()?;
        Some(Watcher { id, root: root.to_string(), stop: stop_i })
    }
}

impl Drop for Watcher {
    fn drop(&mut self) {
        // The thread owns and closes both handles after it sees the stop event.
        unsafe {
            let _ = SetEvent(HANDLE(self.stop as _));
        }
    }
}

fn run(id: u64, root: String, dir: HANDLE, stop: HANDLE) {
    let mut buf = vec![0u32; 16 * 1024]; // 64 KB, DWORD aligned (network limit)
    let ev = unsafe { CreateEventW(None, true, false, None) }.unwrap_or_default();
    let filter = FILE_NOTIFY_CHANGE_FILE_NAME
        | FILE_NOTIFY_CHANGE_DIR_NAME
        | FILE_NOTIFY_CHANGE_SIZE
        | FILE_NOTIFY_CHANGE_LAST_WRITE
        | FILE_NOTIFY_CHANGE_ATTRIBUTES;
    loop {
        let mut ov = OVERLAPPED { hEvent: ev, ..Default::default() };
        let issued = unsafe {
            ReadDirectoryChangesW(dir, buf.as_mut_ptr() as *mut _, (buf.len() * 4) as u32, true, filter, None, Some(&mut ov), None)
        };
        if issued.is_err() {
            if unsafe { WaitForSingleObject(stop, 0) } != WAIT_OBJECT_0 {
                publish(WatchEvent { id, dirs: vec![], overflow: false, error: true });
            }
            break;
        }
        let w = unsafe { WaitForMultipleObjects(&[ev, stop], false, INFINITE) };
        if w != WAIT_OBJECT_0 {
            unsafe {
                let _ = CancelIoEx(dir, Some(&ov));
                let mut n = 0;
                let _ = GetOverlappedResult(dir, &ov, &mut n, true);
            }
            break;
        }
        let mut bytes = 0u32;
        let ok = unsafe { GetOverlappedResult(dir, &ov, &mut bytes, false) }.is_ok();
        unsafe {
            let _ = ResetEvent(ev);
        }
        if !ok {
            publish(WatchEvent { id, dirs: vec![], overflow: false, error: true });
            break;
        }
        if bytes == 0 {
            publish(WatchEvent { id, dirs: vec![], overflow: true, error: false });
            continue;
        }
        let mut dirs: Vec<String> = Vec::new();
        let base = buf.as_ptr() as *const u8;
        let mut off = 0usize;
        loop {
            // SAFETY: the kernel filled `bytes` bytes with FILE_NOTIFY_INFORMATION records.
            let info = unsafe { &*(base.add(off) as *const FILE_NOTIFY_INFORMATION) };
            let len = info.FileNameLength as usize / 2;
            let name = unsafe { std::slice::from_raw_parts(info.FileName.as_ptr(), len) };
            let rel = String::from_utf16_lossy(name);
            let full = join_path(&root, &rel);
            if let Some(p) = parent_path(&full) {
                if !dirs.iter().any(|d| d.eq_ignore_ascii_case(&p)) {
                    dirs.push(p);
                }
            }
            if info.NextEntryOffset == 0 {
                break;
            }
            off += info.NextEntryOffset as usize;
            if off >= bytes as usize {
                break;
            }
        }
        publish(WatchEvent { id, dirs, overflow: false, error: false });
    }
    unsafe {
        let _ = CloseHandle(ev);
        let _ = CloseHandle(dir);
        let _ = CloseHandle(stop);
    }
}
