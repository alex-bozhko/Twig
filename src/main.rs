#![windows_subsystem = "windows"]

mod actionlog;
mod app;
mod defaultapp;
mod dragdrop;
mod fileops;
mod grid;
mod helpwin;
mod instance;
mod layout;
mod loader;
mod logwin;
mod mainwin;
mod model;
mod preview;
mod scan;
mod settings;
mod settingswin;
mod shell;
mod shellsel;
mod theme;
mod updater;
mod util;
mod watch;
mod win;
mod zipfs;

use windows::Win32::System::Ole::{OleInitialize, OleUninitialize};
use windows::Win32::UI::Controls::{InitCommonControlsEx, ICC_STANDARD_CLASSES, ICC_WIN95_CLASSES, INITCOMMONCONTROLSEX};
use windows::Win32::UI::HiDpi::{GetDpiForWindow, SetProcessDpiAwarenessContext, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2};
use windows::Win32::UI::WindowsAndMessaging::*;

/// How long a shell launch keeps listening for "Show in folder" to name the file to select. The
/// folder opens right away; the shell usually calls within a second but can be slower.
const SHELL_SELECT_GRACE: std::time::Duration = std::time::Duration::from_secs(10);

fn main() {
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    updater::wait_from_args(&mut args);
    let from_shell = args.first().is_some_and(|a| a.eq_ignore_ascii_case(defaultapp::SHELL_FLAG));
    if from_shell {
        args.remove(0);
    }
    let start_path = instance::path_from_args(args);
    // "Show in folder" passes only the folder; the file arrives later through the shell's
    // window list, where we stand in for an Explorer window.
    let shell_dir = start_path.clone().filter(|p| from_shell && std::path::Path::new(p).is_dir());
    if !instance::claim() && instance::forward(start_path.as_deref()) {
        if let Some(dir) = shell_dir {
            reveal_shell_selection(&dir);
        }
        return;
    }
    if unsafe { OleInitialize(None) }.is_err() {
        return;
    }
    if let Some(dir) = shell_dir {
        std::thread::spawn(move || reveal_shell_selection(&dir));
    }
    unsafe {
        let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
        let icc = INITCOMMONCONTROLSEX { dwSize: std::mem::size_of::<INITCOMMONCONTROLSEX>() as u32, dwICC: ICC_STANDARD_CLASSES | ICC_WIN95_CLASSES };
        let _ = InitCommonControlsEx(&icc);
    }
    std::thread::spawn(zipfs::cleanup_old);
    std::thread::spawn(defaultapp::upgrade_command);
    std::thread::spawn(updater::cleanup);

    let dark = theme::system_prefers_dark();
    theme::init_app_dark_mode(dark);
    let settings = settings::Settings::load();

    mainwin::register_class();
    let (hwnd, maximized) = mainwin::create_main_window(&settings);
    win::set_main_hwnd(hwnd);
    instance::allow_copydata(hwnd);
    theme::set_title_bar_dark(hwnd, dark);
    let dpi = unsafe { GetDpiForWindow(hwnd) }.max(96);
    app::install(app::App::new(hwnd, dpi, dark, settings));
    mainwin::init_chrome();
    dragdrop::register_main(hwnd);
    unsafe {
        let _ = ShowWindow(hwnd, if maximized { SW_SHOWMAXIMIZED } else { SW_SHOWNORMAL });
    }
    mainwin::focus_active_grid();
    if let Some(p) = start_path {
        app::with_app(|app| app.open_external(&p));
    }

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

/// Waits for the shell to name the item to select in `dir` and forwards it to the main window.
fn reveal_shell_selection(dir: &str) {
    if unsafe { OleInitialize(None) }.is_err() {
        return;
    }
    if let Some(item) = shellsel::selected_item(dir, SHELL_SELECT_GRACE) {
        instance::forward_reveal(&item);
    }
    unsafe { OleUninitialize() };
}