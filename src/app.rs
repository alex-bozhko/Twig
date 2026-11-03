//! Application state and core operations: tabs/panes, navigation, background loading,
//! change notifications, file operations, undo, clipboard, favorites and settings.
//!
//! State lives in a thread-local and is accessed with `with_app`. Modal UI (menus, dialogs,
//! drag & drop) must never run while the state is borrowed; window procedures compute what to do
//! inside `with_app`, release the borrow and then run the modal part.

use crate::fileops::{self, Op, UndoEntry};
use crate::layout::{Layout, PaneId, Side};
use crate::loader::{self, Done, Job, TabId};
use crate::model::{Kind, NodeId, SortKey, Tree};
use crate::settings::{Favorite, LayoutEntry, Settings, TabEntry};
use crate::shell;
use crate::theme::{Fonts, Palette};
use crate::util::*;
use crate::watch::{self, Watcher};
use crate::win::*;
use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use windows::Win32::Foundation::*;
use windows::Win32::System::Com::IDataObject;
use windows::Win32::System::Ole::OleSetClipboard;
use windows::Win32::UI::WindowsAndMessaging::*;

pub const TIMER_STATUS: usize = 1;
pub const TIMER_WATCH: usize = 2;
pub const TIMER_HOVER_TAB: usize = 3;
const STATUS_MS: u32 = 5000;
const WATCH_DEBOUNCE_MS: u32 = 200;

thread_local! {
    static APP: RefCell<Option<Box<App>>> = const { RefCell::new(None) };
}

pub fn install(app: Box<App>) {
    APP.with(|a| *a.borrow_mut() = Some(app));
}

pub fn uninstall() -> Option<Box<App>> {
    APP.with(|a| a.try_borrow_mut().ok().and_then(|mut b| b.take()))
}

/// Runs `f` with the app state; `None` if it is not installed or already borrowed (re-entrancy).
pub fn with_app<R>(f: impl FnOnce(&mut App) -> R) -> Option<R> {
    APP.with(|a| {
        let mut b = a.try_borrow_mut().ok()?;
        let app = b.as_mut()?;
        Some(f(app))
    })
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Reveal {
    /// Scroll so the item is in the middle of the view.
    Center,
    /// Scroll as little as possible to make the item visible.
    Visible,
}

pub struct Tab {
    pub id: TabId,
    pub tree: Tree,
    /// `None` = This PC.
    pub root: Option<String>,
    pub anchor: Option<NodeId>,
    pub focus: Option<NodeId>,
    /// First visible row.
    pub top: usize,
    pub scroll_x: i32,
    pub watchers: Vec<Watcher>,
    /// Path keys to expand once they appear in the tree.
    pub want_expanded: HashSet<String>,
    pub reveal: Option<(String, Reveal)>,
    /// Path keys to select once all of them are resolved.
    pub pending_select: Vec<String>,
    /// Path key of an item to start renaming once it appears (new folder/file).
    pub rename_after: Option<String>,
    pub restore_top: Option<usize>,
    /// Error shown instead of the tree (root can't be listed).
    pub error: Option<String>,
    /// Debounced change notifications (path keys).
    pub changed: HashSet<String>,
    pub rows: Vec<NodeId>,
    dirty: bool,
    focus_row: usize,
}

impl Tab {
    fn new(id: TabId, sort: SortKey, desc: bool) -> Tab {
        Tab {
            id,
            tree: Tree::new(sort, desc),
            root: None,
            anchor: None,
            focus: None,
            top: 0,
            scroll_x: 0,
            watchers: Vec::new(),
            want_expanded: HashSet::new(),
            reveal: None,
            pending_select: Vec::new(),
            rename_after: None,
            restore_top: None,
            error: None,
            changed: HashSet::new(),
            rows: Vec::new(),
            dirty: true,
            focus_row: 0,
        }
    }

    pub fn mark(&mut self) {
        self.dirty = true;
    }

    /// Rebuilds the visible rows if needed and keeps focus/anchor valid.
    pub fn ensure_rows(&mut self) {
        if !self.dirty {
            return;
        }
        self.dirty = false;
        self.rows = self.tree.visible();
        if self.rows.is_empty() {
            self.focus = None;
            self.anchor = None;
            return;
        }
        let mut f = self.focus.filter(|&f| self.tree.exists(f));
        // A focused item inside a collapsed folder moves to the folder.
        while let Some(id) = f {
            if self.rows.contains(&id) {
                break;
            }
            f = self.tree.get(id).and_then(|n| n.parent);
        }
        let idx = match f.and_then(|id| self.rows.iter().position(|&r| r == id)) {
            Some(i) => i,
            None => self.focus_row.min(self.rows.len() - 1),
        };
        self.focus = Some(self.rows[idx]);
        self.focus_row = idx;
        if !self.anchor.is_some_and(|a| self.tree.exists(a)) {
            self.anchor = self.focus;
        }
    }

    pub fn row_of(&mut self, id: NodeId) -> Option<usize> {
        self.ensure_rows();
        self.rows.iter().position(|&r| r == id)
    }

    pub fn set_focus(&mut self, id: NodeId) {
        self.focus = Some(id);
        if let Some(i) = self.row_of(id) {
            self.focus_row = i;
        }
    }

    pub fn root_node(&self) -> Option<NodeId> {
        if self.root.is_some() {
            self.tree.roots.first().copied()
        } else {
            None
        }
    }

    pub fn title(&self) -> String {
        match &self.root {
            None => "This PC".to_string(),
            Some(r) => match self.root_node().and_then(|id| self.tree.get(id)) {
                Some(n) if n.kind == Kind::Drive => n.name.clone(),
                _ => file_name(r),
            },
        }
    }

    /// Selected nodes in display order, or the focused node if nothing is selected.
    pub fn selected(&mut self) -> Vec<NodeId> {
        self.ensure_rows();
        let sel = self.tree.selection(&self.rows);
        if sel.is_empty() {
            self.focus.into_iter().collect()
        } else {
            sel
        }
    }

    pub fn select_only(&mut self, id: NodeId) {
        self.tree.clear_selection();
        self.tree.set_selected(id, true);
        self.anchor = Some(id);
        self.set_focus(id);
    }

    pub fn path_of(&self, id: NodeId) -> Option<String> {
        self.tree.get(id).map(|n| n.path.clone())
    }

    fn entry(&self) -> TabEntry {
        let root_key = self.root.as_deref().map(path_key);
        TabEntry {
            root: self.root.clone(),
            expanded: self.tree.expanded_paths().into_iter().filter(|p| Some(path_key(p)) != root_key).collect(),
            selected: self.focus.and_then(|f| self.path_of(f)),
            scroll: self.top as i32,
            sort: self.tree.sort,
            desc: self.tree.desc,
        }
    }
}

pub struct Pane {
    pub id: PaneId,
    pub tabs: Vec<TabId>,
    pub active: usize,
    pub grid: isize,
    /// Rows that fit in the grid viewport (updated on layout).
    pub view_rows: usize,
}

impl Pane {
    pub fn grid(&self) -> HWND {
        isize_to_hwnd(self.grid)
    }
    pub fn active_tab(&self) -> Option<TabId> {
        self.tabs.get(self.active).copied()
    }
}

struct OpMeta {
    message: Option<String>,
    rename_new: bool,
}

pub struct App {
    pub hwnd: HWND,
    pub dpi: u32,
    pub dark: bool,
    pub pal: Palette,
    pub fonts: Fonts,
    pub panes: HashMap<PaneId, Pane>,
    pub tabs: HashMap<TabId, Tab>,
    pub layout: Layout,
    pub active_pane: PaneId,
    pub maximized: Option<PaneId>,
    pub settings: Settings,
    pub undo: Vec<UndoEntry>,
    pub status: String,
    pub status_error: bool,
    /// Our last "cut" clipboard object and the cut paths (keys) for ghosting.
    pub cut: Option<(IDataObject, HashSet<String>)>,
    pub path_edit: HWND,
    pub grid_ui: crate::grid::GridUi,
    pub chrome: crate::mainwin::Chrome,
    op_meta: HashMap<u64, OpMeta>,
    op_seq: u64,
    next_tab: TabId,
    next_pane: PaneId,
    next_watch: u64,
}

impl App {
    pub fn new(hwnd: HWND, path_edit: HWND, dpi: u32, dark: bool, settings: Settings) -> Box<App> {
        let mut app = Box::new(App {
            hwnd,
            dpi,
            dark,
            pal: Palette::new(dark),
            fonts: Fonts::new(dpi),
            panes: HashMap::new(),
            tabs: HashMap::new(),
            layout: Layout::Leaf(0),
            active_pane: 0,
            maximized: None,
            settings,
            undo: Vec::new(),
            status: String::new(),
            status_error: false,
            cut: None,
            path_edit,
            grid_ui: Default::default(),
            chrome: Default::default(),
            op_meta: HashMap::new(),
            op_seq: 0,
            next_tab: 1,
            next_pane: 1,
            next_watch: 1,
        });
        let saved = app.settings.layout.clone();
        let mut active = None;
        let mut maximized = None;
        let layout = match saved {
            Some(entry) => app.restore_layout(&entry, &mut active, &mut maximized),
            None => {
                let p = app.create_pane();
                app.add_tab(p, &TabEntry::default());
                Layout::Leaf(p)
            }
        };
        app.layout = layout;
        app.active_pane = active.unwrap_or_else(|| app.layout.panes()[0]);
        app.maximized = maximized;
        app.sync_path_edit();
        app
    }

    fn restore_layout(&mut self, e: &LayoutEntry, active: &mut Option<PaneId>, maximized: &mut Option<PaneId>) -> Layout {
        match e {
            LayoutEntry::Leaf { tabs, active: a, is_active, maximized: m } => {
                let p = self.create_pane();
                if tabs.is_empty() {
                    self.add_tab(p, &TabEntry::default());
                } else {
                    for t in tabs {
                        self.add_tab(p, t);
                    }
                }
                let pane = self.panes.get_mut(&p).unwrap();
                pane.active = (*a).min(pane.tabs.len() - 1);
                if *is_active {
                    *active = Some(p);
                }
                if *m {
                    *maximized = Some(p);
                }
                Layout::Leaf(p)
            }
            LayoutEntry::Split { horizontal, weights, children } => {
                let kids: Vec<Layout> = children.iter().map(|c| self.restore_layout(c, active, maximized)).collect();
                if kids.len() == 1 {
                    return kids.into_iter().next().unwrap();
                }
                let mut w = weights.clone();
                w.resize(kids.len(), 1.0);
                Layout::Split { horizontal: *horizontal, children: kids, weights: w }
            }
        }
    }

    // ---------- Panes and tabs ----------

    pub fn create_pane(&mut self) -> PaneId {
        let id = self.next_pane;
        self.next_pane += 1;
        let grid = crate::grid::create(self.hwnd, id);
        crate::theme::set_control_theme(grid, self.dark, "DarkMode_Explorer");
        self.panes.insert(id, Pane { id, tabs: Vec::new(), active: 0, grid: hwnd_to_isize(grid), view_rows: 20 });
        id
    }

    /// Adds a tab restored from (or described by) `e` to the end of `pane`.
    pub fn add_tab(&mut self, pane: PaneId, e: &TabEntry) -> TabId {
        let id = self.next_tab;
        self.next_tab += 1;
        self.tabs.insert(id, Tab::new(id, e.sort, e.desc));
        if let Some(p) = self.panes.get_mut(&pane) {
            p.tabs.push(id);
        }
        let root = e.root.as_deref().and_then(nearest_existing);
        let exact = root.as_deref().map(path_key) == e.root.as_deref().map(path_key);
        self.set_root(id, root);
        if exact {
            let t = self.tabs.get_mut(&id).unwrap();
            let root_key = t.root.as_deref().map(path_key);
            t.want_expanded.extend(e.expanded.iter().map(|p| path_key(p)).filter(|k| Some(k) != root_key.as_ref()));
            if let Some(s) = &e.selected {
                t.pending_select = vec![path_key(s)];
            }
            t.restore_top = Some(e.scroll.max(0) as usize);
        }
        id
    }

    pub fn pane_of_tab(&self, tab: TabId) -> Option<PaneId> {
        self.panes.values().find(|p| p.tabs.contains(&tab)).map(|p| p.id)
    }

    pub fn active_tab(&self) -> Option<TabId> {
        self.panes.get(&self.active_pane).and_then(|p| p.active_tab())
    }

    /// Opens a new tab in the active pane and activates it.
    pub fn open_tab(&mut self, root: Option<String>, sort: SortKey, desc: bool) -> TabId {
        let pane = self.active_pane;
        let e = TabEntry { root, sort, desc, ..Default::default() };
        let id = self.add_tab(pane, &e);
        if let Some(t) = self.tabs.get_mut(&id) {
            if let Some(r) = t.root_node() {
                t.select_only(r);
            }
        }
        let idx = self.panes[&pane].tabs.len() - 1;
        self.activate_tab(pane, idx);
        crate::mainwin::relayout(self);
        id
    }

    pub fn activate_tab(&mut self, pane: PaneId, index: usize) {
        let Some(p) = self.panes.get_mut(&pane) else { return };
        if p.tabs.is_empty() {
            return;
        }
        p.active = index.min(p.tabs.len() - 1);
        let grid = p.grid();
        self.active_pane = pane;
        crate::grid::end_rename(self, false);
        crate::grid::sync(self, pane);
        self.sync_path_edit();
        invalidate(self.hwnd);
        unsafe {
            let _ = windows::Win32::UI::Input::KeyboardAndMouse::SetFocus(grid);
        }
    }

    pub fn activate_pane(&mut self, pane: PaneId) {
        if self.active_pane != pane && self.panes.contains_key(&pane) {
            self.active_pane = pane;
            self.sync_path_edit();
            invalidate(self.hwnd);
            for p in self.panes.values() {
                invalidate(p.grid());
            }
        }
    }

    pub fn close_tab(&mut self, tab: TabId) {
        let Some(pane) = self.pane_of_tab(tab) else { return };
        crate::grid::end_rename(self, false);
        loader::cancel_tab(tab);
        self.tabs.remove(&tab);
        let p = self.panes.get_mut(&pane).unwrap();
        let idx = p.tabs.iter().position(|&t| t == tab).unwrap();
        p.tabs.remove(idx);
        if p.tabs.is_empty() {
            if self.panes.len() == 1 {
                post(self.hwnd, WM_CLOSE, 0, 0);
                return;
            }
            self.remove_pane(pane);
        } else {
            if p.active > idx || p.active >= p.tabs.len() {
                p.active = p.active.saturating_sub(1);
            }
            let active = p.active;
            if self.active_pane == pane {
                self.activate_tab(pane, active);
            } else {
                crate::grid::sync(self, pane);
            }
        }
        crate::mainwin::relayout(self);
    }

    fn remove_pane(&mut self, pane: PaneId) {
        self.layout.remove(pane);
        if let Some(p) = self.panes.remove(&pane) {
            unsafe {
                let _ = DestroyWindow(p.grid());
            }
        }
        if self.maximized == Some(pane) {
            self.maximized = None;
        }
        if self.active_pane == pane {
            let next = self.layout.panes()[0];
            let idx = self.panes[&next].active;
            self.activate_tab(next, idx);
        }
    }

    /// Moves a tab to `pane` at `index` (clamped). Empty source panes are removed.
    pub fn move_tab(&mut self, tab: TabId, pane: PaneId, index: usize) {
        let Some(src) = self.pane_of_tab(tab) else { return };
        let Some(sp) = self.panes.get_mut(&src) else { return };
        let from = sp.tabs.iter().position(|&t| t == tab).unwrap();
        sp.tabs.remove(from);
        let mut index = index;
        if src == pane && index > from {
            index -= 1;
        }
        let src_active_tab = sp.active_tab();
        let src_empty = sp.tabs.is_empty();
        if !src_empty && sp.active >= sp.tabs.len() {
            sp.active = sp.tabs.len() - 1;
        }
        let _ = src_active_tab;
        let dp = self.panes.get_mut(&pane).unwrap();
        let index = index.min(dp.tabs.len());
        dp.tabs.insert(index, tab);
        if src_empty && src != pane {
            self.remove_pane(src);
        } else if src != pane {
            crate::grid::sync(self, src);
        }
        self.activate_tab(pane, index);
        crate::mainwin::relayout(self);
    }

    /// Moves `tab` into a new pane placed at `side` of `target`.
    pub fn split_with_tab(&mut self, tab: TabId, target: PaneId, side: Side) {
        let Some(src) = self.pane_of_tab(tab) else { return };
        if src == target && self.panes[&src].tabs.len() == 1 {
            return;
        }
        let new = self.create_pane();
        self.layout.split(target, new, side);
        self.maximized = None;
        self.move_tab(tab, new, 0);
    }

    /// Splits `target` with a new This PC tab; focus stays where it was.
    pub fn split_new(&mut self, target: PaneId, side: Side) {
        let keep = self.active_pane;
        let new = self.create_pane();
        self.layout.split(target, new, side);
        self.maximized = None;
        self.add_tab(new, &TabEntry::default());
        crate::mainwin::relayout(self);
        crate::grid::sync(self, new);
        let idx = self.panes[&keep].active;
        self.activate_tab(keep, idx);
    }

    pub fn toggle_maximize(&mut self, pane: PaneId) {
        self.maximized = if self.maximized == Some(pane) || self.panes.len() < 2 { None } else { Some(pane) };
        crate::mainwin::relayout(self);
    }

    // ---------- Navigation ----------

    /// Resets a tab to show `root` (None = This PC). The root must exist (or be None).
    fn set_root(&mut self, tid: TabId, root: Option<String>) {
        loader::cancel_tab(tid);
        let Some(t) = self.tabs.get_mut(&tid) else { return };
        t.tree.clear();
        t.watchers.clear();
        t.want_expanded.clear();
        t.reveal = None;
        t.pending_select.clear();
        t.rename_after = None;
        t.restore_top = None;
        t.error = None;
        t.changed.clear();
        t.anchor = None;
        t.focus = None;
        t.focus_row = 0;
        t.top = 0;
        t.scroll_x = 0;
        t.mark();
        match root {
            None => {
                t.root = None;
                t.tree.fixed_root_order = true;
                loader::submit(self.drives_job(tid));
            }
            Some(p) => {
                t.tree.fixed_root_order = false;
                match loader::entry_for_path(&p) {
                    Some(e) => {
                        t.root = Some(normalize_path(&e.path));
                        let id = t.tree.add_root(e);
                        t.tree.get_mut(id).unwrap().expanded = true;
                        t.focus = Some(id);
                        t.anchor = Some(id);
                        self.load_node(tid, id);
                        self.update_watchers(tid);
                    }
                    None => {
                        t.error = Some(format!("Folder not found: {p}"));
                        t.root = Some(p);
                    }
                }
            }
        }
    }

    /// Shows `target` in tab `tid`. `None` = This PC. With `rooted`, the target becomes the tab
    /// root; otherwise the tab shows the target's drive tree with the target selected and centered.
    pub fn navigate(&mut self, tid: TabId, target: Option<&str>, rooted: bool) -> bool {
        crate::grid::end_rename(self, false);
        let Some(target) = target.map(normalize_path) else {
            self.set_root(tid, None);
            self.after_change(tid);
            return true;
        };
        if loader::entry_for_path(&target).is_none() {
            self.set_status_error(format!("Folder not found: {target}"));
            return false;
        }
        let key = path_key(&target);
        if rooted {
            let keep: Vec<String> = self
                .tabs
                .get(&tid)
                .map(|t| t.tree.expanded_paths().into_iter().filter(|p| is_same_or_inside(p, &target)).collect())
                .unwrap_or_default();
            self.set_root(tid, Some(target.clone()));
            let Some(t) = self.tabs.get_mut(&tid) else { return false };
            t.want_expanded.extend(keep.iter().map(|p| path_key(p)).filter(|k| *k != key));
            t.pending_select = vec![key];
        } else {
            let mut chain = vec![target.clone()];
            while let Some(p) = parent_path(chain.last().unwrap()) {
                chain.push(p);
            }
            let top = chain.last().unwrap().clone();
            let Some(t) = self.tabs.get(&tid) else { return false };
            let same_root = t.root.as_deref().map(path_key) == Some(path_key(&top)) && t.error.is_none() && !t.tree.roots.is_empty();
            let keep: Vec<String> = t.tree.expanded_paths().into_iter().filter(|p| is_same_or_inside(p, &target)).collect();
            if !same_root {
                self.set_root(tid, Some(top));
            }
            let Some(t) = self.tabs.get_mut(&tid) else { return false };
            t.want_expanded.extend(chain[1..].iter().map(|p| path_key(p)));
            t.want_expanded.extend(keep.iter().map(|p| path_key(p)));
            t.pending_select = vec![key.clone()];
            t.reveal = Some((key, Reveal::Center));
            self.set_status(format!("Tab: {target}"));
        }
        self.process_wants(tid);
        self.after_change(tid);
        true
    }

    /// Re-roots `tid` at `path`'s nearest existing ancestor (used when the root disappears).
    fn recover_root(&mut self, tid: TabId) {
        let Some(t) = self.tabs.get(&tid) else { return };
        let Some(root) = t.root.clone() else { return };
        let target = nearest_existing(&root);
        self.set_root(tid, target);
        if let Some(t) = self.tabs.get_mut(&tid) {
            if let Some(r) = t.root_node() {
                t.select_only(r);
            }
        }
        self.after_change(tid);
    }

    /// Goes to the parent of the tab root (or This PC from a drive root).
    pub fn go_up(&mut self, tid: TabId) {
        let Some(t) = self.tabs.get(&tid) else { return };
        let Some(root) = t.root.clone() else { return };
        match parent_path(&root) {
            Some(p) => {
                if self.navigate(tid, Some(&p), true) {
                    if let Some(t) = self.tabs.get_mut(&tid) {
                        t.pending_select = vec![path_key(&root)];
                        t.reveal = Some((path_key(&root), Reveal::Center));
                    }
                    self.process_wants(tid);
                }
            }
            None => {
                self.navigate(tid, None, false);
                if let Some(t) = self.tabs.get_mut(&tid) {
                    t.pending_select = vec![path_key(&root)];
                }
            }
        }
    }

    // ---------- Loading ----------

    pub fn load_node(&mut self, tid: TabId, id: NodeId) {
        let Some(t) = self.tabs.get_mut(&tid) else { return };
        let Some(n) = t.tree.get_mut(id) else { return };
        if n.loading {
            return;
        }
        let job = match n.kind {
            Kind::Drive | Kind::Dir => Job::List { tab: tid, node: id, path: n.path.clone() },
            Kind::Zip => Job::Zip { tab: tid, node: id, path: n.path.clone() },
            _ => return,
        };
        n.loading = true;
        loader::submit(job);
    }

    pub fn expand(&mut self, tid: TabId, id: NodeId) {
        let Some(t) = self.tabs.get_mut(&tid) else { return };
        let Some(n) = t.tree.get_mut(id) else { return };
        if !n.kind.is_container() {
            return;
        }
        n.expanded = true;
        let need_load = n.children.is_none();
        t.mark();
        if need_load {
            self.load_node(tid, id);
        }
        self.update_watchers(tid);
    }

    pub fn collapse(&mut self, tid: TabId, id: NodeId) {
        let Some(t) = self.tabs.get_mut(&tid) else { return };
        t.tree.collapse(id);
        if t.focus.is_some_and(|f| t.tree.is_ancestor(id, f)) {
            t.set_focus(id);
        }
        if t.anchor.is_some_and(|a| t.tree.is_ancestor(id, a)) {
            t.anchor = Some(id);
        }
        t.mark();
        self.update_watchers(tid);
    }

    pub fn toggle(&mut self, tid: TabId, id: NodeId) {
        let expanded = self.tabs.get(&tid).and_then(|t| t.tree.get(id)).map(|n| n.expanded);
        match expanded {
            Some(true) => self.collapse(tid, id),
            Some(false) => self.expand(tid, id),
            None => {}
        }
        self.after_change(tid);
    }

    pub fn expand_all(&mut self, tid: TabId, id: NodeId) {
        let mut stack = vec![id];
        while let Some(cur) = stack.pop() {
            let Some(t) = self.tabs.get_mut(&tid) else { return };
            let Some(n) = t.tree.get_mut(cur) else { continue };
            if !n.kind.is_container() {
                continue;
            }
            n.expanded = true;
            n.expand_all = true;
            match &n.children {
                Some(ch) => stack.extend(ch.iter().copied()),
                None => self.load_node(tid, cur),
            }
        }
        if let Some(t) = self.tabs.get_mut(&tid) {
            t.mark();
        }
        self.update_watchers(tid);
        self.after_change(tid);
    }

    pub fn collapse_all(&mut self, tid: TabId, id: NodeId) {
        let Some(t) = self.tabs.get_mut(&tid) else { return };
        let mut stack = vec![id];
        while let Some(cur) = stack.pop() {
            if let Some(n) = t.tree.get_mut(cur) {
                n.expanded = false;
                n.expand_all = false;
                if let Some(ch) = &n.children {
                    stack.extend(ch.iter().copied());
                }
            }
        }
        self.collapse(tid, id);
        self.after_change(tid);
    }

    pub fn refresh_tab(&mut self, tid: TabId) {
        let Some(t) = self.tabs.get_mut(&tid) else { return };
        if t.error.is_some() {
            let root = t.root.clone();
            match root.as_deref().and_then(nearest_existing) {
                Some(r) => {
                    self.set_root(tid, Some(r));
                }
                None => self.set_root(tid, None),
            }
            self.after_change(tid);
            return;
        }
        if t.root.is_none() {
            loader::submit(self.drives_job(tid));
        }
        let Some(t) = self.tabs.get(&tid) else { return };
        let dirs: Vec<NodeId> = t.tree.loaded_dirs().into_iter().map(|(id, _)| id).collect();
        for id in dirs {
            self.load_node(tid, id);
        }
    }

    /// Drives with no tab change: refresh every This PC tab.
    pub fn refresh_drives(&mut self) {
        for t in self.tabs.values() {
            if t.root.is_none() {
                loader::submit(self.drives_job(t.id));
            }
        }
    }

    /// The This PC listing: drives followed by the favorite folders.
    fn drives_job(&self, tab: TabId) -> Job {
        Job::Drives { tab, favorites: self.settings.favorites.iter().map(|f| f.path.clone()).collect() }
    }

    pub fn on_loaded(&mut self) {
        let mut touched = HashSet::new();
        for done in loader::take_results() {
            match done {
                Done::Listed { tab, node, path, result } => {
                    let Some(t) = self.tabs.get_mut(&tab) else { continue };
                    let is_root = t.root.is_some() && t.tree.roots.first() == Some(&node);
                    let Some(n) = t.tree.get_mut(node) else { continue };
                    if path_key(&n.path) != path_key(&path) {
                        n.loading = false;
                        continue;
                    }
                    touched.insert(tab);
                    match result {
                        Ok(entries) => {
                            let details = loader::detail_items(&entries);
                            let expand_all = n.expand_all;
                            t.tree.merge_children(node, entries);
                            if !details.is_empty() {
                                loader::submit(Job::Details { tab, node, path, items: details });
                            }
                            if expand_all {
                                let kids: Vec<NodeId> = t
                                    .tree
                                    .children(node)
                                    .iter()
                                    .copied()
                                    .filter(|&k| t.tree.get(k).is_some_and(|c| c.kind.is_container()))
                                    .collect();
                                for k in kids {
                                    self.expand_all_quiet(tab, k);
                                }
                            }
                        }
                        Err(e) => {
                            n.loading = false;
                            if is_root {
                                if loader::entry_for_path(&path).is_none() {
                                    self.recover_root(tab);
                                    continue;
                                }
                                t.error = Some(e);
                            } else {
                                n.expanded = false;
                                if n.children.is_none() {
                                    n.has_children = Some(false);
                                }
                                let name = n.name.clone();
                                self.set_status_error(format!("{name}: {e}"));
                            }
                        }
                    }
                }
                Done::Drives { tab, entries } => {
                    let Some(t) = self.tabs.get_mut(&tab) else { continue };
                    if t.root.is_some() {
                        continue;
                    }
                    t.tree.merge_roots(entries);
                    touched.insert(tab);
                }
                Done::Zip { tab, node, result } => {
                    let Some(t) = self.tabs.get_mut(&tab) else { continue };
                    let Some(n) = t.tree.get_mut(node) else { continue };
                    touched.insert(tab);
                    match result {
                        Ok(items) => {
                            let expand_all = n.expand_all;
                            let (ft, fi) = loader::folder_type();
                            t.tree.set_zip_children(node, items, ft, fi);
                            if expand_all {
                                self.expand_all_quiet(tab, node);
                            }
                        }
                        Err(e) => {
                            n.loading = false;
                            n.expanded = false;
                            n.has_children = Some(false);
                            let name = n.name.clone();
                            self.set_status_error(format!("{name}: {e}"));
                        }
                    }
                }
                Done::Details { tab, node, details } => {
                    let Some(t) = self.tabs.get_mut(&tab) else { continue };
                    t.tree.apply_details(node, &details);
                    touched.insert(tab);
                }
            }
        }
        for tab in touched {
            if let Some(t) = self.tabs.get_mut(&tab) {
                t.mark();
            }
            self.process_wants(tab);
            self.update_watchers(tab);
            self.after_change(tab);
        }
    }

    fn expand_all_quiet(&mut self, tid: TabId, id: NodeId) {
        let mut stack = vec![id];
        while let Some(cur) = stack.pop() {
            let Some(t) = self.tabs.get_mut(&tid) else { return };
            let Some(n) = t.tree.get_mut(cur) else { continue };
            if !n.kind.is_container() {
                continue;
            }
            n.expanded = true;
            n.expand_all = true;
            match &n.children {
                Some(ch) => stack.extend(ch.iter().copied()),
                None => self.load_node(tid, cur),
            }
        }
    }

    /// Applies pending expansions, selections, scroll restores and renames that became resolvable.
    pub fn process_wants(&mut self, tid: TabId) {
        let Some(t) = self.tabs.get_mut(&tid) else { return };
        // Expansions.
        let wants: Vec<String> = t.want_expanded.iter().cloned().collect();
        let mut to_load = Vec::new();
        for key in wants {
            match t.tree.find_path(&key) {
                Some(id) => {
                    let n = t.tree.get_mut(id).unwrap();
                    if n.kind.is_container() {
                        n.expanded = true;
                        if n.children.is_none() {
                            to_load.push(id);
                        }
                    }
                    t.want_expanded.remove(&key);
                    t.mark();
                }
                None => {
                    if unresolvable(&t.tree, &key) {
                        t.want_expanded.remove(&key);
                    }
                }
            }
        }
        for id in to_load {
            self.load_node(tid, id);
        }
        let Some(t) = self.tabs.get_mut(&tid) else { return };
        t.ensure_rows();
        // Selection (applied once every key is found or known to be absent).
        if !t.pending_select.is_empty() {
            let mut found = Vec::new();
            let mut waiting = false;
            for key in &t.pending_select {
                match t.tree.find_path(key) {
                    Some(id) => found.push(id),
                    None if unresolvable(&t.tree, key) => {}
                    None => waiting = true,
                }
            }
            if !waiting {
                t.pending_select.clear();
                if !found.is_empty() {
                    t.tree.clear_selection();
                    for &id in &found {
                        t.tree.set_selected(id, true);
                    }
                    t.anchor = Some(found[0]);
                    t.focus = Some(found[0]);
                    t.mark();
                    if t.reveal.is_none() {
                        let key = path_key(&t.tree.get(found[0]).unwrap().path);
                        t.reveal = Some((key, Reveal::Visible));
                    }
                }
            }
        }
        t.ensure_rows();
        // Scroll restore once all expansions are done.
        if t.want_expanded.is_empty() {
            if let Some(top) = t.restore_top.take() {
                t.top = top;
            }
        }
        // Reveal.
        if let Some((key, mode)) = t.reveal.clone() {
            match t.tree.find_path(&key) {
                Some(id) => {
                    if let Some(row) = t.rows.iter().position(|&r| r == id) {
                        t.reveal = None;
                        t.restore_top = None;
                        let view = self.view_rows_of(tid);
                        let t = self.tabs.get_mut(&tid).unwrap();
                        t.top = reveal_top(t.top, row, view, mode);
                    } else if !t.want_expanded.is_empty() {
                        // Still expanding ancestors.
                    } else {
                        t.reveal = None;
                    }
                }
                None => {
                    if unresolvable(&t.tree, &key) {
                        t.reveal = None;
                    }
                }
            }
        }
        // Rename a freshly created item.
        let Some(t) = self.tabs.get_mut(&tid) else { return };
        if let Some(key) = t.rename_after.clone() {
            match t.tree.find_path(&key) {
                Some(id) => {
                    t.rename_after = None;
                    if let Some(pane) = self.pane_of_tab(tid) {
                        if self.panes[&pane].active_tab() == Some(tid) {
                            crate::grid::sync(self, pane);
                            crate::grid::begin_rename(self, pane, id);
                        }
                    }
                }
                None => {
                    if unresolvable(&t.tree, &key) {
                        t.rename_after = None;
                    }
                }
            }
        }
    }

    fn view_rows_of(&self, tid: TabId) -> usize {
        self.pane_of_tab(tid).and_then(|p| self.panes.get(&p)).map(|p| p.view_rows).unwrap_or(20)
    }

    /// Scrolls `tid` so `row` is visible.
    pub fn scroll_into_view(&mut self, tid: TabId, row: usize) {
        let view = self.view_rows_of(tid);
        if let Some(t) = self.tabs.get_mut(&tid) {
            t.top = reveal_top(t.top, row, view, Reveal::Visible);
        }
    }

    /// Repaints the tab if it is shown.
    pub fn after_change(&mut self, tid: TabId) {
        if let Some(pane) = self.pane_of_tab(tid) {
            if self.panes[&pane].active_tab() == Some(tid) {
                crate::grid::sync(self, pane);
                if pane == self.active_pane {
                    self.sync_path_edit();
                }
            }
        }
        invalidate(self.hwnd);
    }

    pub fn sync_path_edit(&mut self) {
        if self.chrome.path_editing {
            return;
        }
        let text = self.active_tab().and_then(|t| self.tabs.get(&t)).and_then(|t| t.root.clone()).unwrap_or_default();
        if get_text(self.path_edit) != text {
            set_text(self.path_edit, &text);
        }
    }

    // ---------- Change notifications ----------

    /// Watches every expanded file-system root of the tab.
    fn update_watchers(&mut self, tid: TabId) {
        let Some(t) = self.tabs.get_mut(&tid) else { return };
        let desired: Vec<String> = t
            .tree
            .roots
            .iter()
            .filter_map(|&id| t.tree.get(id))
            .filter(|n| n.kind.is_fs_dir() && n.expanded)
            .map(|n| n.path.clone())
            .collect();
        let keys: HashSet<String> = desired.iter().map(|p| path_key(p)).collect();
        t.watchers.retain(|w| keys.contains(&path_key(&w.root)));
        for p in desired {
            if !t.watchers.iter().any(|w| path_key(&w.root) == path_key(&p)) {
                let id = self.next_watch;
                self.next_watch += 1;
                if let Some(w) = Watcher::start(id, &p) {
                    t.watchers.push(w);
                }
            }
        }
    }

    pub fn on_watch(&mut self) {
        let mut any = false;
        for ev in watch::take_events() {
            let Some(tid) = self.tabs.values().find(|t| t.watchers.iter().any(|w| w.id == ev.id)).map(|t| t.id) else { continue };
            let t = self.tabs.get_mut(&tid).unwrap();
            if ev.error {
                let root = t.watchers.iter().find(|w| w.id == ev.id).map(|w| w.root.clone()).unwrap_or_default();
                t.watchers.retain(|w| w.id != ev.id);
                if t.root.is_some() {
                    if loader::entry_for_path(&root).is_none() {
                        self.recover_root(tid);
                    }
                } else {
                    loader::submit(self.drives_job(tid));
                }
                continue;
            }
            if ev.overflow {
                for (_, p) in t.tree.loaded_dirs() {
                    t.changed.insert(path_key(&p));
                }
            }
            for d in ev.dirs {
                t.changed.insert(path_key(&d));
            }
            any = true;
        }
        if any {
            unsafe {
                SetTimer(self.hwnd, TIMER_WATCH, WATCH_DEBOUNCE_MS, None);
            }
        }
    }

    pub fn on_watch_timer(&mut self) {
        unsafe {
            let _ = KillTimer(self.hwnd, TIMER_WATCH);
        }
        let ids: Vec<TabId> = self.tabs.keys().copied().collect();
        for tid in ids {
            let Some(t) = self.tabs.get_mut(&tid) else { continue };
            let changed: Vec<String> = t.changed.drain().collect();
            let mut nodes = Vec::new();
            for key in changed {
                for id in t.tree.find_paths(&key) {
                    if t.tree.get(id).is_some_and(|n| n.kind.is_fs_dir() && n.children.is_some()) {
                        nodes.push(id);
                    }
                }
            }
            for id in nodes {
                self.load_node(tid, id);
            }
        }
    }

    // ---------- File operations & undo ----------

    pub fn run_op(&mut self, tid: TabId, op: Op, message: Option<String>) {
        self.op_seq += 1;
        let tag = (self.op_seq << 32) | tid as u64;
        let rename_new = matches!(op, Op::NewFolder { .. } | Op::NewFile { .. });
        self.op_meta.insert(tag, OpMeta { message, rename_new });
        fileops::submit(tag, op);
        invalidate(self.hwnd);
    }

    pub fn on_fileop(&mut self) {
        for r in fileops::take_results() {
            let meta = self.op_meta.remove(&r.tag);
            let tid = (r.tag & 0xFFFF_FFFF) as TabId;
            if let Some(u) = r.undo {
                self.undo.push(u);
                if self.undo.len() > 100 {
                    self.undo.remove(0);
                }
            }
            match (&r.error, &r.message) {
                (Some(e), _) => self.set_status_error(e.clone()),
                (None, Some(m)) => self.set_status(m.clone()),
                (None, None) => {
                    if let Some(m) = meta.as_ref().and_then(|m| m.message.clone()) {
                        if !r.select.is_empty() || r.was_undo {
                            self.set_status(m);
                        }
                    }
                }
            }
            // Refresh affected folders in every tab (watchers may lag or be absent).
            let ids: Vec<TabId> = self.tabs.keys().copied().collect();
            for t2 in ids {
                let nodes: Vec<NodeId> = {
                    let t = &self.tabs[&t2];
                    r.refresh
                        .iter()
                        .flat_map(|d| t.tree.find_paths(d))
                        .filter(|&id| t.tree.get(id).is_some_and(|n| n.kind.is_fs_dir() && n.children.is_some()))
                        .collect()
                };
                for id in nodes {
                    self.load_node(t2, id);
                }
            }
            if let Some(t) = self.tabs.get_mut(&tid) {
                if !r.select.is_empty() {
                    t.pending_select = r.select.iter().map(|p| path_key(p)).collect();
                    t.reveal = None;
                    for p in &r.select {
                        if let Some(parent) = parent_path(p) {
                            if t.tree.find_path(&parent).is_some() {
                                t.want_expanded.insert(path_key(&parent));
                            }
                        }
                    }
                    if meta.as_ref().is_some_and(|m| m.rename_new) && r.select.len() == 1 {
                        t.rename_after = Some(path_key(&r.select[0]));
                    }
                }
                self.process_wants(tid);
                self.after_change(tid);
            }
        }
        invalidate(self.hwnd);
    }

    pub fn undo_last(&mut self) {
        let tid = self.active_tab().unwrap_or(0);
        match self.undo.pop() {
            Some(e) => {
                let label = e.label.clone();
                self.run_op(tid, Op::Undo(e), Some(format!("Undid {label}")));
            }
            None => self.set_status("Nothing to undo".into()),
        }
    }

    // ---------- Status ----------

    pub fn set_status(&mut self, s: String) {
        self.status = s;
        self.status_error = false;
        self.arm_status();
    }

    pub fn set_status_error(&mut self, s: String) {
        self.status = s;
        self.status_error = true;
        self.arm_status();
    }

    fn arm_status(&mut self) {
        unsafe {
            SetTimer(self.hwnd, TIMER_STATUS, STATUS_MS, None);
        }
        invalidate(self.hwnd);
    }

    pub fn clear_status(&mut self) {
        unsafe {
            let _ = KillTimer(self.hwnd, TIMER_STATUS);
        }
        self.status.clear();
        invalidate(self.hwnd);
    }

    // ---------- Clipboard ----------

    pub fn on_clipboard_update(&mut self) {
        let stale = self.cut.as_ref().is_some_and(|(obj, _)| !shell::is_current_clipboard(obj));
        if stale {
            self.cut = None;
            self.invalidate_grids();
        }
    }

    pub fn invalidate_grids(&self) {
        for p in self.panes.values() {
            invalidate(p.grid());
        }
    }

    /// Copies/cuts plain file-system paths to the clipboard.
    pub fn set_clipboard(&mut self, paths: &[String], cut: bool) {
        if paths.is_empty() {
            return;
        }
        match shell::clipboard_set_files(self.hwnd, paths, cut) {
            Some(obj) => {
                self.cut = if cut { Some((obj, paths.iter().map(|p| path_key(p)).collect())) } else { None };
                let n = paths.len();
                self.set_status(if cut { format!("Cut {n} item(s)") } else { format!("Copied {n} item(s)") });
            }
            None => self.set_status_error("Could not access the clipboard".into()),
        }
        self.invalidate_grids();
    }

    pub fn paste_into(&mut self, tid: TabId, dest: &str) {
        let Some((files, is_move)) = shell::clipboard_files() else {
            self.set_status("Clipboard has no files".into());
            return;
        };
        let name = file_name(dest);
        let msg = Some(format!("Pasted into {name}"));
        if is_move {
            self.run_op(tid, Op::Move { items: files, dest: dest.to_string() }, msg);
            unsafe {
                let _ = OleSetClipboard(None);
            }
            self.cut = None;
            self.invalidate_grids();
        } else {
            self.run_op(tid, Op::Copy { items: files, dest: dest.to_string() }, msg);
        }
    }

    /// Folder that receives pastes/new items for the given node (folder itself, a file's parent).
    pub fn dest_for(&self, tid: TabId, id: Option<NodeId>) -> Option<String> {
        let t = self.tabs.get(&tid)?;
        let id = id.or(t.focus).or(t.root_node())?;
        let n = t.tree.get(id)?;
        match n.kind {
            Kind::Drive | Kind::Dir => Some(n.path.clone()),
            Kind::File | Kind::Zip => parent_path(&n.path),
            Kind::ZipDir | Kind::ZipFile => None,
        }
    }

    pub fn new_item(&mut self, tid: TabId, dest: &str, folder: bool) {
        let exists = |n: &str| std::path::Path::new(&join_path(dest, n)).exists();
        let (op, label) = if folder {
            let name = unique_name("New folder", "", exists);
            (Op::NewFolder { parent: dest.to_string(), name: name.clone() }, name)
        } else {
            let name = unique_name("New Text Document", ".txt", exists);
            (Op::NewFile { parent: dest.to_string(), name: name.clone() }, name)
        };
        self.run_op(tid, op, Some(format!("Created {label}")));
    }

    // ---------- Favorites ----------

    pub fn add_favorite(&mut self) {
        let Some(tid) = self.active_tab() else { return };
        let t = &self.tabs[&tid];
        let Some(root) = t.root.clone() else {
            self.set_status("This PC can't be a favorite".into());
            return;
        };
        let (sort, desc) = (t.tree.sort, t.tree.desc);
        let name = file_name(&root);
        if let Some(f) = self.settings.favorites.iter_mut().find(|f| path_key(&f.path) == path_key(&root)) {
            f.sort = sort;
            f.desc = desc;
            self.set_status(format!("Updated sort for favorite: {name}"));
        } else {
            self.settings.favorites.push(Favorite { path: root, sort, desc });
            self.set_status(format!("Added favorite: {name}"));
            self.refresh_drives();
        }
        self.save_settings();
    }

    pub fn remove_favorite(&mut self, i: usize) {
        if i < self.settings.favorites.len() {
            let f = self.settings.favorites.remove(i);
            self.set_status(format!("Removed favorite: {}", file_name(&f.path)));
            self.save_settings();
            self.refresh_drives();
        }
    }

    /// Index of the favorite for `path`, if any.
    pub fn favorite_index(&self, path: &str) -> Option<usize> {
        self.settings.favorites.iter().position(|f| path_key(&f.path) == path_key(path))
    }

    pub fn open_favorite(&mut self, i: usize, new_tab: bool) {
        let Some(f) = self.settings.favorites.get(i).cloned() else { return };
        if loader::entry_for_path(&f.path).is_none() {
            self.set_status_error(format!("Folder not found: {}", f.path));
            return;
        }
        if new_tab {
            self.open_tab(Some(f.path), f.sort, f.desc);
        } else if let Some(tid) = self.active_tab() {
            if let Some(t) = self.tabs.get_mut(&tid) {
                t.tree.sort = f.sort;
                t.tree.desc = f.desc;
            }
            self.navigate(tid, Some(&f.path), true);
        }
    }

    // ---------- Sorting ----------

    pub fn sort_by(&mut self, tid: TabId, key: SortKey) {
        let Some(t) = self.tabs.get_mut(&tid) else { return };
        if t.tree.sort == key {
            t.tree.desc = !t.tree.desc;
        } else {
            t.tree.sort = key;
            t.tree.desc = false;
        }
        t.tree.resort();
        t.mark();
        if let Some(f) = t.focus {
            let key = path_key(&t.tree.get(f).map(|n| n.path.clone()).unwrap_or_default());
            t.reveal = Some((key, Reveal::Visible));
        }
        self.process_wants(tid);
        self.after_change(tid);
    }

    // ---------- Settings ----------

    fn layout_entry(&self, l: &Layout) -> LayoutEntry {
        match l {
            Layout::Leaf(p) => {
                let pane = &self.panes[p];
                LayoutEntry::Leaf {
                    tabs: pane.tabs.iter().filter_map(|t| self.tabs.get(t)).map(|t| t.entry()).collect(),
                    active: pane.active,
                    is_active: *p == self.active_pane,
                    maximized: self.maximized == Some(*p),
                }
            }
            Layout::Split { horizontal, children, weights } => LayoutEntry::Split {
                horizontal: *horizontal,
                weights: weights.clone(),
                children: children.iter().map(|c| self.layout_entry(c)).collect(),
            },
        }
    }

    pub fn save_settings(&mut self) {
        self.settings.layout = Some(self.layout_entry(&self.layout));
        let mut wp = WINDOWPLACEMENT { length: std::mem::size_of::<WINDOWPLACEMENT>() as u32, ..Default::default() };
        if unsafe { GetWindowPlacement(self.hwnd, &mut wp) }.is_ok() {
            let r = wp.rcNormalPosition;
            self.settings.window = Some(crate::settings::WindowPos {
                x: r.left,
                y: r.top,
                w: r.right - r.left,
                h: r.bottom - r.top,
                maximized: wp.showCmd == SW_SHOWMAXIMIZED.0 as u32,
            });
        }
        let _ = self.settings.save();
    }

    /// Column widths in device pixels.
    pub fn columns(&self) -> [i32; 4] {
        let c = self.settings.columns;
        [scale(c[0], self.dpi), scale(c[1], self.dpi), scale(c[2], self.dpi), scale(c[3], self.dpi)]
    }

    pub fn set_dpi(&mut self, dpi: u32) {
        self.dpi = dpi;
        self.fonts = Fonts::new(dpi);
        clear_icon_cache();
    }

    pub fn set_dark(&mut self, dark: bool) {
        self.dark = dark;
        self.pal = Palette::new(dark);
        crate::theme::set_title_bar_dark(self.hwnd, dark);
        for p in self.panes.values() {
            crate::theme::set_control_theme(p.grid(), dark, "DarkMode_Explorer");
        }
        crate::theme::set_control_theme(self.path_edit, dark, "DarkMode_CFD");
        unsafe {
            let _ = windows::Win32::Graphics::Gdi::RedrawWindow(
                self.hwnd,
                None,
                None,
                windows::Win32::Graphics::Gdi::RDW_INVALIDATE | windows::Win32::Graphics::Gdi::RDW_ALLCHILDREN | windows::Win32::Graphics::Gdi::RDW_FRAME,
            );
        }
    }
}

/// True if `key` can never be found: its parent is loaded and doesn't contain it, or no loaded
/// node is an ancestor of it.
fn unresolvable(tree: &Tree, key: &str) -> bool {
    let mut cur = parent_path(key);
    while let Some(p) = cur {
        if let Some(id) = tree.find_path(&p) {
            let n = tree.get(id).unwrap();
            // The nearest loaded ancestor: if it is the direct parent and loaded, the key is gone.
            return match &n.children {
                Some(_) => !n.loading && path_key(&p) == parent_path(key).map(|x| path_key(&x)).unwrap_or_default(),
                None => !n.kind.is_container(),
            };
        }
        cur = parent_path(&p);
    }
    // Nothing above it is in the tree (zip-internal paths resolve through the archive node).
    !tree.roots.is_empty()
}

fn reveal_top(top: usize, row: usize, view: usize, mode: Reveal) -> usize {
    let view = view.max(1);
    match mode {
        Reveal::Center => row.saturating_sub(view / 2),
        Reveal::Visible => {
            if row < top {
                row
            } else if row >= top + view {
                row + 1 - view
            } else {
                top
            }
        }
    }
}

/// The path itself or its nearest existing ancestor.
pub fn nearest_existing(path: &str) -> Option<String> {
    let mut cur = Some(normalize_path(path));
    while let Some(p) = cur {
        if loader::entry_for_path(&p).is_some() {
            return Some(p);
        }
        cur = parent_path(&p);
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reveal_math() {
        assert_eq!(reveal_top(0, 50, 10, Reveal::Center), 45);
        assert_eq!(reveal_top(0, 3, 10, Reveal::Center), 0);
        assert_eq!(reveal_top(20, 5, 10, Reveal::Visible), 5);
        assert_eq!(reveal_top(0, 15, 10, Reveal::Visible), 6);
        assert_eq!(reveal_top(10, 12, 10, Reveal::Visible), 10);
    }
}
