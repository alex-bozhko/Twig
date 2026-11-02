//! Shell integration: PIDLs, context menus, data objects, clipboard, launching, Recycle Bin restore.

use crate::util::{file_name, from_wide, join_path, parent_path, wide};
use std::cell::RefCell;
use std::ffi::c_void;
use windows::core::{Interface, HRESULT, PCSTR, PCWSTR, PSTR, PWSTR};
use windows::Win32::Foundation::*;
use windows::Win32::Storage::FileSystem::*;
use windows::Win32::System::Com::*;
use windows::Win32::System::DataExchange::*;
use windows::Win32::System::Memory::*;
use windows::Win32::System::Ole::*;
use windows::Win32::UI::Shell::Common::ITEMIDLIST;
use windows::Win32::UI::Shell::*;
use windows::Win32::UI::Shell::{SetWindowSubclass, RemoveWindowSubclass, DefSubclassProc};
use windows::Win32::UI::WindowsAndMessaging::*;

pub struct Pidl(pub *mut ITEMIDLIST);

impl Drop for Pidl {
    fn drop(&mut self) {
        unsafe { ILFree(Some(self.0)) }
    }
}

pub fn pidl(path: &str) -> Option<Pidl> {
    let w = wide(path);
    let mut p = std::ptr::null_mut();
    unsafe { SHParseDisplayName(PCWSTR(w.as_ptr()), None, &mut p, 0, None) }.ok()?;
    if p.is_null() {
        None
    } else {
        Some(Pidl(p))
    }
}

pub fn shell_item(path: &str) -> Option<IShellItem> {
    let w = wide(path);
    unsafe { SHCreateItemFromParsingName(PCWSTR(w.as_ptr()), None) }.ok()
}

pub fn item_path(item: &IShellItem) -> Option<String> {
    unsafe {
        let p: PWSTR = item.GetDisplayName(SIGDN_FILESYSPATH).ok()?;
        let s = p.to_string().ok();
        CoTaskMemFree(Some(p.0 as *const c_void));
        s
    }
}

/// Binds the parent folder of `paths` (all must share one parent; others are dropped) and returns
/// a UI object (IContextMenu / IDataObject) for them.
fn ui_object<T: Interface>(hwnd: HWND, paths: &[String]) -> Option<T> {
    let pidls: Vec<Pidl> = paths.iter().filter_map(|p| pidl(p)).collect();
    if pidls.is_empty() {
        return None;
    }
    let same_parent = {
        let first = paths.first().and_then(|p| parent_path(p)).unwrap_or_default();
        paths.iter().all(|p| parent_path(p).unwrap_or_default().eq_ignore_ascii_case(&first))
    };
    unsafe {
        if same_parent {
            let mut last = std::ptr::null_mut();
            let folder: IShellFolder = SHBindToParent(pidls[0].0, Some(&mut last)).ok()?;
            let kids: Vec<*const ITEMIDLIST> = pidls.iter().map(|p| ILFindLastID(p.0) as *const _).collect();
            folder.GetUIObjectOf(hwnd, &kids, None).ok()
        } else {
            let desk = SHGetDesktopFolder().ok()?;
            let abs: Vec<*const ITEMIDLIST> = pidls.iter().map(|p| p.0 as *const _).collect();
            desk.GetUIObjectOf(hwnd, &abs, None).ok()
        }
    }
}

// ---------- Context menu ----------

pub const ID_CUSTOM_FIRST: u32 = 0x7E00;
const ID_SHELL_FIRST: u32 = 1;
const ID_SHELL_LAST: u32 = 0x7DFF;

pub enum MenuItem {
    Item(u32, String),
    Sep,
    Sub(String, Vec<(u32, String)>),
}

pub enum MenuResult {
    None,
    Custom(u32),
    /// A shell verb the app implements itself (rename/delete/cut/copy/paste) for undo & ghosting.
    Verb(String),
}

thread_local! {
    static ACTIVE_MENU: RefCell<Option<(Option<IContextMenu2>, Option<IContextMenu3>)>> = const { RefCell::new(None) };
}

unsafe extern "system" fn menu_subclass(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM, _id: usize, _data: usize) -> LRESULT {
    if matches!(msg, WM_INITMENUPOPUP | WM_DRAWITEM | WM_MEASUREITEM | WM_MENUCHAR) {
        let handled = ACTIVE_MENU.with(|m| {
            let m = m.borrow();
            let (cm2, cm3) = m.as_ref()?;
            if let Some(cm3) = cm3 {
                let mut res = LRESULT(0);
                if cm3.HandleMenuMsg2(msg, wp, lp, Some(&mut res)).is_ok() {
                    return Some(res);
                }
            } else if let Some(cm2) = cm2 {
                if cm2.HandleMenuMsg(msg, wp, lp).is_ok() {
                    return Some(LRESULT(if msg == WM_INITMENUPOPUP { 0 } else { 1 }));
                }
            }
            None
        });
        if let Some(r) = handled {
            return r;
        }
    }
    DefSubclassProc(hwnd, msg, wp, lp)
}

fn verb_of(cm: &IContextMenu, id: u32) -> String {
    let mut buf = [0u16; 128];
    unsafe {
        if cm
            .GetCommandString((id - ID_SHELL_FIRST) as usize, GCS_VERBW, None, PSTR(buf.as_mut_ptr() as *mut u8), buf.len() as u32)
            .is_ok()
        {
            return from_wide(&buf).to_ascii_lowercase();
        }
    }
    String::new()
}

/// Shows the shell context menu for `paths` with `extra` items prepended. Modal: the caller must
/// not hold app state borrowed.
pub fn show_context_menu(hwnd: HWND, paths: &[String], pt: POINT, extended: bool, extra: &[MenuItem]) -> MenuResult {
    unsafe {
        let Ok(menu) = CreatePopupMenu() else { return MenuResult::None };
        let cm: Option<IContextMenu> = if paths.is_empty() { None } else { ui_object(hwnd, paths) };
        let mut pos = 0u32;
        for item in extra {
            match item {
                MenuItem::Item(id, text) => {
                    let w = wide(text);
                    let _ = InsertMenuW(menu, pos, MF_BYPOSITION | MF_STRING, *id as usize, PCWSTR(w.as_ptr()));
                }
                MenuItem::Sep => {
                    let _ = InsertMenuW(menu, pos, MF_BYPOSITION | MF_SEPARATOR, 0, PCWSTR::null());
                }
                MenuItem::Sub(text, items) => {
                    let Ok(sub) = CreatePopupMenu() else { continue };
                    for (id, t) in items {
                        crate::win::append_menu(sub, *id as usize, t, true);
                    }
                    let w = wide(text);
                    let _ = InsertMenuW(menu, pos, MF_BYPOSITION | MF_POPUP | MF_STRING, sub.0 as usize, PCWSTR(w.as_ptr()));
                }
            }
            pos += 1;
        }
        if let Some(cm) = &cm {
            let mut flags = CMF_NORMAL | CMF_CANRENAME;
            if extended {
                flags |= CMF_EXTENDEDVERBS;
            }
            let _ = cm.QueryContextMenu(menu, pos, ID_SHELL_FIRST, ID_SHELL_LAST, flags);
        }
        // Drop trailing/duplicate separators left by an empty shell menu.
        let n = GetMenuItemCount(menu);
        if n > 0 {
            let mut info = MENUITEMINFOW { cbSize: std::mem::size_of::<MENUITEMINFOW>() as u32, fMask: MIIM_FTYPE, ..Default::default() };
            if GetMenuItemInfoW(menu, (n - 1) as u32, true, &mut info).is_ok() && info.fType.contains(MFT_SEPARATOR) {
                let _ = DeleteMenu(menu, (n - 1) as u32, MF_BYPOSITION);
            }
        }

        let cm2 = cm.as_ref().and_then(|c| c.cast::<IContextMenu2>().ok());
        let cm3 = cm.as_ref().and_then(|c| c.cast::<IContextMenu3>().ok());
        ACTIVE_MENU.with(|m| *m.borrow_mut() = Some((cm2, cm3)));
        let subclassed = SetWindowSubclass(hwnd, Some(menu_subclass), 0x5C4E, 0).as_bool();
        let cmd = crate::win::track_menu(menu, hwnd, pt, TPM_RIGHTBUTTON);
        if subclassed {
            let _ = RemoveWindowSubclass(hwnd, Some(menu_subclass), 0x5C4E);
        }
        ACTIVE_MENU.with(|m| *m.borrow_mut() = None);

        let result = if cmd == 0 {
            MenuResult::None
        } else if cmd >= ID_CUSTOM_FIRST {
            MenuResult::Custom(cmd)
        } else if let Some(cm) = &cm {
            let verb = verb_of(cm, cmd);
            if matches!(verb.as_str(), "rename" | "delete" | "cut" | "copy" | "paste") {
                MenuResult::Verb(verb)
            } else {
                let dir = paths.first().and_then(|p| parent_path(p)).map(|d| wide(&d)).unwrap_or_else(|| vec![0]);
                let mut mask = CMIC_MASK_UNICODE | CMIC_MASK_PTINVOKE;
                if crate::win::shift() {
                    mask |= CMIC_MASK_SHIFT_DOWN;
                }
                if crate::win::ctrl() {
                    mask |= CMIC_MASK_CONTROL_DOWN;
                }
                let info = CMINVOKECOMMANDINFOEX {
                    cbSize: std::mem::size_of::<CMINVOKECOMMANDINFOEX>() as u32,
                    fMask: mask,
                    hwnd,
                    lpVerb: PCSTR((cmd - ID_SHELL_FIRST) as usize as *const u8),
                    lpVerbW: PCWSTR((cmd - ID_SHELL_FIRST) as usize as *const u16),
                    lpDirectoryW: PCWSTR(dir.as_ptr()),
                    nShow: SW_SHOWNORMAL.0,
                    ptInvoke: pt,
                    ..Default::default()
                };
                let _ = cm.InvokeCommand(&info as *const _ as *const CMINVOKECOMMANDINFO);
                MenuResult::None
            }
        } else {
            MenuResult::None
        };
        let _ = DestroyMenu(menu);
        result
    }
}

const CMIC_MASK_UNICODE: u32 = 0x4000;

// ---------- Data objects & clipboard ----------

pub fn data_object(hwnd: HWND, paths: &[String]) -> Option<IDataObject> {
    ui_object(hwnd, paths)
}

fn cf(name: &str) -> u16 {
    let w = wide(name);
    unsafe { RegisterClipboardFormatW(PCWSTR(w.as_ptr())) as u16 }
}

pub fn cf_preferred_effect() -> u16 {
    cf("Preferred DropEffect")
}

fn fmt(cf: u16) -> FORMATETC {
    FORMATETC { cfFormat: cf, ptd: std::ptr::null_mut(), dwAspect: DVASPECT_CONTENT.0, lindex: -1, tymed: TYMED_HGLOBAL.0 as u32 }
}

pub fn set_preferred_effect(obj: &IDataObject, effect: DROPEFFECT) {
    set_effect_format(obj, cf_preferred_effect(), effect);
}

/// Tells a drag source that the target performed an optimized move (so it must not delete).
pub fn report_optimized_move(obj: &IDataObject) {
    set_effect_format(obj, cf("Performed DropEffect"), DROPEFFECT_NONE);
    set_effect_format(obj, cf("Logical Performed DropEffect"), DROPEFFECT_MOVE);
}

fn set_effect_format(obj: &IDataObject, format: u16, effect: DROPEFFECT) {
    unsafe {
        let Ok(h) = GlobalAlloc(GMEM_MOVEABLE, 4) else { return };
        let p = GlobalLock(h) as *mut u32;
        if p.is_null() {
            return;
        }
        *p = effect.0;
        let _ = GlobalUnlock(h);
        let f = fmt(format);
        let med = STGMEDIUM { tymed: TYMED_HGLOBAL.0 as u32, u: STGMEDIUM_0 { hGlobal: h }, pUnkForRelease: std::mem::ManuallyDrop::new(None) };
        let _ = obj.SetData(&f, &med, true);
    }
}

pub fn get_preferred_effect(obj: &IDataObject) -> Option<DROPEFFECT> {
    unsafe {
        let f = fmt(cf_preferred_effect());
        let mut med = obj.GetData(&f).ok()?;
        let p = GlobalLock(med.u.hGlobal) as *const u32;
        let v = if p.is_null() { None } else { Some(DROPEFFECT(*p)) };
        let _ = GlobalUnlock(med.u.hGlobal);
        ReleaseStgMedium(&mut med);
        v
    }
}

pub fn files_of(obj: &IDataObject) -> Vec<String> {
    let mut out = Vec::new();
    unsafe {
        let Ok(mut med) = obj.GetData(&fmt(CF_HDROP.0)) else { return out };
        let hdrop = HDROP(med.u.hGlobal.0);
        let n = DragQueryFileW(hdrop, u32::MAX, None);
        for i in 0..n {
            let len = DragQueryFileW(hdrop, i, None) as usize;
            let mut buf = vec![0u16; len + 1];
            DragQueryFileW(hdrop, i, Some(&mut buf));
            out.push(from_wide(&buf));
        }
        ReleaseStgMedium(&mut med);
    }
    out
}

/// Puts files on the clipboard (cut => move on paste). Returns the data object for ghosting checks.
pub fn clipboard_set_files(hwnd: HWND, paths: &[String], cut: bool) -> Option<IDataObject> {
    let obj = data_object(hwnd, paths)?;
    set_preferred_effect(&obj, if cut { DROPEFFECT_MOVE } else { DROPEFFECT_COPY | DROPEFFECT_LINK });
    unsafe { OleSetClipboard(&obj) }.ok()?;
    Some(obj)
}

#[link(name = "ole32")]
extern "system" {
    #[link_name = "OleIsCurrentClipboard"]
    fn ole_is_current_clipboard(obj: *mut c_void) -> HRESULT;
}

pub fn is_current_clipboard(obj: &IDataObject) -> bool {
    unsafe { ole_is_current_clipboard(obj.as_raw()) == S_OK }
}

/// Files on the clipboard and whether the source asked for a move (cut).
pub fn clipboard_files() -> Option<(Vec<String>, bool)> {
    let obj = unsafe { OleGetClipboard() }.ok()?;
    let files = files_of(&obj);
    if files.is_empty() {
        return None;
    }
    let mv = get_preferred_effect(&obj).map(|e| e.0 & DROPEFFECT_MOVE.0 != 0 && e.0 & DROPEFFECT_COPY.0 == 0).unwrap_or(false);
    Some((files, mv))
}

pub fn clipboard_has_files() -> bool {
    unsafe { IsClipboardFormatAvailable(CF_HDROP.0 as u32).is_ok() }
}

pub fn clipboard_set_text(hwnd: HWND, text: &str) {
    let w = wide(text);
    unsafe {
        if OpenClipboard(hwnd).is_err() {
            return;
        }
        let _ = EmptyClipboard();
        if let Ok(h) = GlobalAlloc(GMEM_MOVEABLE, w.len() * 2) {
            let p = GlobalLock(h) as *mut u16;
            if !p.is_null() {
                std::ptr::copy_nonoverlapping(w.as_ptr(), p, w.len());
                let _ = GlobalUnlock(h);
                if SetClipboardData(CF_UNICODETEXT.0 as u32, HANDLE(h.0)).is_err() {
                    let _ = GlobalFree(h);
                }
            }
        }
        let _ = CloseClipboard();
    }
}

// ---------- Launching ----------

fn shell_execute(hwnd: HWND, verb: Option<&str>, file: &str, params: Option<&str>, dir: Option<&str>) -> Result<(), String> {
    let v = verb.map(wide);
    let f = wide(file);
    let p = params.map(wide);
    let d = dir.map(wide);
    let mut info = SHELLEXECUTEINFOW {
        cbSize: std::mem::size_of::<SHELLEXECUTEINFOW>() as u32,
        fMask: SEE_MASK_INVOKEIDLIST | SEE_MASK_FLAG_LOG_USAGE,
        hwnd,
        lpVerb: v.as_ref().map(|v| PCWSTR(v.as_ptr())).unwrap_or(PCWSTR::null()),
        lpFile: PCWSTR(f.as_ptr()),
        lpParameters: p.as_ref().map(|v| PCWSTR(v.as_ptr())).unwrap_or(PCWSTR::null()),
        lpDirectory: d.as_ref().map(|v| PCWSTR(v.as_ptr())).unwrap_or(PCWSTR::null()),
        nShow: SW_SHOWNORMAL.0,
        ..Default::default()
    };
    unsafe { ShellExecuteExW(&mut info) }.map_err(|e| e.message())
}

pub fn open(hwnd: HWND, path: &str) -> Result<(), String> {
    let dir = parent_path(path);
    shell_execute(hwnd, None, path, None, dir.as_deref())
}

/// Opens Explorer at a folder, or at a file's folder with the file selected.
pub fn open_in_explorer(hwnd: HWND, path: &str, is_dir: bool) {
    if is_dir {
        let _ = shell_execute(hwnd, None, "explorer.exe", Some(&format!("\"{path}\"")), None);
    } else if let Some(p) = pidl(path) {
        unsafe {
            let _ = SHOpenFolderAndSelectItems(p.0, None, 0);
        }
    }
}

pub fn properties(hwnd: HWND, path: &str) {
    let w = wide(path);
    unsafe {
        let _ = SHObjectProperties(hwnd, SHOP_FILEPATH, PCWSTR(w.as_ptr()), PCWSTR::null());
    }
}

pub fn notify_dir_changed(dir: &str) {
    let w = wide(dir);
    unsafe { SHChangeNotify(SHCNE_UPDATEDIR, SHCNF_PATHW, Some(w.as_ptr() as *const c_void), None) }
}

// ---------- Recycle Bin restore ----------

fn exists(path: &str) -> bool {
    let w = wide(path);
    unsafe { GetFileAttributesW(PCWSTR(w.as_ptr())) != INVALID_FILE_ATTRIBUTES }
}

/// Parses a `$I` metadata file: returns (original path, deletion FILETIME).
pub fn parse_recycle_info(data: &[u8]) -> Option<(String, u64)> {
    if data.len() < 24 {
        return None;
    }
    let version = u64::from_le_bytes(data[0..8].try_into().ok()?);
    let deleted = u64::from_le_bytes(data[16..24].try_into().ok()?);
    let units: Vec<u16> = match version {
        1 => data.get(24..24 + 520)?.chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect(),
        2 => {
            let n = u32::from_le_bytes(data.get(24..28)?.try_into().ok()?) as usize;
            data.get(28..28 + n * 2)?.chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect()
        }
        _ => return None,
    };
    let path = from_wide(&units);
    if path.is_empty() {
        None
    } else {
        Some((path, deleted))
    }
}

fn volume_root(path: &str) -> Option<String> {
    let w = wide(path);
    let mut buf = [0u16; 512];
    unsafe { GetVolumePathNameW(PCWSTR(w.as_ptr()), &mut buf) }.ok()?;
    Some(from_wide(&buf))
}

/// Finds the most recently recycled `$R` item whose original location is `original`.
fn find_recycled(original: &str) -> Option<String> {
    let root = volume_root(original)?;
    let bin = join_path(&root, "$Recycle.Bin");
    let mut best: Option<(u64, String)> = None;
    for sid in std::fs::read_dir(&bin).ok()?.flatten() {
        let Ok(items) = std::fs::read_dir(sid.path()) else { continue };
        for it in items.flatten() {
            let name = it.file_name().to_string_lossy().to_string();
            if !name.starts_with("$I") {
                continue;
            }
            let Ok(data) = std::fs::read(it.path()) else { continue };
            let Some((orig, when)) = parse_recycle_info(&data) else { continue };
            if !orig.eq_ignore_ascii_case(original) {
                continue;
            }
            let r = join_path(&sid.path().to_string_lossy(), &format!("$R{}", &name[2..]));
            if exists(&r) && best.as_ref().map_or(true, |(t, _)| when > *t) {
                best = Some((when, r));
            }
        }
    }
    best.map(|(_, r)| r)
}

/// Restores a recycled item to `original`. `bin_item` is the `$R` path reported when it was
/// recycled (exact match); otherwise the bin is searched by original path. Locale independent.
pub fn restore_from_recycle(original: &str, bin_item: Option<&str>) -> Result<(), String> {
    let r = match bin_item.filter(|p| exists(p)) {
        Some(p) => p.to_string(),
        None => find_recycled(original).ok_or_else(|| format!("'{}' is no longer in the Recycle Bin", file_name(original)))?,
    };
    if exists(original) {
        return Err(format!("'{}' already exists", file_name(original)));
    }
    if let Some(parent) = parent_path(original) {
        let _ = std::fs::create_dir_all(&parent);
    }
    let (rw, ow) = (wide(&r), wide(original));
    unsafe { MoveFileExW(PCWSTR(rw.as_ptr()), PCWSTR(ow.as_ptr()), MOVEFILE_COPY_ALLOWED) }.map_err(|e| e.message())?;
    let rname = file_name(&r);
    if let (Some(dir), Some(suffix)) = (parent_path(&r), rname.strip_prefix("$R")) {
        let _ = std::fs::remove_file(join_path(&dir, &format!("$I{suffix}")));
        notify_dir_changed(&dir);
    }
    if let Some(parent) = parent_path(original) {
        notify_dir_changed(&parent);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v2(path: &str, when: u64) -> Vec<u8> {
        let mut d = Vec::new();
        d.extend_from_slice(&2u64.to_le_bytes());
        d.extend_from_slice(&123u64.to_le_bytes());
        d.extend_from_slice(&when.to_le_bytes());
        let w: Vec<u16> = path.encode_utf16().chain(Some(0)).collect();
        d.extend_from_slice(&(w.len() as u32).to_le_bytes());
        for u in w {
            d.extend_from_slice(&u.to_le_bytes());
        }
        d
    }

    #[test]
    fn parses_recycle_info_v2_and_v1() {
        assert_eq!(parse_recycle_info(&v2(r"C:\a\b.txt", 77)), Some((r"C:\a\b.txt".to_string(), 77)));
        let mut d = Vec::new();
        d.extend_from_slice(&1u64.to_le_bytes());
        d.extend_from_slice(&0u64.to_le_bytes());
        d.extend_from_slice(&5u64.to_le_bytes());
        let mut name = [0u16; 260];
        for (i, u) in r"D:\x".encode_utf16().enumerate() {
            name[i] = u;
        }
        for u in name {
            d.extend_from_slice(&u.to_le_bytes());
        }
        assert_eq!(parse_recycle_info(&d), Some((r"D:\x".to_string(), 5)));
        assert_eq!(parse_recycle_info(&[0; 10]), None);
        assert_eq!(parse_recycle_info(&v2("", 1)), None);
    }
}
