//! The keyboard shortcuts window, opened from the status bar's help button or F1.
//!
//! A small owner-drawn window (no child controls) themed like the main window.

use crate::settingswin::heading_font;
use crate::theme::{Fonts, Palette};
use crate::win::*;
use std::cell::RefCell;
use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::UI::HiDpi::{AdjustWindowRectExForDpi, GetDpiForWindow};
use windows::Win32::UI::Input::KeyboardAndMouse::*;
use windows::Win32::UI::WindowsAndMessaging::*;

const CLASS: PCWSTR = w!("TwigHelp");
const STYLE: WINDOW_STYLE = WINDOW_STYLE(WS_CAPTION.0 | WS_SYSMENU.0);
const EX_STYLE: WINDOW_EX_STYLE = WS_EX_DLGMODALFRAME;
// Layout, in 96-DPI pixels.
const MARGIN: i32 = 24;
const COL_W: i32 = 400;
const GAP: i32 = 32;
const KEY_W: i32 = 150;
const ROW: i32 = 24;
const WIDTH: i32 = MARGIN * 2 + COL_W * 2 + GAP;

type Section = (&'static str, &'static [(&'static str, &'static str)]);

const LEFT: &[Section] = &[
    (
        "Navigation",
        &[
            ("\u{2191} \u{2193}  Home  End", "Move the selection"),
            ("PgUp  PgDn", "Move a page"),
            ("\u{2190}  \u{2192}", "Collapse / expand a folder"),
            ("Shift+arrows, Shift+click", "Extend the selection"),
            ("Ctrl+click, Ctrl+Space", "Add to / remove from selection"),
            ("Ctrl+A", "Select all"),
            ("Letters", "Jump to a matching name"),
            ("Enter, double-click", "Open a file or folder"),
            ("Backspace, Alt+\u{2190}", "Back"),
            ("Alt+\u{2192}", "Forward"),
            ("Mouse side buttons", "Back / forward"),
            ("Ctrl+L", "Type a path"),
            ("F5", "Refresh"),
        ],
    ),
    (
        "Tabs and panes",
        &[
            ("Ctrl+T", "New tab"),
            ("Ctrl+W, middle-click", "Close tab"),
            ("Ctrl+Tab", "Next tab"),
            ("Ctrl+Shift+Tab", "Previous tab"),
        ],
    ),
];

const RIGHT: &[Section] = &[
    (
        "Files",
        &[
            ("Ctrl+C  Ctrl+X  Ctrl+V", "Copy, cut, paste"),
            ("Ctrl+drag, Shift+drag", "Copy, move"),
            ("Delete", "Move to the Recycle Bin"),
            ("Shift+Delete", "Delete permanently"),
            ("F2", "Rename"),
            ("Ctrl+Shift+N", "New folder"),
            ("Ctrl+Z", "Undo"),
            ("Alt+Enter", "Properties"),
        ],
    ),
    (
        "Filter",
        &[
            ("Ctrl+F, :", "Filter this tab"),
            ("::text", "Also search subfolders"),
            ("Up, Down", "Move through the results"),
            ("Alt+S", "Toggle subfolder search"),
            ("Esc", "Clear the filter"),
        ],
    ),
    ("Quick Look", &[("Space", "Preview the selected item"), ("Space, Esc", "Close the preview")]),
    (
        "Windows",
        &[("Ctrl+,", "Settings"), ("Ctrl+H", "Action log"), ("F1", "Keyboard shortcuts")],
    ),
];

struct State {
    hwnd: HWND,
    owner: HWND,
    dpi: u32,
    pal: Palette,
    fonts: Fonts,
    heading: HFONT,
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

/// Lays out (and with `draw`, paints) one column of sections from `top`. Returns its bottom.
fn render_column(s: &State, hdc: HDC, draw: bool, left: i32, top: i32, sections: &[Section]) -> i32 {
    let d = s.dpi;
    let p = &s.pal;
    let (right, row, key_w) = (left + scale(COL_W, d), scale(ROW, d), scale(KEY_W, d));
    let text = |font: HFONT, t: &str, r: RECT, color: COLORREF| {
        if draw {
            unsafe {
                SelectObject(hdc, font);
            }
            draw_text(hdc, t, &r, color, DT_LEFT_V);
        }
    };
    let mut y = top;
    for (i, (title, keys)) in sections.iter().enumerate() {
        if i > 0 {
            y += scale(16, d);
        }
        let h = scale(32, d);
        text(s.heading, title, RECT { left, top: y, right, bottom: y + h }, p.text);
        y += h + scale(2, d);
        for (k, what) in keys.iter() {
            let r = RECT { left, top: y, right, bottom: y + row };
            text(s.fonts.ui, k, RECT { right: left + key_w - scale(8, d), ..r }, p.text);
            text(s.fonts.ui, what, RECT { left: left + key_w, ..r }, p.text_dim);
            y += row;
        }
    }
    y
}

/// Lays out (and with `draw`, paints) the content. Returns the content height.
fn render(s: &State, hdc: HDC, draw: bool) -> i32 {
    let d = s.dpi;
    let m = scale(MARGIN, d);
    let top = scale(14, d);
    let a = render_column(s, hdc, draw, m, top, LEFT);
    let b = render_column(s, hdc, draw, m + scale(COL_W + GAP, d), top, RIGHT);
    a.max(b) + m
}

/// Window size for the content at the current DPI.
fn window_size(s: &State) -> (i32, i32) {
    let hdc = unsafe { GetDC(s.hwnd) };
    let h = render(s, hdc, false);
    unsafe {
        ReleaseDC(s.hwnd, hdc);
    }
    let mut r = rc(0, 0, scale(WIDTH, s.dpi), h);
    unsafe {
        let _ = AdjustWindowRectExForDpi(&mut r, STYLE, false, EX_STYLE, s.dpi);
    }
    (r.right - r.left, r.bottom - r.top)
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    match msg {
        WM_PAINT => {
            let painted = with_state(|s| {
                paint_buffered(hwnd, |hdc, r| {
                    fill(hdc, r, s.pal.bg);
                    render(s, hdc, true);
                });
            });
            if painted.is_some() {
                return LRESULT(0);
            }
        }
        WM_ERASEBKGND => return LRESULT(1),
        WM_KEYDOWN => {
            let vk = VIRTUAL_KEY(wp.0 as u16);
            if vk == VK_ESCAPE || vk == VK_F1 || vk == VK_RETURN {
                let _ = DestroyWindow(hwnd);
            }
            return LRESULT(0);
        }
        WM_SETTINGCHANGE => {
            if lp.0 != 0 && PCWSTR(lp.0 as *const u16).to_string().is_ok_and(|s| s == "ImmersiveColorSet") {
                let dark = crate::theme::system_prefers_dark();
                crate::theme::set_title_bar_dark(hwnd, dark);
                with_state(|s| s.pal = Palette::new(dark));
                invalidate(hwnd);
            }
        }
        WM_DPICHANGED => {
            let dpi = ((wp.0 >> 16) & 0xFFFF) as u32;
            let r = *(lp.0 as *const RECT);
            if let Some((w, h)) = with_state(|s| {
                s.dpi = dpi;
                s.fonts = Fonts::new(dpi);
                let old = std::mem::replace(&mut s.heading, heading_font(s.fonts.ui));
                let _ = DeleteObject(old);
                window_size(s)
            }) {
                let _ = SetWindowPos(hwnd, HWND::default(), r.left, r.top, w, h, SWP_NOZORDER | SWP_NOACTIVATE);
            }
            invalidate(hwnd);
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

/// Opens the shortcuts window over `owner`, or brings an open one to the front.
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
        let Ok(hwnd) = CreateWindowExW(EX_STYLE, CLASS, w!("Keyboard shortcuts"), STYLE, cx, cy, 1, 1, owner, None, instance(), None) else { return };
        let dpi = GetDpiForWindow(hwnd).max(96);
        let fonts = Fonts::new(dpi);
        let heading = heading_font(fonts.ui);
        let state = State { hwnd, owner, dpi, pal: Palette::new(dark), fonts, heading };
        let (w, h) = window_size(&state);
        STATE.with(|s| *s.borrow_mut() = Some(state));
        crate::theme::set_title_bar_dark(hwnd, dark);
        let _ = SetWindowPos(hwnd, HWND::default(), cx - w / 2, cy - h / 2, w, h, SWP_NOZORDER | SWP_NOACTIVATE);
        let _ = ShowWindow(hwnd, SW_SHOW);
    }
}
