//! Pure helpers: strings, paths, natural sort, size formatting.

use std::cmp::Ordering;

/// Null-terminated UTF-16 copy of `s`.
pub fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// UTF-16 buffer (possibly null-terminated) to String.
pub fn from_wide(buf: &[u16]) -> String {
    let end = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
    String::from_utf16_lossy(&buf[..end])
}

/// Natural ("Explorer-like") case-insensitive comparison: digit runs compare numerically.
pub fn natural_cmp(a: &str, b: &str) -> Ordering {
    let mut ai = a.chars().peekable();
    let mut bi = b.chars().peekable();
    loop {
        match (ai.peek().copied(), bi.peek().copied()) {
            (None, None) => break,
            (None, Some(_)) => return Ordering::Less,
            (Some(_), None) => return Ordering::Greater,
            (Some(ca), Some(cb)) => {
                if ca.is_ascii_digit() && cb.is_ascii_digit() {
                    let mut da = String::new();
                    while let Some(&c) = ai.peek() {
                        if !c.is_ascii_digit() {
                            break;
                        }
                        da.push(c);
                        ai.next();
                    }
                    let mut db = String::new();
                    while let Some(&c) = bi.peek() {
                        if !c.is_ascii_digit() {
                            break;
                        }
                        db.push(c);
                        bi.next();
                    }
                    let ta = da.trim_start_matches('0');
                    let tb = db.trim_start_matches('0');
                    let ord = ta.len().cmp(&tb.len()).then_with(|| ta.cmp(tb));
                    if ord != Ordering::Equal {
                        return ord;
                    }
                } else {
                    let la = ca.to_lowercase();
                    let lb = cb.to_lowercase();
                    let ord = la.cmp(lb);
                    if ord != Ordering::Equal {
                        return ord;
                    }
                    ai.next();
                    bi.next();
                }
            }
        }
    }
    // Equal ignoring case and leading zeros: fall back to a stable ordinal comparison.
    a.cmp(b)
}

/// Human-readable size: "0 bytes", "1 KB", "1.5 MB" ... up to PB.
pub fn format_size(bytes: u64) -> String {
    const UNITS: [&str; 6] = ["bytes", "KB", "MB", "GB", "TB", "PB"];
    if bytes < 1024 {
        return format!("{bytes} bytes");
    }
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    if value >= 100.0 {
        format!("{:.0} {}", value, UNITS[unit])
    } else if value >= 10.0 {
        format!("{:.1} {}", value, UNITS[unit])
    } else {
        format!("{:.2} {}", value, UNITS[unit])
    }
}

/// Count with thousands separators: 250000 -> "250,000".
pub fn format_count(n: usize) -> String {
    let s = n.to_string();
    let mut out = String::with_capacity(s.len() + s.len() / 3);
    for (i, c) in s.chars().enumerate() {
        if i > 0 && (s.len() - i) % 3 == 0 {
            out.push(',');
        }
        out.push(c);
    }
    out
}

/// Lower-cased path with trailing separators removed (except for drive roots like `C:\`).
pub fn path_key(path: &str) -> String {
    normalize_path(path).to_lowercase()
}

/// Normalizes separators to `\` and trims trailing separators (keeps `C:\`).
pub fn normalize_path(path: &str) -> String {
    let mut p: String = path.replace('/', "\\");
    while p.len() > 3 && p.ends_with('\\') {
        p.pop();
    }
    if p.len() == 2 && p.ends_with(':') {
        p.push('\\');
    }
    p
}

/// Parent directory of a path, `None` for drive roots / UNC share roots.
pub fn parent_path(path: &str) -> Option<String> {
    let p = normalize_path(path);
    if is_drive_root(&p) {
        return None;
    }
    let idx = p.rfind('\\')?;
    let parent = &p[..idx];
    if parent.is_empty() {
        return None;
    }
    if parent.starts_with("\\\\") {
        // UNC: \\server\share is the minimum.
        let parts = parent.trim_start_matches('\\').split('\\').count();
        if parts < 2 {
            return None;
        }
    }
    Some(normalize_path(parent))
}

pub fn is_drive_root(path: &str) -> bool {
    let b = path.as_bytes();
    (b.len() == 3 && b[1] == b':' && b[2] == b'\\') || (b.len() == 2 && b[1] == b':')
}

/// Last path component (or the whole path for drive roots).
pub fn file_name(path: &str) -> String {
    let p = normalize_path(path);
    if is_drive_root(&p) {
        return p;
    }
    match p.rfind('\\') {
        Some(i) => p[i + 1..].to_string(),
        None => p,
    }
}

/// Breadcrumb segments `(label, path)` from the drive/share root down to `path`.
pub fn crumbs(path: &str) -> Vec<(String, String)> {
    let mut chain = vec![normalize_path(path)];
    while let Some(p) = parent_path(chain.last().unwrap()) {
        chain.push(p);
    }
    chain.reverse();
    chain
        .into_iter()
        .enumerate()
        .map(|(i, p)| {
            let label = if i == 0 { p.trim_end_matches('\\').to_string() } else { file_name(&p) };
            (label, p)
        })
        .collect()
}

pub fn join_path(dir: &str, name: &str) -> String {
    if dir.ends_with('\\') {
        format!("{dir}{name}")
    } else {
        format!("{dir}\\{name}")
    }
}

/// True if `child` equals `ancestor` or lies inside it.
pub fn is_same_or_inside(child: &str, ancestor: &str) -> bool {
    let c = path_key(child);
    let a = path_key(ancestor);
    if c == a {
        return true;
    }
    let prefix = if a.ends_with('\\') { a } else { format!("{a}\\") };
    c.starts_with(&prefix)
}

/// Characters not allowed in file names.
pub fn invalid_name(name: &str) -> Option<&'static str> {
    let trimmed = name.trim();
    if trimmed.is_empty() {
        return Some("A file name can't be empty.");
    }
    if name.chars().any(|c| matches!(c, '\\' | '/' | ':' | '*' | '?' | '"' | '<' | '>' | '|') || (c as u32) < 32) {
        return Some("A file name can't contain any of the following characters: \\ / : * ? \" < > |");
    }
    if trimmed == "." || trimmed == ".." {
        return Some("That name is reserved.");
    }
    None
}

/// Picks `base`, `base (2)`, `base (3)`... with `ext` appended, avoiding names in `exists`.
pub fn unique_name(base: &str, ext: &str, exists: impl Fn(&str) -> bool) -> String {
    let first = format!("{base}{ext}");
    if !exists(&first) {
        return first;
    }
    for n in 2.. {
        let candidate = format!("{base} ({n}){ext}");
        if !exists(&candidate) {
            return candidate;
        }
    }
    unreachable!()
}

/// Position of the dot that starts the extension (none for leading dots like `.gitignore`).
pub fn extension_start(name: &str) -> Option<usize> {
    let idx = name.rfind('.')?;
    if idx == 0 {
        None
    } else {
        Some(idx)
    }
}

/// Lower-cased extension including the dot, or empty.
pub fn extension_lower(name: &str) -> String {
    extension_start(name).map(|i| name[i..].to_lowercase()).unwrap_or_default()
}

/// Case-insensitive substring search. Returns the byte range of the first match in `hay`.
pub fn find_ci(hay: &str, needle: &str) -> Option<(usize, usize)> {
    let needle: Vec<char> = needle.chars().flat_map(char::to_lowercase).collect();
    if needle.is_empty() {
        return None;
    }
    // Lower-cased chars of `hay`, each with the byte range of the original char it came from.
    let low: Vec<(char, usize, usize)> =
        hay.char_indices().flat_map(|(i, c)| c.to_lowercase().map(move |l| (l, i, i + c.len_utf8()))).collect();
    low.windows(needle.len())
        .find(|w| w.iter().zip(&needle).all(|(a, b)| a.0 == *b))
        .map(|w| (w[0].1, w[w.len() - 1].2))
}

/// Case-insensitive substring test that is cheap to repeat over many names.
pub struct Matcher {
    lower: String,
    ascii: bool,
}

impl Matcher {
    pub fn new(needle: &str) -> Matcher {
        let lower: String = needle.chars().flat_map(char::to_lowercase).collect();
        Matcher { ascii: lower.is_ascii(), lower }
    }

    pub fn is_match(&self, hay: &str) -> bool {
        let n = self.lower.as_bytes();
        if n.is_empty() {
            return false;
        }
        if self.ascii && hay.is_ascii() {
            return hay.len() >= n.len() && hay.as_bytes().windows(n.len()).any(|w| w.eq_ignore_ascii_case(n));
        }
        find_ci(hay, &self.lower).is_some()
    }
}

/// Type-to-search: find the index of the next item matching `buffer`.
/// `names` is the search scope, `current` the index of the focused item within it.
/// Repeating a single character cycles through items starting with that character.
pub fn type_search(names: &[&str], current: Option<usize>, buffer: &str) -> Option<usize> {
    if names.is_empty() || buffer.is_empty() {
        return None;
    }
    let lower = buffer.to_lowercase();
    let mut chars = lower.chars();
    let first = chars.next().unwrap();
    let repeated = lower.chars().count() > 1 && lower.chars().all(|c| c == first);
    let n = names.len();
    if repeated || lower.chars().count() == 1 {
        // Cycle: start after the current item.
        let needle = first.to_string();
        let start = current.map(|c| c + 1).unwrap_or(0);
        return (0..n)
            .map(|i| (start + i) % n)
            .find(|&i| names[i].to_lowercase().starts_with(&needle));
    }
    // Multi-char prefix: start at the current item (it may still match).
    let start = current.unwrap_or(0);
    (0..n)
        .map(|i| (start + i) % n)
        .find(|&i| names[i].to_lowercase().starts_with(&lower))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn natural_numbers() {
        let mut v = vec!["file10", "file2", "File1", "file02b", "file"];
        v.sort_by(|a, b| natural_cmp(a, b));
        assert_eq!(v, vec!["file", "File1", "file2", "file02b", "file10"]);
    }

    #[test]
    fn natural_case_insensitive() {
        assert_eq!(natural_cmp("abc", "ABD"), Ordering::Less);
        assert_eq!(natural_cmp("a", "a"), Ordering::Equal);
        assert_ne!(natural_cmp("A", "a"), Ordering::Equal);
    }

    #[test]
    fn natural_huge_numbers() {
        assert_eq!(natural_cmp("x99999999999999999999999", "x100000000000000000000000"), Ordering::Less);
    }

    #[test]
    fn sizes() {
        assert_eq!(format_size(0), "0 bytes");
        assert_eq!(format_size(1023), "1023 bytes");
        assert_eq!(format_size(1024), "1.00 KB");
        assert_eq!(format_size(1536 * 1024), "1.50 MB");
        assert_eq!(format_size(150 * 1024 * 1024 * 1024), "150 GB");
        assert_eq!(format_size(u64::MAX), "16384 PB");
        assert_eq!(format_count(0), "0");
        assert_eq!(format_count(999), "999");
        assert_eq!(format_count(1000), "1,000");
        assert_eq!(format_count(250_000), "250,000");
        assert_eq!(format_count(1_234_567), "1,234,567");
    }

    #[test]
    fn breadcrumbs() {
        let s = |v: Vec<(String, String)>| v.into_iter().map(|(l, p)| format!("{l}={p}")).collect::<Vec<_>>();
        assert_eq!(s(crumbs("C:\\")), ["C:=C:\\"]);
        assert_eq!(s(crumbs("C:\\Users\\bob\\")), ["C:=C:\\", "Users=C:\\Users", "bob=C:\\Users\\bob"]);
        assert_eq!(s(crumbs("\\\\srv\\share\\x")), ["\\\\srv\\share=\\\\srv\\share", "x=\\\\srv\\share\\x"]);
    }

    #[test]
    fn paths() {
        assert_eq!(normalize_path("C:/a/b/"), "C:\\a\\b");
        assert_eq!(normalize_path("C:"), "C:\\");
        assert_eq!(parent_path("C:\\a\\b"), Some("C:\\a".into()));
        assert_eq!(parent_path("C:\\a"), Some("C:\\".into()));
        assert_eq!(parent_path("C:\\"), None);
        assert_eq!(parent_path("\\\\srv\\share\\x"), Some("\\\\srv\\share".into()));
        assert_eq!(parent_path("\\\\srv\\share"), None);
        assert_eq!(file_name("C:\\a\\b.txt"), "b.txt");
        assert_eq!(file_name("D:\\"), "D:\\");
        assert_eq!(join_path("C:\\", "x"), "C:\\x");
        assert_eq!(join_path("C:\\a", "x"), "C:\\a\\x");
    }

    #[test]
    fn inside() {
        assert!(is_same_or_inside("C:\\A\\b", "c:\\a"));
        assert!(is_same_or_inside("C:\\a", "C:\\a\\"));
        assert!(!is_same_or_inside("C:\\ab", "C:\\a"));
        assert!(is_same_or_inside("C:\\x", "C:\\"));
    }

    #[test]
    fn names() {
        assert!(invalid_name("a?b").is_some());
        assert!(invalid_name("  ").is_some());
        assert!(invalid_name("ok.txt").is_none());
        assert_eq!(extension_start(".gitignore"), None);
        assert_eq!(extension_start("a.tar.gz"), Some(5));
        assert_eq!(extension_lower("A.TXT"), ".txt");
        let taken = ["New folder", "New folder (2)"];
        assert_eq!(unique_name("New folder", "", |n| taken.contains(&n)), "New folder (3)");
        assert_eq!(unique_name("New Text Document", ".txt", |_| false), "New Text Document.txt");
    }

    #[test]
    fn type_to_search() {
        let names = ["alpha", "beta", "apple", "Avocado", "banana"];
        assert_eq!(type_search(&names, None, "a"), Some(0));
        assert_eq!(type_search(&names, Some(0), "a"), Some(2));
        assert_eq!(type_search(&names, Some(2), "aa"), Some(3));
        assert_eq!(type_search(&names, Some(3), "aaa"), Some(0));
        assert_eq!(type_search(&names, Some(1), "ap"), Some(2));
        assert_eq!(type_search(&names, Some(2), "app"), Some(2));
        assert_eq!(type_search(&names, Some(0), "be"), Some(1));
        assert_eq!(type_search(&names, Some(0), "ba"), Some(4));
        assert_eq!(type_search(&names, Some(1), "ban"), Some(4));
        assert_eq!(type_search(&names, Some(1), "z"), None);
    }

    #[test]
    fn find_case_insensitive() {
        assert_eq!(find_ci("Report.TXT", "port"), Some((2, 6)));
        assert_eq!(find_ci("Report.TXT", ".txt"), Some((6, 10)));
        assert_eq!(find_ci("Report.TXT", "REP"), Some((0, 3)));
        assert_eq!(find_ci("ÄrgerÖl", "rgerö"), Some((2, 8)));
        assert_eq!(find_ci("abc", "abcd"), None);
        assert_eq!(find_ci("abc", ""), None);
        let m = Matcher::new("PORT");
        assert!(m.is_match("report.txt") && !m.is_match("por") && !Matcher::new("").is_match("a"));
        assert!(Matcher::new("rgerö").is_match("ÄrgerÖl") && Matcher::new("txt").is_match("Ärger.TXT"));
    }
}
