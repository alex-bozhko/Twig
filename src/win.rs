//! Small Win32 helpers shared by the UI modules.

use crate::util::wide;
use std::cell::RefCell;
use std::collections::HashMap;
use std::ffi::c_void;
use std::sync::atomic::{AtomicIsize, Ordering};
use windows::core::PCWSTR;
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::UI::Controls::*;
use windows::Win32::UI::Shell::*;
use windows::Win32::UI::WindowsAndMessaging::*;

pub const WM_LOADED: u32 = WM_APP + 1;
pub const WM_WATCH: u32 = WM_APP + 2;
pub const WM_FILEOP: u32 = WM_APP + 3;
pub const WM_GRID_RELAYOUT: u32 = WM_APP + 10;
pub const WM_RENAME_END: u32 = WM_APP + 11;
pub const WM_FOCUS_CHANGED: u32 = WM_APP + 14;
pub const WM_REPAINT: u32 = WM_APP + 15;
pub const WM_FILTER_CHANGED: u32 = WM_APP + 16;
pub const WM_FILTER_ESCAPE: u32 = WM_APP + 17;
pub const WM_FILTER_ENTER: u32 = WM_APP + 18;
pub const WM_SCAN: u32 = WM_APP + 19;

static MAIN: AtomicIsize = AtomicIsize::new(0);

pub fn set_main_hwnd(h: HWND) {
    MAIN.store(h.0 as isize, Ordering::SeqCst);
}

pub fn main_hwnd() -> HWND {
    HWND(MAIN.load(Ordering::SeqCst) as *mut c_void)
}

/// Thread-safe wake-up of the UI thread.
pub fn post_main(msg: u32) {
    let h = main_hwnd();
    if !h.0.is_null() {
        unsafe {
            let _ = PostMessageW(h, msg, WPARAM(0), LPARAM(0));
        }
    }
}

pub fn post(hwnd: HWND, msg: u32, w: usize, l: isize) {
    unsafe {
        let _ = PostMessageW(hwnd, msg, WPARAM(w), LPARAM(l));
    }
}

pub fn hwnd_to_isize(h: HWND) -> isize {
    h.0 as isize
}

pub fn isize_to_hwnd(v: isize) -> HWND {
    HWND(v as *mut c_void)
}

pub fn x_of(l: LPARAM) -> i32 {
    (l.0 & 0xFFFF) as u16 as i16 as i32
}

pub fn y_of(l: LPARAM) -> i32 {
    ((l.0 >> 16) & 0xFFFF) as u16 as i16 as i32
}

pub fn scale(v: i32, dpi: u32) -> i32 {
    (v as i64 * dpi as i64 / 96) as i32
}

pub fn rgb(hex: u32) -> COLORREF {
    COLORREF(((hex & 0xFF) << 16) | (hex & 0xFF00) | ((hex >> 16) & 0xFF))
}

pub fn rc(x: i32, y: i32, w: i32, h: i32) -> RECT {
    RECT { left: x, top: y, right: x + w.max(0), bottom: y + h.max(0) }
}

pub fn pt_in(r: &RECT, x: i32, y: i32) -> bool {
    x >= r.left && x < r.right && y >= r.top && y < r.bottom
}

pub fn instance() -> HINSTANCE {
    unsafe { windows::Win32::System::LibraryLoader::GetModuleHandleW(None).map(|m| HINSTANCE(m.0)).unwrap_or_default() }
}

pub fn client_rect(hwnd: HWND) -> RECT {
    let mut r = RECT::default();
    unsafe {
        let _ = GetClientRect(hwnd, &mut r);
    }
    r
}

pub fn invalidate(hwnd: HWND) {
    unsafe {
        let _ = InvalidateRect(hwnd, None, false);
    }
}

pub fn set_text(hwnd: HWND, s: &str) {
    let w = wide(s);
    unsafe {
        let _ = SetWindowTextW(hwnd, PCWSTR(w.as_ptr()));
    }
}

pub fn get_text(hwnd: HWND) -> String {
    unsafe {
        let len = GetWindowTextLengthW(hwnd);
        let mut buf = vec![0u16; len as usize + 1];
        let n = GetWindowTextW(hwnd, &mut buf);
        String::from_utf16_lossy(&buf[..n as usize])
    }
}

pub fn key_down(vk: u16) -> bool {
    unsafe { (windows::Win32::UI::Input::KeyboardAndMouse::GetKeyState(vk as i32) as u16 & 0x8000) != 0 }
}

pub fn ctrl() -> bool {
    key_down(0x11)
}
pub fn shift() -> bool {
    key_down(0x10)
}
pub fn alt() -> bool {
    key_down(0x12)
}

pub fn cursor_pos() -> POINT {
    let mut p = POINT::default();
    unsafe {
        let _ = GetCursorPos(&mut p);
    }
    p
}

pub fn screen_to_client(hwnd: HWND, p: POINT) -> POINT {
    let mut p = p;
    unsafe {
        let _ = ScreenToClient(hwnd, &mut p);
    }
    p
}

pub fn client_to_screen(hwnd: HWND, p: POINT) -> POINT {
    let mut p = p;
    unsafe {
        let _ = ClientToScreen(hwnd, &mut p);
    }
    p
}

// ---------- GDI ----------

pub fn fill(hdc: HDC, r: &RECT, color: COLORREF) {
    unsafe {
        SetDCBrushColor(hdc, color);
        FillRect(hdc, r, HBRUSH(GetStockObject(DC_BRUSH).0));
    }
}

pub fn frame(hdc: HDC, r: &RECT, color: COLORREF, width: i32) {
    let w = width.max(1);
    fill(hdc, &RECT { left: r.left, top: r.top, right: r.right, bottom: r.top + w }, color);
    fill(hdc, &RECT { left: r.left, top: r.bottom - w, right: r.right, bottom: r.bottom }, color);
    fill(hdc, &RECT { left: r.left, top: r.top, right: r.left + w, bottom: r.bottom }, color);
    fill(hdc, &RECT { left: r.right - w, top: r.top, right: r.right, bottom: r.bottom }, color);
}

pub fn draw_text(hdc: HDC, s: &str, r: &RECT, color: COLORREF, flags: DRAW_TEXT_FORMAT) {
    if s.is_empty() {
        return;
    }
    let mut w: Vec<u16> = s.encode_utf16().collect();
    let mut r = *r;
    unsafe {
        SetTextColor(hdc, color);
        SetBkMode(hdc, TRANSPARENT);
        DrawTextW(hdc, &mut w, &mut r, flags | DT_NOPREFIX | DT_SINGLELINE);
    }
}

pub const DT_LEFT_V: DRAW_TEXT_FORMAT = DRAW_TEXT_FORMAT(DT_LEFT.0 | DT_VCENTER.0 | DT_END_ELLIPSIS.0);
pub const DT_RIGHT_V: DRAW_TEXT_FORMAT = DRAW_TEXT_FORMAT(DT_RIGHT.0 | DT_VCENTER.0 | DT_END_ELLIPSIS.0);
pub const DT_CENTER_V: DRAW_TEXT_FORMAT = DRAW_TEXT_FORMAT(DT_CENTER.0 | DT_VCENTER.0);

pub fn text_width(hdc: HDC, font: HFONT, s: &str) -> i32 {
    let w: Vec<u16> = s.encode_utf16().collect();
    let mut size = SIZE::default();
    unsafe {
        let old = SelectObject(hdc, font);
        let _ = GetTextExtentPoint32W(hdc, &w, &mut size);
        SelectObject(hdc, old);
    }
    size.cx
}

pub fn font_height(font: HFONT) -> i32 {
    unsafe {
        let hdc = GetDC(None);
        let old = SelectObject(hdc, font);
        let mut tm = TEXTMETRICW::default();
        let _ = GetTextMetricsW(hdc, &mut tm);
        SelectObject(hdc, old);
        ReleaseDC(None, hdc);
        tm.tmHeight
    }
}

/// Double-buffered paint helper: runs `f` on a memory DC and blits it to the window.
pub fn paint_buffered(hwnd: HWND, f: impl FnOnce(HDC, &RECT)) {
    unsafe {
        let mut ps = PAINTSTRUCT::default();
        let hdc = BeginPaint(hwnd, &mut ps);
        let r = client_rect(hwnd);
        let w = (r.right - r.left).max(1);
        let h = (r.bottom - r.top).max(1);
        let mem = CreateCompatibleDC(hdc);
        let bmp = CreateCompatibleBitmap(hdc, w, h);
        let old = SelectObject(mem, bmp);
        f(mem, &r);
        let _ = BitBlt(hdc, 0, 0, w, h, mem, 0, 0, SRCCOPY);
        SelectObject(mem, old);
        let _ = DeleteObject(bmp);
        let _ = DeleteDC(mem);
        let _ = EndPaint(hwnd, &ps);
    }
}

/// Paints `color` over `r` with the given opacity (0-255) — used for dimming icons.
pub fn wash(hdc: HDC, r: &RECT, color: COLORREF, alpha: u8) {
    unsafe {
        let mem = CreateCompatibleDC(hdc);
        let bmp = CreateCompatibleBitmap(hdc, 1, 1);
        let old = SelectObject(mem, bmp);
        let _ = SetPixel(mem, 0, 0, color);
        let blend = BLENDFUNCTION { BlendOp: 0, BlendFlags: 0, SourceConstantAlpha: alpha, AlphaFormat: 0 };
        let _ = AlphaBlend(hdc, r.left, r.top, r.right - r.left, r.bottom - r.top, mem, 0, 0, 1, 1, blend);
        SelectObject(mem, old);
        let _ = DeleteObject(bmp);
        let _ = DeleteDC(mem);
    }
}

// ---------- System image list ----------

struct IconLists {
    lists: Vec<(HIMAGELIST, i32)>,
    scaled: HashMap<(i32, i32), HICON>,
}

thread_local! {
    static ICONS: RefCell<Option<IconLists>> = const { RefCell::new(None) };
}

fn init_icon_lists() -> IconLists {
    let mut lists = Vec::new();
    for which in [SHIL_SMALL, SHIL_LARGE, SHIL_EXTRALARGE] {
        unsafe {
            if let Ok(il) = SHGetImageList::<IImageList>(which as i32) {
                let mut cx = 0;
                let mut cy = 0;
                if il.GetIconSize(&mut cx, &mut cy).is_ok() {
                    let h = HIMAGELIST(windows::core::Interface::as_raw(&il) as isize);
                    // Keep the list alive for the lifetime of the process.
                    std::mem::forget(il);
                    lists.push((h, cx));
                }
            }
        }
    }
    lists.sort_by_key(|l| l.1);
    IconLists { lists, scaled: HashMap::new() }
}

/// Draws system image list icon `index` at `size` pixels.
pub fn draw_icon(hdc: HDC, index: i32, x: i32, y: i32, size: i32) {
    if index < 0 {
        return;
    }
    ICONS.with(|cell| {
        let mut opt = cell.borrow_mut();
        let icons = opt.get_or_insert_with(init_icon_lists);
        let Some(&(list, lsize)) = icons.lists.iter().find(|l| l.1 >= size).or(icons.lists.last()) else { return };
        unsafe {
            if lsize == size {
                let _ = ImageList_Draw(list, index, hdc, x, y, ILD_TRANSPARENT);
            } else {
                let icon = *icons.scaled.entry((index, size)).or_insert_with(|| ImageList_GetIcon(list, index, ILD_TRANSPARENT));
                if !icon.0.is_null() {
                    let _ = DrawIconEx(hdc, x, y, icon, size, size, 0, None, DI_NORMAL);
                }
            }
        }
    });
}

/// Drops cached scaled icons (after DPI changes).
pub fn clear_icon_cache() {
    ICONS.with(|cell| {
        if let Some(icons) = cell.borrow_mut().as_mut() {
            for (_, icon) in icons.scaled.drain() {
                unsafe {
                    let _ = DestroyIcon(icon);
                }
            }
        }
    });
}

// ---------- Menus ----------

pub fn append_menu(menu: HMENU, id: usize, text: &str, enabled: bool) {
    let w = wide(text);
    let flags = if enabled { MF_STRING } else { MF_STRING | MF_GRAYED };
    unsafe {
        let _ = AppendMenuW(menu, flags, id, PCWSTR(w.as_ptr()));
    }
}

pub fn append_sep(menu: HMENU) {
    unsafe {
        let _ = AppendMenuW(menu, MF_SEPARATOR, 0, PCWSTR::null());
    }
}

/// Shows a popup menu and returns the chosen command id (0 = none).
pub fn track_menu(menu: HMENU, owner: HWND, pt: POINT, extra: TRACK_POPUP_MENU_FLAGS) -> u32 {
    unsafe {
        let _ = SetForegroundWindow(owner);
        let r = TrackPopupMenuEx(menu, (TPM_RETURNCMD | TPM_RIGHTBUTTON | extra).0, pt.x, pt.y, owner, None);
        r.0 as u32
    }
}

pub fn wait_cursor() -> HCURSOR {
    unsafe { SetCursor(LoadCursorW(None, IDC_WAIT).unwrap_or_default()) }
}
