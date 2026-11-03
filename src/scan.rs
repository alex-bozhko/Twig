//! Background walk of a folder tree for the filter's "search subfolders" mode.
//!
//! A scan lists every folder under a tab's root, breadth first, and hands the listings to the
//! UI thread in batches (WM_SCAN). There is no item limit: the user stops a long scan by
//! turning subfolder search off or clearing the filter.

use crate::loader::{list_dir, TabId};
use crate::model::{Entry, Kind};
use crate::win::{post_main, WM_SCAN};
use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use windows::Win32::System::Com::{CoInitializeEx, COINIT_APARTMENTTHREADED, COINIT_DISABLE_OLE1DDE};

const PUBLISH_EVERY: Duration = Duration::from_millis(250);
const ATTR_REPARSE_POINT: u32 = 0x400;

pub enum ScanMsg {
    /// Complete listings of some folders, parents before their children.
    Batch { tab: TabId, id: u64, dirs: Vec<(String, Vec<Entry>)>, items: usize, folders: usize },
    /// The walk ended.
    Done { tab: TabId, id: u64, items: usize, errors: usize },
}

pub struct ScanHandle {
    pub id: u64,
    cancel: Arc<AtomicBool>,
}

impl ScanHandle {
    pub fn cancel(&self) {
        self.cancel.store(true, Ordering::SeqCst);
    }
}

impl Drop for ScanHandle {
    fn drop(&mut self) {
        self.cancel();
    }
}

static NEXT_ID: AtomicU64 = AtomicU64::new(1);
static RESULTS: Mutex<Vec<ScanMsg>> = Mutex::new(Vec::new());
static POSTED: AtomicBool = AtomicBool::new(false);

/// Starts scanning everything under `root` for `tab`. Dropping the handle cancels the scan.
pub fn start(tab: TabId, root: String) -> ScanHandle {
    let id = NEXT_ID.fetch_add(1, Ordering::SeqCst);
    let cancel = Arc::new(AtomicBool::new(false));
    let c2 = cancel.clone();
    let _ = std::thread::Builder::new().name("scan".into()).spawn(move || {
        unsafe {
            let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED | COINIT_DISABLE_OLE1DDE);
        }
        let mut last = Instant::now();
        let mut dirs = Vec::new();
        let r = walk(&root, &c2, |dir, entries, items, folders| {
            dirs.push((dir, entries));
            if last.elapsed() >= PUBLISH_EVERY {
                last = Instant::now();
                publish(ScanMsg::Batch { tab, id, dirs: std::mem::take(&mut dirs), items, folders });
            }
        });
        if c2.load(Ordering::SeqCst) {
            return;
        }
        if !dirs.is_empty() {
            publish(ScanMsg::Batch { tab, id, dirs, items: r.items, folders: r.folders });
        }
        publish(ScanMsg::Done { tab, id, items: r.items, errors: r.errors });
    });
    ScanHandle { id, cancel }
}

/// Takes the pending scan messages (UI thread).
pub fn take() -> Vec<ScanMsg> {
    POSTED.store(false, Ordering::SeqCst);
    std::mem::take(&mut *RESULTS.lock().unwrap())
}

fn publish(m: ScanMsg) {
    RESULTS.lock().unwrap().push(m);
    if !POSTED.swap(true, Ordering::SeqCst) {
        post_main(WM_SCAN);
    }
}

#[derive(Debug, Default)]
pub struct WalkResult {
    pub items: usize,
    pub folders: usize,
    pub errors: usize,
}

/// Lists `root` and every folder below it, breadth first, calling `found(dir, entries, items,
/// folders)` per listed folder. Junctions/symlinks and zip files are not entered.
pub fn walk(root: &str, cancel: &AtomicBool, mut found: impl FnMut(String, Vec<Entry>, usize, usize)) -> WalkResult {
    let mut r = WalkResult::default();
    let mut queue = VecDeque::from([root.to_string()]);
    while let Some(dir) = queue.pop_front() {
        if cancel.load(Ordering::Relaxed) {
            break;
        }
        match list_dir(&dir) {
            Ok(entries) => {
                r.items += entries.len();
                r.folders += 1;
                for e in &entries {
                    if e.kind == Kind::Dir && e.attrs & ATTR_REPARSE_POINT == 0 {
                        queue.push_back(e.path.clone());
                    }
                }
                found(dir, entries, r.items, r.folders);
            }
            Err(_) => r.errors += 1,
        }
    }
    r
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn walk_lists_breadth_first() {
        let base = std::env::temp_dir().join(format!("twig-scan-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        for d in ["a\\deep", "b"] {
            std::fs::create_dir_all(base.join(d)).unwrap();
        }
        for f in ["top.txt", "a\\one.txt", "a\\deep\\two.txt", "b\\three.txt"] {
            std::fs::write(base.join(f), b"x").unwrap();
        }
        let root = base.to_string_lossy().to_string();
        let cancel = AtomicBool::new(false);
        let mut seen = Vec::new();
        let r = walk(&root, &cancel, |dir, e, _, _| seen.push((dir[root.len()..].to_string(), e.len())));
        assert_eq!(seen, vec![(String::new(), 3), ("\\a".into(), 2), ("\\b".into(), 1), ("\\a\\deep".into(), 1)]);
        assert_eq!((r.items, r.folders, r.errors), (7, 4, 0));
        // Cancelling stops before listing anything more.
        cancel.store(true, Ordering::Relaxed);
        let r = walk(&root, &cancel, |_, _, _, _| {});
        assert_eq!((r.items, r.folders), (0, 0));
        cancel.store(false, Ordering::Relaxed);
        let missing = walk(&base.join("nope").to_string_lossy(), &cancel, |_, _, _, _| {});
        assert_eq!((missing.items, missing.errors), (0, 1));
        let _ = std::fs::remove_dir_all(&base);
    }
}
