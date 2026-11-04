//! In-app updates from the GitHub releases of the Twig repository.
//!
//! A check asks GitHub for the latest release. Installing downloads its `Twig.exe` next to the
//! running exe, swaps the files (a running exe can be renamed, not overwritten) and restarts:
//! the new process waits for this one to exit before claiming the single-instance mutex.

use crate::util::wide;
use std::ffi::c_void;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::{CloseHandle, WAIT_OBJECT_0};
use windows::Win32::Networking::WinHttp::*;
use windows::Win32::System::Threading::{OpenProcess, WaitForSingleObject, PROCESS_SYNCHRONIZE};

const LATEST_URL: &str = "https://api.github.com/repos/alex-bozhko/Twig/releases/latest";
const ASSET: &str = "Twig.exe";
/// Passed to the updated exe so it waits for the old process before starting.
pub const WAIT_FLAG: &str = "--wait-pid";
const WAIT_MS: u32 = 15_000;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Release {
    pub version: String,
    url: String,
    size: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Phase {
    Idle,
    Checking,
    UpToDate,
    Available(Release),
    Downloading(Release),
    /// The new exe is in place and Twig is restarting.
    Restarting,
    Failed(String),
}

static PHASE: Mutex<Phase> = Mutex::new(Phase::Idle);

pub fn phase() -> Phase {
    PHASE.lock().map(|p| p.clone()).unwrap_or(Phase::Idle)
}

fn set_phase(p: Phase) {
    if let Ok(mut g) = PHASE.lock() {
        *g = p;
    }
    crate::win::post_main(crate::win::WM_UPDATE);
}

pub fn busy() -> bool {
    matches!(phase(), Phase::Checking | Phase::Downloading(_) | Phase::Restarting)
}

/// Starts a background check; the main window gets `WM_UPDATE` on each phase change.
pub fn check() {
    if busy() {
        return;
    }
    set_phase(Phase::Checking);
    std::thread::spawn(|| {
        set_phase(match latest() {
            Ok(r) if is_newer(&r.version, env!("CARGO_PKG_VERSION")) => Phase::Available(r),
            Ok(_) => Phase::UpToDate,
            Err(e) => Phase::Failed(e),
        })
    });
}

/// Downloads and installs `r`, then restarts Twig.
pub fn install(r: Release) {
    if busy() {
        return;
    }
    set_phase(Phase::Downloading(r.clone()));
    std::thread::spawn(move || {
        let result = std::env::current_exe().map_err(|e| e.to_string()).and_then(|exe| {
            let data = get(&r.url, "application/octet-stream")?;
            validate(&data, r.size)?;
            replace_exe(&exe, &data)?;
            restart(&exe)
        });
        set_phase(match result {
            Ok(()) => Phase::Restarting,
            Err(e) => Phase::Failed(e),
        });
    });
}

fn latest_url() -> String {
    #[cfg(debug_assertions)]
    if let Ok(u) = std::env::var("TWIG_UPDATE_URL") {
        return u;
    }
    LATEST_URL.into()
}

fn latest() -> Result<Release, String> {
    let body = get(&latest_url(), "application/vnd.github+json")?;
    parse_release(&body)
}

fn parse_release(body: &[u8]) -> Result<Release, String> {
    let bad = || "GitHub sent an unexpected reply.".to_string();
    let v: serde_json::Value = serde_json::from_slice(body).map_err(|_| bad())?;
    let version = v["tag_name"].as_str().ok_or_else(bad)?.trim_start_matches(['v', 'V']).to_string();
    parse_version(&version).ok_or_else(bad)?;
    let asset = v["assets"].as_array().and_then(|a| a.iter().find(|a| a["name"].as_str() == Some(ASSET)));
    let Some(asset) = asset else { return Err(format!("Release {version} has no {ASSET}.")) };
    let url = asset["browser_download_url"].as_str().ok_or_else(bad)?.to_string();
    let size = asset["size"].as_u64().unwrap_or(0);
    Ok(Release { version, url, size })
}

/// "2.0.1" (optionally "v"-prefixed, with a "-pre" or "+build" suffix) as numbers.
fn parse_version(s: &str) -> Option<Vec<u32>> {
    let core = s.trim().trim_start_matches(['v', 'V']).split(['-', '+']).next()?;
    let parts: Option<Vec<u32>> = core.split('.').map(|p| p.parse().ok()).collect();
    parts.filter(|p| !p.is_empty())
}

fn is_newer(latest: &str, current: &str) -> bool {
    match (parse_version(latest), parse_version(current)) {
        (Some(mut a), Some(mut b)) => {
            let n = a.len().max(b.len());
            a.resize(n, 0);
            b.resize(n, 0);
            a > b
        }
        _ => false,
    }
}

fn validate(data: &[u8], size: u64) -> Result<(), String> {
    if !data.starts_with(b"MZ") || (size != 0 && data.len() as u64 != size) {
        return Err("The download is incomplete or damaged.".into());
    }
    Ok(())
}

fn sibling(exe: &Path, suffix: &str) -> PathBuf {
    let mut name = exe.file_name().unwrap_or_default().to_os_string();
    name.push(suffix);
    exe.with_file_name(name)
}

/// Puts `data` in place of `exe`, keeping the running exe as `<exe>.old`.
fn replace_exe(exe: &Path, data: &[u8]) -> Result<(), String> {
    let new = sibling(exe, ".new");
    let old = sibling(exe, ".old");
    let dir = exe.parent().map(|d| d.display().to_string()).unwrap_or_default();
    std::fs::write(&new, data).map_err(|e| format!("Can't write to {dir}: {e}"))?;
    let _ = std::fs::remove_file(&old);
    if let Err(e) = std::fs::rename(exe, &old) {
        let _ = std::fs::remove_file(&new);
        return Err(format!("Can't replace {}: {e}", exe.display()));
    }
    if let Err(e) = std::fs::rename(&new, exe) {
        let _ = std::fs::rename(&old, exe);
        let _ = std::fs::remove_file(&new);
        return Err(format!("Can't replace {}: {e}", exe.display()));
    }
    Ok(())
}

/// Starts the new exe (it waits for this process) and closes the main window.
fn restart(exe: &Path) -> Result<(), String> {
    std::process::Command::new(exe)
        .arg(WAIT_FLAG)
        .arg(std::process::id().to_string())
        .spawn()
        .map_err(|e| format!("Updated, but couldn't restart ({e}). Start Twig again."))?;
    crate::win::post_main(windows::Win32::UI::WindowsAndMessaging::WM_CLOSE);
    Ok(())
}

/// Removes leftovers of a previous update.
pub fn cleanup() {
    if let Ok(exe) = std::env::current_exe() {
        let _ = std::fs::remove_file(sibling(&exe, ".old"));
        let _ = std::fs::remove_file(sibling(&exe, ".new"));
    }
}

/// Removes `WAIT_FLAG <pid>` from `args` and waits (bounded) for that process to exit.
pub fn wait_from_args(args: &mut Vec<String>) {
    let Some(i) = args.iter().position(|a| a == WAIT_FLAG) else { return };
    let pid = args.get(i + 1).and_then(|p| p.parse::<u32>().ok());
    args.drain(i..(i + 2).min(args.len()));
    if let Some(pid) = pid {
        unsafe {
            if let Ok(h) = OpenProcess(PROCESS_SYNCHRONIZE, false, pid) {
                let _ = WaitForSingleObject(h, WAIT_MS) == WAIT_OBJECT_0;
                let _ = CloseHandle(h);
            }
        }
    }
}

struct Internet(*mut c_void);

impl Drop for Internet {
    fn drop(&mut self) {
        if !self.0.is_null() {
            unsafe {
                let _ = WinHttpCloseHandle(self.0);
            }
        }
    }
}

fn open(h: *mut c_void) -> Result<Internet, String> {
    if h.is_null() {
        Err(net_error(&windows::core::Error::from_win32()))
    } else {
        Ok(Internet(h))
    }
}

fn net_error(e: &windows::core::Error) -> String {
    match (e.code().0 & 0xFFFF) as u32 {
        ERROR_WINHTTP_TIMEOUT => "GitHub didn't answer in time. Try again later.".into(),
        ERROR_WINHTTP_NAME_NOT_RESOLVED | ERROR_WINHTTP_CANNOT_CONNECT | ERROR_WINHTTP_CONNECTION_ERROR => {
            "Can't reach GitHub. Check your internet connection.".into()
        }
        c => format!("Network error {c}."),
    }
}

/// Splits "https://host[:port]/path" into (secure, host, port, path).
fn split_url(url: &str) -> Option<(bool, &str, u16, &str)> {
    let (secure, rest) = if let Some(r) = url.strip_prefix("https://") {
        (true, r)
    } else {
        (false, url.strip_prefix("http://")?)
    };
    let (authority, path) = rest.find('/').map_or((rest, "/"), |i| (&rest[..i], &rest[i..]));
    let (host, port) = match authority.rsplit_once(':') {
        Some((h, p)) => (h, p.parse().ok()?),
        None => (authority, if secure { INTERNET_DEFAULT_HTTPS_PORT } else { INTERNET_DEFAULT_HTTP_PORT }),
    };
    (!host.is_empty()).then_some((secure, host, port, path))
}

/// HTTP GET through WinHTTP (system proxy settings, redirects followed).
fn get(url: &str, accept: &str) -> Result<Vec<u8>, String> {
    let (secure, host, port, path) = split_url(url).ok_or("Bad update address.")?;
    let agent = wide(&format!("Twig/{}", env!("CARGO_PKG_VERSION")));
    let host = wide(host);
    let path = wide(path);
    let headers: Vec<u16> = format!("Accept: {accept}\r\n").encode_utf16().collect();
    let err = |e: windows::core::Error| net_error(&e);
    unsafe {
        let session = open(WinHttpOpen(PCWSTR(agent.as_ptr()), WINHTTP_ACCESS_TYPE_AUTOMATIC_PROXY, PCWSTR::null(), PCWSTR::null(), 0))?;
        let _ = WinHttpSetTimeouts(session.0, 15_000, 15_000, 30_000, 60_000);
        let conn = open(WinHttpConnect(session.0, PCWSTR(host.as_ptr()), port, 0))?;
        let flags = if secure { WINHTTP_FLAG_SECURE | WINHTTP_FLAG_REFRESH } else { WINHTTP_FLAG_REFRESH };
        let req = open(WinHttpOpenRequest(conn.0, w!("GET"), PCWSTR(path.as_ptr()), PCWSTR::null(), PCWSTR::null(), std::ptr::null(), flags))?;
        WinHttpSendRequest(req.0, Some(&headers), None, 0, 0, 0).map_err(err)?;
        WinHttpReceiveResponse(req.0, std::ptr::null_mut()).map_err(err)?;
        let mut status = 0u32;
        let mut len = std::mem::size_of::<u32>() as u32;
        WinHttpQueryHeaders(req.0, WINHTTP_QUERY_STATUS_CODE | WINHTTP_QUERY_FLAG_NUMBER, PCWSTR::null(), Some(&mut status as *mut u32 as *mut c_void), &mut len, std::ptr::null_mut())
            .map_err(err)?;
        match status {
            200 => {}
            404 => return Err("No published Twig release was found.".into()),
            403 | 429 => return Err("GitHub is limiting requests. Try again later.".into()),
            s => return Err(format!("GitHub answered with error {s}.")),
        }
        let mut body = Vec::new();
        let mut buf = vec![0u8; 64 * 1024];
        loop {
            let mut read = 0u32;
            WinHttpReadData(req.0, buf.as_mut_ptr() as *mut c_void, buf.len() as u32, &mut read).map_err(err)?;
            if read == 0 {
                break;
            }
            body.extend_from_slice(&buf[..read as usize]);
        }
        Ok(body)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn newer_versions() {
        assert!(is_newer("2.0.2", "2.0.1"));
        assert!(is_newer("v2.1", "2.0.9"));
        assert!(is_newer("10.0.0", "9.9.9"));
        assert!(!is_newer("2.0.1", "2.0.1"));
        assert!(!is_newer("v2.0.0", "2.0.1"));
        assert!(!is_newer("2.0", "2.0.0"));
        assert!(!is_newer("2.0.1-beta", "2.0.1"));
        assert!(!is_newer("garbage", "2.0.1"));
    }

    #[test]
    fn release_json() {
        let json = br#"{"tag_name":"v2.0.2","assets":[
            {"name":"Other.zip","browser_download_url":"https://x/o","size":1},
            {"name":"Twig.exe","browser_download_url":"https://x/Twig.exe","size":1234}]}"#;
        assert_eq!(parse_release(json), Ok(Release { version: "2.0.2".into(), url: "https://x/Twig.exe".into(), size: 1234 }));
        assert!(parse_release(br#"{"tag_name":"v2.0.2","assets":[]}"#).unwrap_err().contains("no Twig.exe"));
        assert!(parse_release(br#"{"message":"Not Found"}"#).is_err());
    }

    #[test]
    fn urls() {
        assert_eq!(split_url("https://api.github.com/repos/a/b"), Some((true, "api.github.com", 443, "/repos/a/b")));
        assert_eq!(split_url("http://localhost:8123/latest.json"), Some((false, "localhost", 8123, "/latest.json")));
        assert_eq!(split_url("https://host"), Some((true, "host", 443, "/")));
        assert_eq!(split_url("ftp://host/x"), None);
    }

    #[test]
    fn validates_download() {
        assert!(validate(b"MZ\x90\x00", 4).is_ok());
        assert!(validate(b"MZ\x90\x00", 0).is_ok());
        assert!(validate(b"MZ\x90", 4).is_err());
        assert!(validate(b"<html>", 6).is_err());
    }
}
