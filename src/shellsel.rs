//! "Show in folder" support. SHOpenFolderAndSelectItems launches the default folder handler
//! with only the folder, then waits for a matching window in the shell's window list and asks
//! that window's view to select the file. Twig briefly registers a stand-in browser for the
//! folder and reports which item the shell selected.

use crate::util::wide;
use std::cell::RefCell;
use std::time::{Duration, Instant};
use windows::core::{implement, w, Interface, BSTR, GUID, HRESULT, PCWSTR, VARIANT};
use windows::Win32::Foundation::*;
use windows::Win32::System::Com::*;
use windows::Win32::System::Ole::*;
use windows::Win32::System::Threading::{GetCurrentThreadId, INFINITE};
use windows::Win32::System::Variant::InitVariantFromBuffer;
use windows::Win32::UI::Controls::LPFNSVADDPROPSHEETPAGE;
use windows::Win32::UI::Shell::Common::ITEMIDLIST;
use windows::Win32::UI::Shell::*;
use windows::Win32::UI::WindowsAndMessaging::*;
use windows_core::IUnknownImpl;

thread_local! {
    static SELECTED: RefCell<Option<String>> = const { RefCell::new(None) };
}

/// Owns an absolute ITEMIDLIST allocated by the shell.
struct Pidl(*mut ITEMIDLIST);

impl Drop for Pidl {
    fn drop(&mut self) {
        unsafe { ILFree(Some(self.0)) };
    }
}

#[implement(IWebBrowserApp, IServiceProvider, IShellView)]
struct Browser {
    hwnd: HWND,
    folder: *const ITEMIDLIST,
}

impl IDispatch_Impl for Browser_Impl {
    fn GetTypeInfoCount(&self) -> windows::core::Result<u32> {
        Ok(0)
    }
    fn GetTypeInfo(&self, _: u32, _: u32) -> windows::core::Result<ITypeInfo> {
        Err(E_NOTIMPL.into())
    }
    fn GetIDsOfNames(&self, _: *const GUID, _: *const PCWSTR, _: u32, _: u32, _: *mut i32) -> windows::core::Result<()> {
        Err(DISP_E_UNKNOWNNAME.into())
    }
    fn Invoke(&self, _: i32, _: *const GUID, _: u32, _: DISPATCH_FLAGS, _: *const DISPPARAMS, _: *mut VARIANT, _: *mut EXCEPINFO, _: *mut u32) -> windows::core::Result<()> {
        Err(DISP_E_MEMBERNOTFOUND.into())
    }
}

impl IWebBrowser_Impl for Browser_Impl {
    fn GoBack(&self) -> windows::core::Result<()> {
        Err(E_NOTIMPL.into())
    }
    fn GoForward(&self) -> windows::core::Result<()> {
        Err(E_NOTIMPL.into())
    }
    fn GoHome(&self) -> windows::core::Result<()> {
        Err(E_NOTIMPL.into())
    }
    fn GoSearch(&self) -> windows::core::Result<()> {
        Err(E_NOTIMPL.into())
    }
    fn Navigate(&self, _: &BSTR, _: *const VARIANT, _: *const VARIANT, _: *const VARIANT, _: *const VARIANT) -> windows::core::Result<()> {
        Err(E_NOTIMPL.into())
    }
    fn Refresh(&self) -> windows::core::Result<()> {
        Ok(())
    }
    fn Refresh2(&self, _: *const VARIANT) -> windows::core::Result<()> {
        Ok(())
    }
    fn Stop(&self) -> windows::core::Result<()> {
        Ok(())
    }
    fn Application(&self) -> windows::core::Result<IDispatch> {
       
        self.to_interface::<IWebBrowserApp>().cast()
    }
    fn Parent(&self) -> windows::core::Result<IDispatch> {
        self.to_interface::<IWebBrowserApp>().cast()
    }
    fn Container(&self) -> windows::core::Result<IDispatch> {
        self.to_interface::<IWebBrowserApp>().cast()
    }
    fn Document(&self) -> windows::core::Result<IDispatch> {
        self.to_interface::<IWebBrowserApp>().cast()
    }
    fn TopLevelContainer(&self) -> windows::core::Result<VARIANT_BOOL> {
        Ok(VARIANT_TRUE)
    }
    fn Type(&self) -> windows::core::Result<BSTR> {
        Ok(BSTR::from("Twig"))
    }
    fn Left(&self) -> windows::core::Result<i32> {
        Ok(0)
    }
    fn SetLeft(&self, _: i32) -> windows::core::Result<()> {
        Err(E_NOTIMPL.into())
    }
    fn Top(&self) -> windows::core::Result<i32> {
        Ok(0)
    }
    fn SetTop(&self, _: i32) -> windows::core::Result<()> {
        Err(E_NOTIMPL.into())
    }
    fn Width(&self) -> windows::core::Result<i32> {
        Ok(0)
    }
    fn SetWidth(&self, _: i32) -> windows::core::Result<()> {
        Err(E_NOTIMPL.into())
    }
    fn Height(&self) -> windows::core::Result<i32> {
        Ok(0)
    }
    fn SetHeight(&self, _: i32) -> windows::core::Result<()> {
        Err(E_NOTIMPL.into())
    }
    fn LocationName(&self) -> windows::core::Result<BSTR> {
        Ok(BSTR::new())
    }
    fn LocationURL(&self) -> windows::core::Result<BSTR> {
        Ok(BSTR::new())
    }
    fn Busy(&self) -> windows::core::Result<VARIANT_BOOL> {
        Ok(VARIANT_FALSE)
    }
}

impl IWebBrowserApp_Impl for Browser_Impl {
    fn Quit(&self) -> windows::core::Result<()> {
        Ok(())
    }
    fn ClientToWindow(&self, cx: *mut i32, cy: *mut i32) -> windows::core::Result<()> {
        if cx.is_null() || cy.is_null() {
            return Err(E_POINTER.into());
        }
        Ok(())
    }
    fn PutProperty(&self, _: &BSTR, _: &VARIANT) -> windows::core::Result<()> {
        Err(E_NOTIMPL.into())
    }
    fn GetProperty(&self, _: &BSTR) -> windows::core::Result<VARIANT> {
        Err(E_NOTIMPL.into())
    }
    fn Name(&self) -> windows::core::Result<BSTR> {
        Ok(BSTR::from("Twig"))
    }
    fn HWND(&self) -> windows::core::Result<SHANDLE_PTR> {
       
        Ok(SHANDLE_PTR(self.hwnd.0 as isize))
    }
    fn FullName(&self) -> windows::core::Result<BSTR> {
        let exe = std::env::current_exe().map_err(|_| windows::core::Error::from(E_FAIL))?;
        Ok(BSTR::from(exe.to_string_lossy().as_ref()))
    }
    fn Path(&self) -> windows::core::Result<BSTR> {
        self.FullName()
    }
    fn Visible(&self) -> windows::core::Result<VARIANT_BOOL> {
        Ok(VARIANT_FALSE)
    }
    fn SetVisible(&self, _: VARIANT_BOOL) -> windows::core::Result<()> {
        Err(E_NOTIMPL.into())
    }
    fn StatusBar(&self) -> windows::core::Result<VARIANT_BOOL> {
        Ok(VARIANT_FALSE)
    }
    fn SetStatusBar(&self, _: VARIANT_BOOL) -> windows::core::Result<()> {
        Err(E_NOTIMPL.into())
    }
    fn StatusText(&self) -> windows::core::Result<BSTR> {
        Ok(BSTR::new())
    }
    fn SetStatusText(&self, _: &BSTR) -> windows::core::Result<()> {
        Err(E_NOTIMPL.into())
    }
    fn ToolBar(&self) -> windows::core::Result<i32> {
        Ok(0)
    }
    fn SetToolBar(&self, _: i32) -> windows::core::Result<()> {
        Err(E_NOTIMPL.into())
    }
    fn MenuBar(&self) -> windows::core::Result<VARIANT_BOOL> {
        Ok(VARIANT_FALSE)
    }
    fn SetMenuBar(&self, _: VARIANT_BOOL) -> windows::core::Result<()> {
        Err(E_NOTIMPL.into())
    }
    fn FullScreen(&self) -> windows::core::Result<VARIANT_BOOL> {
        Ok(VARIANT_FALSE)
    }
    fn SetFullScreen(&self, _: VARIANT_BOOL) -> windows::core::Result<()> {
        Err(E_NOTIMPL.into())
    }
}

impl IServiceProvider_Impl for Browser_Impl {
    fn QueryService(&self, _service: *const GUID, riid: *const GUID, ppv: *mut *mut core::ffi::c_void) -> windows::core::Result<()> {
        unsafe { self.QueryInterface(riid, ppv) }.ok()
    }
}

impl IOleWindow_Impl for Browser_Impl {
    fn GetWindow(&self) -> windows::core::Result<HWND> {
        Ok(self.hwnd)
    }
    fn ContextSensitiveHelp(&self, _: BOOL) -> windows::core::Result<()> {
        Err(E_NOTIMPL.into())
    }
}

impl IShellView_Impl for Browser_Impl {
    fn TranslateAccelerator(&self, _: *const MSG) -> HRESULT {
        E_NOTIMPL
    }
    fn EnableModeless(&self, _: BOOL) -> windows::core::Result<()> {
        Ok(())
    }
    fn UIActivate(&self, _: u32) -> windows::core::Result<()> {
        Ok(())
    }
    fn Refresh(&self) -> windows::core::Result<()> {
        Ok(())
    }
    fn CreateViewWindow(&self, _: Option<&IShellView>, _: *const FOLDERSETTINGS, _: Option<&IShellBrowser>, _: *const RECT) -> windows::core::Result<HWND> {
        Err(E_NOTIMPL.into())
    }
    fn DestroyViewWindow(&self) -> windows::core::Result<()> {
        Ok(())
    }
    fn GetCurrentInfo(&self) -> windows::core::Result<FOLDERSETTINGS> {
        Ok(FOLDERSETTINGS::default())
    }
    fn AddPropertySheetPages(&self, _: u32, _: LPFNSVADDPROPSHEETPAGE, _: LPARAM) -> windows::core::Result<()> {
        Err(E_NOTIMPL.into())
    }
    fn SaveViewState(&self) -> windows::core::Result<()> {
        Ok(())
    }
    fn SelectItem(&self, item: *const ITEMIDLIST, flags: u32) -> windows::core::Result<()> {
        let wanted = (SVSI_SELECT.0 | SVSI_FOCUSED.0 | SVSI_ENSUREVISIBLE.0 | SVSI_EDIT.0) as u32;
        if item.is_null() || flags & wanted == 0 {
            return Ok(());
        }
        unsafe {
            let full = Pidl(ILCombine(Some(self.folder), Some(item)));
            if full.0.is_null() {
                return Err(E_OUTOFMEMORY.into());
            }
            let name = SHGetNameFromIDList(full.0, SIGDN_DESKTOPABSOLUTEPARSING)?;
            let path = name.to_string().unwrap_or_default();
            CoTaskMemFree(Some(name.0 as *const _));
            if !path.is_empty() {
                SELECTED.with(|s| *s.borrow_mut() = Some(path));
            }
        }
        Ok(())
    }
    fn GetItemObject(&self, _: &_SVGIO, _: *const GUID, ppv: *mut *mut core::ffi::c_void) -> windows::core::Result<()> {
        if !ppv.is_null() {
            unsafe { *ppv = std::ptr::null_mut() };
        }
        Err(E_NOINTERFACE.into())
    }
}

/// A stand-in browser registered in the shell's window list for one folder pidl.
struct Registration {
    hwnd: HWND,
    cookie: i32,
    pending: Option<i32>,
    _browser: IDispatch,
    _pidl: Pidl,
}

impl Registration {
    unsafe fn new(windows: &IShellWindows, pidl: Pidl) -> Option<Self> {
        let location = InitVariantFromBuffer(pidl.0 as *const _, ILGetSize(Some(pidl.0))).ok()?;
        let hwnd = CreateWindowExW(WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE, w!("STATIC"), w!("Twig"), WS_POPUP, 0, 0, 0, 0, None, None, None, None).ok()?;
        let app: IWebBrowserApp = Browser { hwnd, folder: pidl.0 }.into();
        let Ok(browser) = app.cast::<IDispatch>() else {
            let _ = DestroyWindow(hwnd);
            return None;
        };
        let pending = windows.RegisterPending(GetCurrentThreadId() as i32, &location, &VARIANT::default(), SWC_BROWSER).ok();
        let mut reg = Registration { hwnd, cookie: 0, pending, _browser: browser, _pidl: pidl };
        reg.cookie = windows.Register(&reg._browser, hwnd.0 as isize as i32, SWC_BROWSER).ok()?;
        windows.OnNavigate(reg.cookie, &location).ok()?;
        Some(reg)
    }

    unsafe fn revoke(self, windows: &IShellWindows) {
        if self.cookie != 0 {
            let _ = windows.Revoke(self.cookie);
        }
        // Register may complete the pending registration under the same cookie.
        if let Some(p) = self.pending.filter(|p| *p != self.cookie) {
            let _ = windows.Revoke(p);
        }
        let _ = DestroyWindow(self.hwnd);
    }
}

/// Pretends to be a shell window showing `folder` for up to `wait` and returns the item the
/// shell asked to select, if any. COM must be initialized as STA on this thread.
pub fn selected_item(folder: &str, wait: Duration) -> Option<String> {
    unsafe {
        let windows: IShellWindows = CoCreateInstance(&ShellWindows, None, CLSCTX_ALL).ok()?;
        let name = wide(folder);
        // Known folders (Downloads, Documents...) parse to a different pidl than the plain
        // file-system one that callers such as browsers build, so register both.
        let mut pidls = Vec::new();
        let plain = ILCreateFromPathW(PCWSTR(name.as_ptr()));
        if !plain.is_null() {
            pidls.push(Pidl(plain));
        }
        let mut parsed = std::ptr::null_mut();
        if SHParseDisplayName(PCWSTR(name.as_ptr()), None, &mut parsed, 0, None).is_ok() {
            let parsed = Pidl(parsed);
            if !pidls.iter().any(|p| ILIsEqual(p.0, parsed.0).as_bool()) {
                pidls.push(parsed);
            }
        }

        SELECTED.with(|s| s.borrow_mut().take());
        let regs: Vec<Registration> = pidls.into_iter().filter_map(|p| Registration::new(&windows, p)).collect();
        if !regs.is_empty() {
            pump_until(Instant::now() + wait);
        }
        for reg in regs {
            reg.revoke(&windows);
        }
        SELECTED.with(|s| s.borrow_mut().take())
    }
}
/// Runs the message loop (incoming COM calls arrive as messages) until an item is selected or
/// the deadline passes.
fn pump_until(deadline: Instant) {
    let mut msg = MSG::default();
    loop {
        unsafe {
            while PeekMessageW(&mut msg, None, 0, 0, PM_REMOVE).as_bool() {
                let _ = TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
        }
        if SELECTED.with(|s| s.borrow().is_some()) {
            return;
        }
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return;
        }
        let ms = (left.as_millis() as u32).min(INFINITE - 1);
        unsafe { MsgWaitForMultipleObjects(None, false, ms, QS_ALLINPUT) };
    }
}
