//! OLE drop targets: one per grid (drop files into folders) and one for the main window
//! (hovering a tab while dragging activates it).

use crate::app::{with_app, App, TIMER_HOVER_TAB};
use crate::fileops::Op;
use crate::layout::PaneId;
use crate::loader::TabId;
use crate::model::{Kind, NodeId};
use crate::shell;
use crate::util::*;
use crate::win::*;
use std::cell::{Cell, RefCell};
use std::time::Instant;
use windows::core::implement;
use windows::Win32::Foundation::*;
use windows::Win32::System::Com::*;
use windows::Win32::System::Ole::*;
use windows::Win32::System::SystemServices::*;
use windows::Win32::UI::Shell::*;
use windows::Win32::UI::WindowsAndMessaging::*;

const SCROLL_INTERVAL_MS: u128 = 60;

fn helper() -> Option<IDropTargetHelper> {
    unsafe { CoCreateInstance(&CLSID_DragDropHelper, None, CLSCTX_INPROC_SERVER).ok() }
}

/// Drive/share part of a path used to decide between move (same volume) and copy.
fn volume_of(path: &str) -> String {
    let k = path_key(path);
    if let Some(rest) = k.strip_prefix("\\\\") {
        let mut parts = rest.split('\\');
        let server = parts.next().unwrap_or("");
        let share = parts.next().unwrap_or("");
        return format!("\\\\{server}\\{share}");
    }
    k.chars().take(2).collect()
}

fn choose_effect(keys: MODIFIERKEYS_FLAGS, allowed: DROPEFFECT, same_volume: bool) -> DROPEFFECT {
    let ctrl = keys.0 & MK_CONTROL.0 != 0;
    let shift = keys.0 & MK_SHIFT.0 != 0;
    let wanted = if ctrl && !shift {
        DROPEFFECT_COPY
    } else if shift && !ctrl {
        DROPEFFECT_MOVE
    } else if same_volume {
        DROPEFFECT_MOVE
    } else {
        DROPEFFECT_COPY
    };
    if allowed.0 & wanted.0 != 0 {
        wanted
    } else if allowed.0 & DROPEFFECT_COPY.0 != 0 {
        DROPEFFECT_COPY
    } else if allowed.0 & DROPEFFECT_MOVE.0 != 0 {
        DROPEFFECT_MOVE
    } else {
        DROPEFFECT_NONE
    }
}

/// Drop destination under a client point: (tab, highlighted folder node, folder path).
fn target(app: &mut App, pane: PaneId, x: i32, y: i32) -> Option<(TabId, Option<NodeId>, String)> {
    let tid = app.panes.get(&pane)?.active_tab()?;
    let hit = crate::grid::node_at(app, pane, x, y);
    let t = app.tabs.get(&tid)?;
    let id = match hit {
        Some(id) => id,
        None => t.root_node()?,
    };
    let n = t.tree.get(id)?;
    match n.kind {
        Kind::Drive | Kind::Dir => Some((tid, Some(id), n.path.clone())),
        Kind::File | Kind::Zip => {
            let parent = parent_path(&n.path)?;
            let pid = n.parent.filter(|&p| t.tree.get(p).is_some_and(|pn| pn.kind.is_fs_dir()));
            Some((tid, pid, parent))
        }
        Kind::ZipDir | Kind::ZipFile => None,
    }
}

#[implement(IDropTarget)]
struct GridDrop {
    hwnd: isize,
    pane: PaneId,
    files: RefCell<Vec<String>>,
    helper: Option<IDropTargetHelper>,
    last_scroll: Cell<Option<Instant>>,
}

impl GridDrop {
    fn hwnd(&self) -> HWND {
        isize_to_hwnd(self.hwnd)
    }

    /// Computes the effect at `pt`, updates the highlight and auto-scrolls.
    fn evaluate(&self, keys: MODIFIERKEYS_FLAGS, pt: &POINTL, allowed: DROPEFFECT, scroll: bool) -> (DROPEFFECT, Option<(TabId, String)>) {
        let files = self.files.borrow();
        let hwnd = self.hwnd();
        let p = screen_to_client(hwnd, POINT { x: pt.x, y: pt.y });
        let pane = self.pane;
        let r = with_app(|app| {
            if scroll {
                let m = crate::grid::metrics(app);
                let top = crate::grid::scroll_area_top(app, pane);
                let bottom = crate::grid::scroll_area_bottom(app, pane);
                let dir = if p.y >= top && p.y < top + m.row / 2 {
                    -1
                } else if p.y > bottom - m.row / 2 && p.y < bottom {
                    1
                } else {
                    0
                };
                let due = self.last_scroll.get().map_or(true, |t| t.elapsed().as_millis() >= SCROLL_INTERVAL_MS);
                if dir != 0 && due {
                    self.last_scroll.set(Some(Instant::now()));
                    crate::grid::scroll_rows(app, pane, dir);
                }
            }
            let tgt = if files.is_empty() { None } else { target(app, pane, p.x, p.y) };
            let tgt = tgt.filter(|(_, _, dest)| {
                // No drops into one of the dragged items or into the folder they already live in.
                let dk = path_key(dest);
                !files.iter().any(|f| is_same_or_inside(dest, f))
                    && !files.iter().all(|f| parent_path(f).is_some_and(|pp| path_key(&pp) == dk))
            });
            let highlight = tgt.as_ref().and_then(|(_, n, _)| n.map(|n| (pane, n)));
            if app.grid_ui.drop != highlight {
                app.grid_ui.drop = highlight;
                invalidate(hwnd);
            }
            match tgt {
                Some((tid, _, dest)) => {
                    let same = files.iter().all(|f| volume_of(f) == volume_of(&dest));
                    (choose_effect(keys, allowed, same), Some((tid, dest)))
                }
                None => (DROPEFFECT_NONE, None),
            }
        });
        r.unwrap_or((DROPEFFECT_NONE, None))
    }

    fn clear(&self) {
        let pane = self.pane;
        let hwnd = self.hwnd();
        with_app(|app| {
            if app.grid_ui.drop.is_some_and(|d| d.0 == pane) {
                app.grid_ui.drop = None;
                invalidate(hwnd);
            }
        });
    }
}

impl IDropTarget_Impl for GridDrop_Impl {
    fn DragEnter(&self, data: Option<&IDataObject>, keys: MODIFIERKEYS_FLAGS, pt: &POINTL, effect: *mut DROPEFFECT) -> windows::core::Result<()> {
        *self.files.borrow_mut() = data.map(shell::files_of).unwrap_or_default();
        unsafe {
            let (e, _) = self.evaluate(keys, pt, *effect, false);
            *effect = e;
            if let (Some(h), Some(d)) = (&self.helper, data) {
                let _ = h.DragEnter(self.hwnd(), d, &POINT { x: pt.x, y: pt.y }, e);
            }
        }
        Ok(())
    }

    fn DragOver(&self, keys: MODIFIERKEYS_FLAGS, pt: &POINTL, effect: *mut DROPEFFECT) -> windows::core::Result<()> {
        unsafe {
            let (e, _) = self.evaluate(keys, pt, *effect, true);
            *effect = e;
            if let Some(h) = &self.helper {
                let _ = h.DragOver(&POINT { x: pt.x, y: pt.y }, e);
            }
        }
        Ok(())
    }

    fn DragLeave(&self) -> windows::core::Result<()> {
        self.clear();
        self.files.borrow_mut().clear();
        if let Some(h) = &self.helper {
            unsafe {
                let _ = h.DragLeave();
            }
        }
        Ok(())
    }

    fn Drop(&self, data: Option<&IDataObject>, keys: MODIFIERKEYS_FLAGS, pt: &POINTL, effect: *mut DROPEFFECT) -> windows::core::Result<()> {
        unsafe {
            let (e, tgt) = self.evaluate(keys, pt, *effect, false);
            if let (Some(h), Some(d)) = (&self.helper, data) {
                let _ = h.Drop(d, &POINT { x: pt.x, y: pt.y }, e);
            }
            self.clear();
            let items = std::mem::take(&mut *self.files.borrow_mut());
            *effect = DROPEFFECT_NONE;
            let (Some((tid, dest)), Some(obj)) = (tgt, data) else { return Ok(()) };
            if e == DROPEFFECT_NONE || items.is_empty() {
                return Ok(());
            }
            let n = items.len();
            let name = file_name(&dest);
            let is_move = e == DROPEFFECT_MOVE;
            let msg = format!("{} {n} item(s) to {name}", if is_move { "Moved" } else { "Copied" });
            let op = if is_move { Op::Move { items, dest } } else { Op::Copy { items, dest } };
            with_app(|app| app.run_op(tid, op, Some(msg)));
            if is_move {
                // We perform the move ourselves; tell the source not to delete the originals.
                shell::report_optimized_move(obj);
            }
            *effect = e;
        }
        Ok(())
    }
}

pub fn register_grid(hwnd: HWND, pane: PaneId) {
    let target: IDropTarget = GridDrop {
        hwnd: hwnd_to_isize(hwnd),
        pane,
        files: RefCell::new(Vec::new()),
        helper: helper(),
        last_scroll: Cell::new(None),
    }
    .into();
    unsafe {
        let _ = RegisterDragDrop(hwnd, &target);
    }
}

// ---------- Main window: switch tabs while dragging over them ----------

#[implement(IDropTarget)]
struct MainDrop {
    hwnd: isize,
    helper: Option<IDropTargetHelper>,
}

impl MainDrop {
    fn hover(&self, pt: &POINTL) {
        let hwnd = isize_to_hwnd(self.hwnd);
        let p = screen_to_client(hwnd, POINT { x: pt.x, y: pt.y });
        with_app(|app| {
            let tab = crate::mainwin::tab_at(app, p.x, p.y);
            if app.chrome.drag_hover != tab {
                app.chrome.drag_hover = tab;
                unsafe {
                    if tab.is_some() {
                        SetTimer(hwnd, TIMER_HOVER_TAB, 600, None);
                    } else {
                        let _ = KillTimer(hwnd, TIMER_HOVER_TAB);
                    }
                }
            }
        });
    }

    fn leave(&self) {
        let hwnd = isize_to_hwnd(self.hwnd);
        with_app(|app| app.chrome.drag_hover = None);
        unsafe {
            let _ = KillTimer(hwnd, TIMER_HOVER_TAB);
        }
    }
}

impl IDropTarget_Impl for MainDrop_Impl {
    fn DragEnter(&self, data: Option<&IDataObject>, _keys: MODIFIERKEYS_FLAGS, pt: &POINTL, effect: *mut DROPEFFECT) -> windows::core::Result<()> {
        self.hover(pt);
        unsafe {
            *effect = DROPEFFECT_NONE;
            if let (Some(h), Some(d)) = (&self.helper, data) {
                let _ = h.DragEnter(isize_to_hwnd(self.hwnd), d, &POINT { x: pt.x, y: pt.y }, DROPEFFECT_NONE);
            }
        }
        Ok(())
    }

    fn DragOver(&self, _keys: MODIFIERKEYS_FLAGS, pt: &POINTL, effect: *mut DROPEFFECT) -> windows::core::Result<()> {
        self.hover(pt);
        unsafe {
            *effect = DROPEFFECT_NONE;
            if let Some(h) = &self.helper {
                let _ = h.DragOver(&POINT { x: pt.x, y: pt.y }, DROPEFFECT_NONE);
            }
        }
        Ok(())
    }

    fn DragLeave(&self) -> windows::core::Result<()> {
        self.leave();
        if let Some(h) = &self.helper {
            unsafe {
                let _ = h.DragLeave();
            }
        }
        Ok(())
    }

    fn Drop(&self, data: Option<&IDataObject>, _keys: MODIFIERKEYS_FLAGS, pt: &POINTL, effect: *mut DROPEFFECT) -> windows::core::Result<()> {
        self.leave();
        unsafe {
            *effect = DROPEFFECT_NONE;
            if let (Some(h), Some(d)) = (&self.helper, data) {
                let _ = h.Drop(d, &POINT { x: pt.x, y: pt.y }, DROPEFFECT_NONE);
            }
        }
        Ok(())
    }
}

pub fn register_main(hwnd: HWND) {
    let target: IDropTarget = MainDrop { hwnd: hwnd_to_isize(hwnd), helper: helper() }.into();
    unsafe {
        let _ = RegisterDragDrop(hwnd, &target);
    }
}

/// TIMER_HOVER_TAB fired: activate the tab still under the drag cursor.
pub fn on_hover_timer(app: &mut App) {
    unsafe {
        let _ = KillTimer(app.hwnd, TIMER_HOVER_TAB);
    }
    if let Some((pane, idx)) = app.chrome.drag_hover.take() {
        app.activate_tab(pane, idx);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn volumes() {
        assert_eq!(volume_of("C:\\a\\b"), volume_of("c:\\x"));
        assert_ne!(volume_of("C:\\a"), volume_of("D:\\a"));
        assert_eq!(volume_of("\\\\srv\\share\\a"), volume_of("\\\\SRV\\Share\\b"));
        assert_ne!(volume_of("\\\\srv\\one\\a"), volume_of("\\\\srv\\two\\a"));
    }

    #[test]
    fn effects() {
        let all = DROPEFFECT(DROPEFFECT_COPY.0 | DROPEFFECT_MOVE.0);
        assert_eq!(choose_effect(MODIFIERKEYS_FLAGS(0), all, true), DROPEFFECT_MOVE);
        assert_eq!(choose_effect(MODIFIERKEYS_FLAGS(0), all, false), DROPEFFECT_COPY);
        assert_eq!(choose_effect(MK_CONTROL, all, true), DROPEFFECT_COPY);
        assert_eq!(choose_effect(MK_SHIFT, all, false), DROPEFFECT_MOVE);
        assert_eq!(choose_effect(MK_SHIFT, DROPEFFECT_COPY, false), DROPEFFECT_COPY);
    }
}
