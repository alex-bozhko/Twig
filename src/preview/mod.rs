//! Quick Look: Space shows the selected item in a floating preview window that follows the
//! selection, like the macOS Finder.
//!
//! Content is loaded on a worker thread (`content`) and shown by a `Viewer`, one per kind of
//! content: `text`, `image`, and the `info` card for everything else. New file types add a
//! `Payload` variant, a loader branch in `content`, and a viewer.
//!
//! The window never takes activation, so keyboard focus stays in the file list: arrows move the
//! selection (and the preview follows), Space or Esc close it.

mod content;
mod image;
mod info;
mod text;

pub use content::Target;

use crate::theme::{glyph, Fonts, Palette};
use crate::win::*;
use content::Payload;
use std::cell::{Cell, RefCell};
use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Dwm::{DwmExtendFrameIntoClientArea, DwmSetWindowAttribute, DWMWINDOWATTRIBUTE};
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::UI::Controls::{MARGINS, WM_MOUSELEAVE};
use windows::Win32::UI::HiDpi::GetDpiForWindow;
use windows::Win32::UI::Input::KeyboardAndMouse::*;
use windows::Win32::UI::WindowsAndMessaging::*;

/// How a viewer would like the window sized.
pub enum Pref {
    /// Image content of this pixel size, scaled to the document size with its proportions kept.
    Fit(i32, i32),
    /// A document: a tall, reading-sized window the user may resize.
    Doc,
    /// A card of this height (96-DPI pixels).
    Card(i32),
}

/// What viewers paint with.
pub struct Ctx<'a> {
    pub pal: &'a Palette,
    pub fonts: &'a Fonts,
    pub mono: HFONT,
    pub heading: HFONT,
    pub dpi: u32,
}

pub trait Viewer {
    fn preferred(&self) -> Pref;
    fn paint(&mut self, hdc: HDC, r: &RECT, cx: &Ctx);
    /// Scrolls by a wheel `delta` (positive is up / right); true if the view changed.
    fn wheel(&mut self, _delta: i32, _horizontal: bool) -> bool {
        false
    }
    /// Shown dimmed after the name in the title bar.
    fn subtitle(&self) -> String;
}

const CLASS: PCWSTR = w!("TwigPreview");
const TIMER_PLACEHOLDER: usize = 1;
/// A load slower than this shows the placeholder card meanwhile.
const PLACEHOLDER_MS: u32 = 250;
// Layout, in 96-DPI pixels.
const TITLE_H: i32 = 44;
const BUTTON_W: i32 = 40;
const EDGE: i32 = 6;
/// Share of the work area a document preview takes by default, the same both ways.
const DOC_PERCENT: i32 = 80;
const CARD_W: i32 = 520;
const MIN_CONTENT: (i32, i32) = (360, 240);
const MONO_PX: i32 = 13;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Btn {
    Open,
    Close,
}

struct State {
    hwnd: HWND,
    owner: HWND,
    dpi: u32,
    pal: Palette,
    fonts: Fonts,
    mono: HFONT,
    heading: HFONT,
    /// The item asked for (may still be loading).
    want: Target,
    gen: u64,
    /// The item `viewer` shows.
    shown_target: Target,
    viewer: Box<dyn Viewer>,
    is_doc: bool,
    /// The window has been sized and shown.
    visible: bool,
    hot: Option<Btn>,
    pressed: Option<Btn>,
    tracking: bool,
}

impl Drop for State {
    fn drop(&mut self) {
        unsafe {
            let _ = DeleteObject(self.mono);
            let _ = DeleteObject(self.heading);
        }
    }
}

thread_local! {
    static STATE: RefCell<Option<State>> = const { RefCell::new(None) };
    /// Size the user gave document previews (96-DPI pixels), kept for the session.
    static DOC_SIZE: Cell<Option<(i32, i32)>> = const { Cell::new(None) };
}

fn with_state<R>(f: impl FnOnce(&mut State) -> R) -> Option<R> {
    STATE.with(|s| s.try_borrow_mut().ok().and_then(|mut s| s.as_mut().map(f)))
}

impl State {
    fn s(&self, v: i32) -> i32 {
        scale(v, self.dpi)
    }

    fn title_h(&self) -> i32 {
        self.s(TITLE_H)
    }

    fn buttons(&self, r: &RECT) -> Vec<(Btn, RECT)> {
        let (bw, th) = (self.s(BUTTON_W), self.title_h());
        let mut out = vec![(Btn::Close, RECT { left: r.right - bw - self.s(2), top: 0, right: r.right - self.s(2), bottom: th })];
        if self.shown_target.is_file() {
            let x = r.right - 2 * bw - self.s(2);
            out.push((Btn::Open, RECT { left: x, top: 0, right: x + bw, bottom: th }));
        }
        out
    }

    fn hit(&self, hwnd: HWND, x: i32, y: i32) -> Option<Btn> {
        let r = client_rect(hwnd);
        self.buttons(&r).into_iter().find(|(_, br)| pt_in(br, x, y)).map(|(b, _)| b)
    }

    fn set_viewer(&mut self, v: Box<dyn Viewer>, t: Target) {
        self.is_doc = matches!(v.preferred(), Pref::Doc);
        self.viewer = v;
        self.shown_target = t;
    }
}

fn make_viewer(t: &Target, p: Payload) -> Box<dyn Viewer> {
    match p {
        Payload::Text { lines, truncated, total } => Box::new(text::TextView::new(lines, truncated, total)),
        Payload::Image { pixels, orig_w, orig_h } => Box::new(image::ImageView::new(pixels, orig_w, orig_h)),
        Payload::Info { thumb, extra, note } => Box::new(info::InfoView::new(t, thumb, extra, note)),
    }
}

fn paint(s: &mut State, hwnd: HWND, hdc: HDC, r: &RECT) {
    let pal = &s.pal;
    let th = s.title_h();
    let title = RECT { bottom: th, ..*r };
    fill(hdc, &title, pal.toolbar);
    fill(hdc, &RECT { top: th - 1, bottom: th, ..*r }, pal.border);

    let buttons = s.buttons(r);
    for (b, br) in &buttons {
        let hot = s.hot == Some(*b);
        let pressed = hot && s.pressed == Some(*b);
        let mut color = pal.text;
        if hot && *b == Btn::Close {
            fill(hdc, br, rgb(0xC42B1C));
            color = rgb(0xFFFFFF);
        } else if hot {
            let inner = RECT { left: br.left + s.s(4), top: br.top + s.s(6), right: br.right - s.s(4), bottom: br.bottom - s.s(6) };
            fill(hdc, &inner, if pressed { pal.sel } else { pal.hover });
        }
        let g = if *b == Btn::Close { glyph::CLOSE } else { glyph::OPEN };
        unsafe {
            let old = SelectObject(hdc, s.fonts.glyph_small);
            draw_glyph(hdc, g, br, color);
            SelectObject(hdc, old);
        }
    }

    let pad = s.s(12);
    let isz = s.s(16);
    draw_icon(hdc, s.shown_target.icon, r.left + pad, (th - isz) / 2, isz);
    let left = r.left + pad + isz + s.s(8);
    let right = buttons.iter().map(|(_, br)| br.left).min().unwrap_or(r.right) - s.s(8);
    let sub = s.viewer.subtitle();
    unsafe {
        let old = SelectObject(hdc, s.fonts.ui);
        let sub_w = if sub.is_empty() { 0 } else { text_width(hdc, s.fonts.ui, &sub) + s.s(12) };
        // The name keeps at least half the room; the subtitle is clipped first.
        let name_w = text_width(hdc, s.fonts.ui, &s.shown_target.name);
        let name_right = (left + name_w + s.s(2)).min(right - sub_w.min((right - left) / 2));
        draw_text(hdc, &s.shown_target.name, &RECT { left, top: 0, right: name_right, bottom: th }, pal.text, DT_LEFT_V);
        if !sub.is_empty() {
            draw_text(hdc, &sub, &RECT { left: name_right + s.s(12), top: 0, right, bottom: th }, pal.text_dim, DT_LEFT_V);
        }
        SelectObject(hdc, old);
    }

    let body = RECT { top: th, ..*r };
    let cx = Ctx { pal: &s.pal, fonts: &s.fonts, mono: s.mono, heading: s.heading, dpi: s.dpi };
    unsafe {
        let saved = SaveDC(hdc);
        IntersectClipRect(hdc, body.left, body.top, body.right, body.bottom);
        s.viewer.paint(hdc, &body, &cx);
        let _ = RestoreDC(hdc, saved);
    }
    let _ = hwnd;
}

fn work_area(hwnd: HWND) -> RECT {
    unsafe {
        let mon = MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST);
        let mut mi = MONITORINFO { cbSize: std::mem::size_of::<MONITORINFO>() as u32, ..Default::default() };
        let _ = GetMonitorInfoW(mon, &mut mi);
        mi.rcWork
    }
}

/// Sizes the window for its viewer, keeping its center (or centering it on the owner the first
/// time), and shows it. Must not be called while STATE is borrowed.
fn layout(hwnd: HWND) {
    let Some((pref, dpi, visible, owner)) = with_state(|s| (s.viewer.preferred(), s.dpi, s.visible, s.owner)) else { return };
    let anchor = if visible { hwnd } else { owner };
    let work = work_area(anchor);
    let (ww, wh) = (work.right - work.left, work.bottom - work.top);
    let th = scale(TITLE_H, dpi);
    // Documents and images share one size: the user's last document size, or a share of the screen.
    let (dw, dh) = match DOC_SIZE.with(|d| d.get()) {
        Some((w, h)) => (scale(w, dpi), scale(h, dpi) - th),
        None => (ww * DOC_PERCENT / 100, wh * DOC_PERCENT / 100 - th),
    };
    let (cw, ch) = match pref {
        Pref::Fit(w, h) => {
            let (fw, fh) = image::contain((w, h), (dw, dh));
            (fw.max(scale(MIN_CONTENT.0, dpi)), fh.max(scale(MIN_CONTENT.1, dpi)))
        }
        Pref::Doc => (dw, dh),
        Pref::Card(h) => (scale(CARD_W, dpi), scale(h, dpi)),
    };
    let (w, h) = (cw.min(ww), (ch + th).min(wh));
    let mut ar = RECT::default();
    unsafe {
        let _ = GetWindowRect(anchor, &mut ar);
    }
    let (cx, cy) = ((ar.left + ar.right) / 2, (ar.top + ar.bottom) / 2);
    let x = (cx - w / 2).clamp(work.left, work.right - w);
    let y = (cy - h / 2).clamp(work.top, work.bottom - h);
    unsafe {
        let _ = SetWindowPos(hwnd, HWND::default(), x, y, w, h, SWP_NOZORDER | SWP_NOACTIVATE);
        if !visible {
            let _ = ShowWindow(hwnd, SW_SHOWNA);
        }
    }
    with_state(|s| s.visible = true);
    invalidate(hwnd);
}

fn set_dpi(s: &mut State, dpi: u32) {
    s.dpi = dpi;
    s.fonts = Fonts::new(dpi);
    let heading = std::mem::replace(&mut s.heading, crate::settingswin::heading_font(s.fonts.ui));
    let mono = std::mem::replace(&mut s.mono, crate::theme::mono_font(scale(MONO_PX, dpi)));
    unsafe {
        let _ = DeleteObject(heading);
        let _ = DeleteObject(mono);
    }
}

/// Opens the shown item with its default app (extracting archive entries first), then closes.
fn open_shown(hwnd: HWND) {
    let Some((t, owner)) = with_state(|s| (s.shown_target.clone(), s.owner)) else { return };
    let path = match &t.zip {
        Some((archive, inner)) if t.kind.is_virtual() => match crate::zipfs::extract(archive, std::slice::from_ref(inner)) {
            Ok(mut v) if !v.is_empty() => v.remove(0).to_string_lossy().into_owned(),
            Ok(_) => return,
            Err(e) => {
                crate::app::with_app(|a| a.set_status_error(format!("Open failed: {e}")));
                return;
            }
        },
        _ => t.path.clone(),
    };
    unsafe {
        let _ = DestroyWindow(hwnd);
    }
    if let Err(e) = crate::shell::open(owner, &path) {
        crate::app::with_app(|a| a.set_status_error(format!("Open failed: {e}")));
    }
}

fn edge_hit(hwnd: HWND, lp: LPARAM) -> LRESULT {
    let mut p = POINT { x: x_of(lp), y: y_of(lp) };
    unsafe {
        let _ = ScreenToClient(hwnd, &mut p);
    }
    let r = client_rect(hwnd);
    let Some((e, th, on_button)) = with_state(|s| (s.s(EDGE), s.title_h(), s.hit(hwnd, p.x, p.y).is_some())) else { return LRESULT(HTCLIENT as isize) };
    let (l, t, rr, b) = (p.x < e, p.y < e, p.x >= r.right - e, p.y >= r.bottom - e);
    let ht = match (l, t, rr, b) {
        (true, true, _, _) => HTTOPLEFT,
        (_, true, true, _) => HTTOPRIGHT,
        (true, _, _, true) => HTBOTTOMLEFT,
        (_, _, true, true) => HTBOTTOMRIGHT,
        (true, ..) => HTLEFT,
        (_, _, true, _) => HTRIGHT,
        (_, _, _, true) => HTBOTTOM,
        (_, true, ..) if !on_button => HTTOP,
        _ if p.y < th && !on_button => HTCAPTION,
        _ => HTCLIENT,
    };
    LRESULT(ht as isize)
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    match msg {
        WM_NCCALCSIZE if wp.0 != 0 => return LRESULT(0),
        WM_NCHITTEST => return edge_hit(hwnd, lp),
        WM_MOUSEACTIVATE => return LRESULT(MA_NOACTIVATE as isize),
        WM_PAINT => {
            if with_state(|s| paint_buffered(hwnd, |hdc, r| paint(s, hwnd, hdc, r))).is_some() {
                return LRESULT(0);
            }
        }
        WM_ERASEBKGND => return LRESULT(1),
        WM_SIZE => {
            invalidate(hwnd);
            return LRESULT(0);
        }
        WM_GETMINMAXINFO => {
            if let Some(d) = with_state(|s| s.dpi) {
                let mmi = &mut *(lp.0 as *mut MINMAXINFO);
                mmi.ptMinTrackSize = POINT { x: scale(320, d), y: scale(200, d) };
            }
            return LRESULT(0);
        }
        WM_EXITSIZEMOVE => {
            if let Some((true, dpi)) = with_state(|s| (s.is_doc, s.dpi)) {
                let r = client_rect(hwnd);
                DOC_SIZE.with(|d| d.set(Some(((r.right * 96 / dpi as i32).max(320), (r.bottom * 96 / dpi as i32).max(200)))));
            }
            return LRESULT(0);
        }
        WM_PREVIEW_LOADED => {
            let gen = wp.0 as u64;
            let loaded = with_state(|s| {
                if gen != s.gen {
                    return false;
                }
                let Some(p) = content::take(gen) else { return false };
                let t = s.want.clone();
                let v = make_viewer(&t, p);
                s.set_viewer(v, t);
                true
            });
            if loaded == Some(true) {
                let _ = KillTimer(hwnd, TIMER_PLACEHOLDER);
                layout(hwnd);
            }
            return LRESULT(0);
        }
        WM_TIMER if wp.0 == TIMER_PLACEHOLDER => {
            let _ = KillTimer(hwnd, TIMER_PLACEHOLDER);
            with_state(|s| {
                let t = s.want.clone();
                s.set_viewer(Box::new(info::InfoView::loading(&t)), t);
            });
            layout(hwnd);
            return LRESULT(0);
        }
        WM_MOUSEMOVE => {
            with_state(|s| {
                let h = s.hit(hwnd, x_of(lp), y_of(lp));
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
            if with_state(|s| {
                s.pressed = s.hit(hwnd, x_of(lp), y_of(lp));
                s.pressed.is_some()
            }) == Some(true)
            {
                SetCapture(hwnd);
                invalidate(hwnd);
            }
            return LRESULT(0);
        }
        WM_LBUTTONUP => {
            let b = with_state(|s| s.pressed.take().filter(|&b| s.hit(hwnd, x_of(lp), y_of(lp)) == Some(b))).flatten();
            let _ = ReleaseCapture();
            invalidate(hwnd);
            match b {
                Some(Btn::Close) => {
                    let _ = DestroyWindow(hwnd);
                }
                Some(Btn::Open) => open_shown(hwnd),
                None => {}
            }
            return LRESULT(0);
        }
        WM_CAPTURECHANGED => {
            with_state(|s| {
                if s.pressed.take().is_some() {
                    invalidate(hwnd);
                }
            });
        }
        WM_MOUSEWHEEL | WM_MOUSEHWHEEL => {
            let delta = ((wp.0 >> 16) & 0xFFFF) as u16 as i16 as i32;
            let (delta, horizontal) = match msg {
                WM_MOUSEHWHEEL => (delta, true),
                _ if (wp.0 & 0x0004) != 0 => (-delta, true),
                _ => (delta, false),
            };
            if with_state(|s| s.viewer.wheel(delta, horizontal)) == Some(true) {
                invalidate(hwnd);
            }
            return LRESULT(0);
        }
        WM_DPICHANGED => {
            let dpi = ((wp.0 >> 16) & 0xFFFF) as u32;
            let r = *(lp.0 as *const RECT);
            with_state(|s| set_dpi(s, dpi));
            let _ = SetWindowPos(hwnd, HWND::default(), r.left, r.top, r.right - r.left, r.bottom - r.top, SWP_NOZORDER | SWP_NOACTIVATE);
            return LRESULT(0);
        }
        WM_CLOSE => {
            let _ = DestroyWindow(hwnd);
            return LRESULT(0);
        }
        WM_DESTROY => {
            content::cancel();
            let _ = KillTimer(hwnd, TIMER_PLACEHOLDER);
            drop(STATE.with(|s| s.borrow_mut().take()));
            return LRESULT(0);
        }
        _ => {}
    }
    DefWindowProcW(hwnd, msg, wp, lp)
}

fn hwnd() -> Option<HWND> {
    STATE.with(|s| s.try_borrow().ok().and_then(|s| s.as_ref().map(|s| s.hwnd)))
}

pub fn is_open() -> bool {
    hwnd().is_some()
}

pub fn close() {
    if let Some(h) = hwnd() {
        unsafe {
            let _ = DestroyWindow(h);
        }
    }
}

/// Space: closes an open preview, or opens one for `target` over `owner`.
pub fn toggle(owner: HWND, dark: bool, target: Option<Target>) {
    if is_open() {
        close();
    } else if let Some(t) = target {
        open(owner, dark, t);
    }
}

fn open(owner: HWND, dark: bool, t: Target) {
    unsafe {
        static REG: std::sync::Once = std::sync::Once::new();
        REG.call_once(|| {
            let wc = WNDCLASSEXW {
                cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
                style: CS_DBLCLKS,
                lpfnWndProc: Some(wndproc),
                hInstance: instance(),
                hCursor: LoadCursorW(None, IDC_ARROW).unwrap_or_default(),
                lpszClassName: CLASS,
                ..Default::default()
            };
            RegisterClassExW(&wc);
        });
        // Created hidden over the owner's center so it gets that monitor's DPI.
        let mut or = RECT::default();
        let _ = GetWindowRect(owner, &mut or);
        let (cx, cy) = ((or.left + or.right) / 2, (or.top + or.bottom) / 2);
        let style = WINDOW_STYLE(WS_POPUP.0 | WS_THICKFRAME.0 | WS_CLIPCHILDREN.0);
        let ex = WINDOW_EX_STYLE(WS_EX_NOACTIVATE.0 | WS_EX_TOOLWINDOW.0);
        let Ok(hwnd) = CreateWindowExW(ex, CLASS, w!("Quick Look"), style, cx, cy, 1, 1, owner, None, instance(), None) else { return };
        let dpi = GetDpiForWindow(hwnd).max(96);
        let fonts = Fonts::new(dpi);
        let heading = crate::settingswin::heading_font(fonts.ui);
        let mono = crate::theme::mono_font(scale(MONO_PX, dpi));
        let gen = content::request(t.clone(), hwnd);
        let state = State {
            hwnd,
            owner,
            dpi,
            pal: Palette::new(dark),
            fonts,
            mono,
            heading,
            want: t.clone(),
            gen,
            shown_target: t.clone(),
            viewer: Box::new(info::InfoView::loading(&t)),
            is_doc: false,
            visible: false,
            hot: None,
            pressed: None,
            tracking: false,
        };
        STATE.with(|s| *s.borrow_mut() = Some(state));
        crate::theme::set_title_bar_dark(hwnd, dark);
        // Rounded corners and the system border / shadow on Windows 11.
        let round: u32 = 2;
        let _ = DwmSetWindowAttribute(hwnd, DWMWINDOWATTRIBUTE(33), &round as *const _ as *const _, 4);
        let _ = DwmExtendFrameIntoClientArea(hwnd, &MARGINS { cxLeftWidth: 1, cxRightWidth: 1, cyTopHeight: 1, cyBottomHeight: 1 });
        let _ = SetWindowPos(hwnd, HWND::default(), 0, 0, 0, 0, SWP_NOMOVE | SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE | SWP_FRAMECHANGED);
        SetTimer(hwnd, TIMER_PLACEHOLDER, PLACEHOLDER_MS, None);
    }
}

/// Shows `target` in an open preview (the selection moved). Safe to call while the app is
/// borrowed: it only starts a load and a timer.
pub fn follow(target: Option<Target>) {
    let Some(t) = target else { return };
    with_state(|s| {
        if t.same(&s.want) {
            return;
        }
        s.want = t.clone();
        s.gen = content::request(t, s.hwnd);
        unsafe {
            SetTimer(s.hwnd, TIMER_PLACEHOLDER, PLACEHOLDER_MS, None);
        }
    });
}

pub fn set_dark(dark: bool) {
    let Some(h) = with_state(|s| {
        s.pal = Palette::new(dark);
        s.hwnd
    }) else {
        return;
    };
    crate::theme::set_title_bar_dark(h, dark);
    invalidate(h);
}
