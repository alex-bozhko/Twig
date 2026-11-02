#![windows_subsystem = "windows"]

mod app;
mod dragdrop;
mod fileops;
mod grid;
mod layout;
mod loader;
mod mainwin;
mod model;
mod settings;
mod shell;
mod theme;
mod util;
mod watch;
mod win;
mod zipfs;

use windows::Win32::System::Ole::{OleInitialize, OleUninitialize};
use windows::Win32::UI::Controls::{InitCommonControlsEx, ICC_STANDARD_CLASSES, INITCOMMONCONTROLSEX};
use windows::Win32::UI::HiDpi::{GetDpiForWindow, SetProcessDpiAwarenessContext, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2};
use windows::Win32::UI::WindowsAndMessaging::*;

fn main() {
    unsafe {
        let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
        if OleInitialize(None).is_err() {
            return;
        }
        let icc = INITCOMMONCONTROLSEX { dwSize: std::mem::size_of::<INITCOMMONCONTROLSEX>() as u32, dwICC: ICC_STANDARD_CLASSES };
        let _ = InitCommonControlsEx(&icc);
    }
    std::thread::spawn(zipfs::cleanup_old);

    let dark = theme::system_prefers_dark();
    theme::init_app_dark_mode(dark);
    let settings = settings::Settings::load();

    mainwin::register_class();
    let (hwnd, maximized) = mainwin::create_main_window(&settings);
    win::set_main_hwnd(hwnd);
    theme::set_title_bar_dark(hwnd, dark);
    let edit = mainwin::create_path_edit(hwnd);
    let dpi = unsafe { GetDpiForWindow(hwnd) }.max(96);
    app::install(app::App::new(hwnd, edit, dpi, dark, settings));
    mainwin::init_chrome();
    dragdrop::register_main(hwnd);
    unsafe {
        let _ = ShowWindow(hwnd, if maximized { SW_SHOWMAXIMIZED } else { SW_SHOWNORMAL });
    }
    mainwin::focus_active_grid();

    let mut msg = MSG::default();
    unsafe {
        while GetMessageW(&mut msg, None, 0, 0).as_bool() {
            if mainwin::handle_shortcut(&msg) {
                continue;
            }
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
    drop(app::uninstall());
    unsafe { OleUninitialize() };
}
