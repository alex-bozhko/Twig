//! File operations (copy/move/delete/rename/new) on a dedicated STA worker via IFileOperation,
//! with undo records captured from the progress sink.

use crate::shell::{item_path, restore_from_recycle, shell_item};
use crate::util::{file_name, parent_path, path_key};
use crate::win::{isize_to_hwnd, main_hwnd, hwnd_to_isize, post_main, WM_FILEOP};
use std::collections::HashSet;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::{channel, Sender};
use std::sync::{Arc, Mutex, OnceLock};
use windows::core::{implement, HRESULT, PCWSTR};
use windows::Win32::Foundation::*;
use windows::Win32::Storage::FileSystem::{FILE_ATTRIBUTE_DIRECTORY, FILE_ATTRIBUTE_NORMAL};
use windows::Win32::System::Com::*;
use windows::Win32::UI::Shell::*;

#[derive(Clone, Debug)]
pub enum Op {
    Copy { items: Vec<String>, dest: String },
    Move { items: Vec<String>, dest: String },
    Delete { items: Vec<String>, recycle: bool },
    Rename { item: String, name: String },
    NewFolder { parent: String, name: String },
    NewFile { parent: String, name: String },
    Undo(UndoEntry),
}

#[derive(Clone, Debug, PartialEq)]
pub enum Rec {
    Moved { from: String, to: String },
    Created { path: String },
    Recycled { original: String, bin: Option<String> },
}

#[derive(Clone, Debug)]
pub struct UndoEntry {
    pub label: String,
    pub recs: Vec<Rec>,
}

pub struct OpResult {
    /// Opaque caller tag (e.g. tab id) for selecting results.
    pub tag: u64,
    pub refresh: Vec<String>,
    pub select: Vec<String>,
    pub error: Option<String>,
    /// Record to push on the undo stack.
    pub undo: Option<UndoEntry>,
    /// Set when this was an undo; `undo` then holds an entry to push back if nothing was undone.
    pub was_undo: bool,
    pub message: Option<String>,
}

fn results() -> &'static Mutex<Vec<OpResult>> {
    static R: OnceLock<Mutex<Vec<OpResult>>> = OnceLock::new();
    R.get_or_init(|| Mutex::new(Vec::new()))
}

pub fn take_results() -> Vec<OpResult> {
    std::mem::take(&mut *results().lock().unwrap())
}

static PENDING: AtomicUsize = AtomicUsize::new(0);

fn sender() -> &'static Mutex<Sender<(u64, Op)>> {
    static S: OnceLock<Mutex<Sender<(u64, Op)>>> = OnceLock::new();
    S.get_or_init(|| {
        let (tx, rx) = channel::<(u64, Op)>();
        std::thread::spawn(move || {
            unsafe {
                let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
            }
            while let Ok((tag, op)) = rx.recv() {
                let mut r = run(op);
                r.tag = tag;
                results().lock().unwrap().push(r);
                PENDING.fetch_sub(1, Ordering::SeqCst);
                post_main(WM_FILEOP);
            }
        });
        Mutex::new(tx)
    })
}

pub fn submit(tag: u64, op: Op) {
    PENDING.fetch_add(1, Ordering::SeqCst);
    if sender().lock().unwrap().send((tag, op)).is_err() {
        PENDING.fetch_sub(1, Ordering::SeqCst);
    }
}

// ---------- progress sink ----------

#[derive(Default)]
struct SinkState {
    /// Path keys of the top-level items the caller asked for; child callbacks are ignored.
    requested: HashSet<String>,
    recs: Vec<Rec>,
    created: Vec<String>,
}

#[implement(IFileOperationProgressSink)]
struct Sink(Arc<Mutex<SinkState>>);

impl Sink {
    fn requested(&self, item: Option<&IShellItem>) -> Option<String> {
        let p = item_path(item?)?;
        self.0.lock().unwrap().requested.contains(&path_key(&p)).then_some(p)
    }
    fn record(&self, rec: Rec, created: Option<String>) {
        let mut s = self.0.lock().unwrap();
        s.recs.push(rec);
        if let Some(c) = created {
            s.created.push(c);
        }
    }
}

impl IFileOperationProgressSink_Impl for Sink_Impl {
    fn StartOperations(&self) -> windows::core::Result<()> {
        Ok(())
    }
    fn FinishOperations(&self, _hr: HRESULT) -> windows::core::Result<()> {
        Ok(())
    }
    fn PreRenameItem(&self, _f: u32, _i: Option<&IShellItem>, _n: &PCWSTR) -> windows::core::Result<()> {
        Ok(())
    }
    fn PostRenameItem(&self, _f: u32, item: Option<&IShellItem>, _n: &PCWSTR, hr: HRESULT, newly: Option<&IShellItem>) -> windows::core::Result<()> {
        if hr.is_ok() {
            if let (Some(from), Some(to)) = (self.requested(item), newly.and_then(item_path)) {
                self.record(Rec::Moved { from, to: to.clone() }, Some(to));
            }
        }
        Ok(())
    }
    fn PreMoveItem(&self, _f: u32, _i: Option<&IShellItem>, _d: Option<&IShellItem>, _n: &PCWSTR) -> windows::core::Result<()> {
        Ok(())
    }
    fn PostMoveItem(&self, _f: u32, item: Option<&IShellItem>, _d: Option<&IShellItem>, _n: &PCWSTR, hr: HRESULT, newly: Option<&IShellItem>) -> windows::core::Result<()> {
        if hr.is_ok() {
            if let (Some(from), Some(to)) = (self.requested(item), newly.and_then(item_path)) {
                self.record(Rec::Moved { from, to: to.clone() }, Some(to));
            }
        }
        Ok(())
    }
    fn PreCopyItem(&self, _f: u32, _i: Option<&IShellItem>, _d: Option<&IShellItem>, _n: &PCWSTR) -> windows::core::Result<()> {
        Ok(())
    }
    fn PostCopyItem(&self, _f: u32, item: Option<&IShellItem>, _d: Option<&IShellItem>, _n: &PCWSTR, hr: HRESULT, newly: Option<&IShellItem>) -> windows::core::Result<()> {
        if hr.is_ok() && self.requested(item).is_some() {
            if let Some(to) = newly.and_then(item_path) {
                self.record(Rec::Created { path: to.clone() }, Some(to));
            }
        }
        Ok(())
    }
    fn PreDeleteItem(&self, _f: u32, _i: Option<&IShellItem>) -> windows::core::Result<()> {
        Ok(())
    }
    fn PostDeleteItem(&self, f: u32, item: Option<&IShellItem>, hr: HRESULT, newly: Option<&IShellItem>) -> windows::core::Result<()> {
        const TSF_DELETE_RECYCLE_IF_POSSIBLE: u32 = 0x80;
        if hr.is_ok() && f & TSF_DELETE_RECYCLE_IF_POSSIBLE != 0 {
            if let Some(original) = self.requested(item) {
                self.record(Rec::Recycled { original, bin: newly.and_then(item_path) }, None);
            }
        }
        Ok(())
    }
    fn PreNewItem(&self, _f: u32, _d: Option<&IShellItem>, _n: &PCWSTR) -> windows::core::Result<()> {
        Ok(())
    }
    fn PostNewItem(&self, _f: u32, _d: Option<&IShellItem>, _n: &PCWSTR, _t: &PCWSTR, _a: u32, hr: HRESULT, newly: Option<&IShellItem>) -> windows::core::Result<()> {
        if hr.is_ok() {
            if let Some(p) = newly.and_then(item_path) {
                self.record(Rec::Created { path: p.clone() }, Some(p));
            }
        }
        Ok(())
    }
    fn UpdateProgress(&self, _t: u32, _s: u32) -> windows::core::Result<()> {
        Ok(())
    }
    fn ResetTimer(&self) -> windows::core::Result<()> {
        Ok(())
    }
    fn PauseTimer(&self) -> windows::core::Result<()> {
        Ok(())
    }
    fn ResumeTimer(&self) -> windows::core::Result<()> {
        Ok(())
    }
}

// ---------- execution ----------

fn is_cancel(e: &windows::core::Error) -> bool {
    e.code() == COPYENGINE_E_USER_CANCELLED || e.code() == ERROR_CANCELLED.to_hresult()
}

struct Batch {
    op: IFileOperation,
    state: Arc<Mutex<SinkState>>,
    refresh: Vec<String>,
}

impl Batch {
    fn new(flags: FILEOPERATION_FLAGS) -> windows::core::Result<Batch> {
        unsafe {
            let op: IFileOperation = CoCreateInstance(&FileOperation, None, CLSCTX_ALL)?;
            op.SetOperationFlags(flags)?;
            let owner = isize_to_hwnd(hwnd_to_isize(main_hwnd()));
            if !owner.is_invalid() {
                let _ = op.SetOwnerWindow(owner);
            }
            let state = Arc::new(Mutex::new(SinkState::default()));
            let sink: IFileOperationProgressSink = Sink(state.clone()).into();
            op.Advise(&sink)?;
            Ok(Batch { op, state, refresh: Vec::new() })
        }
    }

    fn request(&mut self, path: &str) {
        self.state.lock().unwrap().requested.insert(path_key(path));
        if let Some(p) = parent_path(path) {
            self.touch(&p);
        }
    }

    fn touch(&mut self, dir: &str) {
        if !self.refresh.iter().any(|d| d.eq_ignore_ascii_case(dir)) {
            self.refresh.push(dir.to_string());
        }
    }

    /// Runs queued operations. Returns (records, created paths, dirs, error, aborted).
    /// User cancellation isn't reported as an error.
    fn perform(self) -> (Vec<Rec>, Vec<String>, Vec<String>, Option<String>, bool) {
        let res = unsafe { self.op.PerformOperations() };
        let mut aborted = unsafe { self.op.GetAnyOperationsAborted() }.map(|b| b.as_bool()).unwrap_or(false);
        let err = match res {
            Err(e) if is_cancel(&e) => {
                aborted = true;
                None
            }
            Err(e) => Some(e.message()),
            _ => None,
        };
        let s = std::mem::take(&mut *self.state.lock().unwrap());
        (s.recs, s.created, self.refresh, err, aborted)
    }
}

fn item_or_err(path: &str) -> Result<IShellItem, String> {
    shell_item(path).ok_or_else(|| format!("Cannot access '{}'", file_name(path)))
}

fn plural(n: usize) -> String {
    if n == 1 {
        "1 item".into()
    } else {
        format!("{n} items")
    }
}

fn empty_result() -> OpResult {
    OpResult { tag: 0, refresh: Vec::new(), select: Vec::new(), error: None, undo: None, was_undo: false, message: None }
}

fn run(op: Op) -> OpResult {
    match run_inner(op) {
        Ok(r) => r,
        Err(e) => OpResult { error: Some(e), ..empty_result() },
    }
}

fn run_inner(op: Op) -> Result<OpResult, String> {
    let base = FOF_ALLOWUNDO | FOF_NOCONFIRMMKDIR;
    let label;
    let batch = match &op {
        Op::Undo(entry) => return Ok(run_undo(entry.clone())),
        Op::Copy { items, dest } | Op::Move { items, dest } => {
            let is_move = matches!(op, Op::Move { .. });
            let items: Vec<&String> = items
                .iter()
                .filter(|i| !(is_move && parent_path(i).is_some_and(|p| p.eq_ignore_ascii_case(dest))))
                .collect();
            if let Some(bad) = items.iter().find(|i| crate::util::is_same_or_inside(dest, i)) {
                return Err(format!("Cannot {} '{}' into itself", if is_move { "move" } else { "copy" }, file_name(bad)));
            }
            if items.is_empty() {
                return Ok(empty_result());
            }
            let same_folder = items.iter().any(|i| parent_path(i).is_some_and(|p| p.eq_ignore_ascii_case(dest)));
            let mut flags = base;
            if same_folder {
                flags |= FOF_RENAMEONCOLLISION;
            }
            let mut b = Batch::new(flags).map_err(|e| e.message())?;
            let df = item_or_err(dest)?;
            b.touch(dest);
            for i in items.iter() {
                let it = item_or_err(i)?;
                b.request(i);
                unsafe {
                    if is_move {
                        b.op.MoveItem(&it, &df, PCWSTR::null(), None)
                    } else {
                        b.op.CopyItem(&it, &df, PCWSTR::null(), None)
                    }
                }
                .map_err(|e| e.message())?;
            }
            label = format!("{} of {}", if is_move { "move" } else { "copy" }, plural(items.len()));
            b
        }
        Op::Delete { items, recycle } => {
            let flags = if *recycle { base | FOF_NOCONFIRMATION | FOF_WANTNUKEWARNING | FOFX_RECYCLEONDELETE } else { FOF_NOCONFIRMMKDIR };
            let mut b = Batch::new(flags).map_err(|e| e.message())?;
            for i in items {
                let it = item_or_err(i)?;
                b.request(i);
                unsafe { b.op.DeleteItem(&it, None) }.map_err(|e| e.message())?;
            }
            label = format!("delete of {}", plural(items.len()));
            b
        }
        Op::Rename { item, name } => {
            let mut b = Batch::new(base).map_err(|e| e.message())?;
            let it = item_or_err(item)?;
            b.request(item);
            let w = crate::util::wide(name);
            unsafe { b.op.RenameItem(&it, PCWSTR(w.as_ptr()), None) }.map_err(|e| e.message())?;
            label = format!("rename of '{}'", file_name(item));
            b
        }
        Op::NewFolder { parent, name } | Op::NewFile { parent, name } => {
            let dir = matches!(op, Op::NewFolder { .. });
            let mut b = Batch::new(base).map_err(|e| e.message())?;
            let pf = item_or_err(parent)?;
            b.touch(parent);
            let w = crate::util::wide(name);
            let attrs = if dir { FILE_ATTRIBUTE_DIRECTORY.0 } else { FILE_ATTRIBUTE_NORMAL.0 };
            unsafe { b.op.NewItem(&pf, attrs, PCWSTR(w.as_ptr()), PCWSTR::null(), None) }.map_err(|e| e.message())?;
            label = format!("new {} '{}'", if dir { "folder" } else { "file" }, name);
            b
        }
    };
    let (recs, created, mut refresh, error, _) = batch.perform();
    for c in &created {
        if let Some(p) = parent_path(c) {
            if !refresh.iter().any(|d| d.eq_ignore_ascii_case(&p)) {
                refresh.push(p);
            }
        }
    }
    let undo = (!recs.is_empty()).then(|| UndoEntry { label, recs });
    Ok(OpResult { refresh, select: created, error, undo, ..empty_result() })
}

/// Reverts records in reverse order. If nothing could be undone, the entry is handed back.
fn run_undo(entry: UndoEntry) -> OpResult {
    let mut refresh: Vec<String> = Vec::new();
    let mut select = Vec::new();
    let mut errors = Vec::new();
    let mut failed = Vec::new();
    let mut touch = |p: Option<String>| {
        if let Some(p) = p {
            if !refresh.iter().any(|d| d.eq_ignore_ascii_case(&p)) {
                refresh.push(p);
            }
        }
    };
    for rec in entry.recs.iter().rev() {
        let r: Result<(), String> = match rec {
            Rec::Moved { from, to } => {
                touch(parent_path(from));
                touch(parent_path(to));
                undo_move(from, to).map(|_| select.push(from.clone()))
            }
            Rec::Created { path } => {
                touch(parent_path(path));
                undo_create(path)
            }
            Rec::Recycled { original, bin } => {
                touch(parent_path(original));
                restore_from_recycle(original, bin.as_deref()).map(|_| select.push(original.clone()))
            }
        };
        if let Err(e) = r {
            errors.push(e);
            failed.push(rec.clone());
        }
    }
    let nothing = failed.len() == entry.recs.len();
    OpResult {
        refresh,
        select,
        error: errors.first().cloned(),
        undo: nothing.then(|| entry.clone()),
        was_undo: true,
        message: (!nothing).then(|| format!("Undid {}", entry.label)),
        ..empty_result()
    }
}

fn undo_move(from: &str, to: &str) -> Result<(), String> {
    let b = Batch::new(FOF_ALLOWUNDO | FOF_NOCONFIRMMKDIR).map_err(|e| e.message())?;
    let it = item_or_err(to)?;
    let name = crate::util::wide(&file_name(from));
    let src_parent = parent_path(to).unwrap_or_default();
    let dst_parent = parent_path(from).ok_or("Invalid path")?;
    unsafe {
        if src_parent.eq_ignore_ascii_case(&dst_parent) {
            b.op.RenameItem(&it, PCWSTR(name.as_ptr()), None)
        } else {
            let _ = std::fs::create_dir_all(&dst_parent);
            let df = item_or_err(&dst_parent)?;
            b.op.MoveItem(&it, &df, PCWSTR(name.as_ptr()), None)
        }
    }
    .map_err(|e| e.message())?;
    finish(b)
}

fn undo_create(path: &str) -> Result<(), String> {
    let b = Batch::new(FOF_ALLOWUNDO | FOF_NOCONFIRMATION | FOFX_RECYCLEONDELETE).map_err(|e| e.message())?;
    let it = item_or_err(path)?;
    unsafe { b.op.DeleteItem(&it, None) }.map_err(|e| e.message())?;
    finish(b)
}

fn finish(b: Batch) -> Result<(), String> {
    match b.perform() {
        (_, _, _, Some(e), _) => Err(e),
        (_, _, _, None, true) => Err("Undo was cancelled".into()),
        _ => Ok(()),
    }
}
