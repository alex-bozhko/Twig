//! The tree grid: a custom-drawn, virtualized tree with columns (Name / Date / Type / Size).
//! One grid window per pane; it always shows the pane's active tab.

use crate::app::{with_app, App, Tab};
use crate::fileops::Op;
use crate::layout::PaneId;
use crate::loader::TabId;
use crate::model::{Kind, NodeId, SortKey};
use crate::shell::{self, MenuItem, MenuResult, ID_CUSTOM_FIRST};
use crate::theme::glyph;
use crate::util::*;
use crate::win::*;
use std::time::Instant;
use windows::core::{w, HSTRING, PCWSTR, PWSTR};
use windows::Win32::Foundation::*;
use windows::Win32::Globalization::*;
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::System::Ole::*;
use windows::Win32::System::Time::*;
use windows::Win32::UI::Controls::*;
use windows::Win32::UI::Input::KeyboardAndMouse::*;
use windows::Win32::UI::Shell::*;
use windows::Win32::UI::WindowsAndMessaging::*;

const CHEVRON_UP: &str = "\u{E70E}";
const TITLES: [&str; 4] = ["Name", "Date modified", "Type", "Size"];
const INVALID_CHARS: &str = "\\/:*?\"<>|";

const C_EXPAND_ALL: u32 = ID_CUSTOM_FIRST + 1;
const C_COLLAPSE_ALL: u32 = ID_CUSTOM_FIRST + 2;
const C_NEW_FOLDER: u32 = ID_CUSTOM_FIRST + 3;
const C_NEW_FILE: u32 = ID_CUSTOM_FIRST + 4;
const C_PASTE: u32 = ID_CUSTOM_FIRST + 5;
const C_COPY_PATH: u32 = ID_CUSTOM_FIRST + 6;
const C_NEW_TAB: u32 = ID_CUSTOM_FIRST + 7;
const C_EXPLORER: u32 = ID_CUSTOM_FIRST + 8;
const C_OPEN: u32 = ID_CUSTOM_FIRST + 9;
const C_COPY: u32 = ID_CUSTOM_FIRST + 10;
const C_REFRESH: u32 = ID_CUSTOM_FIRST + 11;
const C_UNFAVORITE: u32 = ID_CUSTOM_FIRST + 12;

struct Press {
    pane: PaneId,
    tid: TabId,
    x: i32,
    y: i32,
    node: NodeId,
    /// Plain click inside a multi-selection: reduce to this item on mouse-up (unless a drag starts).
    deselect: bool,
}

struct Rename {
    edit: isize,
    pane: PaneId,
    tab: TabId,
    node: NodeId,
    original: String,
}

/// Path being typed into a tab's breadcrumb bar.
struct PathEdit {
    edit: isize,
    pane: PaneId,
    tab: TabId,
}

/// Grid interaction state shared by all grids (only one interaction happens at a time).
#[derive(Default)]
pub struct GridUi {
    press: Option<Press>,
    rename: Option<Rename>,
    path_edit: Option<PathEdit>,
    search: String,
    search_at: Option<Instant>,
    search_scope: Vec<NodeId>,
    hover: Option<(PaneId, usize)>,
    /// Hovered breadcrumb segment or Up button.
    bar_hover: Option<(PaneId, BarHot)>,
    /// The last click navigated via the breadcrumb/Up; its double-click must not hit the new layout.
    swallow_dbl: bool,
    /// Current drag-and-drop target row.
    pub drop: Option<(PaneId, NodeId)>,
    /// (pane, column, start x, start width at 96 DPI)
    col_drag: Option<(PaneId, usize, i32, i32)>,
    /// Right-click target: Some(false) = items, Some(true) = background, None = no menu.
    ctx_background: Option<bool>,
    wheel: i32,
    hwheel: i32,
    /// Truncated cell under the mouse and the in-place tooltip showing its full text.
    tip: Option<Tip>,
    tip_state: TipState,
    tip_hwnd: isize,
}

const TIMER_TIP: usize = 4;
const TIP_DELAY_MS: u32 = 500;
const TIP_POLL_MS: u32 = 150;

/// A cell whose text doesn't fit; its full text is shown in place after a short hover.
#[derive(PartialEq)]
struct Tip {
    pane: PaneId,
    row: usize,
    col: usize,
    text: String,
    /// Where the cell's text is drawn, in grid client coordinates.
    rect: RECT,
}

#[derive(Clone, Copy, PartialEq, Default)]
enum TipState {
    #[default]
    Pending,
    Shown,
    /// Hidden by a click/key/scroll; stays hidden until the mouse moves to another cell.
    Dismissed,
}

pub struct Metrics {
    pub header: i32,
    pub row: i32,
    indent: i32,
    chev: i32,
    icon: i32,
    gap: i32,
    pad: i32,
    cols: [i32; 4],
}

pub fn metrics(app: &App) -> Metrics {
    let d = app.dpi;
    Metrics {
        header: scale(26, d),
        row: app.fonts.ui_height + scale(8, d),
        indent: scale(16, d),
        chev: scale(16, d),
        icon: scale(16, d),
        gap: scale(6, d),
        pad: scale(6, d),
        cols: app.columns(),
    }
}

impl Metrics {
    fn total(&self) -> i32 {
        self.cols.iter().sum()
    }
    /// Left edge of the chevron for a node at `depth`.
    fn chevron_x(&self, scroll_x: i32, depth: u16) -> i32 {
        -scroll_x + self.pad + depth as i32 * self.indent
    }
    fn icon_x(&self, scroll_x: i32, depth: u16) -> i32 {
        self.chevron_x(scroll_x, depth) + self.chev + self.pad / 3
    }
    fn text_x(&self, scroll_x: i32, depth: u16) -> i32 {
        self.icon_x(scroll_x, depth) + self.icon + self.gap
    }
}

pub enum Hit {
    Divider(usize),
    Header(usize),
    Row { row: usize, node: NodeId, chevron: bool },
    /// Breadcrumb segment `index`; `target` None = This PC.
    Crumb { index: usize, target: Option<String> },
    /// Breadcrumb bar outside the clickable segments (click to type a path).
    CrumbBar,
    /// The Up button on the pinned root row.
    Up,
    Empty,
    Nothing,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum BarHot {
    Crumb(usize),
    Up,
}

/// Vertical layout: breadcrumb bar (zoomed or typing a path), column header, pinned root row, scrolling rows.
pub struct Geo {
    crumb: i32,
    header_y: i32,
    body: i32,
    pin: usize,
    scroll_y: i32,
    row: i32,
}

impl Geo {
    /// Call after `t.ensure_rows()`.
    fn of(m: &Metrics, t: &Tab) -> Geo {
        let crumb = m.header;
        let pin = t.pinned();
        let body = crumb + m.header;
        Geo { crumb, header_y: crumb, body, pin, scroll_y: body + pin as i32 * m.row, row: m.row }
    }
    /// Top edge of row `i`, if it is laid out (pinned or at/after `top`).
    fn row_y(&self, i: usize, top: usize) -> Option<i32> {
        if i < self.pin {
            Some(self.body + i as i32 * self.row)
        } else if i >= top {
            Some(self.scroll_y + (i - top) as i32 * self.row)
        } else {
            None
        }
    }
    /// Row index under `y` (which must be at or below `body`).
    fn row_at(&self, y: i32, top: usize) -> usize {
        if y < self.scroll_y {
            ((y - self.body) / self.row) as usize
        } else {
            top + ((y - self.scroll_y) / self.row) as usize
        }
    }
}

/// Top of the scrolling rows in `pane` (below the header and any pinned row).
pub fn scroll_area_top(app: &mut App, pane: PaneId) -> i32 {
    let m = metrics(app);
    match active_tid(app, pane).and_then(|t| app.tabs.get_mut(&t)) {
        Some(t) => {
            t.ensure_rows();
            Geo::of(&m, t).scroll_y
        }
        None => m.header,
    }
}

/// Rows that fit in `pane`'s scrolling area for its active tab's current layout.
pub fn scroll_view_rows(app: &mut App, pane: PaneId) -> usize {
    let m = metrics(app);
    let h = client_rect(app.panes[&pane].grid()).bottom;
    ((h - scroll_area_top(app, pane)) / m.row).max(1) as usize
}

/// Width of the Up button at the right end of the pinned root row.
fn up_button(m: &Metrics, w: i32, y: i32) -> RECT {
    rc(w - m.row - m.pad, y, m.row + m.pad, m.row)
}

/// One breadcrumb segment, laid out left to right.
struct Crumb {
    x: i32,
    w: i32,
    label: String,
    /// None = not clickable (current folder or elided "…"); Some(None) = This PC.
    target: Option<Option<String>>,
}

/// The address-field box around the breadcrumb (the path editor covers the same rect).
fn crumb_box(app: &App, w: i32) -> RECT {
    let m = metrics(app);
    rc(m.pad / 2, scale(2, app.dpi), w - m.pad, m.header - scale(5, app.dpi))
}

/// Gap between breadcrumb segments (holds the chevron).
fn crumb_sep(app: &App) -> i32 {
    scale(18, app.dpi)
}

/// Left edge of the first breadcrumb segment (the PC icon).
fn crumb_x0(app: &App) -> i32 {
    metrics(app).pad / 2 + scale(3, app.dpi)
}

/// Lays out the breadcrumb for `path` within width `w`; elides leading folders when it doesn't fit.
fn crumb_layout(app: &App, hdc: HDC, path: &str, w: i32) -> Vec<Crumb> {
    let m = metrics(app);
    let font = app.fonts.ui;
    let sep = crumb_sep(app);
    let x0 = crumb_x0(app);
    let mut segs: Vec<(String, Option<String>)> = crumbs(path).into_iter().map(|(l, p)| (l, Some(p))).collect();
    if let Some(last) = segs.last_mut() {
        last.1 = None;
    }
    let seg_w = |s: &str| text_width(hdc, font, s) + 2 * m.pad;
    let pc_w = m.chev + 2 * m.pad;
    let need = |segs: &[(String, Option<String>)], elided: bool| {
        pc_w + segs.iter().map(|(l, _)| sep + seg_w(l)).sum::<i32>() + if elided { sep + seg_w("\u{2026}") } else { 0 }
    };
    let mut elided = false;
    while segs.len() > 1 && need(&segs, elided) > w - 2 * x0 {
        segs.remove(0);
        elided = true;
    }
    let mut out = vec![Crumb { x: x0, w: pc_w, label: glyph::THIS_PC.to_string(), target: Some(None) }];
    let mut x = x0 + pc_w;
    if elided {
        x += sep;
        let ew = seg_w("\u{2026}");
        out.push(Crumb { x, w: ew, label: "\u{2026}".into(), target: None });
        x += ew;
    }
    for (label, target) in segs {
        x += sep;
        let sw = seg_w(&label);
        out.push(Crumb { x, w: sw, label, target: target.map(Some) });
        x += sw;
    }
    out
}

fn tab_crumbs(app: &App, pane: PaneId, hdc: HDC, w: i32) -> Vec<Crumb> {
    let Some(t) = active_tid(app, pane).and_then(|t| app.tabs.get(&t)) else { return Vec::new() };
    match &t.root {
        Some(r) => crumb_layout(app, hdc, r, w),
        // This PC: just the (current, so unclickable) PC icon.
        None => {
            let m = metrics(app);
            vec![Crumb { x: crumb_x0(app), w: m.chev + 2 * m.pad, label: glyph::THIS_PC.to_string(), target: None }]
        }
    }
}

enum Deferred {
    Open(String),
    Explorer(String, bool),
    Properties(String),
    Drag(Vec<String>, DROPEFFECT),
    Recycle(TabId, Vec<String>, String),
}

fn active_tid(app: &App, pane: PaneId) -> Option<TabId> {
    app.panes.get(&pane).and_then(|p| p.active_tab())
}

// ---------- Window ----------

pub fn create(parent: HWND, pane: PaneId) -> HWND {
    unsafe {
        static REGISTER: std::sync::Once = std::sync::Once::new();
        REGISTER.call_once(|| {
            let wc = WNDCLASSEXW {
                cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
                style: CS_DBLCLKS,
                lpfnWndProc: Some(grid_proc),
                hInstance: instance(),
                hCursor: LoadCursorW(None, IDC_ARROW).unwrap_or_default(),
                lpszClassName: w!("TwigGrid"),
                ..Default::default()
            };
            RegisterClassExW(&wc);
        });
        let hwnd = CreateWindowExW(
            WINDOW_EX_STYLE(0),
            w!("TwigGrid"),
            PCWSTR::null(),
            WS_CHILD | WS_VISIBLE | WS_VSCROLL | WS_HSCROLL | WS_CLIPCHILDREN | WS_TABSTOP,
            0,
            0,
            0,
            0,
            parent,
            None,
            instance(),
            Some(pane as usize as *const std::ffi::c_void),
        )
        .unwrap_or_default();
        crate::dragdrop::register_grid(hwnd, pane);
        hwnd
    }
}

fn run_deferred(hwnd: HWND, d: Option<Deferred>) {
    let Some(d) = d else { return };
    match d {
        Deferred::Open(p) => {
            if let Err(e) = shell::open(hwnd, &p) {
                with_app(|a| a.set_status_error(format!("Open failed: {e}")));
            }
        }
        Deferred::Explorer(p, is_dir) => shell::open_in_explorer(hwnd, &p, is_dir),
        Deferred::Properties(p) => shell::properties(hwnd, &p),
        Deferred::Drag(paths, effects) => {
            let Some(obj) = shell::data_object(hwnd, &paths) else { return };
            unsafe {
                let _ = ReleaseCapture();
                let _ = SHDoDragDrop(hwnd, &obj, None, effects);
            }
        }
        Deferred::Recycle(tid, items, msg) => {
            let prompt = match items.as_slice() {
                [one] => format!("Move \"{}\" to the Recycle Bin?", file_name(one)),
                _ => format!("Move these {} items to the Recycle Bin?", items.len()),
            };
            let owner = unsafe { GetAncestor(hwnd, GA_ROOT) };
            let answer = unsafe { MessageBoxW(owner, &HSTRING::from(prompt), w!("Delete"), MB_YESNO | MB_ICONWARNING) };
            if answer == IDYES {
                with_app(|app| app.run_op(tid, Op::Delete { items, recycle: true }, Some(msg)));
            }
        }
    }
}

unsafe extern "system" fn grid_proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    if msg == WM_NCCREATE {
        let cs = lp.0 as *const CREATESTRUCTW;
        SetWindowLongPtrW(hwnd, GWLP_USERDATA, (*cs).lpCreateParams as isize);
        return DefWindowProcW(hwnd, msg, wp, lp);
    }
    let pane = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as u32;
    let (x, y) = (x_of(lp), y_of(lp));
    if matches!(
        msg,
        WM_LBUTTONDOWN | WM_RBUTTONDOWN | WM_LBUTTONDBLCLK | WM_KEYDOWN | WM_SYSKEYDOWN | WM_MOUSEWHEEL | WM_MOUSEHWHEEL | WM_VSCROLL | WM_HSCROLL
    ) {
        with_app(|app| hide_tip(app, true));
    }
    match msg {
        WM_PAINT => {
            if with_app(|app| {
                paint_buffered(hwnd, |hdc, r| draw(app, pane, hwnd, hdc, r));
                update_summary(app, pane);
            })
            .is_none()
            {
                let mut ps = PAINTSTRUCT::default();
                BeginPaint(hwnd, &mut ps);
                let _ = EndPaint(hwnd, &ps);
                post(hwnd, WM_REPAINT, 0, 0);
            }
            return LRESULT(0);
        }
        WM_ERASEBKGND => return LRESULT(1),
        WM_REPAINT => invalidate(hwnd),
        WM_SIZE | WM_GRID_RELAYOUT => {
            if with_app(|app| sync(app, pane)).is_none() {
                post(hwnd, WM_GRID_RELAYOUT, 0, 0);
            }
            return LRESULT(0);
        }
        WM_SETFOCUS | WM_KILLFOCUS => {
            invalidate(hwnd);
            post_main(WM_FOCUS_CHANGED);
        }
        WM_GETDLGCODE => return LRESULT((DLGC_WANTARROWS | DLGC_WANTCHARS) as isize),
        WM_CTLCOLOREDIT => {
            let hdc = HDC(wp.0 as *mut _);
            if let Some((text, bg)) = with_app(|a| (a.pal.text, a.pal.edit_bg)) {
                SetTextColor(hdc, text);
                SetBkColor(hdc, bg);
                SetDCBrushColor(hdc, bg);
                return LRESULT(GetStockObject(DC_BRUSH).0 as isize);
            }
        }
        WM_SETCURSOR if (lp.0 & 0xFFFF) as u32 == HTCLIENT => {
            let p = screen_to_client(hwnd, cursor_pos());
            let cursor = with_app(|app| {
                if app.grid_ui.col_drag.is_some() {
                    return Some(IDC_SIZEWE);
                }
                match hit(app, pane, p.x, p.y) {
                    Hit::Divider(_) => Some(IDC_SIZEWE),
                    Hit::CrumbBar => Some(IDC_IBEAM),
                    _ => None,
                }
            })
            .flatten();
            if let Some(id) = cursor {
                if let Ok(c) = LoadCursorW(None, id) {
                    SetCursor(c);
                    return LRESULT(1);
                }
            }
        }
        WM_LBUTTONDOWN => {
            let _ = SetFocus(hwnd);
            if with_app(|app| on_lbutton(app, pane, x, y)) == Some(true) {
                SetCapture(hwnd);
            }
            return LRESULT(0);
        }
        WM_LBUTTONDBLCLK => {
            let d = with_app(|app| on_dblclick(app, pane, x, y)).flatten();
            run_deferred(hwnd, d);
            return LRESULT(0);
        }
        WM_MOUSEMOVE => {
            let d = with_app(|app| on_move(app, pane, hwnd, x, y)).flatten();
            run_deferred(hwnd, d);
            return LRESULT(0);
        }
        WM_TIMER if wp.0 == TIMER_TIP => {
            with_app(|app| on_tip_timer(app, pane, hwnd));
            return LRESULT(0);
        }
        WM_MOUSELEAVE => {
            with_app(|app| {
                if app.grid_ui.tip.as_ref().is_some_and(|t| t.pane == pane) {
                    hide_tip(app, false);
                }
                if app.grid_ui.hover.is_some_and(|h| h.0 == pane) || app.grid_ui.bar_hover.is_some_and(|h| h.0 == pane) {
                    app.grid_ui.hover = None;
                    app.grid_ui.bar_hover = None;
                    invalidate(hwnd);
                }
            });
        }
        WM_LBUTTONUP => {
            with_app(|app| on_lbutton_up(app, pane));
            let _ = ReleaseCapture();
            return LRESULT(0);
        }
        WM_CAPTURECHANGED => {
            with_app(|app| {
                app.grid_ui.press = None;
                if app.grid_ui.col_drag.take().is_some() {
                    app.save_settings();
                }
            });
        }
        WM_RBUTTONDOWN => {
            let _ = SetFocus(hwnd);
            with_app(|app| on_rbutton(app, pane, x, y));
            return LRESULT(0);
        }
        WM_CONTEXTMENU => {
            let keyboard = x == -1 && y == -1;
            let prep = with_app(|app| prepare_menu(app, pane, hwnd, keyboard, POINT { x, y })).flatten();
            if let Some(prep) = prep {
                let res = shell::show_context_menu(hwnd, &prep.shell_paths, prep.pt, shift(), &prep.items);
                let d = with_app(|app| handle_menu(app, pane, prep, res)).flatten();
                run_deferred(hwnd, d);
            }
            return LRESULT(0);
        }
        WM_KEYDOWN => {
            let d = with_app(|app| on_key(app, pane, wp.0 as u16)).flatten();
            run_deferred(hwnd, d);
            return LRESULT(0);
        }
        WM_SYSKEYDOWN if wp.0 as u16 == VK_RETURN.0 => {
            let d = with_app(|app| {
                let tid = active_tid(app, pane)?;
                let t = app.tabs.get_mut(&tid)?;
                let id = t.focus?;
                let n = t.tree.get(id)?;
                (!n.kind.is_virtual()).then(|| Deferred::Properties(n.path.clone()))
            })
            .flatten();
            run_deferred(hwnd, d);
            return LRESULT(0);
        }
        WM_CHAR => {
            if let Some(ch) = char::from_u32(wp.0 as u32) {
                if ch >= ' ' && !ctrl() && !alt() {
                    with_app(|app| type_search_char(app, pane, ch));
                }
            }
            return LRESULT(0);
        }
        WM_VSCROLL => {
            with_app(|app| on_vscroll(app, pane, hwnd, (wp.0 & 0xFFFF) as i32));
            return LRESULT(0);
        }
        WM_HSCROLL => {
            with_app(|app| on_hscroll(app, pane, hwnd, (wp.0 & 0xFFFF) as i32));
            return LRESULT(0);
        }
        WM_MOUSEWHEEL | WM_MOUSEHWHEEL => {
            let delta = ((wp.0 >> 16) & 0xFFFF) as u16 as i16 as i32;
            let horizontal = msg == WM_MOUSEHWHEEL || shift();
            with_app(|app| on_wheel(app, pane, delta, horizontal, msg == WM_MOUSEHWHEEL));
            return LRESULT(0);
        }
        WM_RENAME_END => {
            let (mode, edit) = (wp.0, lp.0);
            with_app(|app| {
                if app.grid_ui.rename.as_ref().is_some_and(|r| r.edit == edit) {
                    finish_rename(app, mode);
                }
            });
            return LRESULT(0);
        }
        WM_PATH_END => {
            let (go, edit) = (wp.0 == 1, lp.0);
            with_app(|app| {
                if app.grid_ui.path_edit.as_ref().is_some_and(|pe| pe.edit == edit) {
                    finish_path_edit(app, go);
                }
            });
            return LRESULT(0);
        }
        WM_DESTROY => {
            let _ = RevokeDragDrop(hwnd);
        }
        _ => {}
    }
    DefWindowProcW(hwnd, msg, wp, lp)
}

// ---------- Layout & scrolling ----------

/// Recomputes the viewport, clamps scroll positions, updates scrollbars and repaints.
pub fn sync(app: &mut App, pane: PaneId) {
    let Some(p) = app.panes.get(&pane) else { return };
    let hwnd = p.grid();
    let tid = p.active_tab();
    let m = metrics(app);
    let r = client_rect(hwnd);
    let (w, h) = (r.right - r.left, r.bottom - r.top);
    let (n, top, sx, pin, view) = match tid.and_then(|t| app.tabs.get_mut(&t)) {
        Some(t) => {
            t.ensure_rows();
            let geo = Geo::of(&m, t);
            let view = ((h - geo.scroll_y) / m.row).max(1) as usize;
            let n = if t.error.is_some() { 0 } else { t.rows.len() };
            t.top = t.top.min(n.saturating_sub(view)).max(geo.pin);
            t.scroll_x = t.scroll_x.clamp(0, (m.total() - w).max(0));
            (n, t.top, t.scroll_x, geo.pin, view)
        }
        None => (0, 0, 0, 0, ((h - m.header) / m.row).max(1) as usize),
    };
    if let Some(p) = app.panes.get_mut(&pane) {
        p.view_rows = view;
    }
    unsafe {
        let mut si = SCROLLINFO {
            cbSize: std::mem::size_of::<SCROLLINFO>() as u32,
            fMask: SIF_ALL,
            nMin: 0,
            nMax: (n as i32 - pin as i32 - 1).max(0),
            nPage: view as u32,
            nPos: (top - pin) as i32,
            nTrackPos: 0,
        };
        SetScrollInfo(hwnd, SB_VERT, &si, true);
        si.nMax = (m.total() - 1).max(0);
        si.nPage = w.max(0) as u32;
        si.nPos = sx;
        SetScrollInfo(hwnd, SB_HORZ, &si, true);
    }
    invalidate(hwnd);
    place_rename(app);
    place_path_edit(app);
}

pub fn scroll_rows(app: &mut App, pane: PaneId, delta: i64) {
    let Some(tid) = active_tid(app, pane) else { return };
    if let Some(t) = app.tabs.get_mut(&tid) {
        t.top = (t.top as i64 + delta).max(0) as usize;
    }
    sync(app, pane);
}

fn scroll_px(app: &mut App, pane: PaneId, dx: i32) {
    let Some(tid) = active_tid(app, pane) else { return };
    if let Some(t) = app.tabs.get_mut(&tid) {
        t.scroll_x = (t.scroll_x + dx).max(0);
    }
    sync(app, pane);
}

fn track_pos(hwnd: HWND, bar: SCROLLBAR_CONSTANTS) -> i32 {
    let mut si = SCROLLINFO { cbSize: std::mem::size_of::<SCROLLINFO>() as u32, fMask: SIF_TRACKPOS, ..Default::default() };
    unsafe {
        let _ = GetScrollInfo(hwnd, bar, &mut si);
    }
    si.nTrackPos
}

fn on_vscroll(app: &mut App, pane: PaneId, hwnd: HWND, code: i32) {
    let Some(tid) = active_tid(app, pane) else { return };
    let view = app.panes[&pane].view_rows as i64;
    let Some(t) = app.tabs.get_mut(&tid) else { return };
    t.ensure_rows();
    let pin = t.pinned() as i64;
    let top = t.top as i64;
    let new = match code {
        c if c == SB_LINEUP.0 => top - 1,
        c if c == SB_LINEDOWN.0 => top + 1,
        c if c == SB_PAGEUP.0 => top - view,
        c if c == SB_PAGEDOWN.0 => top + view,
        c if c == SB_THUMBTRACK.0 || c == SB_THUMBPOSITION.0 => track_pos(hwnd, SB_VERT) as i64 + pin,
        c if c == SB_TOP.0 => 0,
        c if c == SB_BOTTOM.0 => i64::MAX / 2,
        _ => return,
    };
    scroll_rows(app, pane, new - top);
}

fn on_hscroll(app: &mut App, pane: PaneId, hwnd: HWND, code: i32) {
    let Some(tid) = active_tid(app, pane) else { return };
    let page = client_rect(hwnd).right;
    let line = scale(20, app.dpi);
    let Some(t) = app.tabs.get_mut(&tid) else { return };
    let x = t.scroll_x;
    let new = match code {
        c if c == SB_LINELEFT.0 => x - line,
        c if c == SB_LINERIGHT.0 => x + line,
        c if c == SB_PAGELEFT.0 => x - page,
        c if c == SB_PAGERIGHT.0 => x + page,
        c if c == SB_THUMBTRACK.0 || c == SB_THUMBPOSITION.0 => track_pos(hwnd, SB_HORZ),
        c if c == SB_LEFT.0 => 0,
        c if c == SB_RIGHT.0 => i32::MAX / 2,
        _ => return,
    };
    scroll_px(app, pane, new - x);
}

fn on_wheel(app: &mut App, pane: PaneId, delta: i32, horizontal: bool, native_h: bool) {
    let mut lines: u32 = 3;
    unsafe {
        let _ = SystemParametersInfoW(
            SPI_GETWHEELSCROLLLINES,
            0,
            Some(&mut lines as *mut u32 as *mut std::ffi::c_void),
            SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS(0),
        );
    }
    let ui = &mut app.grid_ui;
    if horizontal {
        ui.hwheel += delta;
        let notches = ui.hwheel / WHEEL_DELTA as i32;
        ui.hwheel %= WHEEL_DELTA as i32;
        // Vertical wheel delta > 0 means "up" (scroll left); horizontal wheel > 0 means right.
        let dir = if native_h { 1 } else { -1 };
        if notches != 0 {
            let step = scale(48, app.dpi);
            scroll_px(app, pane, dir * notches * step);
        }
    } else {
        ui.wheel += delta;
        let notches = ui.wheel / WHEEL_DELTA as i32;
        ui.wheel %= WHEEL_DELTA as i32;
        if notches != 0 {
            let per = if lines == u32::MAX { app.panes.get(&pane).map_or(10, |p| p.view_rows) as i64 } else { lines.max(1) as i64 };
            scroll_rows(app, pane, -(notches as i64) * per);
        }
    }
}

// ---------- Hit testing ----------

pub fn hit(app: &mut App, pane: PaneId, x: i32, y: i32) -> Hit {
    let m = metrics(app);
    let slop = scale(4, app.dpi);
    let Some(tid) = active_tid(app, pane) else { return Hit::Nothing };
    let Some(t) = app.tabs.get_mut(&tid) else { return Hit::Nothing };
    t.ensure_rows();
    let geo = Geo::of(&m, t);
    if y < geo.crumb {
        if t.editing_path {
            return Hit::Nothing;
        }
        let hwnd = app.panes[&pane].grid();
        let w = client_rect(hwnd).right;
        let crumbs = unsafe {
            let hdc = GetDC(hwnd);
            let c = tab_crumbs(app, pane, hdc, w);
            ReleaseDC(hwnd, hdc);
            c
        };
        return crumbs
            .into_iter()
            .enumerate()
            .find(|(_, c)| x >= c.x && x < c.x + c.w)
            .and_then(|(index, c)| c.target.map(|target| Hit::Crumb { index, target }))
            .unwrap_or(Hit::CrumbBar);
    }
    if y < geo.body {
        let mut edge = -t.scroll_x;
        for (c, w) in m.cols.iter().enumerate() {
            edge += w;
            if (x - edge).abs() <= slop {
                return Hit::Divider(c);
            }
        }
        let mut x0 = -t.scroll_x;
        for (c, w) in m.cols.iter().enumerate() {
            if x >= x0 && x < x0 + w {
                return Hit::Header(c);
            }
            x0 += w;
        }
        return Hit::Nothing;
    }
    if t.error.is_some() {
        return Hit::Nothing;
    }
    let row = geo.row_at(y, t.top);
    let Some(&node) = t.rows.get(row) else { return Hit::Empty };
    let Some(n) = t.tree.get(node) else { return Hit::Empty };
    if row < geo.pin {
        let w = client_rect(app.panes[&pane].grid()).right;
        let up = up_button(&m, w, geo.row_y(row, t.top).unwrap_or(0));
        if x >= up.left && x < up.right {
            return Hit::Up;
        }
    }
    let cx = m.chevron_x(t.scroll_x, n.depth);
    let chevron = n.expandable() && x >= cx && x < cx + m.chev;
    Hit::Row { row, node, chevron }
}

/// Node under a client point (for drag-and-drop).
pub fn node_at(app: &mut App, pane: PaneId, x: i32, y: i32) -> Option<NodeId> {
    match hit(app, pane, x, y) {
        Hit::Row { node, .. } => Some(node),
        _ => None,
    }
}

// ---------- Painting ----------

fn format_time(ft: u64) -> String {
    if ft == 0 {
        return String::new();
    }
    unsafe {
        let f = FILETIME { dwLowDateTime: ft as u32, dwHighDateTime: (ft >> 32) as u32 };
        let (mut utc, mut local) = (SYSTEMTIME::default(), SYSTEMTIME::default());
        if FileTimeToSystemTime(&f, &mut utc).is_err() || SystemTimeToTzSpecificLocalTime(None, &utc, &mut local).is_err() {
            return String::new();
        }
        let (mut d, mut t) = ([0u16; 80], [0u16; 80]);
        GetDateFormatEx(PCWSTR::null(), DATE_SHORTDATE, Some(&local), PCWSTR::null(), Some(&mut d), PCWSTR::null());
        GetTimeFormatEx(PCWSTR::null(), TIME_NOSECONDS, Some(&local), PCWSTR::null(), Some(&mut t));
        format!("{} {}", from_wide(&d), from_wide(&t))
    }
}

/// Refreshes the status-bar item summary from the active pane's tab.
fn update_summary(app: &mut App, pane: PaneId) {
    if pane != app.active_pane {
        return;
    }
    let s = match active_tid(app, pane).and_then(|tid| app.tabs.get_mut(&tid)) {
        Some(t) => {
            t.ensure_rows();
            t.summary()
        }
        None => String::new(),
    };
    crate::mainwin::set_summary(app, s);
}

fn draw(app: &mut App, pane: PaneId, hwnd: HWND, hdc: HDC, r: &RECT) {
    let m = metrics(app);
    let pal = app.pal;
    let (font, glyph_small) = (app.fonts.ui, app.fonts.glyph_small);
    fill(hdc, r, pal.bg);
    let Some(tid) = active_tid(app, pane) else { return };
    let focus_hwnd = unsafe { GetFocus() };
    let focused = focus_hwnd == hwnd || unsafe { IsChild(hwnd, focus_hwnd).as_bool() };
    // Selection stays accent-colored in the active pane even while the toolbar has focus.
    let sel_color = if pane == app.active_pane && unsafe { GetForegroundWindow() } == app.hwnd { pal.sel } else { pal.sel_inactive };
    let hover = app.grid_ui.hover.filter(|h| h.0 == pane).map(|h| h.1);
    let drop = app.grid_ui.drop.filter(|d| d.0 == pane).map(|d| d.1);
    let renaming = app.grid_ui.rename.as_ref().filter(|rn| rn.tab == tid).map(|rn| rn.node);
    let view = app.panes[&pane].view_rows;
    let cut = app.cut.as_ref().map(|(_, set)| set);
    let bar_hover = app.grid_ui.bar_hover.filter(|h| h.0 == pane).map(|h| h.1);
    let crumbs = if app.tabs.get(&tid).is_some_and(|t| !t.editing_path) { tab_crumbs(app, pane, hdc, r.right) } else { Vec::new() };
    let (cbox, sep, glyph_mid) = (crumb_box(app, r.right), crumb_sep(app), app.fonts.glyph_mid);
    let Some(t) = app.tabs.get_mut(&tid) else { return };
    t.ensure_rows();
    let geo = Geo::of(&m, t);
    let w = r.right;
    let sx = t.scroll_x;

    unsafe {
        SelectObject(hdc, font);
    }
    // Breadcrumb (or the path editor's backdrop).
    if geo.crumb > 0 {
        fill(hdc, &rc(0, geo.crumb - 1, w, 1), pal.border);
        let editing = crumbs.is_empty();
        if !editing {
            frame(hdc, &cbox, pal.border, 1);
        }
        let last = crumbs.len().saturating_sub(1);
        for (i, c) in crumbs.iter().enumerate() {
            let cr = rc(c.x, scale(5, app.dpi), c.w, geo.crumb - scale(11, app.dpi));
            if c.target.is_some() && bar_hover == Some(BarHot::Crumb(i)) {
                fill(hdc, &cr, pal.hover);
            }
            let color = if i == last { pal.text } else { pal.text_soft };
            if i > 0 {
                unsafe {
                    SelectObject(hdc, glyph_mid);
                }
                draw_text(hdc, glyph::CHEVRON_RIGHT, &rc(c.x - sep, 0, sep, geo.crumb - scale(1, app.dpi)), pal.text_dim, DT_CENTER_V);
            }
            unsafe {
                SelectObject(hdc, if i == 0 { glyph_mid } else { font });
            }
            draw_text(hdc, &c.label, &rc(c.x, 0, c.w, geo.crumb - scale(1, app.dpi)), color, DT_CENTER_V);
        }
        unsafe {
            SelectObject(hdc, font);
        }
    }
    // Header.
    let hy = geo.header_y;
    fill(hdc, &rc(0, hy, w, m.header), pal.toolbar);
    fill(hdc, &rc(0, hy + m.header - 1, w, 1), pal.border);
    let mut x0 = -sx;
    let sort_col = t.tree.sort.column();
    for (c, title) in TITLES.iter().enumerate() {
        let cw = m.cols[c];
        let right = c == 3;
        let flags = if right { DT_RIGHT_V } else { DT_LEFT_V };
        let sorted = c == sort_col;
        let tr = rc(x0 + m.pad, hy, cw - 2 * m.pad, m.header);
        draw_text(hdc, title, &tr, if sorted { pal.text } else { pal.text_soft }, flags);
        fill(hdc, &rc(x0 + cw - 1, hy + m.header / 4, 1, m.header / 2), pal.border);
        if sorted {
            // Sort arrow right after the title (before it for the right-aligned Size column).
            let aw = scale(16, app.dpi);
            let tw = text_width(hdc, font, title).min(tr.right - tr.left);
            let ax = if right { tr.right - tw - aw } else { tr.left + tw };
            unsafe {
                SelectObject(hdc, glyph_small);
            }
            let g = if t.tree.desc { glyph::CHEVRON_DOWN } else { CHEVRON_UP };
            if ax >= x0 && ax + aw <= x0 + cw {
                draw_text(hdc, g, &rc(ax, hy, aw, m.header), pal.accent, DT_CENTER_V);
            }
            unsafe {
                SelectObject(hdc, font);
            }
        }
        x0 += cw;
    }

    if let Some(err) = &t.error {
        draw_text(hdc, err, &rc(0, geo.body, w, m.row * 3), pal.error, DT_CENTER_V);
        return;
    }

    let last = (t.top + view + 1).min(t.rows.len());
    for i in (0..geo.pin).chain(t.top..last) {
        let id = t.rows[i];
        let Some(y) = geo.row_y(i, t.top) else { continue };
        let pinned = i < geo.pin;
        if id == NodeId::SECTION {
            draw_section(hdc, &m, sx, y, w, glyph_small, font, &pal, "Favorites");
            continue;
        }
        if id == NodeId::SCAN_NOTE {
            if let Some(sc) = &t.scan {
                let label = format!("Searching subfolders\u{2026} {} items in {} folders so far (Alt+S stops)", format_count(sc.items), format_count(sc.folders));
                let tx = m.text_x(sx, 0);
                draw_text(hdc, &label, &rc(tx, y, (w - tx - m.pad).max(0), m.row), pal.text_dim, DT_LEFT_V);
            }
            continue;
        }
        if let Some(&(depth, count)) = id.as_hidden_marker().and_then(|k| t.filtered.hidden.get(k)) {
            let tx = m.text_x(sx, depth);
            let label = if count == 1 { "1 item hidden by the filter".to_string() } else { format!("{count} items hidden by the filter") };
            draw_text(hdc, &label, &rc(tx, y, (w - tx - m.pad).max(0), m.row), pal.text_dim, DT_LEFT_V);
            continue;
        }
        let Some(n) = t.tree.get(id) else { continue };
        let row = rc(0, y, w, m.row);
        let bg = if n.selected {
            sel_color
        } else if drop == Some(id) || hover == Some(i) {
            pal.hover
        } else {
            pal.bg
        };
        if bg != pal.bg {
            fill(hdc, &row, bg);
        } else if pinned {
            wash(hdc, &row, pal.accent, 24);
        }
        if pinned {
            fill(hdc, &rc(0, y, scale(3, app.dpi), m.row), pal.accent);
            fill(hdc, &rc(0, geo.scroll_y - 1, w, 1), pal.border);
        }
        if drop == Some(id) {
            frame(hdc, &row, pal.accent, 1);
        } else if focused && t.focus == Some(id) && !n.selected {
            frame(hdc, &row, pal.border, 1);
        }
        let context = t.filtered.context.contains(&id);
        let dim = context || n.is_hidden() || cut.is_some_and(|c| c.contains(&path_key(&n.path)));
        let color = if dim { pal.text_dim } else { pal.text };

        // Name cell, clipped to the column.
        unsafe {
            SaveDC(hdc);
            IntersectClipRect(hdc, -sx, y, -sx + m.cols[0] - m.pad / 2, y + m.row);
        }
        if n.expandable() {
            let open = n.expanded || t.filtered.opened.contains(&id);
            let g = if open { glyph::CHEVRON_DOWN } else { glyph::CHEVRON_RIGHT };
            unsafe {
                SelectObject(hdc, glyph_small);
            }
            draw_text(hdc, g, &rc(m.chevron_x(sx, n.depth), y, m.chev, m.row), pal.text_dim, DT_CENTER_V);
            unsafe {
                SelectObject(hdc, font);
            }
        }
        let ix = m.icon_x(sx, n.depth);
        let iy = y + (m.row - m.icon) / 2;
        draw_icon(hdc, n.icon, ix, iy, m.icon);
        if dim {
            wash(hdc, &rc(ix, iy, m.icon, m.icon), bg, 128);
        }
        if renaming != Some(id) {
            let tx = m.text_x(sx, n.depth);
            if !context {
                if let Some((a, b)) = find_ci(&n.name, &t.filter) {
                    let hx = tx + text_width(hdc, font, &n.name[..a]);
                    let hw = text_width(hdc, font, &n.name[a..b]);
                    wash(hdc, &rc(hx, y + m.row / 6, hw, m.row - m.row / 3), pal.accent, 90);
                }
            }
            draw_text(hdc, &n.name, &rc(tx, y, -sx + m.cols[0] - m.pad - tx, m.row), color, DT_LEFT_V);
        }
        unsafe {
            let _ = RestoreDC(hdc, -1);
        }

        // Other columns.
        let mut cx = -sx + m.cols[0];
        if n.kind != Kind::Drive {
            draw_text(hdc, &format_time(n.modified), &rc(cx + m.pad, y, m.cols[1] - 2 * m.pad, m.row), color, DT_LEFT_V);
        }
        cx += m.cols[1];
        draw_text(hdc, &n.type_name, &rc(cx + m.pad, y, m.cols[2] - 2 * m.pad, m.row), color, DT_LEFT_V);
        cx += m.cols[2];
        if pinned {
            let up = up_button(&m, w, y);
            fill(hdc, &up, if bar_hover == Some(BarHot::Up) { pal.hover } else { bg });
            if bg == pal.bg && bar_hover != Some(BarHot::Up) {
                wash(hdc, &up, pal.accent, 24);
            }
            unsafe {
                SelectObject(hdc, glyph_small);
            }
            draw_text(hdc, glyph::UP, &up, pal.text, DT_CENTER_V);
            unsafe {
                SelectObject(hdc, font);
            }
            continue;
        }
        let size = size_text(n);
        if !size.is_empty() {
            draw_text(hdc, &size, &rc(cx + m.pad, y, m.cols[3] - 2 * m.pad, m.row), color, DT_RIGHT_V);
        }
    }
}

// ---------- Selection helpers ----------

/// Draws a "★ Label ────" section header row.
#[allow(clippy::too_many_arguments)]
fn draw_section(hdc: HDC, m: &Metrics, sx: i32, y: i32, w: i32, glyph_small: HFONT, font: HFONT, pal: &crate::theme::Palette, label: &str) {
    unsafe {
        SelectObject(hdc, glyph_small);
    }
    draw_text(hdc, glyph::STAR, &rc(m.chevron_x(sx, 0), y, m.chev, m.row), pal.accent, DT_CENTER_V);
    unsafe {
        SelectObject(hdc, font);
    }
    let tx = m.icon_x(sx, 0);
    let tw = text_width(hdc, font, label);
    draw_text(hdc, label, &rc(tx, y, tw + m.pad, m.row), pal.text_dim, DT_LEFT_V);
    let lx = tx + tw + m.pad;
    if lx < w {
        fill(hdc, &rc(lx, y + m.row / 2, w - lx - m.pad, 1), pal.border);
    }
}

fn select_range(t: &mut Tab, to: NodeId, add: bool) {
    let Some(b) = t.row_of(to) else { return };
    let a = t.anchor.and_then(|a| t.row_of(a)).unwrap_or(b);
    if !add {
        t.tree.clear_selection();
    }
    for i in a.min(b)..=a.max(b) {
        let id = t.rows[i];
        if id == to || !t.filtered.context.contains(&id) {
            t.tree.set_selected(id, true);
        }
    }
    t.set_focus(to);
}

/// Keyboard-style move to `row`: Shift extends from the anchor, Ctrl moves focus only.
fn move_to(app: &mut App, tid: TabId, row: usize) {
    let (c, s) = (ctrl(), shift());
    let Some(t) = app.tabs.get_mut(&tid) else { return };
    t.ensure_rows();
    let mut row = row;
    if t.rows.get(row).is_some_and(|id| id.is_placeholder()) {
        // Step over header/marker rows in the direction of travel, else back the other way.
        let down = t.focus.and_then(|f| t.row_of(f)).map_or(true, |cur| cur < row);
        let real = |i: &usize| !t.rows[*i].is_placeholder();
        let ahead = if down { (row + 1..t.rows.len()).find(real) } else { (0..row).rev().find(real) };
        let behind = if down { (0..row).rev().find(real) } else { (row + 1..t.rows.len()).find(real) };
        let Some(r) = ahead.or(behind) else { return };
        row = r;
    }
    let Some(&id) = t.rows.get(row) else { return };
    if s {
        select_range(t, id, c);
    } else if c {
        t.set_focus(id);
    } else {
        t.select_only(id);
    }
    app.scroll_into_view(tid, row);
    app.after_change(tid);
}

/// File-system paths for `nodes`, extracting archive entries to temp files first.
fn materialize(t: &Tab, nodes: &[NodeId]) -> Result<Vec<String>, String> {
    let mut out = Vec::new();
    for &id in nodes {
        let Some(n) = t.tree.get(id) else { continue };
        match &n.zip {
            Some((archive, inner)) if n.kind.is_virtual() => {
                for p in crate::zipfs::extract(archive, std::slice::from_ref(inner))? {
                    out.push(p.to_string_lossy().into_owned());
                }
            }
            _ => out.push(n.path.clone()),
        }
    }
    Ok(out)
}

fn any_virtual(t: &Tab, nodes: &[NodeId]) -> bool {
    nodes.iter().any(|&id| t.tree.get(id).is_some_and(|n| n.kind.is_virtual()))
}

/// A favorite shortcut listed below the drives in This PC.
fn is_favorite_root(t: &Tab, id: NodeId) -> bool {
    t.root.is_none() && t.tree.get(id).is_some_and(|n| n.parent.is_none() && n.kind != Kind::Drive)
}

/// Why drives and This PC favorites in `nodes` can't be `verb` (cut, deleted, ...), if any are.
fn fixed_error(t: &Tab, nodes: &[NodeId], verb: &str) -> Option<String> {
    if nodes.iter().any(|&id| t.tree.get(id).is_some_and(|n| n.kind == Kind::Drive)) {
        Some(format!("Drives can't be {verb}"))
    } else if nodes.iter().any(|&id| is_favorite_root(t, id)) {
        Some(format!("Favorites can't be {verb} from This PC"))
    } else {
        None
    }
}

fn paths_of(t: &Tab, nodes: &[NodeId]) -> Vec<String> {
    nodes.iter().filter_map(|&id| t.path_of(id)).collect()
}

fn copy_nodes(app: &mut App, tid: TabId, nodes: &[NodeId], cut: bool) {
    let Some(t) = app.tabs.get(&tid) else { return };
    if nodes.is_empty() {
        return;
    }
    if any_virtual(t, nodes) {
        if cut {
            app.set_status_error("Items inside an archive can't be cut".into());
            return;
        }
        let prev = wait_cursor();
        let r = materialize(t, nodes);
        unsafe {
            SetCursor(prev);
        }
        match r {
            Ok(p) => app.set_clipboard(&p, false),
            Err(e) => app.set_status_error(format!("Extract failed: {e}")),
        }
        return;
    }
    if let Some(e) = fixed_error(t, nodes, "cut").filter(|_| cut) {
        app.set_status_error(e);
        return;
    }
    let paths = paths_of(t, nodes);
    app.set_clipboard(&paths, cut);
}

/// Recycling is confirmed by a prompt shown outside the app borrow; permanent deletion is
/// confirmed by the shell's own dialog.
fn delete_nodes(app: &mut App, tid: TabId, nodes: &[NodeId], recycle: bool) -> Option<Deferred> {
    let t = app.tabs.get(&tid)?;
    if nodes.is_empty() {
        return None;
    }
    if any_virtual(t, nodes) {
        app.set_status_error("Items inside an archive can't be deleted".into());
        return None;
    }
    if let Some(e) = fixed_error(t, nodes, "deleted") {
        app.set_status_error(e);
        return None;
    }
    let items = paths_of(t, nodes);
    let n = items.len();
    let msg = if n == 1 { format!("Deleted {}", file_name(&items[0])) } else { format!("Deleted {n} items") };
    if recycle {
        return Some(Deferred::Recycle(tid, items, msg));
    }
    app.run_op(tid, Op::Delete { items, recycle }, Some(msg));
    None
}

fn paste(app: &mut App, tid: TabId, node: Option<NodeId>) {
    match app.dest_for(tid, node) {
        Some(d) => app.paste_into(tid, &d),
        None => app.set_status_error("Can't paste here".into()),
    }
}

// ---------- Mouse ----------

fn on_lbutton(app: &mut App, pane: PaneId, x: i32, y: i32) -> bool {
    app.activate_pane(pane);
    app.grid_ui.swallow_dbl = false;
    let Some(tid) = active_tid(app, pane) else { return false };
    match hit(app, pane, x, y) {
        Hit::Crumb { target, .. } => {
            app.grid_ui.swallow_dbl = true;
            app.navigate(tid, target.as_deref(), target.is_some());
            false
        }
        Hit::CrumbBar => {
            app.grid_ui.swallow_dbl = true;
            begin_path_edit(app, pane);
            false
        }
        Hit::Up => {
            app.grid_ui.swallow_dbl = true;
            go_up(app, tid);
            false
        }
        Hit::Divider(c) => {
            app.grid_ui.col_drag = Some((pane, c, x, app.settings.columns[c]));
            true
        }
        Hit::Header(c) => {
            app.sort_by(tid, SortKey::from_column(c));
            false
        }
        Hit::Row { node, chevron: true, .. } => {
            app.toggle(tid, node);
            false
        }
        Hit::Row { row, node, .. } => {
            let (c, s) = (ctrl(), shift());
            let Some(t) = app.tabs.get_mut(&tid) else { return false };
            let mut deselect = false;
            if s {
                select_range(t, node, c);
            } else if c {
                let sel = t.tree.is_selected(node);
                t.tree.set_selected(node, !sel);
                t.anchor = Some(node);
                t.set_focus(node);
            } else if t.tree.is_selected(node) && t.tree.selection(&t.rows).len() > 1 {
                deselect = true;
                t.anchor = Some(node);
                t.set_focus(node);
            } else {
                t.select_only(node);
            }
            app.scroll_into_view(tid, row);
            app.after_change(tid);
            app.grid_ui.press = Some(Press { pane, tid, x, y, node, deselect });
            true
        }
        Hit::Empty => {
            if !ctrl() {
                if let Some(t) = app.tabs.get_mut(&tid) {
                    t.tree.clear_selection();
                }
                app.after_change(tid);
            }
            false
        }
        Hit::Nothing => false,
    }
}

fn on_lbutton_up(app: &mut App, pane: PaneId) {
    if app.grid_ui.col_drag.take().is_some() {
        app.save_settings();
    }
    if let Some(pr) = app.grid_ui.press.take() {
        if pr.deselect && pr.pane == pane {
            if let Some(t) = app.tabs.get_mut(&pr.tid) {
                if t.tree.exists(pr.node) {
                    t.select_only(pr.node);
                }
            }
            app.after_change(pr.tid);
        }
    }
}

fn on_move(app: &mut App, pane: PaneId, hwnd: HWND, x: i32, y: i32) -> Option<Deferred> {
    if let Some((p, c, sx, sw)) = app.grid_ui.col_drag {
        if p == pane {
            let w = (sw + (x - sx) * 96 / app.dpi as i32).clamp(crate::settings::MIN_COLUMN, 4000);
            if app.settings.columns[c] != w {
                app.settings.columns[c] = w;
                let panes: Vec<PaneId> = app.panes.keys().copied().collect();
                for p in panes {
                    sync(app, p);
                }
            }
        }
        return None;
    }
    if let Some(pr) = &app.grid_ui.press {
        if pr.pane == pane && key_down(VK_LBUTTON.0) {
            let (dx, dy) = unsafe { (GetSystemMetrics(SM_CXDRAG), GetSystemMetrics(SM_CYDRAG)) };
            if (x - pr.x).abs() <= dx && (y - pr.y).abs() <= dy {
                return None;
            }
            let pr = app.grid_ui.press.take()?;
            let t = app.tabs.get_mut(&pr.tid)?;
            let nodes = t.selected();
            if fixed_error(t, &nodes, "moved").is_some() {
                return None;
            }
            if any_virtual(t, &nodes) {
                let prev = wait_cursor();
                let r = materialize(t, &nodes);
                unsafe {
                    SetCursor(prev);
                }
                return match r {
                    Ok(p) if !p.is_empty() => Some(Deferred::Drag(p, DROPEFFECT_COPY)),
                    Ok(_) => None,
                    Err(e) => {
                        app.set_status_error(format!("Extract failed: {e}"));
                        None
                    }
                };
            }
            let paths = paths_of(t, &nodes);
            return (!paths.is_empty()).then(|| Deferred::Drag(paths, DROPEFFECT_COPY | DROPEFFECT_MOVE | DROPEFFECT_LINK));
        }
    }
    let (row, bar, cell) = match hit(app, pane, x, y) {
        Hit::Row { row, node, .. } => (Some((pane, row)), None, Some((row, node))),
        Hit::Crumb { index, .. } => (None, Some((pane, BarHot::Crumb(index))), None),
        Hit::Up => (None, Some((pane, BarHot::Up)), None),
        _ => (None, None, None),
    };
    if app.grid_ui.hover != row || app.grid_ui.bar_hover != bar {
        app.grid_ui.hover = row;
        app.grid_ui.bar_hover = bar;
        invalidate(hwnd);
    }
    let tip = cell.and_then(|(row, node)| truncated_cell(app, pane, hwnd, x, row, node));
    if tip != app.grid_ui.tip {
        hide_tip(app, false);
        if tip.is_some() {
            app.grid_ui.tip = tip;
            app.grid_ui.tip_state = TipState::Pending;
            unsafe {
                SetTimer(hwnd, TIMER_TIP, TIP_DELAY_MS, None);
            }
        }
    }
    unsafe {
        let mut tme = TRACKMOUSEEVENT {
            cbSize: std::mem::size_of::<TRACKMOUSEEVENT>() as u32,
            dwFlags: TME_LEAVE,
            hwndTrack: hwnd,
            dwHoverTime: 0,
        };
        let _ = TrackMouseEvent(&mut tme);
    }
    None
}

fn size_text(n: &crate::model::Node) -> String {
    match n.kind {
        Kind::File | Kind::Zip | Kind::ZipFile => format_size(n.size),
        Kind::Drive if n.size > 0 => format_size(n.size),
        _ => String::new(),
    }
}

// ---------- Truncation tooltip ----------

/// The cell of `row` at client `x` if its text is cut off (by its column or the window edge).
fn truncated_cell(app: &mut App, pane: PaneId, hwnd: HWND, x: i32, row: usize, node: NodeId) -> Option<Tip> {
    if app.grid_ui.rename.as_ref().is_some_and(|r| r.node == node) {
        return None;
    }
    let m = metrics(app);
    let (font, line) = (app.fonts.ui, app.fonts.ui_height);
    let t = app.tabs.get(&active_tid(app, pane)?)?;
    let geo = Geo::of(&m, t);
    let y = geo.row_y(row, t.top)?;
    let n = t.tree.get(node)?;
    let mut x0 = -t.scroll_x;
    let col = (0..m.cols.len()).find(|&c| {
        let inside = x >= x0 && x < x0 + m.cols[c];
        if !inside {
            x0 += m.cols[c];
        }
        inside
    })?;
    let (text, left) = match col {
        0 => (n.name.clone(), m.text_x(t.scroll_x, n.depth)),
        1 if n.kind != Kind::Drive => (format_time(n.modified), x0 + m.pad),
        2 => (n.type_name.to_string(), x0 + m.pad),
        3 if row >= geo.pin => (size_text(n), x0 + m.pad),
        _ => return None,
    };
    let right = (x0 + m.cols[col] - m.pad).min(client_rect(hwnd).right);
    if text.is_empty() {
        return None;
    }
    let width = unsafe {
        let hdc = GetDC(hwnd);
        let w = text_width(hdc, font, &text);
        ReleaseDC(hwnd, hdc);
        w
    };
    if left >= 0 && left + width <= right {
        return None;
    }
    Some(Tip { pane, row, col, text, rect: rc(left, y + (m.row - line) / 2, width, line) })
}

/// Hides the tooltip; `dismiss` keeps it hidden until the mouse moves to another cell.
fn hide_tip(app: &mut App, dismiss: bool) {
    let ui = &mut app.grid_ui;
    let Some(tip) = &ui.tip else { return };
    if let Some(p) = app.panes.get(&tip.pane) {
        unsafe {
            let _ = KillTimer(p.grid(), TIMER_TIP);
        }
    }
    if ui.tip_state == TipState::Shown {
        let ti = tool_info(app.hwnd, None);
        send(isize_to_hwnd(ui.tip_hwnd), TTM_TRACKACTIVATE, 0, &ti as *const _ as isize);
    }
    if dismiss {
        ui.tip_state = TipState::Dismissed;
    } else {
        ui.tip = None;
    }
}

fn tool_info(owner: HWND, text: Option<&mut [u16]>) -> TTTOOLINFOW {
    TTTOOLINFOW {
        cbSize: std::mem::size_of::<TTTOOLINFOW>() as u32,
        uFlags: TTF_TRACK | TTF_ABSOLUTE | TTF_TRANSPARENT,
        hwnd: owner,
        uId: 1,
        lpszText: text.map_or(PWSTR::null(), |t| PWSTR(t.as_mut_ptr())),
        ..Default::default()
    }
}

fn send(hwnd: HWND, msg: u32, w: usize, l: isize) -> LRESULT {
    unsafe { SendMessageW(hwnd, msg, WPARAM(w), LPARAM(l)) }
}

/// The shared tracking tooltip, created on first use.
fn tip_window(app: &mut App) -> Option<HWND> {
    if app.grid_ui.tip_hwnd == 0 {
        let style = WINDOW_STYLE(WS_POPUP.0 | TTS_NOPREFIX | TTS_ALWAYSTIP);
        let h = unsafe {
            CreateWindowExW(WS_EX_TOPMOST, TOOLTIPS_CLASSW, PCWSTR::null(), style, 0, 0, 0, 0, app.hwnd, None, instance(), None).ok()?
        };
        let mut empty = [0u16];
        let ti = tool_info(app.hwnd, Some(&mut empty));
        send(h, TTM_ADDTOOLW, 0, &ti as *const _ as isize);
        app.grid_ui.tip_hwnd = h.0 as isize;
    }
    Some(isize_to_hwnd(app.grid_ui.tip_hwnd))
}

fn on_tip_timer(app: &mut App, pane: PaneId, hwnd: HWND) {
    // Re-check the cell under the cursor: rows may have scrolled, changed or been covered.
    let pt = cursor_pos();
    let under = unsafe { WindowFromPoint(pt) };
    let over = under == hwnd || (app.grid_ui.tip_hwnd != 0 && under == isize_to_hwnd(app.grid_ui.tip_hwnd));
    let active = unsafe { GetForegroundWindow() } == app.hwnd && app.grid_ui.press.is_none() && app.grid_ui.col_drag.is_none();
    let p = screen_to_client(hwnd, pt);
    let current = match (over && active).then(|| hit(app, pane, p.x, p.y)) {
        Some(Hit::Row { row, node, .. }) => truncated_cell(app, pane, hwnd, p.x, row, node),
        _ => None,
    };
    if current.is_none() || current != app.grid_ui.tip {
        hide_tip(app, false);
        return;
    }
    match app.grid_ui.tip_state {
        TipState::Pending => {
            if show_tip(app, hwnd).is_some() {
                app.grid_ui.tip_state = TipState::Shown;
                unsafe {
                    SetTimer(hwnd, TIMER_TIP, TIP_POLL_MS, None);
                }
            } else {
                hide_tip(app, false);
            }
        }
        TipState::Dismissed => unsafe {
            let _ = KillTimer(hwnd, TIMER_TIP);
        },
        TipState::Shown => {}
    }
}

/// Shows the tooltip exactly over the truncated text, kept on the monitor.
fn show_tip(app: &mut App, hwnd: HWND) -> Option<()> {
    let tip = app.grid_ui.tip.as_ref()?;
    let (mut text, rect) = (wide(&tip.text), tip.rect);
    let (font, dark) = (app.fonts.ui, app.dark);
    let h = tip_window(app)?;
    crate::theme::set_control_theme(h, dark, "DarkMode_Explorer");
    send(h, WM_SETFONT, font.0 as usize, 0);
    let ti = tool_info(app.hwnd, Some(&mut text));
    send(h, TTM_UPDATETIPTEXTW, 0, &ti as *const _ as isize);
    let tl = client_to_screen(hwnd, POINT { x: rect.left, y: rect.top });
    let mut r = RECT { left: tl.x, top: tl.y, right: tl.x + rect.right - rect.left, bottom: tl.y + rect.bottom - rect.top };
    send(h, TTM_ADJUSTRECT, 1, &mut r as *mut _ as isize);
    let mut mi = MONITORINFO { cbSize: std::mem::size_of::<MONITORINFO>() as u32, ..Default::default() };
    unsafe {
        let _ = GetMonitorInfoW(MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST), &mut mi);
    }
    let x = r.left.min(mi.rcWork.right - (r.right - r.left)).max(mi.rcWork.left);
    send(h, TTM_TRACKPOSITION, 0, ((r.top as u16 as isize) << 16) | x as u16 as isize);
    send(h, TTM_TRACKACTIVATE, 1, &ti as *const _ as isize);
    Some(())
}

fn on_dblclick(app: &mut App, pane: PaneId, x: i32, y: i32) -> Option<Deferred> {
    if std::mem::take(&mut app.grid_ui.swallow_dbl) {
        return None;
    }
    let tid = active_tid(app, pane)?;
    match hit(app, pane, x, y) {
        Hit::Header(c) => app.sort_by(tid, SortKey::from_column(c)),
        Hit::Row { node, chevron: true, .. } => app.toggle(tid, node),
        Hit::Row { node, .. } => {
            let t = app.tabs.get_mut(&tid)?;
            let n = t.tree.get(node)?;
            let (kind, path) = (n.kind, n.path.clone());
            let is_root = t.root_node() == Some(node);
            match kind {
                Kind::File => return Some(Deferred::Open(path)),
                Kind::ZipFile => {
                    let prev = wait_cursor();
                    let r = materialize(t, &[node]);
                    unsafe {
                        SetCursor(prev);
                    }
                    match r {
                        Ok(p) => return p.into_iter().next().map(Deferred::Open),
                        Err(e) => app.set_status_error(format!("Extract failed: {e}")),
                    }
                }
                Kind::ZipDir => app.toggle(tid, node),
                Kind::Dir | Kind::Drive | Kind::Zip => {
                    if is_root {
                        go_up(app, tid);
                    } else {
                        app.navigate(tid, Some(&path), true);
                    }
                }
            }
        }
        _ => {}
    }
    None
}

/// Re-roots the tab at its root's parent (drive roots go to This PC), keeping the old root expanded.
fn go_up(app: &mut App, tid: TabId) {
    let Some(path) = app.tabs.get(&tid).and_then(|t| t.root.clone()) else { return };
    if is_drive_root(&path) {
        app.navigate(tid, None, false);
    } else {
        app.navigate(tid, Some(&path), false);
    }
}

fn on_rbutton(app: &mut App, pane: PaneId, x: i32, y: i32) {
    app.activate_pane(pane);
    let Some(tid) = active_tid(app, pane) else { return };
    app.grid_ui.ctx_background = match hit(app, pane, x, y) {
        Hit::Row { node, .. } => {
            if let Some(t) = app.tabs.get_mut(&tid) {
                if t.tree.is_selected(node) {
                    t.set_focus(node);
                } else {
                    t.select_only(node);
                }
            }
            Some(false)
        }
        Hit::Empty => {
            if let Some(t) = app.tabs.get_mut(&tid) {
                t.tree.clear_selection();
            }
            Some(true)
        }
        _ => None,
    };
    app.after_change(tid);
}

// ---------- Context menu ----------

struct MenuPrep {
    tid: TabId,
    nodes: Vec<NodeId>,
    background: bool,
    shell_paths: Vec<String>,
    pt: POINT,
    items: Vec<MenuItem>,
    dest: Option<String>,
}

fn prepare_menu(app: &mut App, pane: PaneId, hwnd: HWND, keyboard: bool, pt: POINT) -> Option<MenuPrep> {
    let tid = active_tid(app, pane)?;
    let m = metrics(app);
    let background = if keyboard {
        let t = app.tabs.get_mut(&tid)?;
        t.tree.selection(&t.rows).is_empty()
    } else {
        app.grid_ui.ctx_background.take()?
    };
    let t = app.tabs.get_mut(&tid)?;
    t.ensure_rows();
    let pt = if keyboard {
        let row = t.focus.and_then(|f| t.row_of(f)).unwrap_or(t.top);
        let geo = Geo::of(&m, t);
        let y = geo.row_y(row, t.top).unwrap_or(geo.scroll_y) + m.row / 2;
        client_to_screen(hwnd, POINT { x: m.text_x(t.scroll_x, 0), y })
    } else {
        pt
    };
    let mut items = Vec::new();
    if background {
        let dest = app.dest_for(tid, None);
        if dest.is_some() {
            items.push(MenuItem::Sub(
                "New".into(),
                vec![(C_NEW_FOLDER, "Folder".into()), (C_NEW_FILE, "Text Document".into())],
            ));
        }
        if dest.is_some() && shell::clipboard_has_files() {
            items.push(MenuItem::Item(C_PASTE, "Paste".into()));
        }
        items.push(MenuItem::Item(C_REFRESH, "Refresh".into()));
        return Some(MenuPrep { tid, nodes: vec![], background, shell_paths: vec![], pt, items, dest });
    }
    let nodes: Vec<NodeId> = t.selected().into_iter().filter(|&id| t.tree.exists(id)).collect();
    if nodes.is_empty() {
        return None;
    }
    let virt = any_virtual(t, &nodes);
    if nodes.len() == 1 {
        let n = t.tree.get(nodes[0])?;
        if n.kind.is_container() {
            if !n.kind.is_virtual() {
                items.push(MenuItem::Item(C_NEW_TAB, "Open in new tab".into()));
            }
            items.push(MenuItem::Item(C_EXPAND_ALL, "Expand all".into()));
            items.push(MenuItem::Item(C_COLLAPSE_ALL, "Collapse all".into()));
        }
        if n.kind == Kind::ZipFile {
            items.push(MenuItem::Item(C_OPEN, "Open".into()));
        }
        if !virt {
            items.push(MenuItem::Item(C_EXPLORER, "Open in Explorer".into()));
        }
        if is_favorite_root(t, nodes[0]) {
            items.push(MenuItem::Item(C_UNFAVORITE, "Remove from favorites".into()));
        }
    }
    if virt {
        items.push(MenuItem::Item(C_COPY, "Copy".into()));
    }
    items.push(MenuItem::Item(C_COPY_PATH, "Copy path".into()));
    let shell_paths = if virt { vec![] } else { paths_of(t, &nodes) };
    if !shell_paths.is_empty() {
        items.push(MenuItem::Sep);
    }
    let dest = app.dest_for(tid, nodes.first().copied());
    Some(MenuPrep { tid, nodes, background, shell_paths, pt, items, dest })
}

fn handle_menu(app: &mut App, pane: PaneId, prep: MenuPrep, res: MenuResult) -> Option<Deferred> {
    let tid = prep.tid;
    let nodes: Vec<NodeId> = {
        let t = app.tabs.get(&tid)?;
        prep.nodes.iter().copied().filter(|&id| t.tree.exists(id)).collect()
    };
    if !prep.background && nodes.is_empty() && !matches!(res, MenuResult::None) {
        return None;
    }
    match res {
        MenuResult::None => {}
        MenuResult::Custom(id) => match id {
            C_EXPAND_ALL => app.expand_all(tid, nodes[0]),
            C_COLLAPSE_ALL => app.collapse_all(tid, nodes[0]),
            C_NEW_FOLDER | C_NEW_FILE => {
                if let Some(d) = &prep.dest {
                    app.new_item(tid, d, id == C_NEW_FOLDER);
                }
            }
            C_PASTE => {
                if let Some(d) = &prep.dest {
                    app.paste_into(tid, d);
                }
            }
            C_REFRESH => app.refresh_tab(tid),
            C_COPY_PATH => {
                let t = app.tabs.get(&tid)?;
                let text = nodes.iter().filter_map(|&id| t.tree.get(id).map(|n| n.path.clone())).collect::<Vec<_>>().join("\r\n");
                shell::clipboard_set_text(app.hwnd, &text);
                app.set_status("Path copied".into());
            }
            C_NEW_TAB => {
                let t = app.tabs.get(&tid)?;
                let path = t.tree.get(nodes[0])?.path.clone();
                let (sort, desc) = (t.tree.sort, t.tree.desc);
                app.open_tab(Some(path), sort, desc);
            }
            C_EXPLORER => {
                let n = app.tabs.get(&tid)?.tree.get(nodes[0])?;
                return Some(Deferred::Explorer(n.path.clone(), n.kind.is_fs_dir()));
            }
            C_OPEN => {
                let prev = wait_cursor();
                let r = materialize(app.tabs.get(&tid)?, &nodes);
                unsafe {
                    SetCursor(prev);
                }
                match r {
                    Ok(p) => return p.into_iter().next().map(Deferred::Open),
                    Err(e) => app.set_status_error(format!("Extract failed: {e}")),
                }
            }
            C_COPY => copy_nodes(app, tid, &nodes, false),
            C_UNFAVORITE => {
                let path = app.tabs.get(&tid)?.tree.get(nodes[0])?.path.clone();
                if let Some(i) = app.favorite_index(&path) {
                    app.remove_favorite(i);
                }
            }
            _ => {}
        },
        MenuResult::Verb(v) => match v.as_str() {
            "rename" => begin_rename(app, pane, nodes[0]),
            "delete" => return delete_nodes(app, tid, &nodes, !shift()),
            "cut" => copy_nodes(app, tid, &nodes, true),
            "copy" => copy_nodes(app, tid, &nodes, false),
            "paste" => paste(app, tid, nodes.first().copied()),
            _ => {}
        },
    }
    None
}

// ---------- Keyboard ----------

fn on_key(app: &mut App, pane: PaneId, vk: u16) -> Option<Deferred> {
    let tid = active_tid(app, pane)?;
    let view = app.panes.get(&pane)?.view_rows as i64;
    let (c, s) = (ctrl(), shift());
    let t = app.tabs.get_mut(&tid)?;
    t.ensure_rows();
    let n = t.rows.len() as i64;
    let cur = t.focus.and_then(|f| t.row_of(f));
    let focus = cur.map(|r| t.rows[r]);
    let cur_i = cur.map_or(-1, |r| r as i64);
    let target = |r: i64| Some(r.clamp(0, (n - 1).max(0)) as usize);
    let row = match VIRTUAL_KEY(vk) {
        VK_UP => target(cur_i - 1),
        VK_DOWN => target(cur_i + 1),
        VK_HOME => target(0),
        VK_END => target(n - 1),
        VK_PRIOR => target(cur_i - (view - 1).max(1)),
        VK_NEXT => target(cur_i + (view - 1).max(1)),
        _ => None,
    };
    if let Some(r) = row {
        if n > 0 {
            move_to(app, tid, r);
        }
        return None;
    }
    match VIRTUAL_KEY(vk) {
        VK_LEFT => {
            let id = focus?;
            let nd = t.tree.get(id)?;
            if nd.expandable() && nd.expanded {
                app.collapse(tid, id);
            } else if let Some(p) = nd.parent {
                if let Some(r) = t.row_of(p) {
                    move_to(app, tid, r);
                }
            }
        }
        VK_RIGHT => {
            let id = focus?;
            let nd = t.tree.get(id)?;
            if nd.expandable() && !nd.expanded {
                app.expand(tid, id);
            } else if nd.expanded {
                // The first child still shown (the filter may hide some).
                let kids = t.tree.children(id).to_vec();
                if let Some(r) = kids.into_iter().find_map(|c| t.row_of(c)) {
                    move_to(app, tid, r);
                }
            }
        }
        VK_ESCAPE if !t.filter.is_empty() || t.deep => app.clear_filter(tid),
        VK_RETURN | VK_F2 => begin_rename(app, pane, focus?),
        VK_F5 => app.refresh_tab(tid),
        VK_DELETE => {
            let nodes = t.selected();
            return delete_nodes(app, tid, &nodes, !s);
        }
        VK_SPACE if c => {
            let id = focus?;
            let sel = t.tree.is_selected(id);
            t.tree.set_selected(id, !sel);
            t.anchor = Some(id);
            app.after_change(tid);
        }
        k if c && k.0 == b'A' as u16 => {
            // Dimmed filter context (ancestors of matches) is never bulk-selected.
            for &id in &t.rows {
                if !t.filtered.context.contains(&id) {
                    t.tree.set_selected(id, true);
                }
            }
            app.after_change(tid);
        }
        k if c && k.0 == b'Z' as u16 => app.undo_last(),
        k if c && (k.0 == b'C' as u16 || k.0 == b'X' as u16) => {
            let nodes = t.selected();
            copy_nodes(app, tid, &nodes, k.0 == b'X' as u16);
        }
        k if c && k.0 == b'V' as u16 => paste(app, tid, focus),
        _ => {}
    }
    None
}

fn type_search_char(app: &mut App, pane: PaneId, ch: char) {
    let Some(tid) = active_tid(app, pane) else { return };
    let ui = &mut app.grid_ui;
    let fresh = ui.search_at.map_or(true, |at| at.elapsed().as_millis() > 1000);
    ui.search_at = Some(Instant::now());
    let Some(t) = app.tabs.get_mut(&tid) else { return };
    if fresh {
        ui.search.clear();
        t.ensure_rows();
        ui.search_scope = t.tree.search_scope(t.focus, &t.rows);
    }
    ui.search.push(ch);
    ui.search_scope.retain(|&id| t.tree.exists(id));
    let names: Vec<&str> = ui.search_scope.iter().filter_map(|&id| t.tree.get(id).map(|n| n.name.as_str())).collect();
    let cur = t.focus.and_then(|f| ui.search_scope.iter().position(|&id| id == f));
    let found = type_search(&names, cur, &ui.search).map(|i| ui.search_scope[i]);
    let status = format!("Find: {}", ui.search);
    if let Some(id) = found {
        t.select_only(id);
        t.mark();
        t.ensure_rows();
        if let Some(r) = t.row_of(id) {
            app.scroll_into_view(tid, r);
        }
        app.after_change(tid);
    }
    app.set_status(status);
}

// ---------- Typing a path into the breadcrumb bar ----------

/// Opens an editor over `pane`'s breadcrumb bar (shown temporarily on unzoomed tabs) with the
/// current root selected, so a path can be typed or pasted.
pub fn begin_path_edit(app: &mut App, pane: PaneId) {
    let Some(tid) = active_tid(app, pane) else { return };
    if let Some(pe) = app.grid_ui.path_edit.as_ref().filter(|pe| pe.tab == tid) {
        let edit = isize_to_hwnd(pe.edit);
        unsafe {
            SendMessageW(edit, EM_SETSEL, WPARAM(0), LPARAM(-1));
            let _ = SetFocus(edit);
        }
        return;
    }
    close_path_edit(app);
    let Some(grid) = app.panes.get(&pane).map(|p| p.grid()) else { return };
    let Some(root) = app.tabs.get(&tid).map(|t| t.root.clone().unwrap_or_default()) else { return };
    unsafe {
        let text = wide(&root);
        let Ok(edit) = CreateWindowExW(
            WINDOW_EX_STYLE(0),
            w!("EDIT"),
            PCWSTR(text.as_ptr()),
            WS_CHILD | WS_BORDER | WS_CLIPSIBLINGS | WINDOW_STYLE(ES_AUTOHSCROLL as u32),
            0,
            0,
            0,
            0,
            grid,
            None,
            instance(),
            None,
        ) else {
            return;
        };
        SendMessageW(edit, WM_SETFONT, WPARAM(app.fonts.ui.0 as usize), LPARAM(1));
        crate::theme::set_control_theme(edit, app.dark, "DarkMode_CFD");
        let cue = wide("Type or paste a path, e.g. C:\\Users or %TEMP%");
        SendMessageW(edit, EM_SETCUEBANNER, WPARAM(1), LPARAM(cue.as_ptr() as isize));
        let _ = SetWindowSubclass(edit, Some(path_edit_proc), 1, 0);
        if let Some(t) = app.tabs.get_mut(&tid) {
            t.editing_path = true;
        }
        app.grid_ui.path_edit = Some(PathEdit { edit: hwnd_to_isize(edit), pane, tab: tid });
        sync(app, pane);
        // Select only once sized, so the start of the path stays in view.
        SendMessageW(edit, EM_SETSEL, WPARAM(0), LPARAM(-1));
        let _ = SetFocus(edit);
    }
}

/// Positions the path editor over its pane's breadcrumb bar (hidden while another tab is shown).
fn place_path_edit(app: &mut App) {
    let Some(pe) = &app.grid_ui.path_edit else { return };
    let (edit, pane, tid) = (isize_to_hwnd(pe.edit), pe.pane, pe.tab);
    let shown = active_tid(app, pane) == Some(tid);
    let w = app.panes.get(&pane).map_or(0, |p| client_rect(p.grid()).right);
    let mut b = crumb_box(app, w);
    b.right = b.right.max(b.left);
    unsafe {
        if shown {
            let _ = MoveWindow(edit, b.left, b.top, b.right - b.left, b.bottom - b.top, true);
            let _ = ShowWindow(edit, SW_SHOW);
        } else {
            let _ = ShowWindow(edit, SW_HIDE);
        }
    }
}

/// Enter (`go`) goes to the typed path, keeping the editor open if it can't be found; otherwise cancels.
fn finish_path_edit(app: &mut App, go: bool) {
    let Some(pe) = &app.grid_ui.path_edit else { return };
    let (edit, tid) = (isize_to_hwnd(pe.edit), pe.tab);
    if go && !app.go_to(tid, &get_text(edit)) {
        balloon(edit, "Go to", "That path doesn't exist or can't be opened.");
        return;
    }
    close_path_edit(app);
}

fn close_path_edit(app: &mut App) {
    let Some(pe) = app.grid_ui.path_edit.take() else { return };
    if let Some(t) = app.tabs.get_mut(&pe.tab) {
        t.editing_path = false;
    }
    let edit = isize_to_hwnd(pe.edit);
    let pane = app.pane_of_tab(pe.tab).unwrap_or(pe.pane);
    unsafe {
        if let Some(g) = app.panes.get(&pane).map(|p| p.grid()) {
            if GetFocus() == edit {
                let _ = SetFocus(g);
            }
        }
        let _ = DestroyWindow(edit);
    }
    if app.panes.contains_key(&pane) {
        sync(app, pane);
    }
}

/// Closes the path editor without navigating.
pub fn end_path_edit(app: &mut App) {
    close_path_edit(app);
}

unsafe extern "system" fn path_edit_proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM, id: usize, _data: usize) -> LRESULT {
    let notify = |go: usize| {
        if let Ok(parent) = GetParent(hwnd) {
            post(parent, WM_PATH_END, go, hwnd_to_isize(hwnd));
        }
    };
    match msg {
        WM_GETDLGCODE => return LRESULT(DLGC_WANTALLKEYS as isize),
        WM_KEYDOWN if wp.0 as u16 == VK_RETURN.0 => {
            notify(1);
            return LRESULT(0);
        }
        WM_KEYDOWN if wp.0 as u16 == VK_ESCAPE.0 => {
            notify(0);
            return LRESULT(0);
        }
        WM_CHAR if wp.0 == 13 || wp.0 == 27 => return LRESULT(0),
        // Ctrl+A
        WM_CHAR if wp.0 == 1 => {
            SendMessageW(hwnd, EM_SETSEL, WPARAM(0), LPARAM(-1));
            return LRESULT(0);
        }
        WM_KILLFOCUS => notify(0),
        WM_NCDESTROY => {
            let _ = RemoveWindowSubclass(hwnd, Some(path_edit_proc), id);
        }
        _ => {}
    }
    DefSubclassProc(hwnd, msg, wp, lp)
}

// ---------- In-place rename ----------

pub fn begin_rename(app: &mut App, pane: PaneId, id: NodeId) {
    end_rename(app, false);
    let Some(tid) = active_tid(app, pane) else { return };
    let Some(grid) = app.panes.get(&pane).map(|p| p.grid()) else { return };
    let Some(t) = app.tabs.get_mut(&tid) else { return };
    if t.root_node() == Some(id) {
        app.set_status_error("Cannot rename root".into());
        return;
    }
    let Some(n) = t.tree.get(id) else { return };
    if n.kind == Kind::Drive || n.kind.is_virtual() || is_favorite_root(t, id) {
        let msg = format!("Cannot rename {}", n.name);
        app.set_status_error(msg);
        return;
    }
    let (name, kind) = (n.name.clone(), n.kind);
    t.select_only(id);
    t.ensure_rows();
    if let Some(r) = t.row_of(id) {
        app.scroll_into_view(tid, r);
    }
    unsafe {
        let text = wide(&name);
        let Ok(edit) = CreateWindowExW(
            WINDOW_EX_STYLE(0),
            w!("EDIT"),
            PCWSTR(text.as_ptr()),
            WS_CHILD | WS_BORDER | WS_CLIPSIBLINGS | WINDOW_STYLE(ES_AUTOHSCROLL as u32),
            0,
            0,
            0,
            0,
            grid,
            None,
            instance(),
            None,
        ) else {
            return;
        };
        SendMessageW(edit, WM_SETFONT, WPARAM(app.fonts.ui.0 as usize), LPARAM(1));
        crate::theme::set_control_theme(edit, app.dark, "DarkMode_CFD");
        let _ = SetWindowSubclass(edit, Some(edit_proc), 1, 0);
        let end = match (kind, extension_start(&name)) {
            (Kind::File | Kind::Zip, Some(i)) => name[..i].encode_utf16().count() as isize,
            _ => -1,
        };
        SendMessageW(edit, EM_SETSEL, WPARAM(0), LPARAM(end));
        app.grid_ui.rename = Some(Rename { edit: hwnd_to_isize(edit), pane, tab: tid, node: id, original: name });
        app.after_change(tid);
        sync(app, pane);
        let _ = SetFocus(edit);
    }
}

/// Positions the rename edit over its row (hides it when scrolled out of view).
fn place_rename(app: &mut App) {
    let Some(rn) = &app.grid_ui.rename else { return };
    let (edit, pane, tid, node) = (isize_to_hwnd(rn.edit), rn.pane, rn.tab, rn.node);
    let m = metrics(app);
    let shown = active_tid(app, pane) == Some(tid);
    let view = app.panes.get(&pane).map_or(0, |p| p.view_rows);
    let place = app.tabs.get_mut(&tid).filter(|_| shown).and_then(|t| {
        t.ensure_rows();
        let r = t.row_of(node)?;
        let geo = Geo::of(&m, t);
        if r >= geo.pin && (r < t.top || r >= t.top + view) {
            return None;
        }
        let depth = t.tree.get(node)?.depth;
        let x = m.text_x(t.scroll_x, depth) - scale(3, app.dpi);
        let right = -t.scroll_x + m.cols[0];
        let y = geo.row_y(r, t.top)?;
        Some((x, y, (right - x).max(scale(80, app.dpi))))
    });
    unsafe {
        match place {
            Some((x, y, w)) => {
                let _ = MoveWindow(edit, x, y + 1, w, m.row - 2, true);
                let _ = ShowWindow(edit, SW_SHOW);
            }
            None => {
                let _ = ShowWindow(edit, SW_HIDE);
            }
        }
    }
}

fn balloon(edit: HWND, title: &str, text: &str) {
    let (title, body) = (wide(title), wide(text));
    let tip = EDITBALLOONTIP {
        cbStruct: std::mem::size_of::<EDITBALLOONTIP>() as u32,
        pszTitle: PCWSTR(title.as_ptr()),
        pszText: PCWSTR(body.as_ptr()),
        ttiIcon: TTI_ERROR,
    };
    unsafe {
        SendMessageW(edit, EM_SHOWBALLOONTIP, WPARAM(0), LPARAM(&tip as *const _ as isize));
    }
}

/// Ends the rename. mode: 0 = cancel, 1 = commit (Enter), 2 = commit on focus loss.
fn finish_rename(app: &mut App, mode: usize) {
    let Some(rn) = &app.grid_ui.rename else { return };
    let (edit, original, tab, node, pane) = (isize_to_hwnd(rn.edit), rn.original.clone(), rn.tab, rn.node, rn.pane);
    let grid = app.panes.get(&pane).map(|p| p.grid());
    if mode != 0 {
        let name = get_text(edit).trim_end().to_string();
        let changed = !name.is_empty() && name != original;
        if changed {
            let item = app.tabs.get(&tab).and_then(|t| t.path_of(node));
            let error = match &item {
                None => Some("The item no longer exists".to_string()),
                Some(item) => invalid_name(&name).map(str::to_string).or_else(|| {
                    let parent = parent_path(item).unwrap_or_default();
                    let case_only = name.to_lowercase() == original.to_lowercase();
                    (!case_only && std::path::Path::new(&join_path(&parent, &name)).exists())
                        .then(|| format!("\"{name}\" already exists"))
                }),
            };
            match (error, item) {
                (Some(e), _) if mode == 1 => {
                    balloon(edit, "Rename", &e);
                    return;
                }
                (Some(e), _) => app.set_status_error(e),
                (None, Some(item)) => {
                    close_edit(app, edit, grid);
                    app.run_op(tab, Op::Rename { item, name: name.clone() }, Some(format!("Renamed to {name}")));
                    return;
                }
                (None, None) => {}
            }
        }
    }
    close_edit(app, edit, grid);
}

fn close_edit(app: &mut App, edit: HWND, grid: Option<HWND>) {
    let Some(rn) = app.grid_ui.rename.take() else { return };
    unsafe {
        if let Some(g) = grid {
            if GetFocus() == edit {
                let _ = SetFocus(g);
            }
            invalidate(g);
        }
        let _ = DestroyWindow(edit);
    }
    app.after_change(rn.tab);
}

pub fn end_rename(app: &mut App, commit: bool) {
    finish_rename(app, if commit { 1 } else { 0 });
}

unsafe extern "system" fn edit_proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM, id: usize, _data: usize) -> LRESULT {
    let notify = |mode: usize| {
        if let Ok(parent) = GetParent(hwnd) {
            post(parent, WM_RENAME_END, mode, hwnd_to_isize(hwnd));
        }
    };
    match msg {
        WM_GETDLGCODE => return LRESULT(DLGC_WANTALLKEYS as isize),
        WM_KEYDOWN if wp.0 as u16 == VK_RETURN.0 => {
            notify(1);
            return LRESULT(0);
        }
        WM_KEYDOWN if wp.0 as u16 == VK_ESCAPE.0 => {
            notify(0);
            return LRESULT(0);
        }
        WM_CHAR if wp.0 == 13 || wp.0 == 27 => return LRESULT(0),
        WM_CHAR => {
            if let Some(ch) = char::from_u32(wp.0 as u32) {
                if INVALID_CHARS.contains(ch) {
                    balloon(hwnd, "Rename", "A file name can't contain any of the following characters:\n\\ / : * ? \" < > |");
                    return LRESULT(0);
                }
            }
        }
        WM_KILLFOCUS => notify(2),
        WM_NCDESTROY => {
            let _ = RemoveWindowSubclass(hwnd, Some(edit_proc), id);
        }
        _ => {}
    }
    DefSubclassProc(hwnd, msg, wp, lp)
}
