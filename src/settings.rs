//! Persistent settings (%LOCALAPPDATA%\Twig\settings.json) with a one-time import from the old .NET app.

use crate::model::SortKey;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

pub const DEFAULT_COLUMNS: [i32; 4] = [320, 170, 140, 110];
pub const MIN_COLUMN: i32 = 50;
/// Bits (1 << column) of the columns that can be hidden: Date modified, Type, Size.
pub const HIDEABLE_COLUMNS: u8 = 0b1110;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Favorite {
    pub path: String,
    #[serde(default)]
    pub sort: SortKey,
    #[serde(default)]
    pub desc: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct TabEntry {
    /// `None` = This PC.
    pub root: Option<String>,
    pub expanded: Vec<String>,
    pub selected: Option<String>,
    pub scroll: i32,
    pub sort: SortKey,
    pub desc: bool,
    /// Hidden columns (bits 1 << column); None = `Settings::hidden_columns`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hidden_columns: Option<u8>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum LayoutEntry {
    Leaf {
        tabs: Vec<TabEntry>,
        #[serde(default)]
        active: usize,
        #[serde(default)]
        is_active: bool,
        /// The pane's column widths (96-DPI px); None = `Settings::columns`.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        columns: Option<[i32; 4]>,
    },
    Split { horizontal: bool, weights: Vec<f64>, children: Vec<LayoutEntry> },
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct WindowPos {
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
    pub maximized: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub window: Option<WindowPos>,
    /// Column widths in 96-DPI pixels: Name, Modified, Type, Size.
    pub columns: [i32; 4],
    /// Columns hidden in new tabs (bits 1 << column).
    pub hidden_columns: u8,
    pub favorites: Vec<Favorite>,
    pub layout: Option<LayoutEntry>,
}

impl Default for Settings {
    fn default() -> Self {
        Settings { window: None, columns: DEFAULT_COLUMNS, hidden_columns: 0, favorites: Vec::new(), layout: None }
    }
}

pub fn clamp_columns(c: [i32; 4]) -> [i32; 4] {
    c.map(|w| w.clamp(MIN_COLUMN, 4000))
}

/// Column widths with hidden columns (bits 1 << column) zeroed; Name always shows.
pub fn visible_columns(widths: [i32; 4], hidden: u8) -> [i32; 4] {
    let hidden = hidden & HIDEABLE_COLUMNS;
    std::array::from_fn(|c| if hidden & (1 << c) != 0 { 0 } else { widths[c] })
}

fn local_app_data() -> Option<PathBuf> {
    std::env::var_os("LOCALAPPDATA").map(PathBuf::from)
}

pub fn settings_path() -> Option<PathBuf> {
    local_app_data().map(|p| p.join("Twig").join("settings.json"))
}

impl Settings {
    pub fn load() -> Settings {
        if let Some(path) = settings_path() {
            if let Ok(text) = std::fs::read_to_string(&path) {
                if let Ok(s) = serde_json::from_str::<Settings>(&text) {
                    return s.sanitized();
                }
            }
        }
        // First run: import from the old FileBrowser app if present.
        local_app_data()
            .and_then(|p| std::fs::read_to_string(p.join("FileBrowser").join("settings.json")).ok())
            .map(|t| import_legacy(&t))
            .unwrap_or_default()
    }

    fn sanitized(mut self) -> Settings {
        self.columns = clamp_columns(self.columns);
        self.hidden_columns &= HIDEABLE_COLUMNS;
        self
    }

    pub fn save(&self) -> std::io::Result<()> {
        let Some(path) = settings_path() else { return Ok(()) };
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, serde_json::to_string_pretty(self)?)?;
        std::fs::rename(&tmp, &path)
    }
}

/// Imports favorites and column widths from the old .NET app's settings JSON.
pub fn import_legacy(text: &str) -> Settings {
    let mut s = Settings::default();
    let Ok(v) = serde_json::from_str::<serde_json::Value>(text) else { return s };
    if let Some(list) = v.get("FavoriteEntries").and_then(|f| f.as_array()) {
        for f in list {
            let Some(path) = f.get("Path").and_then(|p| p.as_str()).filter(|p| !p.is_empty()) else { continue };
            s.favorites.push(Favorite {
                path: path.to_string(),
                sort: f.get("SortKey").and_then(|k| k.as_str()).map(SortKey::parse_loose).unwrap_or_default(),
                desc: f.get("SortDescending").and_then(|d| d.as_bool()).unwrap_or(false),
            });
        }
    } else if let Some(list) = v.get("Favorites").and_then(|f| f.as_array()) {
        for p in list.iter().filter_map(|p| p.as_str()).filter(|p| !p.is_empty()) {
            s.favorites.push(Favorite { path: p.to_string(), sort: SortKey::Name, desc: false });
        }
    }
    for (i, key) in ["NameWidth", "ModifiedWidth", "TypeWidth", "SizeWidth"].iter().enumerate() {
        if let Some(w) = v.get(*key).and_then(|w| w.as_f64()) {
            if w.is_finite() {
                s.columns[i] = (w.round() as i32).clamp(MIN_COLUMN, 4000);
            }
        }
    }
    s.layout = v.get("Root").and_then(legacy_layout).or_else(|| {
        let tabs = v.get("TabEntries")?.as_array()?;
        legacy_leaf(tabs, v.get("ActiveTabIndex"), true)
    });
    s
}

fn str_field<'a>(v: &'a serde_json::Value, key: &str) -> Option<&'a str> {
    v.get(key).and_then(|s| s.as_str()).filter(|s| !s.is_empty())
}

fn legacy_tab(v: &serde_json::Value) -> Option<TabEntry> {
    let flag = |k: &str| v.get(k).and_then(|b| b.as_bool()).unwrap_or(false);
    let path = str_field(v, "Path");
    let root = if flag("IsDrives") {
        None
    } else if flag("AsRoot") {
        Some(str_field(v, "RootPath").or(path)?.to_string())
    } else {
        // Old non-root tabs were a This PC tree expanded down to `Path`.
        path?;
        None
    };
    Some(TabEntry {
        root,
        expanded: v.get("ExpandedPaths").and_then(|e| e.as_array()).map(|a| a.iter().filter_map(|p| p.as_str()).map(String::from).collect()).unwrap_or_default(),
        selected: str_field(v, "SelectedPath").or(path).map(String::from),
        scroll: 0,
        sort: str_field(v, "SortKey").map(SortKey::parse_loose).unwrap_or_default(),
        desc: flag("SortDescending"),
        hidden_columns: None,
    })
}

fn legacy_leaf(tabs: &[serde_json::Value], active: Option<&serde_json::Value>, is_active: bool) -> Option<LayoutEntry> {
    let tabs: Vec<TabEntry> = tabs.iter().filter_map(legacy_tab).collect();
    if tabs.is_empty() {
        return None;
    }
    let active = active.and_then(|a| a.as_u64()).map_or(0, |a| (a as usize).min(tabs.len() - 1));
    Some(LayoutEntry::Leaf { tabs, active, is_active, columns: None })
}

fn legacy_layout(v: &serde_json::Value) -> Option<LayoutEntry> {
    if str_field(v, "Kind")? == "leaf" {
        let is_active = v.get("IsActive").and_then(|b| b.as_bool()).unwrap_or(false);
        return legacy_leaf(v.get("Tabs")?.as_array()?, v.get("ActiveTabIndex"), is_active);
    }
    let sizes = v.get("Sizes").and_then(|s| s.as_array());
    let (children, weights): (Vec<_>, Vec<_>) = v
        .get("Children")?
        .as_array()?
        .iter()
        .enumerate()
        .filter_map(|(i, c)| {
            let w = sizes.and_then(|s| s.get(i)).and_then(|w| w.as_f64()).filter(|w| w.is_finite() && *w > 0.0).unwrap_or(1.0);
            legacy_layout(c).map(|c| (c, w))
        })
        .unzip();
    match children.len() {
        0 => None,
        1 => children.into_iter().next(),
        _ => Some(LayoutEntry::Split { horizontal: str_field(v, "Orientation") != Some("Vertical"), weights, children }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip() {
        let s = Settings {
            window: Some(WindowPos { x: 1, y: 2, w: 3, h: 4, maximized: true }),
            columns: [100, 200, 300, 400],
            hidden_columns: 0b0100,
            favorites: vec![Favorite { path: "C:\\x".into(), sort: SortKey::Size, desc: true }],
            layout: Some(LayoutEntry::Split {
                horizontal: true,
                weights: vec![1.0, 2.0],
                children: vec![
                    LayoutEntry::Leaf { tabs: vec![TabEntry::default()], active: 0, is_active: true, columns: None },
                    LayoutEntry::Leaf {
                        tabs: vec![TabEntry { root: Some("C:\\".into()), expanded: vec!["C:\\a".into()], selected: None, scroll: 5, sort: SortKey::Type, desc: false, hidden_columns: Some(0b1010) }],
                        active: 0,
                        is_active: false,
                        columns: Some([90, 80, 70, 60]),
                    },
                ],
            }),
        };
        let text = serde_json::to_string(&s).unwrap();
        assert_eq!(serde_json::from_str::<Settings>(&text).unwrap(), s);
        // Missing fields fall back to defaults.
        assert_eq!(serde_json::from_str::<Settings>("{}").unwrap(), Settings::default());
    }

    #[test]
    fn hidden_columns() {
        let w = [100, 200, 300, 400];
        assert_eq!(visible_columns(w, 0), w);
        assert_eq!(visible_columns(w, 0b1010), [100, 0, 300, 0]);
        // Name can't be hidden.
        assert_eq!(visible_columns(w, 0xFF), [100, 0, 0, 0]);
        let s: Settings = serde_json::from_str(r#"{"hidden_columns":255}"#).unwrap();
        assert_eq!(s.sanitized().hidden_columns, HIDEABLE_COLUMNS);
    }

    #[test]
    fn legacy_import() {
        let json = r#"{"FavoriteEntries":[{"Path":"C:\\Work","SortKey":"DateModified","SortDescending":true},{"Path":""}],
                      "NameWidth":400.4,"SizeWidth":10,"Tabs":["C:\\"]}"#;
        let s = import_legacy(json);
        assert_eq!(s.favorites, vec![Favorite { path: "C:\\Work".into(), sort: SortKey::Modified, desc: true }]);
        assert_eq!(s.columns, [400, 170, 140, 50]);
        assert_eq!(import_legacy("not json"), Settings::default());
    }

    #[test]
    fn legacy_layout_import() {
        let json = r#"{"Root":{"Kind":"split","Orientation":"Vertical","Sizes":[2,1,1],"Children":[
            {"Kind":"leaf","ActiveTabIndex":5,"IsActive":true,"Tabs":[
                {"Path":"C:\\a\\b","AsRoot":true,"RootPath":"C:\\a","ExpandedPaths":["C:\\a"],"SortKey":"Size","SortDescending":true},
                {"Path":"C:\\x","AsRoot":false,"SelectedPath":"C:\\x\\f.txt"}]},
            {"Kind":"leaf","Tabs":[]},
            {"Kind":"leaf","Tabs":[{"IsDrives":true}]}]}}"#;
        let Some(LayoutEntry::Split { horizontal, weights, children }) = import_legacy(json).layout else { panic!() };
        assert!(!horizontal);
        assert_eq!(weights, vec![2.0, 1.0]);
        let LayoutEntry::Leaf { tabs, active, is_active, .. } = &children[0] else { panic!() };
        assert_eq!((*active, *is_active), (1, true));
        assert_eq!(tabs[0], TabEntry { root: Some("C:\\a".into()), expanded: vec!["C:\\a".into()], selected: Some("C:\\a\\b".into()), scroll: 0, sort: SortKey::Size, desc: true, hidden_columns: None });
        assert_eq!((tabs[1].root.as_deref(), tabs[1].selected.as_deref()), (None, Some("C:\\x\\f.txt")));
        let LayoutEntry::Leaf { tabs, .. } = &children[1] else { panic!() };
        assert_eq!(tabs[0].root, None);

        let flat = import_legacy(r#"{"TabEntries":[{"Path":"C:\\y","AsRoot":true}],"ActiveTabIndex":0}"#);
        let Some(LayoutEntry::Leaf { tabs, is_active: true, .. }) = flat.layout else { panic!() };
        assert_eq!(tabs[0].root.as_deref(), Some("C:\\y"));
    }
}
