//! Makes Twig the per-user handler for opening folders and drives.
//!
//! Sets the default verb of `HKCU\Software\Classes\{Directory,Drive}\shell` to a `twig` verb whose
//! command launches this executable. Explorer itself (Win+E, `explorer.exe <path>`) is untouched,
//! and restoring removes only what Twig added.

use crate::util::wide;
use windows::core::PCWSTR;
use windows::Win32::Foundation::*;
use windows::Win32::System::Registry::*;
use windows::Win32::UI::Shell::{SHChangeNotify, SHCNE_ASSOCCHANGED, SHCNF_IDLIST};

const VERB: &str = "twig";
/// Shell classes Twig takes over: file-system folders and drive roots.
const CLASSES: [&str; 2] = ["Directory", "Drive"];

/// Who opens a shell class (folders or drives) for the current user.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Handler {
    /// No per-user default verb: Windows' own default (File Explorer).
    Explorer,
    /// This Twig executable.
    Twig,
    /// Twig, but a different copy of the executable.
    TwigAt(String),
    /// Another program's verb is the per-user default.
    Other(String),
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Status {
    pub folders: Handler,
    pub drives: Handler,
}

impl Status {
    pub fn all_twig(&self) -> bool {
        self.folders == Handler::Twig && self.drives == Handler::Twig
    }

    /// True if any class still points at a Twig verb (this copy or another).
    pub fn any_twig(&self) -> bool {
        [&self.folders, &self.drives].iter().any(|h| matches!(h, Handler::Twig | Handler::TwigAt(_)))
    }
}

/// The command line registered for the verb.
pub fn command_for(exe: &str) -> String {
    format!("\"{exe}\" \"%1\"")
}

/// The program path of a registered command line (quoted or bare first token).
pub fn exe_of(command: &str) -> Option<&str> {
    let c = command.trim_start();
    let exe = match c.strip_prefix('"') {
        Some(rest) => &rest[..rest.find('"')?],
        None => c.split_whitespace().next()?,
    };
    (!exe.is_empty()).then_some(exe)
}

fn classify(default_verb: Option<&str>, command: Option<&str>, exe: &str) -> Handler {
    match default_verb.map(str::trim).filter(|v| !v.is_empty()) {
        None => Handler::Explorer,
        Some(v) if v.eq_ignore_ascii_case(VERB) => match command.and_then(exe_of) {
            Some(e) if e.eq_ignore_ascii_case(exe) => Handler::Twig,
            Some(e) => Handler::TwigAt(e.to_string()),
            // Verb without a command: the shell falls back to its own default.
            None => Handler::Explorer,
        },
        Some(v) => Handler::Other(v.to_string()),
    }
}

pub fn current_exe() -> String {
    std::env::current_exe().map(|p| p.to_string_lossy().into_owned()).unwrap_or_default()
}

fn shell_key(class: &str) -> String {
    format!("Software\\Classes\\{class}\\shell")
}

fn read_string(subkey: &str) -> Option<String> {
    let key = wide(subkey);
    let mut size = 0u32;
    unsafe {
        if RegGetValueW(HKEY_CURRENT_USER, PCWSTR(key.as_ptr()), PCWSTR::null(), RRF_RT_REG_SZ, None, None, Some(&mut size)) != ERROR_SUCCESS {
            return None;
        }
        let mut buf = vec![0u16; (size as usize / 2).max(1)];
        let r = RegGetValueW(
            HKEY_CURRENT_USER,
            PCWSTR(key.as_ptr()),
            PCWSTR::null(),
            RRF_RT_REG_SZ,
            None,
            Some(buf.as_mut_ptr() as *mut _),
            Some(&mut size),
        );
        (r == ERROR_SUCCESS).then(|| crate::util::from_wide(&buf))
    }
}

fn write_string(subkey: &str, value: &str) -> Result<(), String> {
    let key = wide(subkey);
    let data = wide(value);
    let r = unsafe {
        RegSetKeyValueW(
            HKEY_CURRENT_USER,
            PCWSTR(key.as_ptr()),
            PCWSTR::null(),
            REG_SZ.0,
            Some(data.as_ptr() as *const _),
            (data.len() * 2) as u32,
        )
    };
    if r == ERROR_SUCCESS {
        Ok(())
    } else {
        Err(format!("Couldn't write HKCU\\{subkey}: {}", windows::core::Error::from(r.to_hresult()).message()))
    }
}

fn handler_of(class: &str, exe: &str) -> Handler {
    let shell = shell_key(class);
    let verb = read_string(&shell);
    let command = read_string(&format!("{shell}\\{VERB}\\command"));
    classify(verb.as_deref(), command.as_deref(), exe)
}

pub fn status() -> Status {
    let exe = current_exe();
    Status { folders: handler_of(CLASSES[0], &exe), drives: handler_of(CLASSES[1], &exe) }
}

fn notify_shell() {
    unsafe { SHChangeNotify(SHCNE_ASSOCCHANGED, SHCNF_IDLIST, None, None) };
}

/// Registers this executable as the default for opening folders and drives.
pub fn make_default() -> Result<(), String> {
    let exe = current_exe();
    if exe.is_empty() {
        return Err("Couldn't determine Twig's location.".into());
    }
    for class in CLASSES {
        let shell = shell_key(class);
        write_string(&format!("{shell}\\{VERB}"), "Open in Twig")?;
        write_string(&format!("{shell}\\{VERB}\\command"), &command_for(&exe))?;
        write_string(&shell, VERB)?;
    }
    notify_shell();
    Ok(())
}

/// Removes Twig's verbs; folders and drives open in Explorer again.
pub fn restore_explorer() -> Result<(), String> {
    for class in CLASSES {
        let shell = shell_key(class);
        let key = wide(&shell);
        if read_string(&shell).is_some_and(|v| v.trim().eq_ignore_ascii_case(VERB)) {
            let r = unsafe { RegDeleteKeyValueW(HKEY_CURRENT_USER, PCWSTR(key.as_ptr()), PCWSTR::null()) };
            if r != ERROR_SUCCESS && r != ERROR_FILE_NOT_FOUND {
                return Err(format!("Couldn't reset HKCU\\{shell}: {}", windows::core::Error::from(r.to_hresult()).message()));
            }
        }
        let verb = wide(&format!("{shell}\\{VERB}"));
        let r = unsafe { RegDeleteTreeW(HKEY_CURRENT_USER, PCWSTR(verb.as_ptr())) };
        if r != ERROR_SUCCESS && r != ERROR_FILE_NOT_FOUND {
            return Err(format!("Couldn't remove HKCU\\{shell}\\{VERB}: {}", windows::core::Error::from(r.to_hresult()).message()));
        }
        // Don't leave behind the keys Twig created.
        if delete_if_empty(&shell) {
            delete_if_empty(&format!("Software\\Classes\\{class}"));
        }
    }
    notify_shell();
    Ok(())
}

/// Deletes an HKCU key that has no subkeys and no values. Returns true if deleted.
fn delete_if_empty(subkey: &str) -> bool {
    let key = wide(subkey);
    unsafe {
        let mut h = HKEY::default();
        if RegOpenKeyExW(HKEY_CURRENT_USER, PCWSTR(key.as_ptr()), 0, KEY_READ, &mut h) != ERROR_SUCCESS {
            return false;
        }
        let (mut subkeys, mut values) = (0u32, 0u32);
        let r = RegQueryInfoKeyW(h, windows::core::PWSTR::null(), None, None, Some(&mut subkeys), None, None, Some(&mut values), None, None, None, None);
        let _ = RegCloseKey(h);
        r == ERROR_SUCCESS && subkeys == 0 && values == 0 && RegDeleteKeyW(HKEY_CURRENT_USER, PCWSTR(key.as_ptr())) == ERROR_SUCCESS
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn command_round_trips_exe() {
        let exe = r"C:\Program Files\Twig\Twig.exe";
        assert_eq!(command_for(exe), r#""C:\Program Files\Twig\Twig.exe" "%1""#);
        assert_eq!(exe_of(&command_for(exe)), Some(exe));
        assert_eq!(exe_of(r"C:\Tools\Twig.exe %1"), Some(r"C:\Tools\Twig.exe"));
        assert_eq!(exe_of(r#""unterminated"#), None);
        assert_eq!(exe_of("  "), None);
    }

    #[test]
    fn classify_handlers() {
        let exe = r"C:\Twig\Twig.exe";
        let cmd = command_for(exe);
        assert_eq!(classify(None, None, exe), Handler::Explorer);
        assert_eq!(classify(Some(""), Some(&cmd), exe), Handler::Explorer);
        assert_eq!(classify(Some("TWIG"), Some(&cmd.to_lowercase()), exe), Handler::Twig);
        assert_eq!(classify(Some("twig"), Some(&command_for(r"D:\old\Twig.exe")), exe), Handler::TwigAt(r"D:\old\Twig.exe".into()));
        assert_eq!(classify(Some("twig"), None, exe), Handler::Explorer);
        assert_eq!(classify(Some("openinfiles"), None, exe), Handler::Other("openinfiles".into()));
    }
}
