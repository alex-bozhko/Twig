//! The global action log: copies, moves, renames, deletes, new items and undos, newest last,
//! kept in %LOCALAPPDATA%\Twig\log.json.

use crate::fileops::Op;
use crate::util::{file_name, parent_path};
use serde::{Deserialize, Serialize};
use std::cell::RefCell;
use std::path::PathBuf;

/// Oldest entries beyond this are dropped.
const CAP: usize = 500;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Kind {
    Copy,
    Move,
    Rename,
    Recycle,
    Delete,
    NewFolder,
    NewFile,
    Undo,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Entry {
    /// Unix time, seconds.
    pub time: u64,
    pub kind: Kind,
    pub title: String,
    /// Affected items as (before, after); an empty side means it didn't exist (new / deleted).
    #[serde(default)]
    pub items: Vec<(String, String)>,
    /// Where it happened: the destination or containing folder.
    #[serde(default)]
    pub folder: Option<String>,
    #[serde(default)]
    pub error: Option<String>,
}

impl Entry {
    /// Second line for the log window: "source → destination" for copies and moves, else the folder.
    pub fn detail(&self) -> String {
        let folder = self.folder.clone().unwrap_or_default();
        let source = self.items.iter().find(|(from, _)| !from.is_empty()).and_then(|(from, _)| parent_path(from));
        match source {
            Some(src) if matches!(self.kind, Kind::Copy | Kind::Move) && !src.eq_ignore_ascii_case(&folder) => format!("{src}  \u{2192}  {folder}"),
            _ => folder,
        }
    }
}

fn now() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

/// "report.pdf", "3 items" or, when only some were done, "2 of 3 items".
fn subject(items: &[String], done: usize) -> String {
    match items {
        [one] => file_name(one),
        _ if done >= items.len() => format!("{} items", items.len()),
        _ => format!("{done} of {} items", items.len()),
    }
}

fn folder_label(path: &str) -> String {
    let name = file_name(path);
    if name.is_empty() {
        path.to_string()
    } else {
        name
    }
}

/// Describes a finished operation from what completed (`done`, as reported by the shell) and its
/// error. `None` when nothing happened and nothing failed (e.g. cancelled), and for undos.
pub fn describe(op: &Op, done: &[(String, String)], error: Option<&str>) -> Option<Entry> {
    if done.is_empty() && error.is_none() {
        return None;
    }
    let failed = done.is_empty();
    let n = done.len();
    let parent_of = |items: &[String]| items.first().and_then(|i| parent_path(i));
    let (kind, title, folder) = match op {
        Op::Copy { items, dest } | Op::Move { items, dest } => {
            let is_move = matches!(op, Op::Move { .. });
            let to = folder_label(dest);
            let title = match (failed, is_move) {
                (true, true) => format!("Couldn't move {} to {to}", subject(items, items.len())),
                (true, false) => format!("Couldn't copy {} to {to}", subject(items, items.len())),
                (false, true) => format!("Moved {} to {to}", subject(items, n)),
                (false, false) => format!("Copied {} to {to}", subject(items, n)),
            };
            (if is_move { Kind::Move } else { Kind::Copy }, title, Some(dest.clone()))
        }
        Op::Delete { items, recycle } => {
            let title = match (failed, recycle) {
                (true, _) => format!("Couldn't delete {}", subject(items, items.len())),
                (false, true) => format!("Moved {} to the Recycle Bin", subject(items, n)),
                (false, false) => format!("Permanently deleted {}", subject(items, n)),
            };
            (if *recycle { Kind::Recycle } else { Kind::Delete }, title, parent_of(items))
        }
        Op::Rename { item, name } => {
            let old = file_name(item);
            let title = match done.first() {
                Some((_, to)) => format!("Renamed {old} to {}", file_name(to)),
                None => format!("Couldn't rename {old} to {name}"),
            };
            (Kind::Rename, title, parent_path(item))
        }
        Op::NewFolder { parent, name } | Op::NewFile { parent, name } => {
            let (kind, what) = if matches!(op, Op::NewFolder { .. }) { (Kind::NewFolder, "folder") } else { (Kind::NewFile, "file") };
            let title = match done.first() {
                Some((_, p)) => format!("Created {what} {} in {}", file_name(p), folder_label(parent)),
                None => format!("Couldn't create {what} {name} in {}", folder_label(parent)),
            };
            (kind, title, Some(parent.clone()))
        }
        Op::Undo(_) => return None,
    };
    let items = match op {
        _ if !failed => done.to_vec(),
        Op::Copy { items, .. } | Op::Move { items, .. } | Op::Delete { items, .. } => items.iter().map(|i| (i.clone(), String::new())).collect(),
        Op::Rename { item, .. } => vec![(item.clone(), String::new())],
        _ => Vec::new(),
    };
    Some(Entry { time: now(), kind, title, items, folder, error: error.map(str::to_string) })
}

/// Describes an undo of `label` ("copy of 3 items") from what was reverted, as (was, now is).
pub fn describe_undo(label: &str, done: Vec<(String, String)>, error: Option<String>) -> Entry {
    let title = match (done.is_empty(), error.is_some()) {
        (true, _) => format!("Couldn't undo {label}"),
        (false, true) => format!("Partly undid {label}"),
        (false, false) => format!("Undid {label}"),
    };
    let folder = done.iter().find_map(|(was, now)| parent_path(if now.is_empty() { was } else { now }));
    Entry { time: now(), kind: Kind::Undo, title, items: done, folder, error }
}

// ---------- storage ----------

thread_local! {
    static LOG: RefCell<Option<Vec<Entry>>> = const { RefCell::new(None) };
}

fn log_path() -> Option<PathBuf> {
    crate::settings::settings_path().map(|p| p.with_file_name("log.json"))
}

fn load() -> Vec<Entry> {
    log_path().and_then(|p| std::fs::read_to_string(p).ok()).and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default()
}

fn save(log: &[Entry]) {
    let Some(path) = log_path() else { return };
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let tmp = path.with_extension("json.tmp");
    if serde_json::to_string(log).ok().is_some_and(|s| std::fs::write(&tmp, s).is_ok()) {
        let _ = std::fs::rename(&tmp, &path);
    }
}

fn with_log<R>(f: impl FnOnce(&mut Vec<Entry>) -> R) -> R {
    LOG.with(|l| f(l.borrow_mut().get_or_insert_with(load)))
}

pub fn push(e: Entry) {
    with_log(|l| {
        l.push(e);
        if l.len() > CAP {
            l.drain(..l.len() - CAP);
        }
        save(l);
    });
}

/// All entries, newest first.
pub fn entries() -> Vec<Entry> {
    with_log(|l| l.iter().rev().cloned().collect())
}

pub fn clear() {
    with_log(|l| {
        l.clear();
        save(l);
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(v: &[&str]) -> Vec<String> {
        v.iter().map(|x| x.to_string()).collect()
    }

    fn pairs(v: &[(&str, &str)]) -> Vec<(String, String)> {
        v.iter().map(|(a, b)| (a.to_string(), b.to_string())).collect()
    }

    #[test]
    fn copy_titles_count_what_was_done() {
        let op = Op::Copy { items: s(&[r"C:\a\x.txt", r"C:\a\y.txt", r"C:\a\z.txt"]), dest: r"D:\Docs".into() };
        let all = pairs(&[(r"C:\a\x.txt", r"D:\Docs\x.txt"), (r"C:\a\y.txt", r"D:\Docs\y.txt"), (r"C:\a\z.txt", r"D:\Docs\z.txt")]);
        let e = describe(&op, &all, None).unwrap();
        assert_eq!(e.title, "Copied 3 items to Docs");
        assert_eq!(e.kind, Kind::Copy);
        assert_eq!(e.detail(), "C:\\a  \u{2192}  D:\\Docs");
        let e = describe(&op, &all[..2], Some("Disk full")).unwrap();
        assert_eq!(e.title, "Copied 2 of 3 items to Docs");
        assert_eq!(e.error.as_deref(), Some("Disk full"));
        let one = Op::Move { items: s(&[r"C:\a\x.txt"]), dest: r"D:\".into() };
        assert_eq!(describe(&one, &all[..1], None).unwrap().title, r"Moved x.txt to D:\");
    }

    #[test]
    fn nothing_done_is_a_failure_or_nothing() {
        let op = Op::Delete { items: s(&[r"C:\a\x.txt"]), recycle: true };
        assert_eq!(describe(&op, &[], None), None);
        let e = describe(&op, &[], Some("Access is denied.")).unwrap();
        assert_eq!(e.title, "Couldn't delete x.txt");
        assert_eq!(e.items, pairs(&[(r"C:\a\x.txt", "")]));
        assert_eq!(e.folder.as_deref(), Some(r"C:\a"));
    }

    #[test]
    fn delete_rename_and_new_titles() {
        let done = pairs(&[(r"C:\a\x.txt", "")]);
        let recycle = Op::Delete { items: s(&[r"C:\a\x.txt"]), recycle: true };
        assert_eq!(describe(&recycle, &done, None).unwrap().title, "Moved x.txt to the Recycle Bin");
        let nuke = Op::Delete { items: s(&[r"C:\a\x.txt"]), recycle: false };
        let e = describe(&nuke, &done, None).unwrap();
        assert_eq!((e.kind, e.title.as_str()), (Kind::Delete, "Permanently deleted x.txt"));
        let rename = Op::Rename { item: r"C:\a\x.txt".into(), name: "y.txt".into() };
        let e = describe(&rename, &pairs(&[(r"C:\a\x.txt", r"C:\a\y.txt")]), None).unwrap();
        assert_eq!(e.title, "Renamed x.txt to y.txt");
        assert_eq!(e.detail(), r"C:\a");
        let new = Op::NewFolder { parent: r"C:\a".into(), name: "New folder".into() };
        let e = describe(&new, &pairs(&[("", r"C:\a\New folder (2)")]), None).unwrap();
        assert_eq!(e.title, "Created folder New folder (2) in a");
    }

    #[test]
    fn undo_titles() {
        let e = describe_undo("copy of 2 items", pairs(&[(r"D:\x.txt", "")]), Some("In use".into()));
        assert_eq!(e.title, "Partly undid copy of 2 items");
        assert_eq!(e.folder.as_deref(), Some(r"D:\"));
        assert_eq!(describe_undo("rename of 'x'", Vec::new(), Some("Gone".into())).title, "Couldn't undo rename of 'x'");
    }
}
