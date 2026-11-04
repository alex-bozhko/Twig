//! Extracting zip entries to temporary folders (for open / copy / drag-out).

use std::io::{Read, Write};
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{Duration, SystemTime};

fn temp_root() -> PathBuf {
    std::env::temp_dir().join("Twig")
}

/// Removes extraction folders older than a day (best effort).
pub fn cleanup_old() {
    let Ok(rd) = std::fs::read_dir(temp_root()) else { return };
    let cutoff = SystemTime::now() - Duration::from_secs(24 * 3600);
    for e in rd.flatten() {
        let old = e.metadata().and_then(|m| m.modified()).map(|t| t < cutoff).unwrap_or(false);
        if old {
            let _ = std::fs::remove_dir_all(e.path());
        }
    }
}

/// A fresh, unique folder so repeated extractions never collide or overwrite.
fn unique_dir() -> std::io::Result<PathBuf> {
    static COUNTER: AtomicU32 = AtomicU32::new(0);
    let ticks = SystemTime::now().duration_since(SystemTime::UNIX_EPOCH).map(|d| d.as_millis()).unwrap_or(0);
    let dir = temp_root().join(format!("{}-{}-{}", std::process::id(), COUNTER.fetch_add(1, Ordering::SeqCst), ticks));
    std::fs::create_dir_all(&dir)?;
    Ok(dir)
}

/// Converts a '/'-separated relative path to a safe relative PathBuf (rejects '..', roots, drive prefixes).
fn safe_relative(rel: &str) -> Option<PathBuf> {
    let p = Path::new(rel);
    let mut out = PathBuf::new();
    for c in p.components() {
        match c {
            Component::Normal(s) => out.push(s),
            Component::CurDir => {}
            _ => return None,
        }
    }
    if out.as_os_str().is_empty() {
        None
    } else {
        Some(out)
    }
}

/// Extracts the given inner paths (files, or folders ending with '/') into a new temp folder.
/// Returns the extracted top-level paths in the same order.
pub fn extract(archive: &str, inner: &[String]) -> Result<Vec<PathBuf>, String> {
    let f = std::fs::File::open(archive).map_err(|e| e.to_string())?;
    let mut ar = zip::ZipArchive::new(std::io::BufReader::new(f)).map_err(|e| e.to_string())?;
    let dest = unique_dir().map_err(|e| e.to_string())?;
    let names: Vec<String> = ar.file_names().map(|s| s.to_string()).collect();
    let mut result = Vec::new();
    for want in inner {
        let is_dir = want.ends_with('/');
        let trimmed = want.trim_end_matches('/');
        let base = trimmed.rsplit_once('/').map(|(p, _)| format!("{p}/")).unwrap_or_default();
        let leaf = trimmed.rsplit('/').next().unwrap_or(trimmed);
        let top = dest.join(safe_relative(leaf).ok_or("Invalid entry name")?);
        if is_dir {
            std::fs::create_dir_all(&top).map_err(|e| e.to_string())?;
        }
        let lower_want = want.to_lowercase();
        for (i, name) in names.iter().enumerate() {
            let norm = name.replace('\\', "/");
            let matches = if is_dir { norm.to_lowercase().starts_with(&lower_want) } else { norm.eq_ignore_ascii_case(want) };
            if !matches {
                continue;
            }
            let Some(rel) = safe_relative(&norm[base.len().min(norm.len())..]) else { continue };
            let out = dest.join(rel);
            if !out.starts_with(&dest) {
                continue;
            }
            let mut zf = ar.by_index(i).map_err(|e| e.to_string())?;
            if zf.is_dir() {
                std::fs::create_dir_all(&out).map_err(|e| e.to_string())?;
                continue;
            }
            if let Some(p) = out.parent() {
                std::fs::create_dir_all(p).map_err(|e| e.to_string())?;
            }
            let mut file = std::fs::File::create(&out).map_err(|e| e.to_string())?;
            let mut buf = vec![0u8; 64 * 1024];
            loop {
                let n = zf.read(&mut buf).map_err(|e| e.to_string())?;
                if n == 0 {
                    break;
                }
                file.write_all(&buf[..n]).map_err(|e| e.to_string())?;
            }
        }
        if !top.exists() {
            return Err(format!("'{leaf}' was not found in the archive"));
        }
        result.push(top);
    }
    Ok(result)
}

/// Reads up to `limit` bytes of the file entry `inner` (for previews). Returns the bytes and the entry's full size.
pub fn read_entry(archive: &str, inner: &str, limit: u64) -> Result<(Vec<u8>, u64), String> {
    let f = std::fs::File::open(archive).map_err(|e| e.to_string())?;
    let mut ar = zip::ZipArchive::new(std::io::BufReader::new(f)).map_err(|e| e.to_string())?;
    // `file_names` follows the archive's index order.
    let names: Vec<String> = ar.file_names().map(|s| s.replace('\\', "/")).collect();
    let i = names.iter().position(|n| n.eq_ignore_ascii_case(inner)).ok_or("The entry was not found in the archive")?;
    let zf = ar.by_index(i).map_err(|e| e.to_string())?;
    let size = zf.size();
    let mut buf = Vec::with_capacity(size.min(limit) as usize);
    zf.take(limit).read_to_end(&mut buf).map_err(|e| e.to_string())?;
    Ok((buf, size))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_escaping_paths() {
        assert!(safe_relative("../x").is_none());
        assert!(safe_relative("a/../../x").is_none());
        assert!(safe_relative("C:/x").is_none());
        assert!(safe_relative("/x").is_none());
        assert_eq!(safe_relative("a/b.txt"), Some(PathBuf::from("a\\b.txt")));
    }

    #[test]
    fn extracts_file_and_folder() {
        let tmp = std::env::temp_dir().join(format!("twig-test-{}.zip", std::process::id()));
        {
            let f = std::fs::File::create(&tmp).unwrap();
            let mut w = zip::ZipWriter::new(f);
            let o = zip::write::SimpleFileOptions::default();
            w.add_directory("dir/", o).unwrap();
            w.start_file("dir/a.txt", o).unwrap();
            w.write_all(b"hello").unwrap();
            w.start_file("dir/sub/b.txt", o).unwrap();
            w.write_all(b"bye").unwrap();
            w.finish().unwrap();
        }
        let a = tmp.to_string_lossy().to_string();
        let r = extract(&a, &["dir/a.txt".into()]).unwrap();
        assert_eq!(std::fs::read_to_string(&r[0]).unwrap(), "hello");
        let r2 = extract(&a, &["dir/".into()]).unwrap();
        assert_ne!(r[0].parent(), r2[0].parent());
        assert_eq!(std::fs::read_to_string(r2[0].join("sub").join("b.txt")).unwrap(), "bye");
        assert_eq!(read_entry(&a, "DIR/a.txt", 3).unwrap(), (b"hel".to_vec(), 5));
        assert!(read_entry(&a, "dir/missing.txt", 3).is_err());
        let _ = std::fs::remove_file(&tmp);
        let _ = std::fs::remove_dir_all(r[0].parent().unwrap());
        let _ = std::fs::remove_dir_all(r2[0].parent().unwrap());
    }
}
