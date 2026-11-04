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
    /// Secondary text that must stay readable (inactive tab titles).
    pub text_soft: COLORREF,
    pub sel: COLORREF,
    pub sel_inactive: COLORREF,
    pub hover: COLORREF,
    pub accent: COLORREF,
    pub border: COLORREF,
    pub edit_bg: COLORREF,
    pub error: COLORREF,
    /// Fill of inactive tabs, so they read as tabs against the strip.
    pub tab_inactive: COLORREF,
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
                text_soft: rgb(0xD0D0D0),
                sel: rgb(0x264F78),
                sel_inactive: rgb(0x3A3A3A),
                hover: rgb(0x2D2D2D),
                accent: rgb(0x4CC2FF),
                border: rgb(0x3A3A3A),
                edit_bg: rgb(0x2B2B2B),
                error: rgb(0xFF99A4),
                tab_inactive: rgb(0x2A2A2A),
                tab_hover: rgb(0x343434),
            }
        } else {
            Palette {
                bg: rgb(0xFFFFFF),
                toolbar: rgb(0xF3F3F3),
                text: rgb(0x1A1A1A),
                text_dim: rgb(0x6E6E6E),
                text_soft: rgb(0x3C3C3C),
                sel: rgb(0xCCE8FF),
                sel_inactive: rgb(0xD9D9D9),
                hover: rgb(0xE5F3FF),
                accent: rgb(0x0078D4),
                border: rgb(0xD5D5D5),
                edit_bg: rgb(0xFFFFFF),
                error: rgb(0xC42B1C),
                tab_inactive: rgb(0xE6E6E6),
                tab_hover: rgb(0xDADADA),
            }
        }
    }
}

pub struct Fonts {
    pub ui: HFONT,
    pub glyph: HFONT,
    pub glyph_small: HFONT,
    /// Breadcrumb chevrons and PC icon.
    pub glyph_mid: HFONT,
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
            let glyph_mid = glyph_font(scale(12, dpi));
            let ui_height = crate::win::font_height(ui);
            Fonts { ui, glyph, glyph_small, glyph_mid, ui_height }
        }
    }
}

impl Drop for Fonts {
    fn drop(&mut self) {
        unsafe {
            let _ = DeleteObject(self.ui);
            let _ = DeleteObject(self.glyph);
            let _ = DeleteObject(self.glyph_small);
            let _ = DeleteObject(self.glyph_mid);
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
    pub const THIS_PC: &str = "\u{E7F4}";
    pub const STAR: &str = "\u{E734}";
    pub const CHEVRON_RIGHT: &str = "\u{E76C}";
    pub const CHEVRON_DOWN: &str = "\u{E70D}";
    pub const CLOSE: &str = "\u{E8BB}";
    pub const ADD: &str = "\u{E710}";
    pub const SEARCH: &str = "\u{E721}";
    pub const FILTER: &str = "\u{E71C}";
    pub const ZOOM: &str = "\u{E838}";
    pub const CHECKBOX: &str = "\u{E739}";
    pub const CHECKBOX_CHECKED: &str = "\u{E73A}";
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
