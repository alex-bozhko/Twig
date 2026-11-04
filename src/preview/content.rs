//! What a preview shows: the previewed item, the loaded payload and the background loader.
//!
//! Loading happens on a worker thread; the result is handed back through `take` after the
//! preview window receives `WM_PREVIEW_LOADED`. Only the latest request's result is kept.

use crate::model::{Kind, Node};
use crate::util::{format_count, format_size};
use crate::win::{hwnd_to_isize, isize_to_hwnd, post, WM_PREVIEW_LOADED};
use std::io::Read;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use windows::core::{Interface, PCWSTR};
use windows::Win32::Foundation::{HWND, SIZE};
use windows::Win32::Globalization::{MultiByteToWideChar, CP_ACP, MULTI_BYTE_TO_WIDE_CHAR_FLAGS};
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::Storage::FileSystem::GetDiskFreeSpaceExW;
use windows::Win32::System::Com::{CoInitializeEx, CoUninitialize, COINIT_APARTMENTTHREADED};
use windows::Win32::UI::Shell::{IShellItemImageFactory, SIIGBF_RESIZETOFIT};

/// Bytes of a text file that are shown.
pub const TEXT_LIMIT: u64 = 1024 * 1024;
const MAX_LINES: usize = 100_000;
const TAB: usize = 4;
/// Largest image read out of an archive into memory.
const ZIP_IMAGE_LIMIT: u64 = 64 * 1024 * 1024;
/// Images are decoded at most this large on their long side.
const MAX_IMAGE_SIDE: u32 = 2560;
const THUMB: i32 = 256;
const COUNT_LIMIT: usize = 10_000;

/// The item being previewed (a snapshot of its tree node).
#[derive(Clone, Debug)]
pub struct Target {
    pub path: String,
    pub name: String,
    pub kind: Kind,
    pub zip: Option<(Arc<str>, String)>,
    pub size: u64,
    pub modified: u64,
    pub type_name: Arc<str>,
    pub icon: i32,
}

impl Target {
    pub fn from_node(n: &Node) -> Target {
        Target {
            path: n.path.clone(),
            name: n.name.clone(),
            kind: n.kind,
            zip: n.zip.clone(),
            size: n.size,
            modified: n.modified,
            type_name: n.type_name.clone(),
            icon: n.icon,
        }
    }

    /// The same item in the same state (a changed file is loaded again).
    pub fn same(&self, o: &Target) -> bool {
        self.path == o.path && self.zip == o.zip && self.kind == o.kind && self.size == o.size && self.modified == o.modified
    }

    /// Can be opened with its default app.
    pub fn is_file(&self) -> bool {
        matches!(self.kind, Kind::File | Kind::ZipFile | Kind::Zip)
    }
}

/// Top-down, premultiplied 32-bit BGRA pixels.
pub struct Pixels {
    pub w: u32,
    pub h: u32,
    pub data: Vec<u8>,
}

pub enum Payload {
    Text { lines: Vec<Vec<u16>>, truncated: bool, total: u64 },
    Image { pixels: Pixels, orig_w: u32, orig_h: u32 },
    Info { thumb: Option<Pixels>, extra: Vec<(String, String)>, note: Option<String> },
}

// ---------- Classification ----------

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Class {
    Text,
    Image,
    /// Known not to be previewable as text or image.
    Binary,
    /// Decided by looking at the content.
    Unknown,
}

const IMAGE_EXT: &[&str] = &[
    "png", "jpg", "jpeg", "jpe", "jfif", "gif", "bmp", "dib", "ico", "tif", "tiff", "webp", "heic", "heif", "avif", "jxr", "wdp", "dds",
];

const TEXT_EXT: &[&str] = &[
    "txt", "text", "md", "markdown", "mdx", "rst", "adoc", "log", "csv", "tsv", "json", "jsonc", "json5", "jsonl", "ndjson", "xml", "xaml", "xsd",
    "xsl", "xslt", "svg", "yaml", "yml", "toml", "ini", "cfg", "conf", "config", "properties", "env", "gitignore", "gitattributes",
    "gitmodules", "editorconfig", "npmrc", "rs", "c", "h", "cpp", "cc", "cxx", "hpp", "hh", "hxx", "inl", "ipp", "idl", "cs", "csx", "vb",
    "fs", "fsi", "fsx", "java", "kt", "kts", "scala", "groovy", "go", "py", "pyw", "pyi", "rb", "pl", "pm", "php", "js", "mjs", "cjs",
    "jsx", "ts", "mts", "cts", "tsx", "vue", "svelte", "astro", "html", "htm", "xhtml", "css", "scss", "sass", "less", "sql", "sh", "bash",
    "zsh", "fish", "ps1", "psm1", "psd1", "bat", "cmd", "lua", "r", "swift", "m", "mm", "dart", "ex", "exs", "erl", "hrl", "hs", "clj",
    "cljs", "lisp", "el", "vim", "tex", "bib", "diff", "patch", "csproj", "vbproj", "fsproj", "vcxproj", "props", "targets", "sln",
    "filters", "manifest", "appxmanifest", "inf", "reg", "nuspec", "resx", "resw", "rc", "def", "mak", "mk", "cmake", "gradle", "lock",
    "srt", "vtt", "asm", "s", "nfo", "plist", "proto", "graphql", "gql", "tf", "hcl", "zig", "nim", "jl", "v", "sv", "vhd", "vhdl",
    "razor", "cshtml", "ejs", "hbs", "mustache", "pug", "dockerfile", "makefile", "gyp", "gn", "bzl", "ipynb",
];

const BINARY_EXT: &[&str] = &[
    "exe", "dll", "sys", "drv", "ocx", "cpl", "scr", "efi", "pdb", "obj", "o", "lib", "a", "so", "dylib", "exp", "ilk", "pch", "ipch",
    "zip", "7z", "rar", "gz", "tgz", "bz2", "xz", "zst", "tar", "cab", "msi", "msix", "appx", "nupkg", "jar", "war", "apk", "iso",
    "vhdx", "img", "wim", "pdf", "doc", "docx", "xls", "xlsx", "ppt", "pptx", "odt", "ods", "odp", "rtf", "epub", "mp3", "mp4", "m4a",
    "m4v", "mkv", "avi", "mov", "wmv", "wav", "flac", "ogg", "opus", "aac", "webm", "ttf", "otf", "woff", "woff2", "eot", "class", "pyc",
    "db", "sqlite", "mdb", "accdb", "bin", "dat", "dmp", "etl", "evtx", "lnk", "url", "psd", "ai", "blend", "fbx", "winmd", "node",
];

/// The lowercase extension, or the whole name when there is none (`Makefile`).
fn extension(name: &str) -> String {
    match name.rsplit_once('.') {
        Some((_, e)) => e.to_ascii_lowercase(),
        None => name.to_ascii_lowercase(),
    }
}

pub fn classify(name: &str) -> Class {
    let ext = extension(name);
    let ext = ext.as_str();
    if IMAGE_EXT.contains(&ext) {
        Class::Image
    } else if TEXT_EXT.contains(&ext) {
        Class::Text
    } else if BINARY_EXT.contains(&ext) {
        Class::Binary
    } else {
        Class::Unknown
    }
}

/// Guesses whether the start of a file is text: a BOM, or no NULs and few control characters.
pub fn looks_like_text(head: &[u8]) -> bool {
    if head.starts_with(&[0xEF, 0xBB, 0xBF]) || head.starts_with(&[0xFF, 0xFE]) || head.starts_with(&[0xFE, 0xFF]) {
        return true;
    }
    if head.contains(&0) {
        return false;
    }
    let controls = head.iter().filter(|&&b| b < 0x20 && !matches!(b, b'\t' | b'\n' | b'\r' | 0x0C | 0x1B)).count();
    controls * 100 <= head.len()
}

// ---------- Text ----------

/// Decodes text as UTF-8 / UTF-16 (by BOM or shape) or the ANSI code page. None for binary data.
pub fn decode_text(b: &[u8]) -> Option<Vec<u16>> {
    if let Some(rest) = b.strip_prefix(&[0xEF, 0xBB, 0xBF]) {
        return Some(utf8_prefix(rest).encode_utf16().collect());
    }
    if let Some(rest) = b.strip_prefix(&[0xFF, 0xFE]) {
        return Some(rest.chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect());
    }
    if let Some(rest) = b.strip_prefix(&[0xFE, 0xFF]) {
        return Some(rest.chunks_exact(2).map(|c| u16::from_be_bytes([c[0], c[1]])).collect());
    }
    if b.contains(&0) {
        // UTF-16 LE without a BOM: zero high bytes on (mostly) ASCII text.
        let pairs = b.len() / 2;
        let odd_zero = b.iter().skip(1).step_by(2).filter(|&&x| x == 0).count();
        let even_zero = b.iter().step_by(2).filter(|&&x| x == 0).count();
        if pairs > 0 && odd_zero * 2 >= pairs && even_zero * 50 <= pairs {
            return Some(b.chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect());
        }
        return None;
    }
    match std::str::from_utf8(b) {
        Ok(s) => Some(s.encode_utf16().collect()),
        // Cut off mid-character by the read limit.
        Err(e) if e.error_len().is_none() => Some(utf8_prefix(b).encode_utf16().collect()),
        Err(_) => Some(ansi(b)),
    }
}

fn utf8_prefix(b: &[u8]) -> std::borrow::Cow<'_, str> {
    match std::str::from_utf8(b) {
        Ok(s) => s.into(),
        Err(e) if e.error_len().is_none() => std::str::from_utf8(&b[..e.valid_up_to()]).unwrap_or_default().into(),
        Err(_) => String::from_utf8_lossy(b),
    }
}

fn ansi(b: &[u8]) -> Vec<u16> {
    unsafe {
        let n = MultiByteToWideChar(CP_ACP, MULTI_BYTE_TO_WIDE_CHAR_FLAGS(0), b, None);
        let mut out = vec![0u16; n.max(0) as usize];
        let n = MultiByteToWideChar(CP_ACP, MULTI_BYTE_TO_WIDE_CHAR_FLAGS(0), b, Some(&mut out));
        out.truncate(n.max(0) as usize);
        out
    }
}

/// Splits text into display lines: any line break style, tabs expanded, control characters
/// blanked. Returns (lines, cut short).
pub fn prepare_lines(text: &[u16]) -> (Vec<Vec<u16>>, bool) {
    const CR: u16 = b'\r' as u16;
    const LF: u16 = b'\n' as u16;
    let mut lines = Vec::new();
    let mut cur: Vec<u16> = Vec::new();
    let mut i = 0;
    while i < text.len() {
        let c = text[i];
        i += 1;
        if c == CR || c == LF {
            if c == CR && text.get(i) == Some(&LF) {
                i += 1;
            }
            lines.push(std::mem::take(&mut cur));
            if lines.len() >= MAX_LINES {
                return (lines, i < text.len());
            }
            continue;
        }
        if c == b'\t' as u16 {
            let n = TAB - cur.len() % TAB;
            cur.extend(std::iter::repeat(b' ' as u16).take(n));
        } else if c < 0x20 || c == 0x7F || c == 0xFEFF {
            cur.push(b' ' as u16);
        } else {
            cur.push(c);
        }
    }
    if !cur.is_empty() || lines.is_empty() {
        lines.push(cur);
    }
    (lines, false)
}

// ---------- Loading ----------

static GEN: AtomicU64 = AtomicU64::new(0);
static RESULT: Mutex<Option<(u64, Payload)>> = Mutex::new(None);

/// Starts loading `t` in the background; `hwnd` gets `WM_PREVIEW_LOADED` with the returned generation.
pub fn request(t: Target, hwnd: HWND) -> u64 {
    let gen = GEN.fetch_add(1, Ordering::SeqCst) + 1;
    let h = hwnd_to_isize(hwnd);
    std::thread::spawn(move || {
        unsafe {
            let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
        }
        let payload = load(&t);
        unsafe {
            CoUninitialize();
        }
        let mut r = RESULT.lock().unwrap_or_else(|e| e.into_inner());
        if GEN.load(Ordering::SeqCst) == gen {
            *r = Some((gen, payload));
            drop(r);
            post(isize_to_hwnd(h), WM_PREVIEW_LOADED, gen as usize, 0);
        }
    });
    gen
}

/// Drops any pending result (the preview closed).
pub fn cancel() {
    GEN.fetch_add(1, Ordering::SeqCst);
    *RESULT.lock().unwrap_or_else(|e| e.into_inner()) = None;
}

/// The loaded payload of request `gen`, if it is the latest.
pub fn take(gen: u64) -> Option<Payload> {
    let mut r = RESULT.lock().unwrap_or_else(|e| e.into_inner());
    if r.as_ref().is_some_and(|x| x.0 == gen) {
        r.take().map(|x| x.1)
    } else {
        None
    }
}

fn load(t: &Target) -> Payload {
    match t.kind {
        Kind::Drive => drive_info(t),
        Kind::Dir => folder_info(t),
        Kind::ZipDir => Payload::Info { thumb: None, extra: Vec::new(), note: None },
        Kind::Zip => zip_info(t),
        Kind::File | Kind::ZipFile => {
            let class = classify(&t.name);
            let loaded = match class {
                Class::Image => load_image(t).map_err(|e| format!("This image can't be shown: {e}")),
                Class::Text | Class::Unknown => load_text(t, class == Class::Text),
                Class::Binary => Err(String::new()),
            };
            match loaded {
                Ok(p) => p,
                Err(note) => Payload::Info { thumb: thumbnail(t), extra: Vec::new(), note: (!note.is_empty()).then_some(note) },
            }
        }
    }
}

/// Reads up to `limit` bytes; returns them and the full size.
fn read_head(t: &Target, limit: u64) -> Result<(Vec<u8>, u64), String> {
    match &t.zip {
        Some((archive, inner)) if t.kind.is_virtual() => crate::zipfs::read_entry(archive, inner, limit),
        _ => {
            let f = std::fs::File::open(&t.path).map_err(|e| e.to_string())?;
            let total = f.metadata().map(|m| m.len()).unwrap_or(t.size);
            let mut buf = Vec::with_capacity(total.min(limit) as usize);
            f.take(limit).read_to_end(&mut buf).map_err(|e| e.to_string())?;
            Ok((buf, total))
        }
    }
}

/// Err("") means "not text" (show the info card without a note).
fn load_text(t: &Target, by_extension: bool) -> Result<Payload, String> {
    let (bytes, total) = read_head(t, TEXT_LIMIT)?;
    if !by_extension && !looks_like_text(&bytes[..bytes.len().min(4096)]) {
        return Err(String::new());
    }
    let text = decode_text(&bytes).ok_or_else(String::new)?;
    let (lines, cut) = prepare_lines(&text);
    Ok(Payload::Text { lines, truncated: cut || total > bytes.len() as u64, total })
}

fn load_image(t: &Target) -> Result<Payload, String> {
    let (pixels, orig_w, orig_h) = if t.kind.is_virtual() {
        if t.size > ZIP_IMAGE_LIMIT {
            return Err("it is too large to read from the archive".into());
        }
        let (bytes, _) = read_head(t, ZIP_IMAGE_LIMIT)?;
        super::image::decode_bytes(&bytes, MAX_IMAGE_SIDE)?
    } else {
        super::image::decode_file(&t.path, MAX_IMAGE_SIDE)?
    };
    Ok(Payload::Image { pixels, orig_w, orig_h })
}

fn folder_info(t: &Target) -> Payload {
    let (mut dirs, mut files, mut more) = (0usize, 0usize, false);
    match std::fs::read_dir(&t.path) {
        Ok(rd) => {
            for e in rd.flatten() {
                if dirs + files >= COUNT_LIMIT {
                    more = true;
                    break;
                }
                if e.file_type().is_ok_and(|ft| ft.is_dir()) {
                    dirs += 1;
                } else {
                    files += 1;
                }
            }
        }
        Err(e) => return Payload::Info { thumb: thumbnail(t), extra: Vec::new(), note: Some(e.to_string()) },
    }
    let contains = if dirs + files == 0 {
        "Nothing".to_string()
    } else {
        let plus = if more { "+" } else { "" };
        let mut parts = Vec::new();
        if dirs > 0 {
            parts.push(format!("{}{plus} {}", format_count(dirs), if dirs == 1 { "folder" } else { "folders" }));
        }
        if files > 0 {
            parts.push(format!("{}{plus} {}", format_count(files), if files == 1 { "file" } else { "files" }));
        }
        parts.join(", ")
    };
    Payload::Info { thumb: thumbnail(t), extra: vec![("Contains".into(), contains)], note: None }
}

fn drive_info(t: &Target) -> Payload {
    if crate::util::is_server_path(&t.path) {
        return Payload::Info { thumb: thumbnail(t), extra: Vec::new(), note: None };
    }
    // UNC share roots need a trailing backslash.
    let w = crate::util::wide(&format!("{}\\", t.path.trim_end_matches('\\')));
    let (mut free, mut total) = (0u64, 0u64);
    let ok = unsafe { GetDiskFreeSpaceExW(PCWSTR(w.as_ptr()), Some(&mut free), Some(&mut total), None) }.is_ok();
    let extra = if ok {
        vec![("Free space".into(), format!("{} of {}", format_size(free), format_size(total)))]
    } else {
        Vec::new()
    };
    Payload::Info { thumb: thumbnail(t), extra, note: None }
}

fn zip_info(t: &Target) -> Payload {
    let entries = std::fs::File::open(&t.path).ok().and_then(|f| zip::ZipArchive::new(std::io::BufReader::new(f)).ok()).map(|a| a.len());
    let extra = entries.map(|n| vec![("Contains".to_string(), format!("{} {}", format_count(n), if n == 1 { "entry" } else { "entries" }))]);
    Payload::Info { thumb: thumbnail(t), extra: extra.unwrap_or_default(), note: None }
}

/// The shell's thumbnail (or large icon) for a real file-system item.
fn thumbnail(t: &Target) -> Option<Pixels> {
    if t.kind.is_virtual() {
        return None;
    }
    let item = crate::shell::shell_item(&t.path)?;
    unsafe {
        let f: IShellItemImageFactory = item.cast().ok()?;
        let hbmp = f.GetImage(SIZE { cx: THUMB, cy: THUMB }, SIIGBF_RESIZETOFIT).ok()?;
        let px = hbitmap_pixels(hbmp);
        let _ = DeleteObject(hbmp);
        px
    }
}

unsafe fn hbitmap_pixels(hbmp: HBITMAP) -> Option<Pixels> {
    let mut bm = BITMAP::default();
    if GetObjectW(hbmp, std::mem::size_of::<BITMAP>() as i32, Some(&mut bm as *mut _ as *mut _)) == 0 {
        return None;
    }
    let (w, h) = (bm.bmWidth, bm.bmHeight.abs());
    if w <= 0 || h <= 0 {
        return None;
    }
    let mut bi = BITMAPINFO {
        bmiHeader: BITMAPINFOHEADER {
            biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
            biWidth: w,
            biHeight: -h,
            biPlanes: 1,
            biBitCount: 32,
            biCompression: BI_RGB.0,
            ..Default::default()
        },
        ..Default::default()
    };
    let mut data = vec![0u8; (w * h * 4) as usize];
    let hdc = GetDC(None);
    let n = GetDIBits(hdc, hbmp, 0, h as u32, Some(data.as_mut_ptr() as *mut _), &mut bi, DIB_RGB_COLORS);
    ReleaseDC(None, hdc);
    if n == 0 {
        return None;
    }
    fix_alpha(&mut data);
    Some(Pixels { w: w as u32, h: h as u32, data })
}

/// Shell bitmaps come opaque with zero alpha, or with straight (not premultiplied) alpha.
fn fix_alpha(data: &mut [u8]) {
    if data.chunks_exact(4).all(|p| p[3] == 0) {
        data.chunks_exact_mut(4).for_each(|p| p[3] = 255);
    } else if data.chunks_exact(4).any(|p| p[0] > p[3] || p[1] > p[3] || p[2] > p[3]) {
        for p in data.chunks_exact_mut(4) {
            let a = p[3] as u32;
            for c in &mut p[..3] {
                *c = ((*c as u32 * a + 127) / 255) as u8;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn w(s: &str) -> Vec<u16> {
        s.encode_utf16().collect()
    }

    #[test]
    fn classifies_by_extension() {
        assert_eq!(classify("a.RS"), Class::Text);
        assert_eq!(classify(".gitignore"), Class::Text);
        assert_eq!(classify("Makefile"), Class::Text);
        assert_eq!(classify("photo.JPG"), Class::Image);
        assert_eq!(classify("setup.exe"), Class::Binary);
        assert_eq!(classify("LICENSE"), Class::Unknown);
    }

    #[test]
    fn sniffs_text() {
        assert!(looks_like_text(b"hello\r\nworld\t!"));
        assert!(looks_like_text(&[0xFF, 0xFE, b'a', 0]));
        assert!(!looks_like_text(b"MZ\x90\0\x03"));
        assert!(!looks_like_text(&[1, 2, 3, 4, 5, b'a']));
    }

    #[test]
    fn decodes_encodings() {
        assert_eq!(decode_text(b"\xEF\xBB\xBFh\xC3\xA9"), Some(w("hé")));
        assert_eq!(decode_text(&[0xFF, 0xFE, b'h', 0, b'i', 0]), Some(w("hi")));
        assert_eq!(decode_text(&[0xFE, 0xFF, 0, b'h', 0, b'i']), Some(w("hi")));
        assert_eq!(decode_text(&[b'h', 0, b'i', 0]), Some(w("hi")));
        // A multi-byte character cut by the read limit is dropped.
        assert_eq!(decode_text(b"ab\xC3"), Some(w("ab")));
        assert_eq!(decode_text(b"MZ\x90\0\x03\0\0\0\x04\0"), None);
        assert_eq!(decode_text(b"plain"), Some(w("plain")));
    }

    #[test]
    fn prepares_lines() {
        let (lines, cut) = prepare_lines(&w("a\tb\r\nxy\rz\n\tq\x07\n"));
        let s: Vec<String> = lines.iter().map(|l| String::from_utf16_lossy(l)).collect();
        assert_eq!(s, ["a   b", "xy", "z", "    q "]);
        assert!(!cut);
        assert_eq!(prepare_lines(&[]).0.len(), 1);
    }

    #[test]
    fn fixes_shell_alpha() {
        let mut opaque = vec![10, 20, 30, 0];
        fix_alpha(&mut opaque);
        assert_eq!(opaque, [10, 20, 30, 255]);
        let mut straight = vec![200, 100, 0, 128, 0, 0, 0, 0];
        fix_alpha(&mut straight);
        assert_eq!(straight, [100, 50, 0, 128, 0, 0, 0, 0]);
    }
}
