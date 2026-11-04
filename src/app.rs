//! Application state and core operations: tabs/panes, navigation, background loading,
//! change notifications, file operations, undo, clipboard, favorites and settings.
//!
//! State lives in a thread-local and is accessed with `with_app`. Modal UI (menus, dialogs,
//! drag & drop) must never run while the state is borrowed; window procedures compute what to do
//! inside `with_app`, release the borrow and then run the modal part.

use crate::fileops::{self, Op, UndoEntry};
use crate::layout::{Layout, PaneId, Side};
use crate::loader::{self, Done, Job, TabId};
use crate::model::{Filtered, Kind, NodeId, SortKey, Tree};
use crate::scan::ScanMsg;
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
pub const TIMER_OPEN_RETRY: usize = 4;
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
    /// Name filter for this tab ("" = none).
    pub filter: String,
    /// Filter details for drawing (its `rows` are moved into `rows`).
    pub filtered: Filtered,
    /// The filter also searches inside collapsed folders (subfolder search).
    pub deep: bool,
    /// Background scan feeding subfolder search.
    pub scan: Option<ScanState>,
    /// A path is being typed into the breadcrumb bar.
    pub editing_path: bool,
    /// Navigation history: earlier views (most recent last) and views gone back from.
    back: Vec<HistEntry>,
    fwd: Vec<HistEntry>,
    dirty: bool,
    focus_row: usize,
}

/// A tab's view as recorded for Back/Forward.
struct HistEntry {
    root: Option<String>,
    /// Path keys.
    selected: Vec<String>,
    expanded: Vec<String>,
    top: usize,
}

const HISTORY_MAX: usize = 50;

fn push_history(stack: &mut Vec<HistEntry>, e: HistEntry) {
    stack.push(e);
    if stack.len() > HISTORY_MAX {
        stack.remove(0);
    }
}

/// Progress of a tab's subfolder-search scan.
pub struct ScanState {
    handle: crate::scan::ScanHandle,
    pub items: usize,
    pub folders: usize,
    pub done: bool,
    /// Folders that couldn't be listed.
    pub errors: usize,
    /// Path key -> node, for folders the scan will list.
    index: HashMap<String, NodeId>,
}

impl ScanState {
    /// Shown as a note above the rows while scanning.
    pub fn needs_note(&self) -> bool {
        !self.done
    }
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
            filter: String::new(),
            filtered: Filtered::default(),
            deep: false,
            scan: None,
            editing_path: false,
            back: Vec::new(),
            fwd: Vec::new(),
            dirty: true,
            focus_row: 0,
        }
    }

    pub fn mark(&mut self) {
        self.dirty = true;
    }

    pub fn can_back(&self) -> bool {
        !self.back.is_empty()
    }

    pub fn can_forward(&self) -> bool {
        !self.fwd.is_empty()
    }

    /// The current view for the history, counting selection and expansion still pending.
    fn snapshot(&mut self) -> HistEntry {
        let selected = if self.pending_select.is_empty() {
            let ids = self.selected();
            ids.into_iter().filter_map(|id| self.tree.get(id)).map(|n| path_key(&n.path)).collect()
        } else {
            self.pending_select.clone()
        };
        let mut expanded: Vec<String> = self.tree.expanded_paths().iter().map(|p| path_key(p)).collect();
        expanded.extend(self.want_expanded.iter().cloned());
        HistEntry { root: self.root.clone(), selected, expanded, top: self.restore_top.unwrap_or(self.top) }
    }

    /// Rebuilds the visible rows if needed and keeps focus/anchor valid.
    pub fn ensure_rows(&mut self) {
        if !self.dirty {
            return;
        }
        self.dirty = false;
        if self.filter.is_empty() {
            self.filtered = Filtered::default();
            self.rows = self.tree.visible();
        } else {
            self.filtered = self.tree.filtered(&self.filter, self.deep);
            self.rows = std::mem::take(&mut self.filtered.rows);
        }
        if self.root.is_none() {
            // This PC: a "Favorites" header precedes the first favorite root.
            let first_fav = self.rows.iter().position(|&id| self.tree.get(id).is_some_and(|n| n.parent.is_none() && n.kind != Kind::Drive));
            if let Some(i) = first_fav {
                self.rows.insert(i, NodeId::SECTION);
            }
        }
        if self.deep && self.scan.as_ref().is_some_and(|s| s.needs_note()) {
            // Below a pinned root, so the root stays the first row.
            let at = usize::from(self.root_node().is_some_and(|r| self.rows.first() == Some(&r)));
            self.rows.insert(at, NodeId::SCAN_NOTE);
        }
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
            if !self.filter.is_empty() && (self.deep || self.tree.is_shown(id)) {
                // Hidden by the filter: keep the row position instead.
                f = None;
                break;
            }
            f = self.tree.get(id).and_then(|n| n.parent);
        }
        let idx = match f.and_then(|id| self.rows.iter().position(|&r| r == id)) {
            Some(i) => i,
            None => self.focus_row.min(self.rows.len() - 1),
        };
        // Placeholder rows (header, hidden markers) can't take focus: use the nearest real row.
        let real = |i: &usize| !self.rows[*i].is_placeholder();
        let Some(idx) = (idx..self.rows.len()).find(real).or_else(|| (0..idx).rev().find(real)) else {
            self.focus = None;
            return;
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

    /// Rooted at a folder (or archive) rather than at a drive or This PC.
    pub fn zoomed(&self) -> bool {
        self.error.is_none() && self.root_node().and_then(|id| self.tree.get(id)).is_some_and(|n| n.kind != Kind::Drive)
    }

    /// Rows pinned above the scrolling area: the root row while zoomed (call after `ensure_rows`).
    pub fn pinned(&self) -> usize {
        usize::from(self.zoomed() && self.rows.first().copied() == self.root_node())
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

    /// Status-bar summary of the shown rows, e.g. "120 items · 3 selected (1.2 MB)" (call after `ensure_rows`).
    pub fn summary(&self) -> String {
        if self.error.is_some() {
            return String::new();
        }
        let (mut items, mut sel, mut bytes, mut files) = (0usize, 0usize, 0u64, false);
        for &id in &self.rows[self.pinned()..] {
            if id.is_placeholder() || self.filtered.context.contains(&id) {
                continue;
            }
            let Some(n) = self.tree.get(id) else { continue };
            items += 1;
            if n.selected {
                sel += 1;
                if !n.kind.is_container() {
                    files = true;
                    bytes += n.size;
                }
            }
        }
        let mut s = format!("{} {}", format_count(items), if items == 1 { "item" } else { "items" });
        if sel > 0 {
            s += &format!(" \u{00B7} {} selected", format_count(sel));
            if files {
                s += &format!(" ({})", format_size(bytes));
            }
        }
        s
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
    pub grid_ui: crate::grid::GridUi,
    pub chrome: crate::mainwin::Chrome,
    op_meta: HashMap<u64, OpMeta>,
    op_seq: u64,
    next_tab: TabId,
    next_pane: PaneId,
    next_watch: u64,
    /// The tab opened by the last external request, so a follow-up "reveal" can reuse it.
    external_tab: Option<TabId>,
}

impl App {
    pub fn new(hwnd: HWND, dpi: u32, dark: bool, settings: Settings) -> Box<App> {
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
            grid_ui: Default::default(),
            chrome: Default::default(),
            op_meta: HashMap::new(),
            op_seq: 0,
            next_tab: 1,
            next_pane: 1,
            next_watch: 1,
            external_tab: None,
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
    /// Sort order for a new tab: the active tab's, else name ascending.
    pub fn active_sort(&self) -> (SortKey, bool) {
        self.active_tab().and_then(|t| self.tabs.get(&t)).map_or((SortKey::Name, false), |t| (t.tree.sort, t.tree.desc))
    }

    /// Opens a path handed over from outside (command line or another Twig launch) in a new tab.
    pub fn open_external(&mut self, text: &str) {
        let path = normalize_path(&expand_env(&clean_path_input(text)));
        if path.is_empty() {
            return;
        }
        if loader::entry_for_path(&path).is_none() {
            self.set_status_error(format!("Not found: {path}"));
            return;
        }
        let (sort, desc) = self.active_sort();
        let tid = self.open_tab(None, sort, desc);
        self.go_to(tid, &path, true);
        if let Some(t) = self.tabs.get_mut(&tid) {
            // A fresh tab: nothing to go back to.
            t.back.clear();
        }
        self.external_tab = Some(tid);
    }

    /// Selects `text` in the tab the last external request opened on its folder ("Show in
    /// folder" names the file only after the folder is open); otherwise opens it normally.
    pub fn reveal_external(&mut self, text: &str) {
        let path = normalize_path(&expand_env(&clean_path_input(text)));
        let parent = parent_path(&path).map(|p| path_key(&p));
        let tid = self.external_tab.filter(|tid| {
            let root = self.tabs.get(tid).and_then(|t| t.root.as_deref()).map(path_key);
            root.is_some() && root == parent
        });
        match tid {
            Some(tid) if self.go_to(tid, &path, true) => {}
            _ => self.open_external(&path),
        }
    }

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
        if self.active_pane != pane {
            for p in self.panes.values() {
                invalidate(p.grid());
            }
        }
        self.active_pane = pane;
        crate::grid::end_rename(self, false);
        crate::grid::end_path_edit(self);
        crate::grid::sync(self, pane);
        invalidate(self.hwnd);
        self.preview_follow();
        unsafe {
            let _ = windows::Win32::UI::Input::KeyboardAndMouse::SetFocus(grid);
        }
    }

    pub fn activate_pane(&mut self, pane: PaneId) {
        if self.active_pane != pane && self.panes.contains_key(&pane) {
            self.active_pane = pane;
            invalidate(self.hwnd);
            for p in self.panes.values() {
                invalidate(p.grid());
            }
            self.preview_follow();
        }
    }

    /// Points an open Quick Look window at the active tab's focused item.
    pub fn preview_follow(&self) {
        if !crate::preview::is_open() {
            return;
        }
        let t = self.panes.get(&self.active_pane).and_then(|p| p.active_tab()).and_then(|tid| self.tabs.get(&tid));
        crate::preview::follow(t.and_then(|t| t.focus.and_then(|id| t.tree.get(id))).map(crate::preview::Target::from_node));
    }

    pub fn close_tab(&mut self, tab: TabId) {
        let Some(pane) = self.pane_of_tab(tab) else { return };
        crate::grid::end_rename(self, false);
        crate::grid::end_path_edit(self);
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
        let fav = root.as_deref().and_then(|p| self.favorite_index(p)).map(|i| &self.settings.favorites[i]).map(|f| (f.sort, f.desc));
        let Some(t) = self.tabs.get_mut(&tid) else { return };
        t.tree.clear();
        if let Some((sort, desc)) = fav {
            t.tree.sort = sort;
            t.tree.desc = desc;
        }
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
        t.filter.clear();
        t.deep = false;
        t.scan = None;
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
    /// The view it leaves is recorded for Back.
    pub fn navigate(&mut self, tid: TabId, target: Option<&str>, rooted: bool) -> bool {
        crate::grid::end_rename(self, false);
        let snap = self.tabs.get_mut(&tid).map(Tab::snapshot);
        if !self.show(tid, target, rooted) {
            return false;
        }
        if let (Some(snap), Some(t)) = (snap, self.tabs.get_mut(&tid)) {
            push_history(&mut t.back, snap);
            t.fwd.clear();
        }
        true
    }

    /// Goes back (or forward) in `tid`'s history: restores that view's root, expanded folders,
    /// scroll position and selection. Views identical to the current one are skipped.
    pub fn nav_history(&mut self, tid: TabId, back: bool) -> bool {
        crate::grid::end_rename(self, false);
        let Some(t) = self.tabs.get_mut(&tid) else { return false };
        let cur = t.snapshot();
        let same_view = |e: &HistEntry| e.root.as_deref().map(path_key) == cur.root.as_deref().map(path_key) && e.selected == cur.selected;
        let e = loop {
            let stack = if back { &mut t.back } else { &mut t.fwd };
            match stack.pop() {
                Some(e) if same_view(&e) => continue,
                Some(e) => break e,
                None => return false,
            }
        };
        let same_root = e.root.as_deref().map(path_key) == cur.root.as_deref().map(path_key) && t.error.is_none() && !t.tree.roots.is_empty();
        let root = match e.root.as_deref() {
            Some(r) if !same_root => match nearest_existing(r) {
                Some(r) => Some(r),
                None => {
                    let msg = format!("Folder not found: {r}");
                    push_history(if back { &mut t.back } else { &mut t.fwd }, e);
                    self.set_status_error(msg);
                    return false;
                }
            },
            _ => None,
        };
        push_history(if back { &mut t.fwd } else { &mut t.back }, cur);
        if !same_root {
            self.set_root(tid, root);
        }
        let Some(t) = self.tabs.get_mut(&tid) else { return false };
        let root_key = t.root.as_deref().map(path_key);
        t.want_expanded.extend(e.expanded.into_iter().filter(|k| Some(k) != root_key.as_ref()));
        t.pending_select = e.selected;
        t.reveal = None;
        t.restore_top = Some(e.top);
        self.process_wants(tid);
        self.after_change(tid);
        true
    }

    fn show(&mut self, tid: TabId, target: Option<&str>, rooted: bool) -> bool {
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

    /// Re-roots `tid` at an ancestor of its root (`None` = This PC), keeping the folder it came
    /// from expanded into view and selected.
    pub fn navigate_up(&mut self, tid: TabId, target: Option<&str>) -> bool {
        let from = self.tabs.get(&tid).and_then(|t| t.root.clone());
        if !self.navigate(tid, target, target.is_some()) {
            return false;
        }
        let Some(from) = from else { return true };
        let top = target.map(path_key);
        if top.as_deref() == Some(path_key(&from).as_str()) || target.is_some_and(|t| !is_same_or_inside(&from, t)) {
            return true;
        }
        let mut between = Vec::new();
        let mut p = parent_path(&from);
        while let Some(dir) = p {
            let key = path_key(&dir);
            if top.as_deref() == Some(key.as_str()) {
                break;
            }
            p = parent_path(&dir);
            between.push(key);
        }
        let key = path_key(&from);
        if let Some(t) = self.tabs.get_mut(&tid) {
            t.want_expanded.extend(between);
            t.pending_select = vec![key.clone()];
            t.reveal = Some((key, Reveal::Center));
        }
        self.process_wants(tid);
        self.after_change(tid);
        true
    }

    /// Goes to a typed or pasted path (quotes and %VARS% allowed; empty = This PC). Folders and
    /// archives become the tab root (unless `zip_as_file`); a file roots the tab at its folder
    /// with the file selected.
    pub fn go_to(&mut self, tid: TabId, text: &str, zip_as_file: bool) -> bool {
        let path = expand_env(&clean_path_input(text));
        if path.is_empty() {
            return self.navigate(tid, None, true);
        }
        let file = loader::entry_for_path(&path).filter(|e| e.kind == Kind::File || (zip_as_file && e.kind == Kind::Zip));
        let Some((parent, file)) = file.and_then(|e| parent_path(&e.path).map(|p| (p, e.path))) else {
            return self.navigate(tid, Some(&path), true);
        };
        if !self.navigate(tid, Some(&parent), true) {
            return false;
        }
        let key = path_key(&file);
        if let Some(t) = self.tabs.get_mut(&tid) {
            t.pending_select = vec![key.clone()];
            t.reveal = Some((key, Reveal::Center));
        }
        self.process_wants(tid);
        self.after_change(tid);
        true
    }

    /// Shows where a logged action happened in the active tab: roots it at the folder holding the
    /// affected items (results first, else the originals) with them selected, else at the folder
    /// the action happened in (or its nearest existing ancestor).
    pub fn jump_to_log(&mut self, e: &crate::actionlog::Entry) {
        let Some(tid) = self.active_tab() else { return };
        let exists = |p: &&String| !p.is_empty() && loader::entry_for_path(p).is_some();
        let mut targets: Vec<&String> = e.items.iter().map(|(_, to)| to).filter(exists).collect();
        if targets.is_empty() {
            targets = e.items.iter().map(|(from, _)| from).filter(exists).collect();
        }
        if let Some(parent) = targets.first().and_then(|p| parent_path(p)) {
            let keys: Vec<String> = targets.iter().filter(|p| parent_path(p).as_deref() == Some(parent.as_str())).map(|p| path_key(p)).collect();
            if !self.navigate(tid, Some(&parent), true) {
                return;
            }
            if let Some(t) = self.tabs.get_mut(&tid) {
                t.reveal = keys.first().map(|k| (k.clone(), Reveal::Center));
                t.pending_select = keys;
            }
            self.process_wants(tid);
            self.after_change(tid);
            return;
        }
        match e.folder.as_deref().and_then(nearest_existing) {
            Some(dir) => {
                self.navigate(tid, Some(&dir), true);
            }
            None => self.set_status_error("That location no longer exists".into()),
        }
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
        // Scanned contents lack exact icons and details: list the folder properly.
        let need_load = n.children.is_none() || n.scanned;
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
        let deep = t.deep;
        let dirs: Vec<NodeId> = t.tree.loaded_dirs().into_iter().map(|(id, _)| id).collect();
        for id in dirs {
            self.load_node(tid, id);
        }
        if deep {
            self.start_scan(tid);
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
                            t.tree.merge_children(node, entries);
                            if let Some(n) = t.tree.get_mut(node).filter(|n| n.expanded) {
                                n.scanned = false;
                            }
                            if !details.is_empty() {
                                loader::submit(Job::Details { tab, node, path, items: details });
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
                            let (ft, fi) = loader::folder_type();
                            t.tree.set_zip_children(node, items, ft, fi);
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
                    // A pending scroll restore reveals the selection once it's applied.
                    if t.reveal.is_none() && t.restore_top.is_none() {
                        let key = path_key(&t.tree.get(found[0]).unwrap().path);
                        t.reveal = Some((key, Reveal::Visible));
                    }
                }
            }
        }
        t.ensure_rows();
        // Scroll restore once all expansions are done, then keep the selection in view.
        if t.want_expanded.is_empty() {
            if let Some(top) = t.restore_top.take() {
                t.top = top;
                if t.reveal.is_none() && t.pending_select.is_empty() {
                    let first = t.tree.selection(&t.rows).first().copied();
                    if let Some(n) = first.and_then(|id| t.tree.get(id)) {
                        t.reveal = Some((path_key(&n.path), Reveal::Visible));
                    }
                }
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
                        t.top = reveal_pinned(t.top, row, view, mode, t.pinned());
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

    fn view_rows_of(&mut self, tid: TabId) -> usize {
        let Some(pane) = self.pane_of_tab(tid) else { return 20 };
        if self.panes[&pane].active_tab() == Some(tid) {
            return crate::grid::scroll_view_rows(self, pane);
        }
        self.panes.get(&pane).map(|p| p.view_rows).unwrap_or(20)
    }

    /// Scrolls `tid` so `row` is visible.
    pub fn scroll_into_view(&mut self, tid: TabId, row: usize) {
        let view = self.view_rows_of(tid);
        if let Some(t) = self.tabs.get_mut(&tid) {
            t.ensure_rows();
            t.top = reveal_pinned(t.top, row, view, Reveal::Visible, t.pinned());
        }
    }

    /// Repaints the tab if it is shown.
    pub fn after_change(&mut self, tid: TabId) {
        if let Some(pane) = self.pane_of_tab(tid) {
            if crate::mainwin::tabs_stale(self, pane) {
                crate::mainwin::relayout(self);
            }
            if self.panes[&pane].active_tab() == Some(tid) {
                crate::grid::sync(self, pane);
            }
        }
        invalidate(self.hwnd);
        self.preview_follow();
    }

    /// Clears the filter and turns subfolder search off (Esc, the × button).
    pub fn clear_filter(&mut self, tid: TabId) {
        self.stop_deep(tid);
        self.set_filter(tid, "");
        self.reveal_focus(tid);
        self.after_change(tid);
    }

    /// Turns subfolder search on or off for the tab (not available in This PC).
    pub fn toggle_deep(&mut self, tid: TabId) {
        let Some(t) = self.tabs.get_mut(&tid) else { return };
        if t.deep {
            self.stop_deep(tid);
            self.reveal_focus(tid);
        } else if t.root.is_none() || t.error.is_some() {
            self.set_status("Subfolder search works inside a folder or drive \u{2014} open one first".into());
            return;
        } else {
            t.deep = true;
            self.start_scan(tid);
        }
        self.after_change(tid);
    }

    /// Turns subfolder search on or off (no-op if already so).
    pub fn set_deep(&mut self, tid: TabId, on: bool) {
        if self.tabs.get(&tid).is_some_and(|t| t.deep != on) {
            self.toggle_deep(tid);
        }
    }

    /// Starts (or restarts) the background scan behind subfolder search.
    fn start_scan(&mut self, tid: TabId) {
        let Some(t) = self.tabs.get_mut(&tid) else { return };
        let Some(root) = t.root.clone() else { return };
        t.scan = Some(ScanState {
            handle: crate::scan::start(tid, root),
            items: 0,
            folders: 0,
            done: false,
            errors: 0,
            index: HashMap::new(),
        });
        t.mark();
    }

    /// Turns subfolder search off: stops the scan, keeps the focused item visible by opening its
    /// folders, and forgets scanned contents of collapsed folders.
    fn stop_deep(&mut self, tid: TabId) {
        let Some(t) = self.tabs.get_mut(&tid) else { return };
        if !t.deep {
            return;
        }
        t.deep = false;
        t.scan = None;
        let mut closed = Vec::new();
        let mut cur = t.focus.and_then(|f| t.tree.get(f)).and_then(|n| n.parent);
        while let Some(id) = cur {
            let Some(n) = t.tree.get(id) else { break };
            if !n.expanded {
                closed.push(id);
            }
            cur = n.parent;
        }
        for id in closed {
            self.expand(tid, id);
        }
        let Some(t) = self.tabs.get_mut(&tid) else { return };
        t.tree.drop_scanned();
        if !t.anchor.is_some_and(|a| t.tree.exists(a)) {
            t.anchor = t.focus;
        }
        t.mark();
    }

    fn reveal_focus(&mut self, tid: TabId) {
        let Some(t) = self.tabs.get_mut(&tid) else { return };
        if let Some(r) = t.focus.and_then(|f| t.row_of(f)) {
            self.scroll_into_view(tid, r);
        }
    }

    /// Merges subfolder-search scan results (WM_SCAN).
    pub fn on_scan(&mut self) {
        let mut touched = HashSet::new();
        for msg in crate::scan::take() {
            match msg {
                ScanMsg::Batch { tab, id, dirs, items, folders } => {
                    let Some(t) = self.tabs.get_mut(&tab) else { continue };
                    let Some(s) = t.scan.as_mut().filter(|s| s.handle.id == id) else { continue };
                    s.items = items;
                    s.folders = folders;
                    for (dir, entries) in dirs {
                        let key = path_key(&dir);
                        let indexed = s.index.remove(&key).filter(|&n| t.tree.get(n).is_some_and(|n| path_key(&n.path) == key));
                        let Some(node) = indexed.or_else(|| t.tree.find_path(&dir)) else { continue };
                        let Some(n) = t.tree.get(node) else { continue };
                        if !n.kind.is_fs_dir() {
                            continue;
                        }
                        // Folders listed normally keep their (richer) listing.
                        if n.children.is_none() || n.scanned {
                            t.tree.merge_children(node, entries);
                            if let Some(n) = t.tree.get_mut(node) {
                                n.scanned = true;
                            }
                        }
                        for &c in t.tree.children(node) {
                            if let Some(cn) = t.tree.get(c).filter(|cn| cn.kind == Kind::Dir) {
                                s.index.insert(path_key(&cn.path), c);
                            }
                        }
                    }
                    touched.insert(tab);
                }
                ScanMsg::Done { tab, id, items, errors } => {
                    let Some(t) = self.tabs.get_mut(&tab) else { continue };
                    let Some(s) = t.scan.as_mut().filter(|s| s.handle.id == id) else { continue };
                    s.items = items;
                    s.done = true;
                    s.errors = errors;
                    s.index = HashMap::new();
                    touched.insert(tab);
                }
            }
        }
        for tab in touched {
            if let Some(t) = self.tabs.get_mut(&tab) {
                t.mark();
            }
            self.after_change(tab);
        }
    }

    /// Narrows the tab to rows whose names contain `text` ("" clears the filter).
    pub fn set_filter(&mut self, tid: TabId, text: &str) {
        let Some(t) = self.tabs.get_mut(&tid) else { return };
        if t.filter == text {
            return;
        }
        t.filter = text.to_string();
        t.mark();
        t.ensure_rows();
        // Move focus to the first match unless it already is on one.
        if !text.is_empty() {
            let is_match = |id: NodeId| !id.is_placeholder() && !t.filtered.context.contains(&id);
            if !t.focus.is_some_and(is_match) {
                if let Some(&id) = t.rows.iter().find(|&&id| is_match(id)) {
                    t.set_focus(id);
                }
            }
        }
        if let Some(r) = t.focus.and_then(|f| t.row_of(f)) {
            self.scroll_into_view(tid, r);
        }
        self.after_change(tid);
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
            if let Some(entry) = r.log {
                crate::actionlog::push(entry);
                crate::logwin::refresh();
            }
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

    /// Persistent status line for a filtered active tab (or one with subfolder search on).
    pub fn filter_status(&self) -> Option<String> {
        let t = self.active_tab().and_then(|t| self.tabs.get(&t)).filter(|t| !t.filter.is_empty() || t.deep)?;
        let f = &t.filtered;
        let mut s = if t.filter.is_empty() {
            "Subfolder search on \u{2014} press Ctrl+F and type to search all subfolders".to_string()
        } else {
            let hidden: usize = f.hidden.iter().map(|h| h.1).sum();
            format!("Filter \u{201C}{}\u{201D}: {} of {} items match, {} hidden", t.filter, format_count(f.matched), format_count(f.total), format_count(hidden))
        };
        if let Some(sc) = &t.scan {
            if !sc.done {
                s += &format!(" \u{00B7} searching subfolders\u{2026} {} items in {} folders so far (Alt+S stops)", format_count(sc.items), format_count(sc.folders));
            } else {
                s += &format!(" \u{00B7} all {} subfolders searched", format_count(sc.folders.saturating_sub(1)));
            }
            if sc.errors > 0 {
                s += &format!(" \u{00B7} {} folder(s) couldn't be read", format_count(sc.errors));
            }
        }
        s += " \u{2014} Esc clears it";
        Some(s)
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

    /// Adds `path` as a favorite, remembering `tid`'s sort for when it is opened.
    pub fn add_favorite(&mut self, tid: TabId, path: String) {
        if self.favorite_index(&path).is_some() {
            return;
        }
        let Some(t) = self.tabs.get(&tid) else { return };
        let (sort, desc) = (t.tree.sort, t.tree.desc);
        self.set_status(format!("Added favorite: {}", file_name(&path)));
        self.settings.favorites.push(Favorite { path, sort, desc });
        self.save_settings();
        self.refresh_drives();
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
        let (sort, desc, root) = (t.tree.sort, t.tree.desc, t.root.clone());
        if let Some(i) = root.and_then(|r| self.favorite_index(&r)) {
            let f = &mut self.settings.favorites[i];
            f.sort = sort;
            f.desc = desc;
            self.save_settings();
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
        crate::preview::set_dark(dark);
        for p in self.panes.values() {
            crate::theme::set_control_theme(p.grid(), dark, "DarkMode_Explorer");
        }
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

/// `reveal_top` for a list whose first `pin` rows never scroll; `view` counts the scrolling rows.
fn reveal_pinned(top: usize, row: usize, view: usize, mode: Reveal, pin: usize) -> usize {
    if row < pin {
        top.max(pin)
    } else {
        pin + reveal_top(top.saturating_sub(pin), row - pin, view, mode)
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

    #[test]
    fn reveal_with_pinned_root() {
        assert_eq!(reveal_pinned(0, 0, 10, Reveal::Visible, 1), 1);
        assert_eq!(reveal_pinned(30, 0, 10, Reveal::Center, 1), 30);
        assert_eq!(reveal_pinned(1, 10, 10, Reveal::Visible, 1), 1);
        assert_eq!(reveal_pinned(1, 11, 10, Reveal::Visible, 1), 2);
        assert_eq!(reveal_pinned(20, 5, 10, Reveal::Visible, 1), 5);
        assert_eq!(reveal_pinned(1, 50, 10, Reveal::Center, 1), 45);
        assert_eq!(reveal_pinned(0, 50, 10, Reveal::Center, 0), 45);
    }
}
