//! The main window: per-pane tab strips,
//! splitters, tab drag & dock, status bar and top-level message handling.

use crate::app::{with_app, App, TIMER_HOVER_TAB, TIMER_OPEN_RETRY, TIMER_STATUS, TIMER_WATCH};
use crate::layout::{PaneId, Rect, Side, Splitter};
use crate::loader::TabId;
use crate::model::SortKey;
use crate::shell;
use crate::theme::glyph;
use crate::win::*;
use std::sync::atomic::{AtomicBool, Ordering as AtomicOrdering};
use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Dwm::{DwmSetWindowAttribute, DWMWA_CLOAK};
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::System::DataExchange::{AddClipboardFormatListener, RemoveClipboardFormatListener};
use windows::Win32::UI::Controls::*;
use windows::Win32::UI::HiDpi::{GetDpiForWindow, GetSystemMetricsForDpi};
use windows::Win32::UI::Input::KeyboardAndMouse::*;
use windows::Win32::UI::WindowsAndMessaging::*;

const STATUS: i32 = 24;
const STRIP: i32 = 34;
const SPLIT: i32 = 6;
const MIN_PANE: i32 = 120;
/// Room for each badge glyph (zoom, filter) before a tab's title.
const BADGE_W: i32 = 18;
/// Width of each caption button (minimize, maximize, close), as in Windows 11.
const CAPTION_W: i32 = 46;
/// Strip space kept free next to the caption buttons so the window can always be dragged.
const CAPTION_GAP: i32 = 24;
/// Height of the top resize band, inside the client area since the title bar is removed.
const TOP_RESIZE: i32 = 4;
/// Corner radius of tabs, and of the active tab's concave foot.
const TAB_RADIUS: i32 = 6;
/// Corner radius of each pane's card (the grid below its tab strip).
const CARD_RADIUS: i32 = 6;

/// The main window is minimized (its DWM surface is gone).
static MINIMIZED: AtomicBool = AtomicBool::new(false);
/// The main window is cloaked while it is restored from minimized.
static CLOAKED: AtomicBool = AtomicBool::new(false);

fn set_cloak(hwnd: HWND, on: bool) {
    let v: BOOL = on.into();
    unsafe {
        let _ = DwmSetWindowAttribute(hwnd, DWMWA_CLOAK, &v as *const _ as *const _, 4);
    }
}

/// What a tab header shows: title, zoomed-in badge, filter badge.
type TabLabel = (String, bool, bool);

fn tab_label(app: &App, tid: &TabId) -> TabLabel {
    app.tabs.get(tid).map(|t| (t.title(), t.zoomed(), !t.filter.is_empty())).unwrap_or_default()
}

/// True when `pane`'s tab headers were laid out for different labels (title or badges changed).
pub fn tabs_stale(app: &App, pane: PaneId) -> bool {
    let (Some(p), Some(pc)) = (app.panes.get(&pane), app.chrome.panes.iter().find(|c| c.pane == pane)) else { return false };
    pc.labels.len() != p.tabs.len() || p.tabs.iter().zip(&pc.labels).any(|(t, l)| tab_label(app, t) != *l)
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Hot {
    Tab(PaneId, usize),
    TabClose(PaneId, usize),
    Plus(PaneId),
    Splitter(usize),
    Strip(PaneId),
    Gear,
    Log,
    Help,
    CapMin,
    CapMax,
    CapClose,
}

impl Hot {
    fn is_caption(self) -> bool {
        matches!(self, Hot::CapMin | Hot::CapMax | Hot::CapClose)
    }
}

struct TabRect {
    rect: RECT,
    close: RECT,
    /// The close button on a hovered inactive tab, centered in its inset hover body.
    close_idle: RECT,
}

struct PaneChrome {
    pane: PaneId,
    strip: RECT,
    tabs: Vec<TabRect>,
    plus: RECT,
    grid: RECT,
    labels: Vec<TabLabel>,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum DropSpot {
    /// Insert into a tab strip before index.
    Strip(PaneId, usize),
    /// Split a pane, putting the tab on `side`.
    Dock(PaneId, Side),
}

struct TabDrag {
    tab: TabId,
    start: POINT,
    active: bool,
    spot: Option<DropSpot>,
}

#[derive(Default)]
pub struct Chrome {
    /// Tab under an OLE drag (activated after a delay).
    pub drag_hover: Option<(PaneId, usize)>,
    status: RECT,
    /// Settings button at the right end of the status bar.
    gear: RECT,
    /// Action log button, left of the gear.
    log: RECT,
    /// Keyboard shortcuts button, left of the log button.
    help: RECT,
    /// Caption buttons at the top right, drawn over the top-right pane's tab strip.
    cap_min: RECT,
    cap_max: RECT,
    cap_close: RECT,
    /// Strip-colored band above the top tab strips (the window drag area), 0 when maximized.
    top: i32,
    panes: Vec<PaneChrome>,
    splitters: Vec<Splitter>,
    hot: Option<Hot>,
    pressed: Option<Hot>,
    tracking: bool,
    /// Tracking WM_NCMOUSELEAVE (the caption buttons and drag area are non-client).
    nc_tracking: bool,
    tab_drag: Option<TabDrag>,
    /// Splitter being dragged and the grab offset inside it.
    split_drag: Option<(Splitter, i32)>,
    overlay: isize,
    last_focus: isize,
    /// Item/selection summary of the active tab, right-aligned in the status bar.
    summary: String,
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
    let sb = scale(STATUS, d);

    let c = &mut app.chrome;
    c.status = rc(0, h - sb, w, sb);
    c.gear = rc(w - scale(32, d), h - sb + 1, scale(32, d), sb - 1);
    c.log = rc(w - scale(64, d), h - sb + 1, scale(32, d), sb - 1);
    c.help = rc(w - scale(96, d), h - sb + 1, scale(32, d), sb - 1);

    let strip_h = scale(STRIP, d);
    let cap_w = scale(CAPTION_W, d);
    // Same gap above the tabs as between stacked panes; maximized windows keep tabs at the screen edge.
    let top = if unsafe { IsZoomed(hwnd).as_bool() } { 0 } else { scale(SPLIT, d) };
    c.top = top;
    c.cap_close = rc(w - cap_w, 0, cap_w, top + strip_h - 1);
    c.cap_max = rc(w - 2 * cap_w, 0, cap_w, top + strip_h - 1);
    c.cap_min = rc(w - 3 * cap_w, 0, cap_w, top + strip_h - 1);

    // Panes are cards on the strip-colored backdrop, with the same gutter on every side.
    let gut = scale(SPLIT, d);
    let area = Rect::new(gut, top, w - 2 * gut, h - sb - top - gut);
    let mut rects = Vec::new();
    let mut splitters = Vec::new();
    app.layout.arrange(area, scale(SPLIT, d), &mut rects, &mut splitters);
    app.chrome.splitters = splitters;

    let hdc = unsafe { GetDC(hwnd) };
    let mut panes = Vec::new();
    for &(pid, r) in &rects {
        let Some(p) = app.panes.get(&pid) else { continue };
        let strip = rc(r.x, r.y, r.w, strip_h.min(r.h));
        let grid = rc(r.x, r.y + strip_h, r.w, r.h - strip_h);
        let labels: Vec<TabLabel> = p.tabs.iter().map(|t| tab_label(app, t)).collect();
        // The top-right pane's strip doubles as the title bar: keep its tabs clear of the caption buttons.
        let reserve = if strip.top == top && strip.right == w - gut { 3 * cap_w + scale(CAPTION_GAP, d) - gut } else { 0 };
        let (tabs, plus) = layout_tabs(hdc, app.fonts.ui, d, &strip, reserve, &labels);
        panes.push(PaneChrome { pane: pid, strip, tabs, plus, grid, labels });
        unsafe {
            let g = p.grid();
            let (gw, gh) = (grid.right - grid.left, grid.bottom - grid.top);
            // The grid repaints fully anyway; copied bits would only smear stale content.
            let _ = SetWindowPos(g, HWND::default(), grid.left, grid.top, gw, gh, SWP_NOZORDER | SWP_NOACTIVATE | SWP_NOCOPYBITS);
            SetWindowRgn(g, round_region(gw, gh, scale(CARD_RADIUS, d)), true);
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

fn layout_tabs(hdc: HDC, font: HFONT, d: u32, strip: &RECT, reserve: i32, titles: &[TabLabel]) -> (Vec<TabRect>, RECT) {
    let top = strip.top + scale(4, d);
    let bottom = strip.bottom;
    let h = (bottom - top).max(0);
    let pad = scale(10, d);
    let close = scale(20, d);
    let plus_w = scale(30, d);
    // Clear of the card's rounded corner, so the active tab's foot fillet lands on its straight top edge.
    let left = strip.left + scale(CARD_RADIUS + TAB_RADIUS, d);
    let avail = (strip.right - reserve - left - plus_w - scale(6, d)).max(0);
    // Explorer's tab width; longer titles may grow a bit before ellipsizing.
    let (min_w, max_w) = (scale(232, d), scale(260, d));
    let mut widths: Vec<i32> = titles
        .iter()
        .map(|(t, zoomed, filtered)| {
            let badges = scale(BADGE_W, d) * (*zoomed as i32 + *filtered as i32);
            (pad + badges + text_width(hdc, font, t) + scale(6, d) + close + scale(6, d)).clamp(min_w, max_w)
        })
        .collect();
    let gap = scale(2, d);
    let n = widths.len() as i32;
    let total: i32 = widths.iter().sum::<i32>() + gap * (n - 1).max(0);
    if total > avail && n > 0 {
        let each = ((avail - gap * (n - 1)) / n).max(scale(44, d));
        widths.iter_mut().for_each(|w| *w = (*w).min(each));
    }
    let mut x = left;
    let mut tabs = Vec::with_capacity(widths.len());
    for w in widths {
        let rect = RECT { left: x, top, right: x + w, bottom };
        // The same gap to the tab's right edge as above and below.
        let inset = (h - close) / 2;
        let cx = rect.right - inset - close;
        let cy = top + inset;
        // The hover body is inset `gap` less at the sides than above and below.
        tabs.push(TabRect { rect, close: rc(cx, cy, close, close), close_idle: rc(cx + gap, cy, close, close) });
        x += w + gap;
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
    for (h, r) in [(Hot::CapMin, &c.cap_min), (Hot::CapMax, &c.cap_max), (Hot::CapClose, &c.cap_close)] {
        if pt_in(r, x, y) {
            return Some(h);
        }
    }
    if let Some(i) = c.splitters.iter().position(|s| s.rect.contains(x, y)) {
        return Some(Hot::Splitter(i));
    }
    if pt_in(&c.gear, x, y) {
        return Some(Hot::Gear);
    }
    if pt_in(&c.log, x, y) {
        return Some(Hot::Log);
    }
    if pt_in(&c.help, x, y) {
        return Some(Hot::Help);
    }
    for pc in &c.panes {
        if !pt_in(&pc.strip, x, y) {
            continue;
        }
        for (i, t) in pc.tabs.iter().enumerate() {
            if pt_in(&t.close, x, y) || pt_in(&t.close_idle, x, y) {
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

/// WM_NCHITTEST for a client point the default handler called HTCLIENT: the top resize band,
/// the caption buttons, and empty title-row strip space (drags the window).
fn caption_hit(app: &App, p: POINT) -> Option<u32> {
    let d = app.dpi;
    let maximized = unsafe { IsZoomed(app.hwnd).as_bool() };
    if !maximized && p.y >= 0 && p.y < scale(TOP_RESIZE, d) {
        let w = client_rect(app.hwnd).right;
        let corner = scale(16, d);
        return Some(if p.x < corner {
            HTTOPLEFT
        } else if p.x >= w - corner {
            HTTOPRIGHT
        } else {
            HTTOP
        });
    }
    let top = app.chrome.top;
    match hit(&app.chrome, p.x, p.y) {
        Some(Hot::CapMin) => Some(HTMINBUTTON),
        Some(Hot::CapMax) => Some(HTMAXBUTTON),
        Some(Hot::CapClose) => Some(HTCLOSE),
        Some(Hot::Strip(pane)) if app.chrome.panes.iter().any(|pc| pc.pane == pane && pc.strip.top == top) => Some(HTCAPTION),
        None if p.y >= 0 && p.y < top => Some(HTCAPTION),
        _ => None,
    }
}

/// Where a dragged tab would land at client point (x, y). Anywhere inside the window
/// snaps somewhere: the nearest pane's strip, or a split toward its nearest grid edge.
fn drop_spot(app: &App, tab: TabId, x: i32, y: i32) -> Option<DropSpot> {
    let src = app.pane_of_tab(tab)?;
    if !pt_in(&client_rect(app.hwnd), x, y) {
        return None;
    }
    // Nearest pane, so splitters and the status bar don't become dead zones.
    let pc = app.chrome.panes.iter().min_by_key(|pc| {
        let r = RECT { top: pc.strip.top, ..pc.grid };
        let dx = (r.left - x).max(x - (r.right - 1)).max(0) as i64;
        let dy = (r.top - y).max(y - (r.bottom - 1)).max(0) as i64;
        dx * dx + dy * dy
    })?;
    let strip_idx = |x: i32| pc.tabs.iter().position(|t| x < (t.rect.left + t.rect.right) / 2).unwrap_or(pc.tabs.len());
    if y < pc.grid.top {
        return Some(DropSpot::Strip(pc.pane, strip_idx(x)));
    }
    let src_tabs = app.panes.get(&src).map_or(0, |p| p.tabs.len());
    if pc.pane == src && src_tabs == 1 {
        // A lone tab can't split its own pane; it stays where it is.
        let at = app.panes.get(&src).and_then(|p| p.tabs.iter().position(|&t| t == tab)).unwrap_or(0);
        return Some(DropSpot::Strip(src, at));
    }
    let g = pc.grid;
    let fx = (x.clamp(g.left, g.right) - g.left) as f64 / (g.right - g.left).max(1) as f64;
    let fy = (y.clamp(g.top, g.bottom) - g.top) as f64 / (g.bottom - g.top).max(1) as f64;
    let edges = [(fx, Side::Left), (1.0 - fx, Side::Right), (fy, Side::Top), (1.0 - fy, Side::Bottom)];
    let side = edges.iter().copied().fold((f64::MAX, Side::Left), |a, b| if b.0 < a.0 { b } else { a }).1;
    Some(DropSpot::Dock(pc.pane, side))
}

// ---------- Painting ----------

fn paint(app: &App, hdc: HDC, client: &RECT) {
    let pal = app.pal;
    let c = &app.chrome;
    let d = app.dpi;
    fill(hdc, client, pal.strip);

    // Splitters
    for (i, s) in c.splitters.iter().enumerate() {
        let r = to_rect(s.rect);
        let active = c.hot == Some(Hot::Splitter(i)) || c.split_drag.as_ref().is_some_and(|(sp, _)| sp.path == s.path && sp.index == s.index);
        if active {
            fill(hdc, &r, pal.accent);
        } else {
            // The strip's darker tone runs between panes, framing each one like Explorer's window.
            fill(hdc, &r, pal.strip);
        }
    }

    // Tab strips
    let drag_target = c.tab_drag.as_ref().filter(|t| t.active).and_then(|t| t.spot);
    let rad = scale(TAB_RADIUS, d);
    fill(hdc, &rc(0, 0, client.right, c.top), pal.strip);
    for pc in &c.panes {
        fill(hdc, &pc.strip, pal.strip);
        // The card's header and breadcrumb bar meet its rounded corners.
        fill_corners(hdc, &pc.grid, scale(CARD_RADIUS, d), pal.toolbar, pal.strip);
        let Some(p) = app.panes.get(&pc.pane) else { continue };
        let hovered = |i: usize| {
            matches!(c.hot, Some(Hot::Tab(pp, ii)) | Some(Hot::TabClose(pp, ii)) if pp == pc.pane && ii == i) || c.drag_hover == Some((pc.pane, i))
        };
        // Separators between neighboring tabs that are both plain.
        let plain = |i: usize| i != p.active && !hovered(i);
        for (i, w) in pc.tabs.windows(2).enumerate() {
            if plain(i) && plain(i + 1) {
                let h = w[0].rect.bottom - w[0].rect.top;
                let x = (w[0].rect.right + w[1].rect.left) / 2;
                fill(hdc, &rc(x, w[0].rect.top + h / 4, 1, h - 2 * (h / 4)), mix(pal.strip, pal.text_dim, 0.3));
            }
        }
        if let Some(tr) = pc.tabs.get(p.active) {
            // The active tab shares the header's color and flows into it.
            fill_round(hdc, &tr.rect, rad, pal.toolbar, pal.strip, false);
            fill_fillets(hdc, &tr.rect, rad, pal.toolbar, pal.strip);
        }
        // Drawn after the active tab so its foot fillet can't notch the fill.
        for (i, tr) in pc.tabs.iter().enumerate() {
            if i != p.active && hovered(i) {
                // One margin all around: above the header, and beside neighbors
                // (the side inset plus the inter-tab gap). Keeps contents centered.
                let m = scale(3, d);
                let side = m - scale(2, d);
                let body = RECT { left: tr.rect.left + side, top: tr.rect.top + m, right: tr.rect.right - side, bottom: tr.rect.bottom - m };
                fill_round(hdc, &body, rad, pal.strip_hover, pal.strip, true);
            }
        }
        for (i, tr) in pc.tabs.iter().enumerate() {
            let Some(tab) = p.tabs.get(i).and_then(|t| app.tabs.get(t)) else { continue };
            let active = i == p.active;
            let hovered = hovered(i);
            let close_r = if active { &tr.close } else { &tr.close_idle };
            let mut text_r = RECT { left: tr.rect.left + scale(10, d), right: tr.close.left - scale(2, d), ..tr.rect };
            let badges = [(tab.zoomed(), glyph::ZOOM), (!tab.filter.is_empty(), glyph::FILTER)];
            for (_, g) in badges.into_iter().filter(|b| b.0) {
                let badge = RECT { right: text_r.left + scale(BADGE_W, d), ..text_r };
                unsafe {
                    SelectObject(hdc, app.fonts.glyph_small);
                }
                draw_text(hdc, g, &badge, pal.accent, DT_LEFT_V);
                text_r.left = badge.right;
            }
            unsafe {
                SelectObject(hdc, app.fonts.ui);
            }
            draw_text(hdc, &tab.title(), &text_r, if active { pal.text } else { pal.text_soft }, DT_LEFT_V);
            if active || hovered {
                if c.hot == Some(Hot::TabClose(pc.pane, i)) {
                    let under = if active { pal.toolbar } else { pal.strip_hover };
                    fill_round(hdc, close_r, scale(4, d), pal.tab_hover, under, true);
                }
                unsafe {
                    SelectObject(hdc, app.fonts.glyph_tab);
                }
                draw_glyph(hdc, glyph::CLOSE, close_r, pal.text_dim);
            }
        }
        if c.hot == Some(Hot::Plus(pc.pane)) {
            fill_round(hdc, &pc.plus, rad, pal.strip_hover, pal.strip, true);
        }
        unsafe {
            SelectObject(hdc, app.fonts.glyph_tab);
        }
        draw_glyph(hdc, glyph::ADD, &pc.plus, pal.text);
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

    // Caption buttons
    let foreground = unsafe { GetActiveWindow() } == app.hwnd;
    let max_glyph = if unsafe { IsZoomed(app.hwnd).as_bool() } { glyph::RESTORE } else { glyph::MAXIMIZE };
    unsafe {
        SelectObject(hdc, app.fonts.glyph_tab);
    }
    for (h, r, g) in [(Hot::CapMin, &c.cap_min, glyph::MINIMIZE), (Hot::CapMax, &c.cap_max, max_glyph), (Hot::CapClose, &c.cap_close, glyph::CLOSE)] {
        let pressed = c.pressed == Some(h);
        let hot = c.hot == Some(h) && (c.pressed.is_none() || pressed);
        let mut color = if foreground { pal.text } else { pal.text_dim };
        if hot && h == Hot::CapClose {
            fill(hdc, r, if pressed { rgb(0xC83C31) } else { rgb(0xC42B1C) });
            color = rgb(0xFFFFFF);
        } else if hot {
            fill(hdc, r, if pressed { pal.caption_pressed } else { pal.caption_hover });
        }
        draw_glyph(hdc, g, r, color);
    }

    // Status bar
    fill(hdc, &c.status, pal.toolbar);
    fill(hdc, &rc(0, c.status.top, client.right, 1), pal.border);
    let sr = RECT { left: c.status.left + scale(10, d), right: c.status.right - scale(10, d), ..c.status };
    unsafe {
        SelectObject(hdc, app.fonts.ui);
    }
    let filter_status;
    let hint = app.status.is_empty().then(|| crate::grid::bar_hint(app).or_else(|| app.filter_status())).flatten();
    let (status, color) = match hint {
        Some(s) => {
            filter_status = s;
            (&filter_status, pal.accent)
        }
        None => (&app.status, if app.status_error { pal.error } else { pal.text_dim }),
    };
    let mut sr = sr;
    unsafe {
        SelectObject(hdc, app.fonts.glyph_mid);
    }
    for (h, r, g) in [(Hot::Gear, &c.gear, glyph::GEAR), (Hot::Log, &c.log, glyph::HISTORY), (Hot::Help, &c.help, glyph::HELP)] {
        if c.hot == Some(h) {
            fill(hdc, r, if c.pressed == Some(h) { pal.sel_inactive } else { pal.hover });
        }
        draw_text(hdc, g, r, if c.hot == Some(h) { pal.text } else { pal.text_dim }, DT_CENTER_V);
    }
    unsafe {
        SelectObject(hdc, app.fonts.ui);
    }
    sr.right = c.help.left - scale(4, d);
    if !c.summary.is_empty() {
        draw_text(hdc, &c.summary, &sr, pal.text_dim, DT_RIGHT_V);
        sr.right -= text_width(hdc, app.fonts.ui, &c.summary) + scale(24, d);
    }
    draw_text(hdc, status, &sr, color, DT_LEFT_V);
}

/// Updates the status-bar item summary, repainting the status bar if it changed.
pub fn set_summary(app: &mut App, s: String) {
    if app.chrome.summary != s {
        app.chrome.summary = s;
        unsafe {
            let _ = InvalidateRect(app.hwnd, Some(&app.chrome.status), false);
        }
    }
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

/// Screen rect highlighting a dock target (the half of the grid toward `side`).
fn dock_rect(app: &App, spot: DropSpot) -> Option<RECT> {
    let DropSpot::Dock(pane, side) = spot else { return None };
    let g = app.chrome.panes.iter().find(|p| p.pane == pane)?.grid;
    let (w, h) = (g.right - g.left, g.bottom - g.top);
    let r = match side {
        Side::Left => RECT { right: g.left + w / 2, ..g },
        Side::Right => RECT { left: g.right - w / 2, ..g },
        Side::Top => RECT { bottom: g.top + h / 2, ..g },
        Side::Bottom => RECT { top: g.bottom - h / 2, ..g },
    };
    let tl = client_to_screen(app.hwnd, POINT { x: r.left, y: r.top });
    Some(rc(tl.x, tl.y, r.right - r.left, r.bottom - r.top))
}

// ---------- Mouse ----------

/// Work that must run outside the `with_app` borrow (modal menus/dialogs pump messages).
enum Deferred {
    TabMenu(TabId, POINT),
    PlusMenu(PaneId, POINT),
}

fn on_button_down(hwnd: HWND, x: i32, y: i32) {
    with_app(|app| {
        let h = hit(&app.chrome, x, y);
        match h {
            Some(Hot::TabClose(..)) | Some(Hot::Plus(_)) | Some(Hot::Gear) | Some(Hot::Log) | Some(Hot::Help) | Some(Hot::CapMin) | Some(Hot::CapMax) | Some(Hot::CapClose) => {
                app.chrome.pressed = h;
                app.chrome.hot = h;
                unsafe {
                    SetCapture(hwnd);
                }
                invalidate(hwnd);
            }
            Some(Hot::Tab(p, i)) => {
                app.activate_tab(p, i);
                if let Some(tab) = app.panes.get(&p).and_then(|pp| pp.tabs.get(i).copied()) {
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
            Some(Hot::Strip(p)) => app.activate_pane(p),
            None => {}
        }
    });
}

fn on_mouse_move(hwnd: HWND, x: i32, y: i32) {
    let dragged = with_app(|app| {
        let (s, grab) = app.chrome.split_drag.clone()?;
        let pos = if s.horizontal { x - grab } else { y - grab };
        app.layout.drag_splitter(&s, pos, scale(MIN_PANE, app.dpi));
        relayout(app);
        Some(())
    });
    if dragged.flatten().is_some() {
        // Mouse moves outpace relayout, so queued WM_PAINTs would starve until the drag
        // pauses, leaving stale pixels behind. Paint the whole tree now (outside the borrow).
        unsafe {
            let _ = RedrawWindow(hwnd, None, None, RDW_INVALIDATE | RDW_UPDATENOW | RDW_ALLCHILDREN);
        }
        return;
    }
    with_app(|app| {
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
    let open = with_app(|app| {
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
                Some(DropSpot::Dock(p, side)) => app.split_with_tab(td.tab, p, side),
                None => {}
            }
            return None;
        }
        let Some(pressed) = pressed.filter(|p| hit(&app.chrome, x, y) == Some(*p)) else { return None };
        match pressed {
            Hot::TabClose(p, i) => {
                if let Some(t) = app.panes.get(&p).and_then(|pp| pp.tabs.get(i).copied()) {
                    app.close_tab(t);
                }
            }
            Hot::Plus(p) => {
                app.activate_pane(p);
                app.open_tab(None, SortKey::Name, false);
            }
            Hot::Gear => return Some(Hot::Gear),
            Hot::Log => return Some(Hot::Log),
            Hot::Help => return Some(Hot::Help),
            h if h.is_caption() => {
                // The window is about to minimize/close; don't leave the button lit.
                app.chrome.hot = None;
                return Some(h);
            }
            _ => {}
        }
        None
    });
    let sys = |cmd: u32| post(hwnd, WM_SYSCOMMAND, cmd as usize, 0);
    match open.flatten() {
        Some(Hot::Gear) => open_settings(hwnd),
        Some(Hot::Log) => open_log(hwnd),
        Some(Hot::Help) => open_help(hwnd),
        Some(Hot::CapMin) => sys(SC_MINIMIZE),
        Some(Hot::CapMax) => sys(if unsafe { IsZoomed(hwnd).as_bool() } { SC_RESTORE } else { SC_MAXIMIZE }),
        Some(Hot::CapClose) => sys(SC_CLOSE),
        _ => {}
    }
}

/// The hot item under the cursor if it is over the window's own client area (not a child).
fn hot_under_cursor(app: &App) -> Option<Hot> {
    let p = screen_to_client(app.hwnd, cursor_pos());
    let over = unsafe { WindowFromPoint(cursor_pos()) } == app.hwnd;
    over.then(|| hit(&app.chrome, p.x, p.y)).flatten()
}

/// Hover over the non-client parts of the strip: caption buttons and the drag area.
fn on_nc_mouse_move(hwnd: HWND) {
    with_app(|app| {
        let h = hot_under_cursor(app);
        if h != app.chrome.hot {
            app.chrome.hot = h;
            invalidate(hwnd);
        }
        if !app.chrome.nc_tracking {
            app.chrome.nc_tracking = true;
            let mut tme = TRACKMOUSEEVENT {
                cbSize: std::mem::size_of::<TRACKMOUSEEVENT>() as u32,
                dwFlags: TME_LEAVE | TME_NONCLIENT,
                hwndTrack: hwnd,
                dwHoverTime: 0,
            };
            unsafe {
                let _ = TrackMouseEvent(&mut tme);
            }
        }
    });
}

/// The cursor left the client (`nc` false) or non-client area: it may have moved to the other one,
/// so re-hit-test rather than clearing the hot item.
fn on_mouse_leave(hwnd: HWND, nc: bool) {
    with_app(|app| {
        if nc {
            app.chrome.nc_tracking = false;
        } else {
            app.chrome.tracking = false;
        }
        if app.chrome.pressed.is_some() {
            return;
        }
        let h = hot_under_cursor(app);
        if h != app.chrome.hot {
            app.chrome.hot = h;
            invalidate(hwnd);
        }
    });
}

/// Opens (or focuses) the keyboard shortcuts window.
fn open_help(hwnd: HWND) {
    let dark = with_app(|app| app.dark).unwrap_or(false);
    crate::helpwin::show(hwnd, dark);
}

/// Opens (or focuses) the action log window.
fn open_log(hwnd: HWND) {
    let dark = with_app(|app| app.dark).unwrap_or(false);
    crate::logwin::show(hwnd, dark);
}

/// Opens (or focuses) the settings window.
fn open_settings(hwnd: HWND) {
    let dark = with_app(|app| app.dark).unwrap_or(false);
    crate::settingswin::show(hwnd, dark);
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
        if let Some(Hot::Plus(p)) = hit(&app.chrome, x, y) {
            return Some(Deferred::PlusMenu(p, client_to_screen(app.hwnd, POINT { x, y })));
        }
        let (p, i) = tab_at(app, x, y)?;
        app.activate_tab(p, i);
        let tab = app.panes.get(&p)?.tabs.get(i).copied()?;
        Some(Deferred::TabMenu(tab, client_to_screen(app.hwnd, POINT { x, y })))
    })
    .flatten();
    match d {
        Some(Deferred::TabMenu(tab, pt)) => tab_menu(hwnd, tab, pt),
        Some(Deferred::PlusMenu(p, pt)) => plus_menu(hwnd, p, pt),
        None => {}
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
const PLUS_NEW: u32 = 7;
const PLUS_CLIPBOARD: u32 = 8;

/// The "+" button's context menu: a new tab, or one on the path in the clipboard.
fn plus_menu(hwnd: HWND, pane: PaneId, pt: POINT) {
    let cmd = unsafe {
        let Ok(m) = CreatePopupMenu() else { return };
        append_menu(m, PLUS_NEW as usize, "New tab\tCtrl+T", true);
        append_menu(m, PLUS_CLIPBOARD as usize, "Open path from clipboard", shell::clipboard_has_text());
        let cmd = track_menu(m, hwnd, pt, TRACK_POPUP_MENU_FLAGS(0));
        let _ = DestroyMenu(m);
        cmd
    };
    if cmd == 0 {
        return;
    }
    let text = if cmd == PLUS_CLIPBOARD { shell::clipboard_text(hwnd).unwrap_or_default() } else { String::new() };
    with_app(|app| {
        app.activate_pane(pane);
        let (sort, desc) = app.active_sort();
        match cmd {
            PLUS_NEW => {
                app.open_tab(None, sort, desc);
            }
            PLUS_CLIPBOARD => app.open_clipboard_path(&text),
            _ => {}
        }
    });
}

fn tab_menu(hwnd: HWND, tab: TabId, pt: POINT) {
    let info = with_app(|app| {
        let p = app.pane_of_tab(tab)?;
        let pane = app.panes.get(&p)?;
        let idx = pane.tabs.iter().position(|&t| t == tab)?;
        Some((idx, pane.tabs.len()))
    })
    .flatten();
    let Some((idx, n)) = info else { return };
    let cmd = unsafe {
        let Ok(m) = CreatePopupMenu() else { return };
        append_menu(m, TAB_CLOSE as usize, "Close\tCtrl+W", true);
        append_menu(m, TAB_CLOSE_OTHERS as usize, "Close other tabs", n > 1);
        append_menu(m, TAB_CLOSE_LEFT as usize, "Close tabs to the left", idx > 0);
        append_menu(m, TAB_CLOSE_RIGHT as usize, "Close tabs to the right", idx + 1 < n);
        append_sep(m);
        append_menu(m, TAB_SPLIT_RIGHT as usize, "Split right", true);
        append_menu(m, TAB_SPLIT_DOWN as usize, "Split down", true);
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
            _ => {}
        }
    });
}

fn on_focus_changed() {
    with_app(|app| unsafe {
        let f = GetFocus();
        if let Some(p) = app.panes.values().find(|p| p.grid() == f).map(|p| p.id) {
            app.activate_pane(p);
        }
        if !f.is_invalid() && IsChild(app.hwnd, f).as_bool() {
            app.chrome.last_focus = hwnd_to_isize(f);
        }
    });
}

// ---------- Window procedure ----------

// Debug builds use their own class (and instance mutex) so they never forward to a release Twig.
#[cfg(debug_assertions)]
pub const CLASS: PCWSTR = w!("TwigMainWindowDebug");
#[cfg(not(debug_assertions))]
pub const CLASS: PCWSTR = w!("TwigMainWindow");

/// Opens paths forwarded by other launches; retries later while the app is busy (modal loop).
fn open_pending(hwnd: HWND) {
    crate::instance::bring_to_front(hwnd);
    let opened = with_app(|app| {
        for p in crate::instance::take_pending() {
            if p.reveal {
                app.reveal_external(&p.path);
            } else {
                app.open_external(&p.path);
            }
        }
    });
    if opened.is_none() {
        unsafe { SetTimer(hwnd, TIMER_OPEN_RETRY, 250, None) };
    }
}

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
        WM_COPYDATA => {
            if crate::instance::receive(hwnd, lp) {
                return LRESULT(1);
            }
        }
        WM_OPEN_PATH => {
            open_pending(hwnd);
            return LRESULT(0);
        }
        WM_UPDATE => {
            crate::settingswin::update_changed();
            return LRESULT(0);
        }
        WM_ERASEBKGND => return LRESULT(1),
        WM_NCCALCSIZE if wp.0 != 0 => {
            // Remove the title bar: keep the default side/bottom borders but let the client area
            // reach the top of the window. Maximized windows hang their frame off-screen, so inset.
            let params = &mut *(lp.0 as *mut NCCALCSIZE_PARAMS);
            let top = params.rgrc[0].top;
            DefWindowProcW(hwnd, msg, wp, lp);
            params.rgrc[0].top = top;
            if IsZoomed(hwnd).as_bool() {
                let dpi = GetDpiForWindow(hwnd).max(96);
                params.rgrc[0].top += GetSystemMetricsForDpi(SM_CYFRAME, dpi) + GetSystemMetricsForDpi(SM_CXPADDEDBORDER, dpi);
            }
            return LRESULT(0);
        }
        WM_NCHITTEST => {
            let r = DefWindowProcW(hwnd, msg, wp, lp);
            if r.0 as u32 != HTCLIENT {
                return r;
            }
            let p = screen_to_client(hwnd, POINT { x: x_of(lp), y: y_of(lp) });
            let ht = with_app(|app| caption_hit(app, p)).flatten();
            return LRESULT(ht.unwrap_or(HTCLIENT) as isize);
        }
        WM_NCLBUTTONDOWN | WM_NCLBUTTONDBLCLK => {
            let ht = wp.0 as u32;
            if matches!(ht, HTMINBUTTON | HTMAXBUTTON | HTCLOSE | HTCAPTION) {
                let p = screen_to_client(hwnd, POINT { x: x_of(lp), y: y_of(lp) });
                on_button_down(hwnd, p.x, p.y);
                // The default handler would draw classic caption buttons over ours.
                if ht != HTCAPTION {
                    return LRESULT(0);
                }
            }
        }
        WM_NCMOUSEMOVE => on_nc_mouse_move(hwnd),
        WM_NCMOUSELEAVE => on_mouse_leave(hwnd, true),
        WM_SIZE => {
            if wp.0 as u32 == SIZE_MINIMIZED {
                MINIMIZED.store(true, AtomicOrdering::Relaxed);
                return LRESULT(0);
            }
            with_app(relayout);
        }
        WM_WINDOWPOSCHANGING => {
            // Restoring from minimized discards the window's surface, and DWM would present it
            // black until every pane has painted. Cloak until the whole tree is repainted.
            let pos = &*(lp.0 as *const WINDOWPOS);
            if MINIMIZED.load(AtomicOrdering::Relaxed) && !pos.flags.contains(SWP_NOSIZE) {
                set_cloak(hwnd, true);
                CLOAKED.store(true, AtomicOrdering::Relaxed);
            }
        }
        WM_WINDOWPOSCHANGED => {
            let r = DefWindowProcW(hwnd, msg, wp, lp);
            if !IsIconic(hwnd).as_bool() {
                MINIMIZED.store(false, AtomicOrdering::Relaxed);
            }
            if CLOAKED.swap(false, AtomicOrdering::Relaxed) {
                let _ = RedrawWindow(hwnd, None, None, RDW_INVALIDATE | RDW_UPDATENOW | RDW_ALLCHILDREN | RDW_FRAME);
                set_cloak(hwnd, false);
            }
            return r;
        }
        WM_GETMINMAXINFO => {
            let mmi = &mut *(lp.0 as *mut MINMAXINFO);
            let dpi = GetDpiForWindow(hwnd).max(96);
            mmi.ptMinTrackSize = POINT { x: scale(480, dpi), y: scale(320, dpi) };
            return LRESULT(0);
        }
        WM_DPICHANGED => {
            let dpi = ((wp.0 >> 16) & 0xFFFF) as u32;
            with_app(|app| app.set_dpi(dpi));
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
                        app.invalidate_grids();
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
        WM_SCAN => {
            with_app(|app| app.on_scan());
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
        WM_TIMER => {
            match wp.0 {
                TIMER_STATUS => with_app(|app| app.clear_status()),
                TIMER_WATCH => with_app(|app| app.on_watch_timer()),
                TIMER_HOVER_TAB => with_app(crate::dragdrop::on_hover_timer),
                TIMER_OPEN_RETRY => {
                    let _ = KillTimer(hwnd, TIMER_OPEN_RETRY);
                    open_pending(hwnd);
                    None
                }
                _ => None,
            };
            return LRESULT(0);
        }
        WM_LBUTTONDOWN | WM_LBUTTONDBLCLK => {
            on_button_down(hwnd, x_of(lp), y_of(lp));
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
            on_mouse_leave(hwnd, false);
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
        WM_ACTIVATE => {
            // Caption glyphs dim when inactive.
            invalidate(hwnd);
            // The active pane's selection color depends on whether the window is in the foreground.
            if let Some(Some(g)) = with_app(|app| app.panes.get(&app.active_pane).map(|p| p.grid())) {
                invalidate(g);
            }
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
        // Recompute the frame with the custom WM_NCCALCSIZE (no title bar).
        let _ = SetWindowPos(hwnd, HWND::default(), 0, 0, 0, 0, SWP_FRAMECHANGED | SWP_NOMOVE | SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE);
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

/// Lays out the window chrome after the app state is installed.
pub fn init_chrome() {
    with_app(|app| {
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
    let main = main_hwnd();
    if msg.message == WM_SYSKEYDOWN && msg.wParam.0 == 0x53 && !ctrl() && !shift() {
        // Alt+S: toggle subfolder search
        if msg.hwnd != main && unsafe { !IsChild(main, msg.hwnd).as_bool() } {
            return false;
        }
        with_app(|app| {
            if let Some(t) = app.active_tab() {
                crate::grid::toggle_deep(app, t);
            }
        });
        return true;
    }
    if msg.message == WM_SYSKEYDOWN && (msg.wParam.0 == 0x25 || msg.wParam.0 == 0x27) && !ctrl() && !shift() {
        // Alt+Left / Alt+Right: back / forward in the active tab's history
        if msg.hwnd != main && unsafe { !IsChild(main, msg.hwnd).as_bool() } {
            return false;
        }
        let back = msg.wParam.0 == 0x25;
        with_app(|app| {
            if let Some(t) = app.active_tab() {
                crate::grid::nav_history(app, t, back);
            }
        });
        return true;
    }
    if msg.message != WM_KEYDOWN {
        return false;
    }
    if msg.hwnd != main && unsafe { !IsChild(main, msg.hwnd).as_bool() } {
        return false;
    }
    if msg.wParam.0 as u16 == VK_F1.0 && !ctrl() && !alt() && !shift() {
        open_help(main);
        return true;
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
                let (sort, desc) = app.active_sort();
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
        0x46 if !sh => {
            // Ctrl+F: filter the active tab (":" in its breadcrumb bar).
            with_app(|app| {
                let p = app.active_pane;
                crate::grid::begin_path_edit(app, p, true);
            });
            true
        }
        0x4C if !sh => {
            // Ctrl+L: type a path into the active tab's breadcrumb bar.
            with_app(|app| {
                let p = app.active_pane;
                crate::grid::begin_path_edit(app, p, false);
            });
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
        0xBC if !sh => {
            // Ctrl+, (VK_OEM_COMMA): settings
            open_settings(main);
            true
        }
        0x48 if !sh => {
            // Ctrl+H: action log
            open_log(main);
            true
        }
        _ => false,
    }
}
