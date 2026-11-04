//! The action log window: every copy, move, rename, delete, new item and undo, newest first.
//! Clicking an entry shows where it happened in the active tab.
//!
//! Owner-drawn and themed like the settings window; the list scrolls with a standard scrollbar.

use crate::actionlog::{self, Entry, Kind};
use crate::theme::{glyph, Fonts, Palette};
use crate::win::*;
use std::cell::RefCell;
use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::UI::Controls::{SetScrollInfo, WM_MOUSELEAVE};
use windows::Win32::UI::HiDpi::GetDpiForWindow;
use windows::Win32::UI::Input::KeyboardAndMouse::*;
use windows::Win32::UI::WindowsAndMessaging::*;

const CLASS: PCWSTR = w!("TwigLog");
// Layout, in 96-DPI pixels.
const WIDTH: i32 = 600;
const HEIGHT: i32 = 640;
const MARGIN: i32 = 16;
const HEADER: i32 = 56;
const ROW: i32 = 50;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Hot {
    Row(usize),
    Clear,
}

struct State {
    hwnd: HWND,
    owner: HWND,
    dpi: u32,
    dark: bool,
    pal: Palette,
    fonts: Fonts,
    heading: HFONT,
    entries: Vec<Entry>,
    /// First visible row.
    top: usize,
    /// Keyboard / last clicked row.
    sel: Option<usize>,
    hot: Option<Hot>,
    pressed: Option<Hot>,
    tracking: bool,
}

impl Drop for State {
    fn drop(&mut self) {
        unsafe {
            let _ = DeleteObject(self.heading);
        }
    }
}

thread_local! {
    static STATE: RefCell<Option<State>> = const { RefCell::new(None) };
}

fn with_state<R>(f: impl FnOnce(&mut State) -> R) -> Option<R> {
    STATE.with(|s| s.try_borrow_mut().ok().and_then(|mut s| s.as_mut().map(f)))
}

fn glyph_of(k: Kind) -> &'static str {
    match k {
        Kind::Copy => glyph::COPY,
        Kind::Move => glyph::MOVE,
        Kind::Rename => glyph::RENAME,
        Kind::Recycle | Kind::Delete => glyph::DELETE,
        Kind::NewFolder => glyph::NEW_FOLDER,
        Kind::NewFile => glyph::NEW_FILE,
        Kind::Undo => glyph::UNDO,
    }
}

fn unix_to_filetime(secs: u64) -> u64 {
    (secs + 11_644_473_600) * 10_000_000
}

const DT_PATH: DRAW_TEXT_FORMAT = DRAW_TEXT_FORMAT(DT_LEFT.0 | DT_VCENTER.0 | DT_PATH_ELLIPSIS.0);

impl State {
    fn row_h(&self) -> i32 {
        scale(ROW, self.dpi)
    }

    fn header_h(&self) -> i32 {
        scale(HEADER, self.dpi)
    }

    fn view_rows(&self) -> usize {
        let h = client_rect(self.hwnd).bottom - self.header_h();
        (h / self.row_h()).max(1) as usize
    }

    fn max_top(&self) -> usize {
        self.entries.len().saturating_sub(self.view_rows())
    }

    fn clear_rect(&self) -> RECT {
        let d = self.dpi;
        let w = client_rect(self.hwnd).right;
        let bw = scale(96, d);
        rc(w - scale(MARGIN, d) - bw, scale(12, d), bw, scale(32, d))
    }

    fn row_rect(&self, i: usize) -> RECT {
        let w = client_rect(self.hwnd).right;
        let y = self.header_h() + (i as i32 - self.top as i32) * self.row_h();
        rc(0, y, w, self.row_h())
    }

    fn hit(&self, x: i32, y: i32) -> Option<Hot> {
        if pt_in(&self.clear_rect(), x, y) {
            return (!self.entries.is_empty()).then_some(Hot::Clear);
        }
        if y < self.header_h() {
            return None;
        }
        let i = self.top + ((y - self.header_h()) / self.row_h()) as usize;
        (i < self.entries.len()).then_some(Hot::Row(i))
    }
}

fn paint(s: &State, hdc: HDC, r: &RECT) {
    let d = s.dpi;
    let p = &s.pal;
    let m = scale(MARGIN, d);
    fill(hdc, r, p.bg);
    // Header: title and Clear button.
    unsafe {
        SelectObject(hdc, s.heading);
    }
    let hh = s.header_h();
    draw_text(hdc, "Action log", &RECT { left: m, top: 0, right: r.right - m, bottom: hh }, p.text, DT_LEFT_V);
    let cr = s.clear_rect();
    let enabled = !s.entries.is_empty();
    let hot = enabled && s.hot == Some(Hot::Clear);
    fill(hdc, &cr, if hot && s.pressed == Some(Hot::Clear) { p.sel_inactive } else if hot { p.tab_hover } else { p.toolbar });
    frame(hdc, &cr, p.border, 1);
    unsafe {
        SelectObject(hdc, s.fonts.ui);
    }
    draw_text(hdc, "Clear log", &cr, if enabled { p.text } else { p.text_dim }, DT_CENTER_V);
    fill(hdc, &rc(0, hh - 1, r.right, 1), p.border);

    if s.entries.is_empty() {
        let body = RECT { left: 0, top: hh, right: r.right, bottom: r.bottom };
        draw_text(hdc, "No actions yet. Copies, moves, renames, deletes and new items show up here.", &body, p.text_dim, DT_CENTER_V);
        return;
    }
    let rh = s.row_h();
    let line = rh / 2 - scale(4, d);
    let time_w = scale(150, d);
    for i in s.top..s.entries.len() {
        let row = s.row_rect(i);
        if row.top >= r.bottom {
            break;
        }
        let e = &s.entries[i];
        if s.sel == Some(i) {
            fill(hdc, &row, p.sel_inactive);
        } else if s.hot == Some(Hot::Row(i)) {
            fill(hdc, &row, p.hover);
        }
        let failed = e.error.is_some();
        let text_l = m + scale(32, d);
        let top_line = RECT { left: text_l, top: row.top + scale(5, d), right: row.right - m - time_w, bottom: row.top + scale(5, d) + line };
        let bottom_line = RECT { top: top_line.bottom, bottom: top_line.bottom + line, right: row.right - m, ..top_line };
        unsafe {
            SelectObject(hdc, s.fonts.glyph);
        }
        draw_text(hdc, glyph_of(e.kind), &RECT { left: m, right: text_l, ..top_line }, if failed { p.error } else { p.text_soft }, DT_LEFT_V);
        unsafe {
            SelectObject(hdc, s.fonts.ui);
        }
        draw_text(hdc, &e.title, &top_line, if failed { p.error } else { p.text }, DT_LEFT_V);
        let time = crate::grid::format_time(unix_to_filetime(e.time));
        draw_text(hdc, &time, &RECT { left: top_line.right, right: row.right - m, ..top_line }, p.text_dim, DT_RIGHT_V);
        match &e.error {
            Some(err) => draw_text(hdc, err.lines().next().unwrap_or(""), &bottom_line, p.error, DT_LEFT_V),
            None => draw_text(hdc, &e.detail(), &bottom_line, p.text_dim, DT_PATH),
        }
        fill(hdc, &rc(m, row.bottom - 1, row.right - 2 * m, 1), p.border);
    }
}

/// Clamps the scroll position, updates the scrollbar and repaints.
fn sync(hwnd: HWND) {
    let Some(si) = with_state(|s| {
        s.top = s.top.min(s.max_top());
        SCROLLINFO {
            cbSize: std::mem::size_of::<SCROLLINFO>() as u32,
            fMask: SIF_ALL | SIF_DISABLENOSCROLL,
            nMin: 0,
            nMax: s.entries.len().saturating_sub(1) as i32,
            nPage: s.view_rows() as u32,
            nPos: s.top as i32,
            nTrackPos: 0,
        }
    }) else {
        return;
    };
    unsafe {
        SetScrollInfo(hwnd, SB_VERT, &si, true);
    }
    invalidate(hwnd);
}

fn scroll_to(hwnd: HWND, top: i64) {
    with_state(|s| s.top = top.clamp(0, s.max_top() as i64) as usize);
    sync(hwnd);
}

/// Selects row `i` and scrolls it into view.
fn select(hwnd: HWND, i: usize) {
    let top = with_state(|s| {
        let i = i.min(s.entries.len().saturating_sub(1));
        s.sel = (!s.entries.is_empty()).then_some(i);
        let view = s.view_rows();
        if i < s.top {
            i
        } else if i >= s.top + view {
            i + 1 - view
        } else {
            s.top
        }
    });
    if let Some(top) = top {
        scroll_to(hwnd, top as i64);
    }
}

fn jump(i: usize) {
    if let Some(e) = with_state(|s| s.entries.get(i).cloned()).flatten() {
        crate::app::with_app(|app| app.jump_to_log(&e));
    }
}

/// Reloads the entries if the window is open (called when an action is logged).
pub fn refresh() {
    let Some(hwnd) = with_state(|s| {
        let added = actionlog::entries().len().saturating_sub(s.entries.len());
        s.entries = actionlog::entries();
        // Keep the selection on the same entry; new ones arrive at the top.
        s.sel = s.sel.map(|i| i + added).filter(|&i| i < s.entries.len());
        if s.top > 0 {
            s.top += added;
        }
        s.hwnd
    }) else {
        return;
    };
    sync(hwnd);
}

fn set_dpi(s: &mut State, dpi: u32) {
    s.dpi = dpi;
    s.fonts = Fonts::new(dpi);
    let old = std::mem::replace(&mut s.heading, crate::settingswin::heading_font(s.fonts.ui));
    unsafe {
        let _ = DeleteObject(old);
    }
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    match msg {
        WM_PAINT => {
            if with_state(|s| paint_buffered(hwnd, |hdc, r| paint(s, hdc, r))).is_some() {
                return LRESULT(0);
            }
        }
        WM_ERASEBKGND => return LRESULT(1),
        WM_SIZE => {
            sync(hwnd);
            return LRESULT(0);
        }
        WM_GETMINMAXINFO => {
            if let Some(d) = with_state(|s| s.dpi) {
                let mmi = &mut *(lp.0 as *mut MINMAXINFO);
                mmi.ptMinTrackSize = POINT { x: scale(360, d), y: scale(240, d) };
            }
            return LRESULT(0);
        }
        WM_MOUSEMOVE => {
            with_state(|s| {
                let h = s.hit(x_of(lp), y_of(lp));
                if h != s.hot {
                    s.hot = h;
                    invalidate(hwnd);
                }
                if !s.tracking {
                    s.tracking = true;
                    let mut tme = TRACKMOUSEEVENT { cbSize: std::mem::size_of::<TRACKMOUSEEVENT>() as u32, dwFlags: TME_LEAVE, hwndTrack: hwnd, dwHoverTime: 0 };
                    let _ = TrackMouseEvent(&mut tme);
                }
            });
            return LRESULT(0);
        }
        WM_MOUSELEAVE => {
            with_state(|s| {
                s.tracking = false;
                if s.hot.take().is_some() {
                    invalidate(hwnd);
                }
            });
            return LRESULT(0);
        }
        WM_LBUTTONDOWN | WM_LBUTTONDBLCLK => {
            let h = with_state(|s| s.hit(x_of(lp), y_of(lp))).flatten();
            match h {
                Some(Hot::Row(i)) => {
                    select(hwnd, i);
                    jump(i);
                }
                Some(Hot::Clear) => {
                    with_state(|s| s.pressed = Some(Hot::Clear));
                    SetCapture(hwnd);
                    invalidate(hwnd);
                }
                None => {}
            }
            return LRESULT(0);
        }
        WM_LBUTTONUP => {
            let clear = with_state(|s| s.pressed.take() == Some(Hot::Clear) && s.hit(x_of(lp), y_of(lp)) == Some(Hot::Clear)).unwrap_or(false);
            let _ = ReleaseCapture();
            if clear {
                actionlog::clear();
                with_state(|s| {
                    s.entries.clear();
                    s.sel = None;
                    s.hot = None;
                    s.top = 0;
                });
                sync(hwnd);
            }
            invalidate(hwnd);
            return LRESULT(0);
        }
        WM_CAPTURECHANGED => {
            with_state(|s| {
                if s.pressed.take().is_some() {
                    invalidate(hwnd);
                }
            });
        }
        WM_MOUSEWHEEL => {
            let delta = ((wp.0 >> 16) & 0xFFFF) as u16 as i16 as i64;
            if let Some(top) = with_state(|s| s.top as i64) {
                scroll_to(hwnd, top - delta * 2 / 120);
            }
            return LRESULT(0);
        }
        WM_VSCROLL => {
            let code = (wp.0 & 0xFFFF) as i32;
            let Some((top, view)) = with_state(|s| (s.top as i64, s.view_rows() as i64)) else { return LRESULT(0) };
            let new = match code {
                c if c == SB_LINEUP.0 => top - 1,
                c if c == SB_LINEDOWN.0 => top + 1,
                c if c == SB_PAGEUP.0 => top - view,
                c if c == SB_PAGEDOWN.0 => top + view,
                c if c == SB_THUMBTRACK.0 || c == SB_THUMBPOSITION.0 => {
                    let mut si = SCROLLINFO { cbSize: std::mem::size_of::<SCROLLINFO>() as u32, fMask: SIF_TRACKPOS, ..Default::default() };
                    let _ = GetScrollInfo(hwnd, SB_VERT, &mut si);
                    si.nTrackPos as i64
                }
                c if c == SB_TOP.0 => 0,
                c if c == SB_BOTTOM.0 => i64::MAX / 2,
                _ => return LRESULT(0),
            };
            scroll_to(hwnd, new);
            return LRESULT(0);
        }
        WM_KEYDOWN => {
            let vk = VIRTUAL_KEY(wp.0 as u16);
            let Some((sel, n, view)) = with_state(|s| (s.sel, s.entries.len(), s.view_rows())) else { return LRESULT(0) };
            let last = n.saturating_sub(1);
            let target = match vk {
                VK_ESCAPE => {
                    let _ = DestroyWindow(hwnd);
                    return LRESULT(0);
                }
                VK_RETURN => {
                    if let Some(i) = sel {
                        jump(i);
                    }
                    return LRESULT(0);
                }
                VK_UP => sel.map_or(0, |i| i.saturating_sub(1)),
                VK_DOWN => sel.map_or(0, |i| (i + 1).min(last)),
                VK_PRIOR => sel.map_or(0, |i| i.saturating_sub(view)),
                VK_NEXT => sel.map_or(0, |i| (i + view).min(last)),
                VK_HOME => 0,
                VK_END => last,
                _ => return LRESULT(0),
            };
            if n > 0 {
                select(hwnd, target);
            }
            return LRESULT(0);
        }
        WM_SETTINGCHANGE => {
            if lp.0 != 0 && PCWSTR(lp.0 as *const u16).to_string().is_ok_and(|s| s == "ImmersiveColorSet") {
                let dark = crate::theme::system_prefers_dark();
                crate::theme::set_title_bar_dark(hwnd, dark);
                with_state(|s| {
                    s.dark = dark;
                    s.pal = Palette::new(dark);
                });
                invalidate(hwnd);
            }
        }
        WM_DPICHANGED => {
            let dpi = ((wp.0 >> 16) & 0xFFFF) as u32;
            let r = *(lp.0 as *const RECT);
            with_state(|s| set_dpi(s, dpi));
            let _ = SetWindowPos(hwnd, HWND::default(), r.left, r.top, r.right - r.left, r.bottom - r.top, SWP_NOZORDER | SWP_NOACTIVATE);
            sync(hwnd);
            return LRESULT(0);
        }
        WM_CLOSE => {
            let _ = DestroyWindow(hwnd);
            return LRESULT(0);
        }
        WM_DESTROY => {
            let owner = with_state(|s| s.owner);
            drop(STATE.with(|s| s.borrow_mut().take()));
            if let Some(o) = owner {
                let _ = SetForegroundWindow(o);
            }
            return LRESULT(0);
        }
        _ => {}
    }
    DefWindowProcW(hwnd, msg, wp, lp)
}

/// Opens the action log over `owner`, or brings an open one to the front.
pub fn show(owner: HWND, dark: bool) {
    if let Some(h) = STATE.with(|s| s.borrow().as_ref().map(|s| s.hwnd)) {
        unsafe {
            let _ = ShowWindow(h, SW_SHOWNORMAL);
            let _ = SetForegroundWindow(h);
        }
        return;
    }
    unsafe {
        static REG: std::sync::Once = std::sync::Once::new();
        REG.call_once(|| {
            let wc = WNDCLASSEXW {
                cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
                lpfnWndProc: Some(wndproc),
                hInstance: instance(),
                hCursor: LoadCursorW(None, IDC_ARROW).unwrap_or_default(),
                lpszClassName: CLASS,
                ..Default::default()
            };
            RegisterClassExW(&wc);
        });
        // Create it over the owner's center so it gets that monitor's DPI.
        let mut or = RECT::default();
        let _ = GetWindowRect(owner, &mut or);
        let (cx, cy) = ((or.left + or.right) / 2, (or.top + or.bottom) / 2);
        let style = WINDOW_STYLE(WS_OVERLAPPEDWINDOW.0 | WS_VSCROLL.0);
        let Ok(hwnd) = CreateWindowExW(WINDOW_EX_STYLE::default(), CLASS, w!("Twig action log"), style, cx, cy, 1, 1, owner, None, instance(), None) else { return };
        let dpi = GetDpiForWindow(hwnd).max(96);
        let fonts = Fonts::new(dpi);
        let heading = crate::settingswin::heading_font(fonts.ui);
        let state = State {
            hwnd,
            owner,
            dpi,
            dark,
            pal: Palette::new(dark),
            fonts,
            heading,
            entries: actionlog::entries(),
            top: 0,
            sel: None,
            hot: None,
            pressed: None,
            tracking: false,
        };
        STATE.with(|s| *s.borrow_mut() = Some(state));
        crate::theme::set_title_bar_dark(hwnd, dark);
        let (w, h) = (scale(WIDTH, dpi), scale(HEIGHT, dpi));
        let _ = SetWindowPos(hwnd, HWND::default(), cx - w / 2, cy - h / 2, w, h, SWP_NOZORDER | SWP_NOACTIVATE);
        sync(hwnd);
        let _ = ShowWindow(hwnd, SW_SHOW);
    }
}
