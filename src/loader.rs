//! Background directory/drive/zip listing on a small pool of COM-initialized worker threads.
//!
//! Listings go to a high-priority queue; per-item details (exact icons, "has subfolders" probes)
//! go to a low-priority queue so a slow folder never blocks other tabs.

use crate::model::{Entry, Kind, NodeId, ZipItem, ATTR_HIDDEN, ATTR_SYSTEM};
use crate::util::{extension_lower, join_path, wide};
use crate::win::{post_main, WM_LOADED};
use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex, OnceLock};
use windows::core::PCWSTR;
use windows::Win32::Foundation::*;
use windows::Win32::Storage::FileSystem::*;
use windows::Win32::System::Com::{CoInitializeEx, COINIT_APARTMENTTHREADED, COINIT_DISABLE_OLE1DDE};
use windows::Win32::UI::Shell::*;

pub type TabId = u32;

pub enum Job {
    List { tab: TabId, node: NodeId, path: String },
    Drives { tab: TabId, favorites: Vec<String> },
    Zip { tab: TabId, node: NodeId, path: String },
    Details { tab: TabId, node: NodeId, path: String, items: Vec<(String, u32, bool)> },
}

pub enum Done {
    Listed { tab: TabId, node: NodeId, path: String, result: Result<Vec<Entry>, String> },
    Drives { tab: TabId, entries: Vec<Entry> },
    Zip { tab: TabId, node: NodeId, result: Result<Vec<ZipItem>, String> },
    Details { tab: TabId, node: NodeId, details: Vec<(String, Option<i32>, Option<bool>)> },
}

struct Queues {
    high: VecDeque<Job>,
    low: VecDeque<Job>,
}

struct Shared {
    queues: Mutex<Queues>,
    cv: Condvar,
    results: Mutex<Vec<Done>>,
    posted: AtomicBool,
}

static SHARED: OnceLock<Arc<Shared>> = OnceLock::new();

fn shared() -> &'static Arc<Shared> {
    SHARED.get_or_init(|| {
        let s = Arc::new(Shared {
            queues: Mutex::new(Queues { high: VecDeque::new(), low: VecDeque::new() }),
            cv: Condvar::new(),
            results: Mutex::new(Vec::new()),
            posted: AtomicBool::new(false),
        });
        for i in 0..4 {
            let s2 = s.clone();
            std::thread::Builder::new().name(format!("loader-{i}")).spawn(move || worker(s2)).ok();
        }
        s
    })
}

pub fn submit(job: Job) {
    let s = shared();
    {
        let mut q = s.queues.lock().unwrap();
        match job {
            Job::Details { .. } => q.low.push_back(job),
            _ => q.high.push_back(job),
        }
    }
    s.cv.notify_one();
}

/// Drops queued (not yet started) jobs for a closed tab.
pub fn cancel_tab(tab: TabId) {
    let s = shared();
    let mut q = s.queues.lock().unwrap();
    let keep = |j: &Job| job_tab(j) != tab;
    q.high.retain(keep);
    q.low.retain(keep);
}

fn job_tab(j: &Job) -> TabId {
    match j {
        Job::List { tab, .. } | Job::Drives { tab, .. } | Job::Zip { tab, .. } | Job::Details { tab, .. } => *tab,
    }
}

/// Takes all finished results (UI thread).
pub fn take_results() -> Vec<Done> {
    let s = shared();
    s.posted.store(false, Ordering::SeqCst);
    std::mem::take(&mut *s.results.lock().unwrap())
}

fn publish(s: &Shared, d: Done) {
    s.results.lock().unwrap().push(d);
    if !s.posted.swap(true, Ordering::SeqCst) {
        post_main(WM_LOADED);
    }
}

fn worker(s: Arc<Shared>) {
    unsafe {
        let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED | COINIT_DISABLE_OLE1DDE);
    }
    loop {
        let job = {
            let mut q = s.queues.lock().unwrap();
            loop {
                if let Some(j) = q.high.pop_front() {
                    break j;
                }
                if let Some(j) = q.low.pop_front() {
                    break j;
                }
                q = s.cv.wait(q).unwrap();
            }
        };
        match job {
            Job::List { tab, node, path } => {
                let result = list_dir(&path);
                publish(&s, Done::Listed { tab, node, path, result });
            }
            Job::Drives { tab, favorites } => publish(&s, Done::Drives { tab, entries: this_pc(&favorites) }),
            Job::Zip { tab, node, path } => publish(&s, Done::Zip { tab, node, result: list_zip(&path) }),
            Job::Details { tab, node, path, items } => {
                for chunk in items.chunks(48) {
                    let details: Vec<_> = chunk.iter().map(|(name, attrs, is_dir)| detail(&path, name, *attrs, *is_dir)).collect();
                    publish(&s, Done::Details { tab, node, details });
                }
            }
        }
    }
}

// ---------- Shell info cache ----------

#[derive(Clone)]
struct TypeInfo {
    icon: i32,
    type_name: Arc<str>,
}

fn cache() -> &'static Mutex<HashMap<String, TypeInfo>> {
    static C: OnceLock<Mutex<HashMap<String, TypeInfo>>> = OnceLock::new();
    C.get_or_init(|| Mutex::new(HashMap::new()))
}

fn shfi(path: &str, attrs: FILE_FLAGS_AND_ATTRIBUTES, flags: SHGFI_FLAGS) -> Option<SHFILEINFOW> {
    let w = wide(path);
    let mut fi = SHFILEINFOW::default();
    let r = unsafe { SHGetFileInfoW(PCWSTR(w.as_ptr()), attrs, Some(&mut fi), std::mem::size_of::<SHFILEINFOW>() as u32, flags) };
    if r == 0 {
        None
    } else {
        Some(fi)
    }
}

fn type_info(ext_key: &str, is_dir: bool) -> TypeInfo {
    let key = if is_dir { "<dir>".to_string() } else { ext_key.to_string() };
    if let Some(t) = cache().lock().unwrap().get(&key) {
        return t.clone();
    }
    let probe = if is_dir { "folder".to_string() } else { format!("file{ext_key}") };
    let attrs = if is_dir { FILE_ATTRIBUTE_DIRECTORY } else { FILE_ATTRIBUTE_NORMAL };
    let info = match shfi(&probe, attrs, SHGFI_USEFILEATTRIBUTES | SHGFI_SYSICONINDEX | SHGFI_TYPENAME) {
        Some(fi) => TypeInfo { icon: fi.iIcon, type_name: Arc::from(crate::util::from_wide(&fi.szTypeName)) },
        None => TypeInfo {
            icon: -1,
            type_name: Arc::from(if is_dir {
                "File folder".to_string()
            } else if ext_key.is_empty() {
                "File".to_string()
            } else {
                format!("{} File", ext_key.trim_start_matches('.').to_uppercase())
            }),
        },
    };
    cache().lock().unwrap().insert(key, info.clone());
    info
}

pub fn folder_type() -> (Arc<str>, i32) {
    let t = type_info("", true);
    (t.type_name, t.icon)
}

pub fn file_type(name: &str) -> (Arc<str>, i32) {
    let t = type_info(&extension_lower(name), false);
    (t.type_name, t.icon)
}

/// Extensions whose icon depends on the file itself, not just the extension.
fn per_file_icon(ext: &str) -> bool {
    matches!(ext, ".exe" | ".lnk" | ".ico" | ".url" | ".cur" | ".ani" | ".msc" | ".appref-ms" | ".scr" | ".cpl")
}

fn dir_needs_own_icon(attrs: u32) -> bool {
    attrs & (FILE_ATTRIBUTE_READONLY.0 | FILE_ATTRIBUTE_SYSTEM.0) != 0
}

/// Builds an Entry for a path from find data (shared with single-item refreshes).
pub fn make_entry(dir: &str, fd: &WIN32_FIND_DATAW) -> Option<Entry> {
    let name = crate::util::from_wide(&fd.cFileName);
    if name == "." || name == ".." {
        return None;
    }
    let attrs = fd.dwFileAttributes;
    if attrs & ATTR_HIDDEN != 0 && attrs & ATTR_SYSTEM != 0 {
        return None;
    }
    let is_dir = attrs & FILE_ATTRIBUTE_DIRECTORY.0 != 0;
    let ext = extension_lower(&name);
    let kind = if is_dir {
        Kind::Dir
    } else if ext == ".zip" {
        Kind::Zip
    } else {
        Kind::File
    };
    let ti = type_info(&ext, is_dir);
    let needs_detail = if is_dir { dir_needs_own_icon(attrs) } else { per_file_icon(&ext) };
    Some(Entry {
        path: join_path(dir, &name),
        name,
        kind,
        attrs,
        size: if is_dir { 0 } else { ((fd.nFileSizeHigh as u64) << 32) | fd.nFileSizeLow as u64 },
        modified: ((fd.ftLastWriteTime.dwHighDateTime as u64) << 32) | fd.ftLastWriteTime.dwLowDateTime as u64,
        type_name: ti.type_name,
        icon: ti.icon,
        icon_final: !needs_detail,
        has_children: if kind == Kind::File { Some(false) } else { None },
    })
}

fn err_text(e: &windows::core::Error) -> String {
    let m = e.message();
    let m = m.trim();
    if m.is_empty() {
        format!("Error 0x{:08X}", e.code().0)
    } else {
        m.to_string()
    }
}

/// Lists one directory. Hidden+system items are skipped, like Explorer's default view.
pub fn list_dir(path: &str) -> Result<Vec<Entry>, String> {
    let pattern = wide(&join_path(path, "*"));
    let mut fd = WIN32_FIND_DATAW::default();
    let h = unsafe {
        FindFirstFileExW(
            PCWSTR(pattern.as_ptr()),
            FindExInfoBasic,
            &mut fd as *mut _ as *mut _,
            FindExSearchNameMatch,
            None,
            FIND_FIRST_EX_LARGE_FETCH,
        )
    };
    let h = match h {
        Ok(h) => h,
        Err(e) if e.code() == ERROR_FILE_NOT_FOUND.to_hresult() => return Ok(Vec::new()),
        Err(e) => return Err(err_text(&e)),
    };
    let mut out = Vec::new();
    loop {
        if let Some(e) = make_entry(path, &fd) {
            out.push(e);
        }
        if unsafe { FindNextFileW(h, &mut fd) }.is_err() {
            break;
        }
    }
    unsafe {
        let _ = FindClose(h);
    }
    Ok(out)
}

/// True if the directory has at least one visible child.
fn has_visible_children(dir: &str) -> bool {
    let pattern = wide(&join_path(dir, "*"));
    let mut fd = WIN32_FIND_DATAW::default();
    let Ok(h) = (unsafe {
        FindFirstFileExW(PCWSTR(pattern.as_ptr()), FindExInfoBasic, &mut fd as *mut _ as *mut _, FindExSearchNameMatch, None, FIND_FIRST_EX_FLAGS(0))
    }) else {
        return false;
    };
    let mut found = false;
    loop {
        let name = crate::util::from_wide(&fd.cFileName);
        let a = fd.dwFileAttributes;
        if name != "." && name != ".." && !(a & ATTR_HIDDEN != 0 && a & ATTR_SYSTEM != 0) {
            found = true;
            break;
        }
        if unsafe { FindNextFileW(h, &mut fd) }.is_err() {
            break;
        }
    }
    unsafe {
        let _ = FindClose(h);
    }
    found
}

fn detail(dir: &str, name: &str, attrs: u32, is_dir: bool) -> (String, Option<i32>, Option<bool>) {
    let full = join_path(dir, name);
    let ext = extension_lower(name);
    let own_icon = if is_dir { dir_needs_own_icon(attrs) } else { per_file_icon(&ext) };
    let icon = if own_icon { shfi(&full, FILE_FLAGS_AND_ATTRIBUTES(0), SHGFI_SYSICONINDEX).map(|fi| fi.iIcon) } else { None };
    let has = if is_dir { Some(has_visible_children(&full)) } else { None };
    (name.to_string(), icon, has)
}

/// Items of a listing that need a details pass: (name, attrs, is_dir).
pub fn detail_items(entries: &[Entry]) -> Vec<(String, u32, bool)> {
    entries
        .iter()
        .filter(|e| e.kind == Kind::Dir || !e.icon_final)
        .map(|e| (e.name.clone(), e.attrs, e.kind == Kind::Dir))
        .collect()
}

pub fn list_drives() -> Vec<Entry> {
    let mask = unsafe { windows::Win32::Storage::FileSystem::GetLogicalDrives() };
    let mut out = Vec::new();
    for i in 0..26u32 {
        if mask & (1 << i) == 0 {
            continue;
        }
        let letter = (b'A' + i as u8) as char;
        let root = format!("{letter}:\\");
        let w = wide(&root);
        let dtype = unsafe { GetDriveTypeW(PCWSTR(w.as_ptr())) };
        let fi = shfi(&root, FILE_FLAGS_AND_ATTRIBUTES(0), SHGFI_DISPLAYNAME | SHGFI_SYSICONINDEX | SHGFI_TYPENAME);
        let (name, icon, type_name) = match fi {
            Some(fi) => (crate::util::from_wide(&fi.szDisplayName), fi.iIcon, crate::util::from_wide(&fi.szTypeName)),
            None => (format!("({letter}:)"), -1, "Drive".to_string()),
        };
        let mut total = 0u64;
        // Network drives can hang for a long time when disconnected; skip their size.
        if dtype != 4 /* DRIVE_REMOTE */ {
            unsafe {
                let _ = GetDiskFreeSpaceExW(PCWSTR(w.as_ptr()), None, Some(&mut total), None);
            }
        }
        out.push(Entry {
            name: if name.is_empty() { format!("({letter}:)") } else { name },
            path: root,
            kind: Kind::Drive,
            attrs: 0,
            size: total,
            modified: 0,
            type_name: Arc::from(type_name),
            icon,
            icon_final: true,
            has_children: None,
        });
    }
    out
}

/// This PC: the drives, then each existing favorite folder (drive roots are already listed).
fn this_pc(favorites: &[String]) -> Vec<Entry> {
    let mut out = list_drives();
    let mut seen: std::collections::HashSet<String> = out.iter().map(|e| crate::util::path_key(&e.path)).collect();
    for f in favorites {
        if let Some(e) = entry_for_path(f).filter(|e| e.kind != Kind::Drive) {
            if seen.insert(crate::util::path_key(&e.path)) {
                out.push(e);
            }
        }
    }
    out
}

/// Builds a standalone entry for a tab root (drive root, folder or zip). None if it doesn't exist.
pub fn entry_for_path(path: &str) -> Option<Entry> {
    if crate::util::is_drive_root(path) {
        let root = format!("{}\\", path.trim_end_matches(['\\', '/']));
        let w = wide(&root);
        if unsafe { GetFileAttributesW(PCWSTR(w.as_ptr())) } == INVALID_FILE_ATTRIBUTES {
            return None;
        }
        let fi = shfi(&root, FILE_FLAGS_AND_ATTRIBUTES(0), SHGFI_DISPLAYNAME | SHGFI_SYSICONINDEX | SHGFI_TYPENAME);
        let (name, icon, type_name) = match fi {
            Some(fi) => (crate::util::from_wide(&fi.szDisplayName), fi.iIcon, crate::util::from_wide(&fi.szTypeName)),
            None => (root.clone(), -1, "Drive".to_string()),
        };
        return Some(Entry {
            name: if name.is_empty() { root.clone() } else { name },
            path: root,
            kind: Kind::Drive,
            attrs: 0,
            size: 0,
            modified: 0,
            type_name: Arc::from(type_name),
            icon,
            icon_final: true,
            has_children: None,
        });
    }
    let trimmed = path.trim_end_matches(['\\', '/']);
    let parent = crate::util::parent_path(trimmed)?;
    let w = wide(trimmed);
    let mut fd = WIN32_FIND_DATAW::default();
    let h = unsafe {
        FindFirstFileExW(PCWSTR(w.as_ptr()), FindExInfoBasic, &mut fd as *mut _ as *mut _, FindExSearchNameMatch, None, FIND_FIRST_EX_FLAGS(0))
    }
    .ok()?;
    unsafe {
        let _ = FindClose(h);
    }
    // A root is shown even if it is hidden+system.
    fd.dwFileAttributes &= !ATTR_SYSTEM;
    let mut e = make_entry(&parent, &fd)?;
    e.attrs = fd.dwFileAttributes;
    if !e.icon_final {
        if let Some(fi) = shfi(&e.path, FILE_FLAGS_AND_ATTRIBUTES(0), SHGFI_SYSICONINDEX) {
            e.icon = fi.iIcon;
        }
        e.icon_final = true;
    }
    Some(e)
}

fn zip_time(dt: Option<zip::DateTime>) -> u64 {
    use windows::Win32::System::Time::*;
    let Some(dt) = dt else { return 0 };
    let local = SYSTEMTIME {
        wYear: dt.year(),
        wMonth: dt.month() as u16,
        wDay: dt.day() as u16,
        wHour: dt.hour() as u16,
        wMinute: dt.minute() as u16,
        wSecond: dt.second().min(59) as u16,
        ..Default::default()
    };
    let mut utc = SYSTEMTIME::default();
    let mut ft = FILETIME::default();
    unsafe {
        if TzSpecificLocalTimeToSystemTime(None, &local, &mut utc).is_err() {
            return 0;
        }
        if SystemTimeToFileTime(&utc, &mut ft).is_err() {
            return 0;
        }
    }
    ((ft.dwHighDateTime as u64) << 32) | ft.dwLowDateTime as u64
}

pub fn list_zip(path: &str) -> Result<Vec<ZipItem>, String> {
    let f = std::fs::File::open(path).map_err(|e| e.to_string())?;
    let mut ar = zip::ZipArchive::new(std::io::BufReader::new(f)).map_err(|e| e.to_string())?;
    let mut out = Vec::with_capacity(ar.len());
    for i in 0..ar.len() {
        let Ok(zf) = ar.by_index_raw(i) else { continue };
        let inner = zf.name().replace('\\', "/");
        // Skip entries that try to escape the archive root.
        if inner.split('/').any(|p| p == "..") || inner.starts_with('/') {
            continue;
        }
        let is_dir = zf.is_dir();
        let leaf = inner.trim_end_matches('/').rsplit('/').next().unwrap_or("").to_string();
        let (type_name, icon) = if is_dir { folder_type() } else { file_type(&leaf) };
        out.push(ZipItem { inner, is_dir, size: zf.size(), modified: zip_time(zf.last_modified()), type_name, icon });
    }
    Ok(out)
}
