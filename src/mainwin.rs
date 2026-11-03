//! The main window: toolbar (navigation buttons, favorites, path box), per-pane tab strips,
//! splitters, tab drag & dock, status bar and top-level message handling.

use crate::app::{with_app, App, TIMER_HOVER_TAB, TIMER_STATUS, TIMER_WATCH};
use crate::layout::{PaneId, Rect, Side, Splitter};
use crate::loader::TabId;
use crate::model::SortKey;
use crate::shell;
use crate::theme::glyph;
use crate::util::*;
use crate::win::*;
use std::cell::Cell;
use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::System::Com::*;
use windows::Win32::System::DataExchange::{AddClipboardFormatListener, RemoveClipboardFormatListener};
use windows::Win32::UI::Controls::*;
use windows::Win32::UI::HiDpi::GetDpiForWindow;
use windows::Win32::UI::Input::KeyboardAndMouse::*;
use windows::Win32::UI::Shell::*;
use windows::Win32::UI::WindowsAndMessaging::*;

const TOOLBAR: i32 = 44;
const STATUS: i32 = 24;
const STRIP: i32 = 34;
const SPLIT: i32 = 6;
const MIN_PANE: i32 = 120;
/// Room for the funnel glyph before a filtered tab's title.
const FUNNEL_W: i32 = 18;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Btn {
    Up,
    Refresh,
    ThisPc,
    Favorites,
    Browse,
    ClearFilter,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Hot {
    Button(Btn),
    Tab(PaneId, usize),
    TabClose(PaneId, usize),
    Plus(PaneId),
    Splitter(usize),
    Strip(PaneId),
}

struct TabRect {
    rect: RECT,
    close: RECT,
}

struct PaneChrome {
    pane: PaneId,
    strip: RECT,
    tabs: Vec<TabRect>,
    plus: RECT,
    grid: RECT,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum DropSpot {
    /// Insert into a tab strip before index.
    Strip(PaneId, usize),
    /// Dock onto a pane: `None` = add as a tab, `Some(side)` = split.
    Dock(PaneId, Option<Side>),
}

struct TabDrag {
    tab: TabId,
    start: POINT,
    active: bool,
    spot: Option<DropSpot>,
}

#[derive(Default)]
pub struct Chrome {
    /// The path box has focus; don't overwrite what the user is typing.
    pub path_editing: bool,
    /// The filter box has focus.
    pub search_editing: bool,
    /// The active tab is filtered (shows the clear button and accent border).
    pub filter_active: bool,
    /// Tab under an OLE drag (activated after a delay).
    pub drag_hover: Option<(PaneId, usize)>,
    toolbar: RECT,
    status: RECT,
    path_rect: RECT,
    search_rect: RECT,
    clear_rect: RECT,
    buttons: Vec<(Btn, RECT)>,
    panes: Vec<PaneChrome>,
    splitters: Vec<Splitter>,
    hot: Option<Hot>,
    pressed: Option<Hot>,
    tracking: bool,
    tab_drag: Option<TabDrag>,
    /// Splitter being dragged and the grab offset inside it.
    split_drag: Option<(Splitter, i32)>,
    overlay: isize,
    last_focus: isize,
}

fn to_rect(r: Rect) -> RECT {
    rc(r.x, r.y, r.w, r.h)
}

// ---------- Layout ----------

/// Recomputes all chrome geometry, moves the grids and repaints.
pub fn relayout(app: &mut App) {
    let hwnd = app.hwnd;
    let d = app.dpi;
    let cr = client_rect(hwnd);
    let (w, h) = (cr.right - cr.left, cr.bottom - cr.top);
    if w <= 0 || h <= 0 {
        return;
    }
    let tb = scale(TOOLBAR, d);
    let sb = scale(STATUS, d);
    let pad = scale(6, d);
    let bw = scale(34, d);
    let bh = scale(32, d);
    let by = (tb - bh) / 2;

    let c = &mut app.chrome;
    c.toolbar = rc(0, 0, w, tb);
    c.status = rc(0, h - sb, w, sb);
    c.buttons.clear();
    let mut x = pad;
    for b in [Btn::Up, Btn::Refresh, Btn::ThisPc, Btn::Favorites] {
        c.buttons.push((b, rc(x, by, bw, bh)));
        x += bw + scale(2, d);
    }
    x += scale(6, d);
    let browse_w = scale(84, d);
    let avail = (w - pad - browse_w - pad - x).max(0);
    let search_w = scale(220, d).min(avail * 2 / 5);
    let path_right = (x + avail - search_w - pad).max(x + scale(60, d));
    c.path_rect = rc(x, by, path_right - x, bh);
    c.search_rect = rc(path_right + pad, by, search_w, bh);
    let icon_w = scale(28, d);
    c.clear_rect = rc(c.search_rect.right - icon_w, by, icon_w, bh);
    c.buttons.push((Btn::Browse, rc(c.search_rect.right + pad, by, browse_w, bh)));
    let eh = app.fonts.ui_height + 2;
    unsafe {
        let _ = MoveWindow(
            app.path_edit,
            c.path_rect.left + scale(10, d),
            c.path_rect.top + (bh - eh) / 2,
            (c.path_rect.right - c.path_rect.left - scale(20, d)).max(0),
            eh,
            true,
        );
        let _ = MoveWindow(
            app.search_edit,
            c.search_rect.left + icon_w,
            c.search_rect.top + (bh - eh) / 2,
            (search_w - 2 * icon_w).max(0),
            eh,
            true,
        );
    }

    let area = Rect::new(0, tb, w, h - tb - sb);
    let mut rects = Vec::new();
    let mut splitters = Vec::new();
    match app.maximized.filter(|p| app.panes.contains_key(p)) {
        Some(p) => rects.push((p, area)),
        None => app.layout.arrange(area, scale(SPLIT, d), &mut rects, &mut splitters),
    }
    app.chrome.splitters = splitters;

    let strip_h = scale(STRIP, d);
    let hdc = unsafe { GetDC(hwnd) };
    let mut panes = Vec::new();
    for &(pid, r) in &rects {
        let Some(p) = app.panes.get(&pid) else { continue };
        let strip = rc(r.x, r.y, r.w, strip_h.min(r.h));
        let grid = rc(r.x, r.y + strip_h, r.w, r.h - strip_h);
        let titles: Vec<(String, bool)> = p
            .tabs
            .iter()
            .map(|t| app.tabs.get(t).map(|t| (t.title(), !t.filter.is_empty())).unwrap_or_default())
            .collect();
        let (tabs, plus) = layout_tabs(hdc, app.fonts.ui, d, &strip, &titles);
        panes.push(PaneChrome { pane: pid, strip, tabs, plus, grid });
        unsafe {
            let g = p.grid();
            let _ = MoveWindow(g, grid.left, grid.top, grid.right - grid.left, grid.bottom - grid.top, true);
            let _ = ShowWindow(g, SW_SHOWNA);
        }
    }
    unsafe {
        ReleaseDC(hwnd, hdc);
    }
    for p in app.panes.values() {
        if !rects.iter().any(|r| r.0 == p.id) {
            unsafe {
                let _ = ShowWindow(p.grid(), SW_HIDE);
            }
        }
    }
    app.chrome.panes = panes;
    for (pid, _) in rects {
        crate::grid::sync(app, pid);
    }
    invalidate(hwnd);
}

fn layout_tabs(hdc: HDC, font: HFONT, d: u32, strip: &RECT, titles: &[(String, bool)]) -> (Vec<TabRect>, RECT) {
    let top = strip.top + scale(4, d);
    let bottom = strip.bottom - 1;
    let h = (bottom - top).max(0);
    let pad = scale(10, d);
    let close = scale(20, d);
    let plus_w = scale(30, d);
    let left = strip.left + scale(4, d);
    let avail = (strip.right - left - plus_w - scale(6, d)).max(0);
    let (min_w, max_w) = (scale(90, d), scale(220, d));
    let mut widths: Vec<i32> = titles
        .iter()
        .map(|(t, filtered)| {
            let funnel = if *filtered { scale(FUNNEL_W, d) } else { 0 };
            (pad + funnel + text_width(hdc, font, t) + scale(6, d) + close + scale(6, d)).clamp(min_w, max_w)
        })
        .collect();
    let total: i32 = widths.iter().sum();
    if total > avail && !widths.is_empty() {
        let each = (avail / widths.len() as i32).max(scale(44, d));
        widths.iter_mut().for_each(|w| *w = (*w).min(each));
    }
    let mut x = left;
    let mut tabs = Vec::with_capacity(widths.len());
    for w in widths {
        let rect = RECT { left: x, top, right: x + w, bottom };
        let cx = rect.right - scale(6, d) - close;
        let cy = top + (h - close) / 2;
        tabs.push(TabRect { rect, close: rc(cx, cy, close, close) });
        x += w + 1;
    }
    let ph = scale(28, d).min(h);
    let plus = rc(x + scale(2, d), top + (h - ph) / 2, plus_w, ph);
    (tabs, plus)
}

/// The tab under client point (x, y).
pub fn tab_at(app: &App, x: i32, y: i32) -> Option<(PaneId, usize)> {
    app.chrome.panes.iter().find_map(|pc| pc.tabs.iter().position(|t| pt_in(&t.rect, x, y)).map(|i| (pc.pane, i)))
}

fn hit(c: &Chrome, x: i32, y: i32) -> Option<Hot> {
    if c.filter_active && pt_in(&c.clear_rect, x, y) {
        return Some(Hot::Button(Btn::ClearFilter));
    }
    if let Some((b, _)) = c.buttons.iter().find(|(_, r)| pt_in(r, x, y)) {
        return Some(Hot::Button(*b));
    }
    if let Some(i) = c.splitters.iter().position(|s| s.rect.contains(x, y)) {
        return Some(Hot::Splitter(i));
    }
    for pc in &c.panes {
        if !pt_in(&pc.strip, x, y) {
            continue;
        }
        for (i, t) in pc.tabs.iter().enumerate() {
            if pt_in(&t.close, x, y) {
                return Some(Hot::TabClose(pc.pane, i));
            }
            if pt_in(&t.rect, x, y) {
                return Some(Hot::Tab(pc.pane, i));
            }
        }
        if pt_in(&pc.plus, x, y) {
            return Some(Hot::Plus(pc.pane));
        }
        return Some(Hot::Strip(pc.pane));
    }
    None
}

/// Where a dragged tab would land at client point (x, y).
fn drop_spot(app: &App, tab: TabId, x: i32, y: i32) -> Option<DropSpot> {
    let src = app.pane_of_tab(tab)?;
    for pc in &app.chrome.panes {
        if pt_in(&pc.strip, x, y) {
            let idx = pc.tabs.iter().position(|t| x < (t.rect.left + t.rect.right) / 2).unwrap_or(pc.tabs.len());
            return Some(DropSpot::Strip(pc.pane, idx));
        }
        if pt_in(&pc.grid, x, y) {
            let gw = (pc.grid.right - pc.grid.left).max(1) as f64;
            let gh = (pc.grid.bottom - pc.grid.top).max(1) as f64;
            let fx = (x - pc.grid.left) as f64 / gw;
            let fy = (y - pc.grid.top) as f64 / gh;
            let edges = [(fx, Side::Left), (1.0 - fx, Side::Right), (fy, Side::Top), (1.0 - fy, Side::Bottom)];
            let (dist, side) = edges.iter().copied().fold((f64::MAX, Side::Left), |a, b| if b.0 < a.0 { b } else { a });
            let side = (dist < 0.25).then_some(side);
            let alone = src == pc.pane && app.panes.get(&src).is_some_and(|p| p.tabs.len() == 1);
            if src == pc.pane && (alone || side.is_none()) {
                return None;
            }
            return Some(DropSpot::Dock(pc.pane, side));
        }
    }
    None
}

// ---------- Painting ----------

fn paint(app: &App, hdc: HDC, client: &RECT) {
    let pal = app.pal;
    let c = &app.chrome;
    let d = app.dpi;
    fill(hdc, client, pal.bg);

    // Toolbar
    fill(hdc, &c.toolbar, pal.toolbar);
    let rooted = app.active_tab().and_then(|t| app.tabs.get(&t)).is_some_and(|t| t.root.is_some());
    for &(b, r) in &c.buttons {
        let hot = c.hot == Some(Hot::Button(b));
        let pressed = hot && c.pressed == Some(Hot::Button(b));
        if pressed {
            fill(hdc, &r, pal.sel);
        } else if hot {
            fill(hdc, &r, pal.hover);
        }
        let enabled = b != Btn::Up || rooted;
        let color = if enabled { pal.text } else { pal.text_dim };
        unsafe {
            if b == Btn::Browse {
                SelectObject(hdc, app.fonts.ui);
                draw_text(hdc, "Browse…", &r, color, DT_CENTER_V);
            } else {
                let g = match b {
                    Btn::Up => glyph::UP,
                    Btn::Refresh => glyph::REFRESH,
                    Btn::ThisPc => glyph::THIS_PC,
                    _ => glyph::STAR,
                };
                SelectObject(hdc, app.fonts.glyph);
                draw_text(hdc, g, &r, color, DT_CENTER_V);
            }
        }
    }
    fill(hdc, &c.path_rect, pal.edit_bg);
    let border = if c.path_editing { pal.accent } else { pal.border };
    frame(hdc, &c.path_rect, border, if c.path_editing { scale(2, d).max(1) } else { 1 });
    if c.search_rect.right > c.search_rect.left {
        fill(hdc, &c.search_rect, pal.edit_bg);
        let lit = c.search_editing || c.filter_active;
        frame(hdc, &c.search_rect, if lit { pal.accent } else { pal.border }, if lit { scale(2, d).max(1) } else { 1 });
        let icon = RECT { right: c.search_rect.left + (c.clear_rect.right - c.clear_rect.left), ..c.search_rect };
        unsafe {
            SelectObject(hdc, app.fonts.glyph_small);
        }
        draw_text(hdc, if c.filter_active { glyph::FILTER } else { glyph::SEARCH }, &icon, if c.filter_active { pal.accent } else { pal.text_dim }, DT_CENTER_V);
        if c.filter_active {
            let k = scale(4, d);
            let inner = RECT { left: c.clear_rect.left + k, top: c.clear_rect.top + k, right: c.clear_rect.right - k, bottom: c.clear_rect.bottom - k };
            if c.hot == Some(Hot::Button(Btn::ClearFilter)) {
                fill(hdc, &inner, pal.hover);
            }
            draw_text(hdc, glyph::CLOSE, &inner, pal.text_dim, DT_CENTER_V);
        }
    }
    let line = rc(0, c.toolbar.bottom - 1, client.right, 1);
    fill(hdc, &line, pal.border);

    // Splitters
    for (i, s) in c.splitters.iter().enumerate() {
        let r = to_rect(s.rect);
        let active = c.hot == Some(Hot::Splitter(i)) || c.split_drag.as_ref().is_some_and(|(sp, _)| sp.path == s.path && sp.index == s.index);
        fill(hdc, &r, if active { pal.accent } else { pal.toolbar });
    }

    // Tab strips
    let drag_target = c.tab_drag.as_ref().filter(|t| t.active).and_then(|t| t.spot);
    for pc in &c.panes {
        fill(hdc, &pc.strip, pal.toolbar);
        let Some(p) = app.panes.get(&pc.pane) else { continue };
        let pane_active = pc.pane == app.active_pane;
        for (i, tr) in pc.tabs.iter().enumerate() {
            let Some(tab) = p.tabs.get(i).and_then(|t| app.tabs.get(t)) else { continue };
            let active = i == p.active;
            let hovered = matches!(c.hot, Some(Hot::Tab(pp, ii)) | Some(Hot::TabClose(pp, ii)) if pp == pc.pane && ii == i)
                || c.drag_hover == Some((pc.pane, i));
            if active {
                fill(hdc, &tr.rect, pal.bg);
                if pane_active {
                    fill(hdc, &RECT { bottom: tr.rect.top + scale(2, d), ..tr.rect }, pal.accent);
                }
            } else if hovered {
                fill(hdc, &tr.rect, pal.tab_hover);
            }
            let mut text_r = RECT { left: tr.rect.left + scale(10, d), right: tr.close.left - scale(2, d), ..tr.rect };
            if !tab.filter.is_empty() {
                let funnel = RECT { right: text_r.left + scale(FUNNEL_W, d), ..text_r };
                unsafe {
                    SelectObject(hdc, app.fonts.glyph_small);
                }
                draw_text(hdc, glyph::FILTER, &funnel, pal.accent, DT_LEFT_V);
                text_r.left = funnel.right;
            }
            unsafe {
                SelectObject(hdc, app.fonts.ui);
            }
            draw_text(hdc, &tab.title(), &text_r, if active { pal.text } else { pal.text_dim }, DT_LEFT_V);
            if active || hovered {
                if c.hot == Some(Hot::TabClose(pc.pane, i)) {
                    fill(hdc, &tr.close, pal.hover);
                }
                unsafe {
                    SelectObject(hdc, app.fonts.glyph_small);
                }
                draw_text(hdc, glyph::CLOSE, &tr.close, pal.text_dim, DT_CENTER_V);
            }
        }
        if c.hot == Some(Hot::Plus(pc.pane)) {
            fill(hdc, &pc.plus, pal.hover);
        }
        unsafe {
            SelectObject(hdc, app.fonts.glyph_small);
        }
        draw_text(hdc, glyph::ADD, &pc.plus, pal.text, DT_CENTER_V);
        if let Some(DropSpot::Strip(tp, idx)) = drag_target {
            if tp == pc.pane {
                let x = match pc.tabs.get(idx) {
                    Some(t) => t.rect.left - 1,
                    None => pc.tabs.last().map(|t| t.rect.right).unwrap_or(pc.strip.left + scale(4, d)),
                };
                fill(hdc, &rc(x - 1, pc.strip.top + scale(4, d), scale(3, d), pc.strip.bottom - pc.strip.top - scale(6, d)), pal.accent);
            }
        }
    }

    // Status bar
    fill(hdc, &c.status, pal.toolbar);
    fill(hdc, &rc(0, c.status.top, client.right, 1), pal.border);
    let sr = RECT { left: c.status.left + scale(10, d), right: c.status.right - scale(10, d), ..c.status };
    unsafe {
        SelectObject(hdc, app.fonts.ui);
    }
    let filter_status;
    let (status, color) = match app.filter_status().filter(|_| app.status.is_empty()) {
        Some(s) => {
            filter_status = s;
            (&filter_status, pal.accent)
        }
        None => (&app.status, if app.status_error { pal.error } else { pal.text_dim }),
    };
    draw_text(hdc, status, &sr, color, DT_LEFT_V);
}

// ---------- Drag overlay ----------

unsafe extern "system" fn overlay_proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    match msg {
        WM_ERASEBKGND => {
            let color = COLORREF(GetWindowLongPtrW(hwnd, GWLP_USERDATA) as u32);
            fill(HDC(wp.0 as _), &client_rect(hwnd), color);
            LRESULT(1)
        }
        WM_NCHITTEST => LRESULT(HTTRANSPARENT as isize),
        _ => DefWindowProcW(hwnd, msg, wp, lp),
    }
}

fn show_overlay(app: &mut App, screen: Option<RECT>) {
    let hwnd = isize_to_hwnd(app.chrome.overlay);
    let Some(r) = screen else {
        if !hwnd.is_invalid() {
            unsafe {
                let _ = ShowWindow(hwnd, SW_HIDE);
            }
        }
        return;
    };
    let hwnd = if hwnd.is_invalid() {
        unsafe {
            static REG: std::sync::Once = std::sync::Once::new();
            REG.call_once(|| {
                let wc = WNDCLASSEXW {
                    cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
                    lpfnWndProc: Some(overlay_proc),
                    hInstance: instance(),
                    lpszClassName: w!("TwigOverlay"),
                    ..Default::default()
                };
                RegisterClassExW(&wc);
            });
            let h = CreateWindowExW(
                WS_EX_LAYERED | WS_EX_TRANSPARENT | WS_EX_NOACTIVATE | WS_EX_TOOLWINDOW,
                w!("TwigOverlay"),
                PCWSTR::null(),
                WS_POPUP,
                0,
                0,
                0,
                0,
                app.hwnd,
                None,
                instance(),
                None,
            )
            .unwrap_or_default();
            let _ = SetLayeredWindowAttributes(h, COLORREF(0), 80, LWA_ALPHA);
            app.chrome.overlay = hwnd_to_isize(h);
            h
        }
    } else {
        hwnd
    };
    unsafe {
        SetWindowLongPtrW(hwnd, GWLP_USERDATA, app.pal.accent.0 as isize);
        let _ = SetWindowPos(
            hwnd,
            HWND_TOP,
            r.left,
            r.top,
            r.right - r.left,
            r.bottom - r.top,
            SWP_NOACTIVATE | SWP_SHOWWINDOW,
        );
        let _ = InvalidateRect(hwnd, None, true);
    }
}

/// Screen rect highlighting a dock target (whole grid, or the half toward `side`).
fn dock_rect(app: &App, spot: DropSpot) -> Option<RECT> {
    let DropSpot::Dock(pane, side) = spot else { return None };
    let g = app.chrome.panes.iter().find(|p| p.pane == pane)?.grid;
    let (w, h) = (g.right - g.left, g.bottom - g.top);
    let r = match side {
        None => g,
        Some(Side::Left) => RECT { right: g.left + w / 2, ..g },
        Some(Side::Right) => RECT { left: g.right - w / 2, ..g },
        Some(Side::Top) => RECT { bottom: g.top + h / 2, ..g },
        Some(Side::Bottom) => RECT { top: g.bottom - h / 2, ..g },
    };
    let tl = client_to_screen(app.hwnd, POINT { x: r.left, y: r.top });
    Some(rc(tl.x, tl.y, r.right - r.left, r.bottom - r.top))
}

// ---------- Mouse ----------

/// Work that must run outside the `with_app` borrow (modal menus/dialogs pump messages).
enum Deferred {
    Favorites(RECT),
    Browse,
    TabMenu(TabId, POINT),
    Focus(HWND),
}

fn on_button_down(hwnd: HWND, x: i32, y: i32, dbl: bool) {
    let focus_edit = with_app(|app| {
        let h = hit(&app.chrome, x, y);
        match h {
            Some(Hot::Button(_)) | Some(Hot::TabClose(..)) | Some(Hot::Plus(_)) => {
                app.chrome.pressed = h;
                unsafe {
                    SetCapture(hwnd);
                }
                invalidate(hwnd);
            }
            Some(Hot::Tab(p, i)) => {
                app.activate_tab(p, i);
                if dbl {
                    app.toggle_maximize(p);
                } else if let Some(tab) = app.panes.get(&p).and_then(|pp| pp.tabs.get(i).copied()) {
                    app.chrome.tab_drag = Some(TabDrag { tab, start: POINT { x, y }, active: false, spot: None });
                    unsafe {
                        SetCapture(hwnd);
                    }
                }
            }
            Some(Hot::Splitter(i)) => {
                let s = app.chrome.splitters[i].clone();
                let grab = if s.horizontal { x - s.rect.x } else { y - s.rect.y };
                app.chrome.split_drag = Some((s, grab));
                unsafe {
                    SetCapture(hwnd);
                }
                invalidate(hwnd);
            }
            Some(Hot::Strip(p)) => {
                if dbl {
                    app.toggle_maximize(p);
                } else {
                    app.activate_pane(p);
                }
            }
            None => {
                let c = &app.chrome;
                return if pt_in(&c.path_rect, x, y) {
                    Some(app.path_edit)
                } else {
                    pt_in(&c.search_rect, x, y).then_some(app.search_edit)
                };
            }
        }
        None
    })
    .flatten();
    if let Some(edit) = focus_edit {
        unsafe {
            let _ = SetFocus(edit);
        }
    }
}

fn on_mouse_move(hwnd: HWND, x: i32, y: i32) {
    with_app(|app| {
        if let Some((s, grab)) = app.chrome.split_drag.clone() {
            let pos = if s.horizontal { x - grab } else { y - grab };
            app.layout.drag_splitter(&s, pos, scale(MIN_PANE, app.dpi));
            relayout(app);
            return;
        }
        if let Some(td) = &app.chrome.tab_drag {
            let (tab, start, mut active, old) = (td.tab, td.start, td.active, td.spot);
            if !active {
                let (cx, cy) = unsafe { (GetSystemMetrics(SM_CXDRAG), GetSystemMetrics(SM_CYDRAG)) };
                active = (x - start.x).abs() > cx || (y - start.y).abs() > cy;
            }
            let spot = if active { drop_spot(app, tab, x, y) } else { None };
            if let Some(td) = &mut app.chrome.tab_drag {
                td.active = active;
                td.spot = spot;
            }
            if spot != old {
                let r = spot.and_then(|s| dock_rect(app, s));
                show_overlay(app, r);
                invalidate(hwnd);
            }
            return;
        }
        let h = hit(&app.chrome, x, y);
        if h != app.chrome.hot {
            app.chrome.hot = h;
            invalidate(hwnd);
        }
        if !app.chrome.tracking {
            app.chrome.tracking = true;
            let mut tme = TRACKMOUSEEVENT {
                cbSize: std::mem::size_of::<TRACKMOUSEEVENT>() as u32,
                dwFlags: TME_LEAVE,
                hwndTrack: hwnd,
                dwHoverTime: 0,
            };
            unsafe {
                let _ = TrackMouseEvent(&mut tme);
            }
        }
    });
}

fn on_button_up(hwnd: HWND, x: i32, y: i32) {
    let deferred = with_app(|app| {
        let pressed = app.chrome.pressed.take();
        let tab_drag = app.chrome.tab_drag.take();
        let split = app.chrome.split_drag.take();
        unsafe {
            let _ = ReleaseCapture();
        }
        show_overlay(app, None);
        invalidate(hwnd);
        if split.is_some() {
            return None;
        }
        if let Some(td) = tab_drag {
            match td.spot.filter(|_| td.active) {
                Some(DropSpot::Strip(p, idx)) => app.move_tab(td.tab, p, idx),
                Some(DropSpot::Dock(p, None)) => app.move_tab(td.tab, p, usize::MAX),
                Some(DropSpot::Dock(p, Some(side))) => app.split_with_tab(td.tab, p, side),
                None => {}
            }
            return None;
        }
        let pressed = pressed.filter(|p| hit(&app.chrome, x, y) == Some(*p))?;
        match pressed {
            Hot::Button(b) => {
                let tid = app.active_tab()?;
                match b {
                    Btn::Up => app.go_up(tid),
                    Btn::Refresh => app.refresh_tab(tid),
                    Btn::ThisPc => {
                        app.navigate(tid, None, true);
                    }
                    Btn::Favorites => {
                        let r = app.chrome.buttons.iter().find(|(bb, _)| *bb == b).map(|(_, r)| *r)?;
                        return Some(Deferred::Favorites(r));
                    }
                    Btn::Browse => return Some(Deferred::Browse),
                    Btn::ClearFilter => {
                        app.set_filter(tid, "");
                        return app.panes.get(&app.active_pane).map(|p| Deferred::Focus(p.grid()));
                    }
                }
            }
            Hot::TabClose(p, i) => {
                if let Some(t) = app.panes.get(&p).and_then(|pp| pp.tabs.get(i).copied()) {
                    app.close_tab(t);
                }
            }
            Hot::Plus(p) => {
                app.activate_pane(p);
                app.open_tab(None, SortKey::Name, false);
            }
            _ => {}
        }
        None
    })
    .flatten();
    match deferred {
        Some(Deferred::Favorites(r)) => favorites_menu(hwnd, r),
        Some(Deferred::Browse) => browse(hwnd),
        Some(Deferred::Focus(h)) => unsafe {
            let _ = SetFocus(h);
        },
        Some(Deferred::TabMenu(..)) | None => {}
    }
}

fn on_middle_up(x: i32, y: i32) {
    with_app(|app| {
        if let Some((p, i)) = tab_at(app, x, y) {
            if let Some(t) = app.panes.get(&p).and_then(|pp| pp.tabs.get(i).copied()) {
                app.close_tab(t);
            }
        }
    });
}

fn on_right_up(hwnd: HWND, x: i32, y: i32) {
    let d = with_app(|app| {
        let (p, i) = tab_at(app, x, y)?;
        app.activate_tab(p, i);
        let tab = app.panes.get(&p)?.tabs.get(i).copied()?;
        Some(Deferred::TabMenu(tab, client_to_screen(app.hwnd, POINT { x, y })))
    })
    .flatten();
    if let Some(Deferred::TabMenu(tab, pt)) = d {
        tab_menu(hwnd, tab, pt);
    }
}

fn set_cursor(x: i32, y: i32) -> bool {
    with_app(|app| {
        let horizontal = match &app.chrome.split_drag {
            Some((s, _)) => Some(s.horizontal),
            None => match hit(&app.chrome, x, y) {
                Some(Hot::Splitter(i)) => Some(app.chrome.splitters[i].horizontal),
                _ => None,
            },
        };
        let Some(horizontal) = horizontal else { return false };
        unsafe {
            if let Ok(c) = LoadCursorW(None, if horizontal { IDC_SIZEWE } else { IDC_SIZENS }) {
                SetCursor(c);
            }
        }
        true
    })
    .unwrap_or(false)
}

// ---------- Menus & dialogs ----------

const TAB_CLOSE: u32 = 1;
const TAB_CLOSE_OTHERS: u32 = 2;
const TAB_CLOSE_LEFT: u32 = 3;
const TAB_CLOSE_RIGHT: u32 = 4;
const TAB_SPLIT_RIGHT: u32 = 5;
const TAB_SPLIT_DOWN: u32 = 6;
const TAB_MAXIMIZE: u32 = 7;

fn tab_menu(hwnd: HWND, tab: TabId, pt: POINT) {
    let info = with_app(|app| {
        let p = app.pane_of_tab(tab)?;
        let pane = app.panes.get(&p)?;
        let idx = pane.tabs.iter().position(|&t| t == tab)?;
        Some((idx, pane.tabs.len(), app.panes.len(), app.maximized == Some(p)))
    })
    .flatten();
    let Some((idx, n, npanes, maxed)) = info else { return };
    let cmd = unsafe {
        let Ok(m) = CreatePopupMenu() else { return };
        append_menu(m, TAB_CLOSE as usize, "Close\tCtrl+W", true);
        append_menu(m, TAB_CLOSE_OTHERS as usize, "Close other tabs", n > 1);
        append_menu(m, TAB_CLOSE_LEFT as usize, "Close tabs to the left", idx > 0);
        append_menu(m, TAB_CLOSE_RIGHT as usize, "Close tabs to the right", idx + 1 < n);
        append_sep(m);
        append_menu(m, TAB_SPLIT_RIGHT as usize, "Split right", true);
        append_menu(m, TAB_SPLIT_DOWN as usize, "Split down", true);
        append_menu(m, TAB_MAXIMIZE as usize, if maxed { "Restore pane" } else { "Maximize pane" }, npanes > 1);
        let cmd = track_menu(m, hwnd, pt, TRACK_POPUP_MENU_FLAGS(0));
        let _ = DestroyMenu(m);
        cmd
    };
    with_app(|app| {
        let Some(p) = app.pane_of_tab(tab) else { return };
        let tabs = app.panes[&p].tabs.clone();
        let Some(idx) = tabs.iter().position(|&t| t == tab) else { return };
        let close = |app: &mut App, list: &[TabId]| list.iter().for_each(|&t| app.close_tab(t));
        match cmd {
            TAB_CLOSE => app.close_tab(tab),
            TAB_CLOSE_OTHERS => close(app, &tabs.iter().copied().filter(|&t| t != tab).collect::<Vec<_>>()),
            TAB_CLOSE_LEFT => close(app, &tabs[..idx]),
            TAB_CLOSE_RIGHT => close(app, &tabs[idx + 1..]),
            TAB_SPLIT_RIGHT => app.split_new(p, Side::Right),
            TAB_SPLIT_DOWN => app.split_new(p, Side::Bottom),
            TAB_MAXIMIZE => app.toggle_maximize(p),
            _ => {}
        }
    });
}

const FAV_ADD: u32 = 1;
const FAV_BASE: u32 = 100;
const FAV_OPEN: u32 = 1;
const FAV_OPEN_TAB: u32 = 2;
const FAV_REMOVE: u32 = 3;

thread_local! {
    /// The favorites popup while it is open (for WM_MENURBUTTONUP).
    static FAV_MENU: Cell<isize> = const { Cell::new(0) };
    /// Action picked from a favorite's right-click menu: (favorite index, command).
    static FAV_PENDING: Cell<Option<(usize, u32)>> = const { Cell::new(None) };
    /// Path box colors, cached for WM_CTLCOLOREDIT arriving while the app is borrowed.
    static EDIT_COLORS: Cell<(COLORREF, COLORREF)> = const { Cell::new((COLORREF(0), COLORREF(0xFFFFFF))) };
}

fn favorites_menu(hwnd: HWND, button: RECT) {
    let info = with_app(|app| {
        let root = app.active_tab().and_then(|t| app.tabs.get(&t)).and_then(|t| t.root.clone());
        let favs: Vec<String> = app.settings.favorites.iter().map(|f| f.path.clone()).collect();
        (root, favs)
    });
    let Some((root, favs)) = info else { return };
    let current = root.as_deref().map(path_key);
    let is_fav = current.as_ref().is_some_and(|k| favs.iter().any(|f| path_key(f) == *k));
    let cmd = unsafe {
        let Ok(m) = CreatePopupMenu() else { return };
        let add = if is_fav { "Update sort for current folder" } else { "Add current folder" };
        append_menu(m, FAV_ADD as usize, add, root.is_some());
        append_sep(m);
        if favs.is_empty() {
            append_menu(m, 0, "No favorites yet", false);
        }
        for (i, f) in favs.iter().enumerate() {
            append_menu(m, FAV_BASE as usize + i, &f.replace('&', "&&"), true);
            if Some(path_key(f)) == current {
                let _ = CheckMenuItem(m, FAV_BASE + i as u32, MF_CHECKED.0);
            }
        }
        FAV_MENU.set(m.0 as isize);
        FAV_PENDING.set(None);
        let pt = client_to_screen(hwnd, POINT { x: button.left, y: button.bottom });
        let _ = SetForegroundWindow(hwnd);
        let cmd = TrackPopupMenuEx(m, (TPM_RETURNCMD | TPM_LEFTALIGN | TPM_TOPALIGN).0, pt.x, pt.y, hwnd, None).0 as u32;
        FAV_MENU.set(0);
        let _ = DestroyMenu(m);
        cmd
    };
    let ctrl_down = ctrl();
    with_app(|app| {
        if let Some((i, sub)) = FAV_PENDING.take() {
            match sub {
                FAV_OPEN => app.open_favorite(i, false),
                FAV_OPEN_TAB => app.open_favorite(i, true),
                FAV_REMOVE => app.remove_favorite(i),
                _ => {}
            }
        } else if cmd == FAV_ADD {
            app.add_favorite();
        } else if cmd >= FAV_BASE {
            app.open_favorite((cmd - FAV_BASE) as usize, ctrl_down);
        }
    });
}

/// Right-click on a favorite inside the open favorites menu.
fn on_menu_rbutton_up(hwnd: HWND, pos: usize, menu: HMENU) {
    if FAV_MENU.get() == 0 || FAV_MENU.get() != menu.0 as isize {
        return;
    }
    let id = unsafe { GetMenuItemID(menu, pos as i32) };
    if id == u32::MAX || id < FAV_BASE {
        return;
    }
    unsafe {
        let Ok(m) = CreatePopupMenu() else { return };
        append_menu(m, FAV_OPEN as usize, "Open", true);
        append_menu(m, FAV_OPEN_TAB as usize, "Open in new tab", true);
        append_sep(m);
        append_menu(m, FAV_REMOVE as usize, "Remove from favorites", true);
        let pt = cursor_pos();
        let cmd = TrackPopupMenuEx(m, (TPM_RETURNCMD | TPM_RECURSE | TPM_RIGHTBUTTON).0, pt.x, pt.y, hwnd, None).0 as u32;
        let _ = DestroyMenu(m);
        if cmd != 0 {
            FAV_PENDING.set(Some(((id - FAV_BASE) as usize, cmd)));
            let _ = EndMenu();
        }
    }
}

fn browse(hwnd: HWND) {
    let start = with_app(|app| app.active_tab().and_then(|t| app.tabs.get(&t)).and_then(|t| t.root.clone())).flatten();
    let picked = unsafe {
        (|| -> Option<String> {
            let dlg: IFileOpenDialog = CoCreateInstance(&FileOpenDialog, None, CLSCTX_INPROC_SERVER).ok()?;
            let opts = dlg.GetOptions().ok()?;
            dlg.SetOptions(opts | FOS_PICKFOLDERS | FOS_FORCEFILESYSTEM).ok()?;
            if let Some(item) = start.as_deref().and_then(shell::shell_item) {
                let _ = dlg.SetFolder(&item);
            }
            dlg.Show(hwnd).ok()?;
            shell::item_path(&dlg.GetResult().ok()?)
        })()
    };
    let Some(path) = picked else { return };
    let grid = with_app(|app| {
        let tid = app.active_tab()?;
        app.navigate(tid, Some(&path), true).then(|| app.panes[&app.active_pane].grid())
    })
    .flatten();
    if let Some(g) = grid {
        unsafe {
            let _ = SetFocus(g);
        }
    }
}

// ---------- Path box ----------

unsafe extern "system" fn edit_proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM, _id: usize, _data: usize) -> LRESULT {
    match msg {
        WM_KEYDOWN if wp.0 == VK_RETURN.0 as usize => {
            post_main(WM_PATH_ENTER);
            return LRESULT(0);
        }
        WM_KEYDOWN if wp.0 == VK_ESCAPE.0 as usize => {
            post_main(WM_PATH_ESCAPE);
            return LRESULT(0);
        }
        // Swallow Enter/Esc characters (no beep); Ctrl+A selects all.
        WM_CHAR if wp.0 == 0x0D || wp.0 == 0x1B => return LRESULT(0),
        WM_CHAR if wp.0 == 0x01 => {
            SendMessageW(hwnd, EM_SETSEL, WPARAM(0), LPARAM(-1));
            return LRESULT(0);
        }
        WM_SETFOCUS | WM_KILLFOCUS => post_main(WM_FOCUS_CHANGED),
        WM_NCDESTROY => {
            let _ = RemoveWindowSubclass(hwnd, Some(edit_proc), 1);
        }
        _ => {}
    }
    DefSubclassProc(hwnd, msg, wp, lp)
}

pub fn create_path_edit(parent: HWND) -> HWND {
    unsafe {
        let style = WS_CHILD | WS_VISIBLE | WS_TABSTOP | WINDOW_STYLE(ES_AUTOHSCROLL as u32);
        let h = CreateWindowExW(WINDOW_EX_STYLE(0), w!("EDIT"), PCWSTR::null(), style, 0, 0, 0, 0, parent, None, instance(), None)
            .unwrap_or_default();
        let _ = SetWindowSubclass(h, Some(edit_proc), 1, 0);
        h
    }
}

fn apply_edit_font(app: &App) {
    unsafe {
        for e in [app.path_edit, app.search_edit] {
            SendMessageW(e, WM_SETFONT, WPARAM(app.fonts.ui.0 as usize), LPARAM(1));
            SendMessageW(e, EM_SETMARGINS, WPARAM((EC_LEFTMARGIN | EC_RIGHTMARGIN) as usize), LPARAM(0));
        }
    }
}

// ---------- Filter box ----------

unsafe extern "system" fn search_proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM, _id: usize, _data: usize) -> LRESULT {
    match msg {
        WM_KEYDOWN if wp.0 == VK_ESCAPE.0 as usize => {
            post_main(WM_FILTER_ESCAPE);
            return LRESULT(0);
        }
        WM_KEYDOWN if wp.0 == VK_RETURN.0 as usize || wp.0 == VK_DOWN.0 as usize => {
            post_main(WM_FILTER_ENTER);
            return LRESULT(0);
        }
        WM_CHAR if wp.0 == 0x0D || wp.0 == 0x1B => return LRESULT(0),
        WM_CHAR if wp.0 == 0x01 => {
            SendMessageW(hwnd, EM_SETSEL, WPARAM(0), LPARAM(-1));
            return LRESULT(0);
        }
        WM_SETFOCUS | WM_KILLFOCUS => post_main(WM_FOCUS_CHANGED),
        WM_NCDESTROY => {
            let _ = RemoveWindowSubclass(hwnd, Some(search_proc), 2);
        }
        _ => {}
    }
    DefSubclassProc(hwnd, msg, wp, lp)
}

pub fn create_search_edit(parent: HWND) -> HWND {
    unsafe {
        let style = WS_CHILD | WS_VISIBLE | WS_TABSTOP | WINDOW_STYLE(ES_AUTOHSCROLL as u32);
        let h = CreateWindowExW(WINDOW_EX_STYLE(0), w!("EDIT"), PCWSTR::null(), style, 0, 0, 0, 0, parent, None, instance(), None)
            .unwrap_or_default();
        let _ = SetWindowSubclass(h, Some(search_proc), 2, 0);
        let cue = wide("Filter this tab (Ctrl+F)");
        SendMessageW(h, EM_SETCUEBANNER, WPARAM(1), LPARAM(cue.as_ptr() as isize));
        h
    }
}

/// The filter box text changed (typed, pasted, or synced from a tab).
fn on_filter_changed() {
    with_app(|app| {
        let Some(tid) = app.active_tab() else { return };
        let text = get_text(app.search_edit);
        app.set_filter(tid, &text);
    });
}

fn on_filter_escape() {
    with_app(|app| {
        if let Some(tid) = app.active_tab() {
            app.set_filter(tid, "");
        }
    });
    focus_active_grid();
}

fn on_focus_changed() {
    with_app(|app| unsafe {
        let f = GetFocus();
        let searching = f == app.search_edit;
        if app.chrome.search_editing != searching {
            app.chrome.search_editing = searching;
            invalidate(app.hwnd);
        }
        if f == app.path_edit {
            if !app.chrome.path_editing {
                app.chrome.path_editing = true;
                SendMessageW(app.path_edit, EM_SETSEL, WPARAM(0), LPARAM(-1));
                invalidate(app.hwnd);
            }
        } else {
            if app.chrome.path_editing {
                app.chrome.path_editing = false;
                app.sync_path_edit();
                invalidate(app.hwnd);
            }
            if let Some(p) = app.panes.values().find(|p| p.grid() == f).map(|p| p.id) {
                app.activate_pane(p);
            }
        }
        if !f.is_invalid() && IsChild(app.hwnd, f).as_bool() {
            app.chrome.last_focus = hwnd_to_isize(f);
        }
    });
}

fn on_path_enter() {
    let grid = with_app(|app| {
        let text = get_text(app.path_edit);
        let text = text.trim().trim_matches('"').trim();
        let expanded = expand_env(text);
        let tid = app.active_tab()?;
        let target = (!expanded.is_empty()).then_some(expanded.as_str());
        app.navigate(tid, target, true).then(|| {
            app.chrome.path_editing = false;
            app.sync_path_edit();
            app.panes[&app.active_pane].grid()
        })
    })
    .flatten();
    if let Some(g) = grid {
        unsafe {
            let _ = SetFocus(g);
        }
    }
}

fn expand_env(s: &str) -> String {
    if !s.contains('%') {
        return s.to_string();
    }
    let src = wide(s);
    let mut buf = vec![0u16; 4096];
    let n = unsafe { windows::Win32::System::Environment::ExpandEnvironmentStringsW(PCWSTR(src.as_ptr()), Some(&mut buf)) };
    if n == 0 || n as usize > buf.len() {
        return s.to_string();
    }
    from_wide(&buf)
}

fn on_path_escape() {
    let grid = with_app(|app| {
        app.chrome.path_editing = false;
        app.sync_path_edit();
        app.panes.get(&app.active_pane).map(|p| p.grid())
    })
    .flatten();
    if let Some(g) = grid {
        unsafe {
            let _ = SetFocus(g);
        }
    }
}

// ---------- Window procedure ----------

const CLASS: PCWSTR = w!("TwigMainWindow");

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    match msg {
        WM_PAINT => {
            if with_app(|app| paint_buffered(hwnd, |hdc, r| paint(app, hdc, r))).is_none() {
                // Painting while the state is borrowed (modal loop): validate now, repaint later.
                let mut ps = PAINTSTRUCT::default();
                BeginPaint(hwnd, &mut ps);
                let _ = EndPaint(hwnd, &ps);
                post(hwnd, WM_REPAINT, 0, 0);
            }
            return LRESULT(0);
        }
        WM_REPAINT => invalidate(hwnd),
        WM_ERASEBKGND => return LRESULT(1),
        WM_SIZE => {
            with_app(relayout);
        }
        WM_GETMINMAXINFO => {
            let mmi = &mut *(lp.0 as *mut MINMAXINFO);
            let dpi = GetDpiForWindow(hwnd).max(96);
            mmi.ptMinTrackSize = POINT { x: scale(480, dpi), y: scale(320, dpi) };
            return LRESULT(0);
        }
        WM_DPICHANGED => {
            let dpi = ((wp.0 >> 16) & 0xFFFF) as u32;
            with_app(|app| {
                app.set_dpi(dpi);
                apply_edit_font(app);
            });
            let r = &*(lp.0 as *const RECT);
            let _ = SetWindowPos(hwnd, HWND::default(), r.left, r.top, r.right - r.left, r.bottom - r.top, SWP_NOZORDER | SWP_NOACTIVATE);
            with_app(|app| {
                relayout(app);
                app.invalidate_grids();
            });
            return LRESULT(0);
        }
        WM_SETTINGCHANGE => {
            if lp.0 != 0 && PCWSTR(lp.0 as *const u16).to_string().is_ok_and(|s| s == "ImmersiveColorSet") {
                let dark = crate::theme::system_prefers_dark();
                crate::theme::init_app_dark_mode(dark);
                with_app(|app| {
                    if app.dark != dark {
                        app.set_dark(dark);
                        EDIT_COLORS.set((app.pal.text, app.pal.edit_bg));
                        crate::theme::set_control_theme(app.path_edit, dark, "DarkMode_CFD");
                        crate::theme::set_control_theme(app.search_edit, dark, "DarkMode_CFD");
                        app.invalidate_grids();
                        invalidate(app.path_edit);
                        invalidate(app.search_edit);
                        invalidate(hwnd);
                    }
                });
            }
        }
        WM_DEVICECHANGE => {
            with_app(|app| app.refresh_drives());
        }
        WM_CLIPBOARDUPDATE => {
            with_app(|app| app.on_clipboard_update());
            return LRESULT(0);
        }
        WM_LOADED => {
            with_app(|app| app.on_loaded());
            return LRESULT(0);
        }
        WM_WATCH => {
            with_app(|app| app.on_watch());
            return LRESULT(0);
        }
        WM_FILEOP => {
            with_app(|app| app.on_fileop());
            return LRESULT(0);
        }
        WM_FOCUS_CHANGED => {
            on_focus_changed();
            return LRESULT(0);
        }
        WM_PATH_ENTER => {
            on_path_enter();
            return LRESULT(0);
        }
        WM_PATH_ESCAPE => {
            on_path_escape();
            return LRESULT(0);
        }
        WM_COMMAND => {
            if (wp.0 >> 16) as u32 == EN_CHANGE {
                // Sent synchronously (possibly while the state is borrowed): handle it later.
                // on_filter_changed is a no-op unless the filter box text differs from the tab.
                post(hwnd, WM_FILTER_CHANGED, 0, 0);
                return LRESULT(0);
            }
        }
        WM_FILTER_CHANGED => {
            on_filter_changed();
            return LRESULT(0);
        }
        WM_FILTER_ESCAPE => {
            on_filter_escape();
            return LRESULT(0);
        }
        WM_FILTER_ENTER => {
            focus_active_grid();
            return LRESULT(0);
        }
        WM_TIMER => {
            match wp.0 {
                TIMER_STATUS => with_app(|app| app.clear_status()),
                TIMER_WATCH => with_app(|app| app.on_watch_timer()),
                TIMER_HOVER_TAB => with_app(crate::dragdrop::on_hover_timer),
                _ => None,
            };
            return LRESULT(0);
        }
        WM_LBUTTONDOWN | WM_LBUTTONDBLCLK => {
            on_button_down(hwnd, x_of(lp), y_of(lp), msg == WM_LBUTTONDBLCLK);
            return LRESULT(0);
        }
        WM_MOUSEMOVE => {
            on_mouse_move(hwnd, x_of(lp), y_of(lp));
            return LRESULT(0);
        }
        WM_LBUTTONUP => {
            on_button_up(hwnd, x_of(lp), y_of(lp));
            return LRESULT(0);
        }
        WM_MBUTTONUP => {
            on_middle_up(x_of(lp), y_of(lp));
            return LRESULT(0);
        }
        WM_RBUTTONUP => {
            on_right_up(hwnd, x_of(lp), y_of(lp));
            return LRESULT(0);
        }
        WM_MOUSELEAVE => {
            with_app(|app| {
                app.chrome.tracking = false;
                if app.chrome.hot.take().is_some() {
                    invalidate(hwnd);
                }
            });
            return LRESULT(0);
        }
        WM_CAPTURECHANGED => {
            // Capture lost mid-drag (Alt+Tab, another window grabbed it): cancel.
            with_app(|app| {
                if app.chrome.tab_drag.take().is_some() || app.chrome.split_drag.take().is_some() {
                    show_overlay(app, None);
                    invalidate(hwnd);
                }
                app.chrome.pressed = None;
            });
        }
        WM_SETCURSOR => {
            if HWND(wp.0 as _) == hwnd && (lp.0 & 0xFFFF) == 1 {
                let p = screen_to_client(hwnd, cursor_pos());
                if set_cursor(p.x, p.y) {
                    return LRESULT(1);
                }
            }
        }
        WM_CTLCOLOREDIT => {
            let (fg, bg) = with_app(|app| (app.pal.text, app.pal.edit_bg)).unwrap_or_else(|| EDIT_COLORS.get());
            EDIT_COLORS.set((fg, bg));
            let hdc = HDC(wp.0 as _);
            SetTextColor(hdc, fg);
            SetBkColor(hdc, bg);
            SetDCBrushColor(hdc, bg);
            return LRESULT(GetStockObject(DC_BRUSH).0 as isize);
        }
        WM_ACTIVATE => {
            if wp.0 & 0xFFFF != 0 {
                // Re-activated: restore the focus that was inside the window.
                let target = with_app(|app| {
                    let last = isize_to_hwnd(app.chrome.last_focus);
                    if app.chrome.last_focus != 0 && IsWindowVisible(last).as_bool() && IsChild(hwnd, last).as_bool() {
                        Some(last)
                    } else {
                        app.panes.get(&app.active_pane).map(|p| p.grid())
                    }
                })
                .flatten();
                if let Some(t) = target {
                    let _ = SetFocus(t);
                    return LRESULT(0);
                }
            }
        }
        WM_MENURBUTTONUP => {
            on_menu_rbutton_up(hwnd, wp.0, HMENU(lp.0 as _));
            return LRESULT(0);
        }
        WM_CLOSE => {
            with_app(|app| app.save_settings());
            let _ = RemoveClipboardFormatListener(hwnd);
            let _ = DestroyWindow(hwnd);
            return LRESULT(0);
        }
        WM_DESTROY => {
            PostQuitMessage(0);
            return LRESULT(0);
        }
        _ => {}
    }
    DefWindowProcW(hwnd, msg, wp, lp)
}

pub fn register_class() {
    unsafe {
        let small = LoadImageW(instance(), PCWSTR(1 as _), IMAGE_ICON, GetSystemMetrics(SM_CXSMICON), GetSystemMetrics(SM_CYSMICON), LR_DEFAULTCOLOR)
            .map(|h| HICON(h.0))
            .unwrap_or_default();
        let wc = WNDCLASSEXW {
            cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
            style: CS_DBLCLKS,
            lpfnWndProc: Some(wndproc),
            hInstance: instance(),
            hIcon: LoadIconW(instance(), PCWSTR(1 as _)).unwrap_or_default(),
            hIconSm: small,
            hCursor: LoadCursorW(None, IDC_ARROW).unwrap_or_default(),
            lpszClassName: CLASS,
            ..Default::default()
        };
        RegisterClassExW(&wc);
    }
}

/// Creates the (hidden) main window, restoring the saved placement. Returns the window and
/// whether it should be shown maximized.
pub fn create_main_window(settings: &crate::settings::Settings) -> (HWND, bool) {
    unsafe {
        let style = WS_OVERLAPPEDWINDOW | WS_CLIPCHILDREN;
        let hwnd = CreateWindowExW(
            WINDOW_EX_STYLE(0),
            CLASS,
            w!("Twig"),
            style,
            CW_USEDEFAULT,
            CW_USEDEFAULT,
            CW_USEDEFAULT,
            CW_USEDEFAULT,
            None,
            None,
            instance(),
            None,
        )
        .expect("CreateWindowExW");
        let mut maximized = false;
        if let Some(p) = settings.window.as_ref().filter(|p| p.w > 0 && p.h > 0) {
            // SetWindowPlacement uses workspace coordinates (what save_settings stored) and
            // moves the window back on screen if its monitor is gone.
            let wp = WINDOWPLACEMENT {
                length: std::mem::size_of::<WINDOWPLACEMENT>() as u32,
                showCmd: SW_HIDE.0 as u32,
                rcNormalPosition: RECT { left: p.x, top: p.y, right: p.x + p.w, bottom: p.y + p.h },
                ..Default::default()
            };
            let _ = SetWindowPlacement(hwnd, &wp);
            maximized = p.maximized;
        } else {
            let dpi = GetDpiForWindow(hwnd).max(96);
            let _ = SetWindowPos(hwnd, HWND::default(), 0, 0, scale(1100, dpi), scale(720, dpi), SWP_NOMOVE | SWP_NOZORDER | SWP_NOACTIVATE);
        }
        (hwnd, maximized)
    }
}

/// Sets up the path box font/theme after the app state is installed.
pub fn init_chrome() {
    with_app(|app| {
        apply_edit_font(app);
        EDIT_COLORS.set((app.pal.text, app.pal.edit_bg));
        crate::theme::set_control_theme(app.path_edit, app.dark, "DarkMode_CFD");
        crate::theme::set_control_theme(app.search_edit, app.dark, "DarkMode_CFD");
        app.sync_search_edit();
        relayout(app);
        unsafe {
            let _ = AddClipboardFormatListener(app.hwnd);
        }
    });
}

pub fn focus_active_grid() {
    if let Some(g) = with_app(|app| app.panes.get(&app.active_pane).map(|p| p.grid())).flatten() {
        unsafe {
            let _ = SetFocus(g);
        }
    }
}

// ---------- Global shortcuts ----------

/// Window-level shortcuts, checked before dispatching each message. Returns true if handled.
pub fn handle_shortcut(msg: &MSG) -> bool {
    if msg.message != WM_KEYDOWN {
        return false;
    }
    let main = main_hwnd();
    if msg.hwnd != main && unsafe { !IsChild(main, msg.hwnd).as_bool() } {
        return false;
    }
    if !ctrl() || alt() {
        return false;
    }
    let vk = msg.wParam.0 as u16;
    let sh = shift();
    match vk {
        0x54 if !sh => {
            // Ctrl+T
            with_app(|app| {
                let sort = app.active_tab().and_then(|t| app.tabs.get(&t)).map(|t| (t.tree.sort, t.tree.desc));
                let (sort, desc) = sort.unwrap_or((SortKey::Name, false));
                app.open_tab(None, sort, desc);
            });
            true
        }
        0x57 if !sh => {
            // Ctrl+W
            with_app(|app| {
                if let Some(t) = app.active_tab() {
                    app.close_tab(t);
                }
            });
            true
        }
        v if v == VK_TAB.0 => {
            with_app(|app| {
                let p = app.active_pane;
                let Some(pane) = app.panes.get(&p) else { return };
                let n = pane.tabs.len();
                if n > 1 {
                    let next = if sh { (pane.active + n - 1) % n } else { (pane.active + 1) % n };
                    app.activate_tab(p, next);
                    relayout(app);
                }
            });
            true
        }
        0x4C if !sh => {
            // Ctrl+L
            let edit = with_app(|app| app.path_edit);
            if let Some(e) = edit {
                unsafe {
                    let _ = SetFocus(e);
                    SendMessageW(e, EM_SETSEL, WPARAM(0), LPARAM(-1));
                }
            }
            true
        }
        0x46 if !sh => {
            // Ctrl+F: filter the active tab.
            let edit = with_app(|app| app.search_edit);
            if let Some(e) = edit {
                unsafe {
                    let _ = SetFocus(e);
                    SendMessageW(e, EM_SETSEL, WPARAM(0), LPARAM(-1));
                }
            }
            true
        }
        0x4E if sh => {
            // Ctrl+Shift+N: new folder next to the focused item.
            with_app(|app| {
                let Some(tid) = app.active_tab() else { return };
                match app.dest_for(tid, None) {
                    Some(dest) => app.new_item(tid, &dest, true),
                    None => app.set_status_error("Can't create a folder here".into()),
                }
            });
            true
        }
        _ => false,
    }
}
