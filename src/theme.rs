//! Colors, fonts and dark-mode plumbing.

use crate::util::wide;
use crate::win::{rgb, scale};
use windows::core::{PCSTR, PCWSTR};
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Dwm::DwmSetWindowAttribute;
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::System::LibraryLoader::{GetProcAddress, LoadLibraryW};
use windows::Win32::System::Registry::*;
use windows::Win32::UI::Controls::SetWindowTheme;
use windows::Win32::UI::WindowsAndMessaging::*;

#[derive(Clone, Copy)]
pub struct Palette {
    pub bg: COLORREF,
    pub toolbar: COLORREF,
    pub text: COLORREF,
    pub text_dim: COLORREF,
    pub sel: COLORREF,
    pub sel_inactive: COLORREF,
    pub hover: COLORREF,
    pub accent: COLORREF,
    pub border: COLORREF,
    pub edit_bg: COLORREF,
    pub error: COLORREF,
    pub tab_hover: COLORREF,
}

impl Palette {
    pub fn new(dark: bool) -> Palette {
        if dark {
            Palette {
                bg: rgb(0x191919),
                toolbar: rgb(0x202020),
                text: rgb(0xF2F2F2),
                text_dim: rgb(0x9A9A9A),
                sel: rgb(0x4C4C4C),
                sel_inactive: rgb(0x3A3A3A),
                hover: rgb(0x2D2D2D),
                accent: rgb(0x4CC2FF),
                border: rgb(0x3A3A3A),
                edit_bg: rgb(0x2B2B2B),
                error: rgb(0xFF99A4),
                tab_hover: rgb(0x2D2D2D),
            }
        } else {
            Palette {
                bg: rgb(0xFFFFFF),
                toolbar: rgb(0xF3F3F3),
                text: rgb(0x1A1A1A),
                text_dim: rgb(0x6E6E6E),
                sel: rgb(0xCCE8FF),
                sel_inactive: rgb(0xD9D9D9),
                hover: rgb(0xE5F3FF),
                accent: rgb(0x0078D4),
                border: rgb(0xD5D5D5),
                edit_bg: rgb(0xFFFFFF),
                error: rgb(0xC42B1C),
                tab_hover: rgb(0xE8E8E8),
            }
        }
    }
}

pub struct Fonts {
    pub ui: HFONT,
    pub glyph: HFONT,
    pub glyph_small: HFONT,
    pub ui_height: i32,
}

impl Fonts {
    pub fn new(dpi: u32) -> Fonts {
        let mut ncm = NONCLIENTMETRICSW { cbSize: std::mem::size_of::<NONCLIENTMETRICSW>() as u32, ..Default::default() };
        unsafe {
            let ok = windows::Win32::UI::HiDpi::SystemParametersInfoForDpi(
                SPI_GETNONCLIENTMETRICS.0,
                ncm.cbSize,
                Some(&mut ncm as *mut _ as *mut _),
                0,
                dpi,
            )
            .is_ok();
            if !ok {
                ncm.lfMessageFont.lfHeight = -scale(12, dpi);
                let face = wide("Segoe UI");
                ncm.lfMessageFont.lfFaceName[..face.len()].copy_from_slice(&face);
            }
            let ui = CreateFontIndirectW(&ncm.lfMessageFont);
            let glyph = glyph_font(scale(16, dpi));
            let glyph_small = glyph_font(scale(10, dpi));
            let ui_height = crate::win::font_height(ui);
            Fonts { ui, glyph, glyph_small, ui_height }
        }
    }
}

impl Drop for Fonts {
    fn drop(&mut self) {
        unsafe {
            let _ = DeleteObject(self.ui);
            let _ = DeleteObject(self.glyph);
            let _ = DeleteObject(self.glyph_small);
        }
    }
}

fn glyph_font(px: i32) -> HFONT {
    for face in ["Segoe Fluent Icons", "Segoe MDL2 Assets"] {
        let mut lf = LOGFONTW { lfHeight: -px, lfCharSet: DEFAULT_CHARSET, lfQuality: CLEARTYPE_QUALITY, ..Default::default() };
        let w = wide(face);
        lf.lfFaceName[..w.len()].copy_from_slice(&w);
        unsafe {
            let f = CreateFontIndirectW(&lf);
            // Verify the face exists (GDI silently substitutes otherwise).
            let hdc = GetDC(None);
            let old = SelectObject(hdc, f);
            let mut buf = [0u16; 64];
            let n = GetTextFaceW(hdc, Some(&mut buf));
            SelectObject(hdc, old);
            ReleaseDC(None, hdc);
            if n > 0 && String::from_utf16_lossy(&buf[..(n as usize).saturating_sub(1)]).eq_ignore_ascii_case(face) {
                return f;
            }
            let _ = DeleteObject(f);
        }
    }
    let mut lf = LOGFONTW { lfHeight: -px, ..Default::default() };
    let w = wide("Segoe UI Symbol");
    lf.lfFaceName[..w.len()].copy_from_slice(&w);
    unsafe { CreateFontIndirectW(&lf) }
}

pub mod glyph {
    pub const UP: &str = "\u{E74A}";
    pub const REFRESH: &str = "\u{E72C}";
    pub const THIS_PC: &str = "\u{EC4E}";
    pub const STAR: &str = "\u{E734}";
    pub const CHEVRON_RIGHT: &str = "\u{E76C}";
    pub const CHEVRON_DOWN: &str = "\u{E70D}";
    pub const CLOSE: &str = "\u{E8BB}";
    pub const ADD: &str = "\u{E710}";
}

/// Reads the "apps use light theme" preference.
pub fn system_prefers_dark() -> bool {
    let key = wide("Software\\Microsoft\\Windows\\CurrentVersion\\Themes\\Personalize");
    let val = wide("AppsUseLightTheme");
    let mut data: u32 = 1;
    let mut size = 4u32;
    let r = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            PCWSTR(key.as_ptr()),
            PCWSTR(val.as_ptr()),
            RRF_RT_REG_DWORD,
            None,
            Some(&mut data as *mut u32 as *mut _),
            Some(&mut size),
        )
    };
    r == ERROR_SUCCESS && data == 0
}

type FnSetPreferredAppMode = unsafe extern "system" fn(i32) -> i32;
type FnAllowDarkModeForWindow = unsafe extern "system" fn(HWND, BOOL) -> BOOL;
type FnVoid = unsafe extern "system" fn();

fn uxtheme_ordinal(ord: u16) -> Option<unsafe extern "system" fn() -> isize> {
    unsafe {
        let name = wide("uxtheme.dll");
        let lib = LoadLibraryW(PCWSTR(name.as_ptr())).ok()?;
        GetProcAddress(lib, PCSTR(ord as usize as *const u8))
    }
}

/// Opts the process into dark context menus etc. (undocumented uxtheme APIs; silently ignored if missing).
pub fn init_app_dark_mode(dark: bool) {
    unsafe {
        if let Some(f) = uxtheme_ordinal(135) {
            let f: FnSetPreferredAppMode = std::mem::transmute(f);
            f(if dark { 2 } else { 3 }); // ForceDark / ForceLight
        }
        if let Some(f) = uxtheme_ordinal(104) {
            let f: FnVoid = std::mem::transmute(f);
            f();
        }
        if let Some(f) = uxtheme_ordinal(136) {
            let f: FnVoid = std::mem::transmute(f);
            f();
        }
    }
}

pub fn allow_dark_for_window(hwnd: HWND, dark: bool) {
    unsafe {
        if let Some(f) = uxtheme_ordinal(133) {
            let f: FnAllowDarkModeForWindow = std::mem::transmute(f);
            let _ = f(hwnd, BOOL::from(dark));
        }
    }
}

pub fn set_title_bar_dark(hwnd: HWND, dark: bool) {
    let v = BOOL::from(dark);
    unsafe {
        let _ = DwmSetWindowAttribute(hwnd, windows::Win32::Graphics::Dwm::DWMWINDOWATTRIBUTE(20), &v as *const _ as *const _, 4);
    }
}

pub fn set_control_theme(hwnd: HWND, dark: bool, dark_class: &str) {
    allow_dark_for_window(hwnd, dark);
    let name = wide(if dark { dark_class } else { "Explorer" });
    unsafe {
        let _ = SetWindowTheme(hwnd, PCWSTR(name.as_ptr()), PCWSTR::null());
    }
}
