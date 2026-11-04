//! The settings window: default file manager registration and app information.
//!
//! A small owner-drawn window (no child controls) themed like the main window.

use crate::defaultapp::{self, Handler, Status};
use crate::theme::{glyph, Fonts, Palette};
use crate::win::*;
use std::cell::RefCell;
use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::UI::Controls::WM_MOUSELEAVE;
use windows::Win32::UI::HiDpi::{AdjustWindowRectExForDpi, GetDpiForWindow};
use windows::Win32::UI::Input::KeyboardAndMouse::*;
use windows::Win32::UI::WindowsAndMessaging::*;

const CLASS: PCWSTR = w!("TwigSettings");
const STYLE: WINDOW_STYLE = WINDOW_STYLE(WS_CAPTION.0 | WS_SYSMENU.0);
const EX_STYLE: WINDOW_EX_STYLE = WS_EX_DLGMODALFRAME;
// Layout, in 96-DPI pixels.
const WIDTH: i32 = 600;
const MARGIN: i32 = 24;
const LABEL_W: i32 = 92;
const ROW: i32 = 26;
const BUTTON_H: i32 = 32;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Action {
    MakeDefault,
    RestoreExplorer,
    ShowSettingsFile,
}

const ACTIONS: [Action; 3] = [Action::MakeDefault, Action::RestoreExplorer, Action::ShowSettingsFile];

struct State {
    hwnd: HWND,
    owner: HWND,
    dpi: u32,
    dark: bool,
    pal: Palette,
    fonts: Fonts,
    heading: HFONT,
    status: Status,
    exe: String,
    settings_file: String,
    /// Failure of the last action.
    error: Option<String>,
    buttons: Vec<(Action, RECT)>,
    hot: Option<Action>,
    pressed: Option<Action>,
    /// Keyboard focus; shown once Tab is used.
    focus: Option<Action>,
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

pub(crate) fn heading_font(ui: HFONT) -> HFONT {
    let mut lf = LOGFONTW::default();
    unsafe {
        GetObjectW(ui, std::mem::size_of::<LOGFONTW>() as i32, Some(&mut lf as *mut _ as *mut _));
        lf.lfHeight = lf.lfHeight * 4 / 3;
        lf.lfWeight = 600;
        CreateFontIndirectW(&lf)
    }
}

fn enabled(s: &State, a: Action) -> bool {
    match a {
        Action::MakeDefault => !s.status.all_twig(),
        Action::RestoreExplorer => s.status.any_twig(),
        Action::ShowSettingsFile => true,
    }
}

fn label(a: Action) -> &'static str {
    match a {
        Action::MakeDefault => "Make Twig the default",
        Action::RestoreExplorer => "Restore Explorer",
        Action::ShowSettingsFile => "Open settings folder",
    }
}

fn handler_text(h: &Handler) -> String {
    match h {
        Handler::Explorer => "File Explorer".into(),
        Handler::Twig => "Twig".into(),
        Handler::TwigAt(p) => format!("Twig at {p}"),
        Handler::Other(v) => format!("Another app (\u{201C}{v}\u{201D} verb)"),
    }
}

/// Status line: glyph, whether it's good news (accent) and text.
fn summary(st: &Status) -> (&'static str, Option<bool>, &'static str) {
    let other_copy = [&st.folders, &st.drives].iter().any(|h| matches!(h, Handler::TwigAt(_)));
    if st.all_twig() {
        (glyph::CHECK, Some(true), "Twig opens folders and drives.")
    } else if other_copy {
        (glyph::WARNING, Some(false), "Another copy of Twig is registered.")
    } else if st.folders == Handler::Explorer && st.drives == Handler::Explorer {
        (glyph::INFO, None, "File Explorer opens folders and drives.")
    } else {
        (glyph::INFO, None, "Folders and drives open in different apps.")
    }
}

const DT_PATH: DRAW_TEXT_FORMAT = DRAW_TEXT_FORMAT(DT_LEFT.0 | DT_VCENTER.0 | DT_PATH_ELLIPSIS.0);

/// Lays out (and with `draw`, paints) the content. Returns the button rects and content height.
fn render(s: &State, hdc: HDC, draw: bool) -> (Vec<(Action, RECT)>, i32) {
    let d = s.dpi;
    let p = &s.pal;
    let m = scale(MARGIN, d);
    let right = scale(WIDTH, d) - m;
    let row = scale(ROW, d);
    let mut y = scale(18, d);
    let mut buttons = Vec::new();
    let text = |font: HFONT, t: &str, r: RECT, color: COLORREF, flags: DRAW_TEXT_FORMAT| {
        if draw {
            unsafe {
                SelectObject(hdc, font);
            }
            draw_text(hdc, t, &r, color, flags);
        }
    };
    let heading = |y: &mut i32, t: &str| {
        let h = scale(32, d);
        text(s.heading, t, RECT { left: m, top: *y, right, bottom: *y + h }, p.text, DT_LEFT_V);
        *y += h + scale(4, d);
    };
    let field = |y: &mut i32, name: &str, value: &str, flags: DRAW_TEXT_FORMAT| {
        let r = RECT { left: m, top: *y, right, bottom: *y + row };
        text(s.fonts.ui, name, r, p.text_dim, DT_LEFT_V);
        text(s.fonts.ui, value, RECT { left: m + scale(LABEL_W, d), ..r }, p.text, flags);
        *y += row;
    };
    let mut button_row = |y: &mut i32, actions: &[Action]| {
        let mut x = m;
        let h = scale(BUTTON_H, d);
        for &a in actions {
            let w = text_width(hdc, s.fonts.ui, label(a)) + scale(36, d);
            let r = rc(x, *y, w, h);
            if draw {
                paint_button(s, hdc, a, &r, a == Action::MakeDefault);
            }
            buttons.push((a, r));
            x += w + scale(10, d);
        }
        *y += h;
    };

    heading(&mut y, "Default file manager");
    let (g, good, line) = summary(&s.status);
    let color = match good {
        Some(true) => p.accent,
        Some(false) => p.error,
        None => p.text_dim,
    };
    let r = RECT { left: m, top: y, right, bottom: y + row };
    text(s.fonts.glyph, g, RECT { right: m + scale(20, d), ..r }, color, DT_LEFT_V);
    text(s.fonts.ui, line, RECT { left: m + scale(28, d), ..r }, p.text, DT_LEFT_V);
    y += row + scale(6, d);
    field(&mut y, "Folders", &handler_text(&s.status.folders), DT_PATH);
    field(&mut y, "Drives", &handler_text(&s.status.drives), DT_PATH);
    y += scale(14, d);
    button_row(&mut y, &[Action::MakeDefault, Action::RestoreExplorer]);
    y += scale(10, d);
    if let Some(e) = &s.error {
        text(s.fonts.ui, e, RECT { left: m, top: y, right, bottom: y + row }, p.error, DT_LEFT_V);
        y += row;
    }
    for note in ["Applies to your Windows account only. Opening a folder anywhere (desktop, apps,", "Run dialog) opens it in Twig. Win+E and File Explorer's own windows are unchanged."] {
        text(s.fonts.ui, note, RECT { left: m, top: y, right, bottom: y + scale(20, d) }, p.text_dim, DT_LEFT_V);
        y += scale(20, d);
    }

    y += scale(18, d);
    if draw {
        fill(hdc, &rc(m, y, right - m, 1), p.border);
    }
    y += scale(18, d);

    heading(&mut y, "About");
    field(&mut y, "Version", env!("CARGO_PKG_VERSION"), DT_LEFT_V);
    field(&mut y, "Program", &s.exe, DT_PATH);
    field(&mut y, "Settings", &s.settings_file, DT_PATH);
    y += scale(14, d);
    button_row(&mut y, &[Action::ShowSettingsFile]);
    y += m;
    (buttons, y)
}

fn paint_button(s: &State, hdc: HDC, a: Action, r: &RECT, primary: bool) {
    let p = &s.pal;
    let en = enabled(s, a);
    let hot = en && s.hot == Some(a);
    let pressed = hot && s.pressed == Some(a);
    let fg = if primary && en {
        fill(hdc, r, p.accent);
        if hot {
            wash(hdc, r, if s.dark { rgb(0x000000) } else { rgb(0xFFFFFF) }, if pressed { 70 } else { 40 });
        }
        if s.dark {
            rgb(0x000000)
        } else {
            rgb(0xFFFFFF)
        }
    } else {
        fill(hdc, r, if pressed { p.sel_inactive } else if hot { p.tab_hover } else { p.toolbar });
        frame(hdc, r, p.border, 1);
        if en {
            p.text
        } else {
            p.text_dim
        }
    };
    unsafe {
        SelectObject(hdc, s.fonts.ui);
    }
    draw_text(hdc, label(a), r, fg, DT_CENTER_V);
    if s.focus == Some(a) {
        let o = scale(3, s.dpi);
        frame(hdc, &RECT { left: r.left - o, top: r.top - o, right: r.right + o, bottom: r.bottom + o }, p.text, 1);
    }
}

fn hit(s: &State, x: i32, y: i32) -> Option<Action> {
    s.buttons.iter().find(|(a, r)| pt_in(r, x, y) && enabled(s, *a)).map(|b| b.0)
}

/// Window size for the content at the current DPI.
fn window_size(s: &State) -> (i32, i32) {
    let hdc = unsafe { GetDC(s.hwnd) };
    let (_, h) = render(s, hdc, false);
    unsafe {
        ReleaseDC(s.hwnd, hdc);
    }
    let mut r = rc(0, 0, scale(WIDTH, s.dpi), h);
    unsafe {
        let _ = AdjustWindowRectExForDpi(&mut r, STYLE, false, EX_STYLE, s.dpi);
    }
    (r.right - r.left, r.bottom - r.top)
}

/// Re-reads the registration state (it may change outside Twig).
fn refresh(s: &mut State) {
    let st = defaultapp::status();
    if st != s.status {
        s.status = st;
        if s.focus.is_some_and(|a| !enabled(s, a)) {
            s.focus = None;
        }
        invalidate(s.hwnd);
    }
}

fn run(hwnd: HWND, a: Action) {
    let result = match a {
        Action::MakeDefault => defaultapp::make_default(),
        Action::RestoreExplorer => defaultapp::restore_explorer(),
        Action::ShowSettingsFile => {
            let file = with_state(|s| s.settings_file.clone()).unwrap_or_default();
            let dir = std::path::Path::new(&file).parent().map(|p| p.to_string_lossy().into_owned()).unwrap_or_default();
            let _ = std::fs::create_dir_all(&dir);
            crate::app::with_app(|app| app.open_external(&dir));
            Ok(())
        }
    };
    with_state(|s| {
        s.error = result.err();
        s.status = defaultapp::status();
        if s.focus.is_some_and(|a| !enabled(s, a)) {
            s.focus = ACTIONS.into_iter().find(|&a| enabled(s, a));
        }
    });
    invalidate(hwnd);
}

fn move_focus(s: &mut State, back: bool) {
    let list: Vec<Action> = ACTIONS.into_iter().filter(|&a| enabled(s, a)).collect();
    if list.is_empty() {
        return;
    }
    let n = list.len();
    let next = match s.focus.and_then(|f| list.iter().position(|&a| a == f)) {
        Some(i) if back => (i + n - 1) % n,
        Some(i) => (i + 1) % n,
        None if back => n - 1,
        None => 0,
    };
    s.focus = Some(list[next]);
    invalidate(s.hwnd);
}

fn set_dpi(s: &mut State, dpi: u32) {
    s.dpi = dpi;
    s.fonts = Fonts::new(dpi);
    let old = std::mem::replace(&mut s.heading, heading_font(s.fonts.ui));
    unsafe {
        let _ = DeleteObject(old);
    }
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    match msg {
        WM_PAINT => {
            let painted = with_state(|s| {
                let mut buttons = Vec::new();
                paint_buffered(hwnd, |hdc, r| {
                    fill(hdc, r, s.pal.bg);
                    buttons = render(s, hdc, true).0;
                });
                s.buttons = buttons;
            });
            if painted.is_some() {
                return LRESULT(0);
            }
        }
        WM_ERASEBKGND => return LRESULT(1),
        WM_MOUSEMOVE => {
            with_state(|s| {
                let h = hit(s, x_of(lp), y_of(lp));
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
            with_state(|s| {
                s.pressed = hit(s, x_of(lp), y_of(lp));
                if s.pressed.is_some() {
                    SetCapture(hwnd);
                    invalidate(hwnd);
                }
            });
            return LRESULT(0);
        }
        WM_LBUTTONUP => {
            let action = with_state(|s| {
                let p = s.pressed.take();
                invalidate(hwnd);
                p.filter(|&a| hit(s, x_of(lp), y_of(lp)) == Some(a))
            })
            .flatten();
            let _ = ReleaseCapture();
            if let Some(a) = action {
                run(hwnd, a);
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
        WM_KEYDOWN => {
            let vk = VIRTUAL_KEY(wp.0 as u16);
            if vk == VK_ESCAPE {
                let _ = DestroyWindow(hwnd);
            } else if vk == VK_TAB {
                with_state(|s| move_focus(s, shift()));
            } else if vk == VK_RETURN || vk == VK_SPACE {
                if let Some(a) = with_state(|s| s.focus).flatten() {
                    run(hwnd, a);
                }
            }
            return LRESULT(0);
        }
        WM_ACTIVATE => {
            if wp.0 & 0xFFFF != 0 {
                with_state(refresh);
            }
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
            if let Some((w, h)) = with_state(|s| {
                set_dpi(s, dpi);
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

/// Opens the settings window over `owner`, or brings an open one to the front.
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
        let Ok(hwnd) = CreateWindowExW(EX_STYLE, CLASS, w!("Twig settings"), STYLE, cx, cy, 1, 1, owner, None, instance(), None) else { return };
        let dpi = GetDpiForWindow(hwnd).max(96);
        let fonts = Fonts::new(dpi);
        let heading = heading_font(fonts.ui);
        let state = State {
            hwnd,
            owner,
            dpi,
            dark,
            pal: Palette::new(dark),
            fonts,
            heading,
            status: defaultapp::status(),
            exe: defaultapp::current_exe(),
            settings_file: crate::settings::settings_path().map(|p| p.to_string_lossy().into_owned()).unwrap_or_default(),
            error: None,
            buttons: Vec::new(),
            hot: None,
            pressed: None,
            focus: None,
            tracking: false,
        };
        let (w, h) = window_size(&state);
        STATE.with(|s| *s.borrow_mut() = Some(state));
        crate::theme::set_title_bar_dark(hwnd, dark);
        let _ = SetWindowPos(hwnd, HWND::default(), cx - w / 2, cy - h / 2, w, h, SWP_NOZORDER | SWP_NOACTIVATE);
        let _ = ShowWindow(hwnd, SW_SHOW);
    }
}
