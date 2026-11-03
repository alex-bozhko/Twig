//! Tree model for one tab: a generational arena of nodes with sorting, merging and flattening.

use crate::util::{natural_cmp, normalize_path, path_key, Matcher};
use std::cmp::Ordering;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct NodeId {
    idx: u32,
    gen: u32,
}

impl NodeId {
    /// Placeholder row (a section header) that never resolves to a node.
    pub const SECTION: NodeId = NodeId { idx: u32::MAX, gen: u32::MAX };

    /// Placeholder row with the subfolder-search progress.
    pub const SCAN_NOTE: NodeId = NodeId { idx: u32::MAX - 2, gen: 0 };

    /// Placeholder row standing for the `i`-th group of items hidden by a filter.
    pub fn hidden_marker(i: u32) -> NodeId {
        NodeId { idx: u32::MAX - 1, gen: i }
    }

    pub fn as_hidden_marker(self) -> Option<usize> {
        (self.idx == u32::MAX - 1).then_some(self.gen as usize)
    }

    /// A row that is not a node (section header, hidden-items marker or scan note).
    pub fn is_placeholder(self) -> bool {
        self.idx >= u32::MAX - 2
    }
}

/// The visible rows of a tree narrowed by a name filter.
#[derive(Default, Debug)]
pub struct Filtered {
    /// Kept nodes in display order, with a hidden marker after each group that lost items.
    pub rows: Vec<NodeId>,
    /// Kept only because they lead to a match (or are an open root).
    pub context: HashSet<NodeId>,
    /// (depth, count) for each hidden marker, indexed by `NodeId::as_hidden_marker`.
    pub hidden: Vec<(u16, usize)>,
    /// Collapsed folders shown open because they contain matches (subfolder search).
    pub opened: HashSet<NodeId>,
    pub matched: usize,
    pub total: usize,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Kind {
    Drive,
    Dir,
    File,
    Zip,
    ZipDir,
    ZipFile,
}

impl Kind {
    /// Can be expanded in the tree.
    pub fn is_container(self) -> bool {
        matches!(self, Kind::Drive | Kind::Dir | Kind::Zip | Kind::ZipDir)
    }
    /// Sorted before files.
    pub fn is_folder(self) -> bool {
        matches!(self, Kind::Drive | Kind::Dir | Kind::ZipDir)
    }
    /// Lives inside a zip archive (virtual).
    pub fn is_virtual(self) -> bool {
        matches!(self, Kind::ZipDir | Kind::ZipFile)
    }
    /// A real file-system directory we can list, drop into, create items in.
    pub fn is_fs_dir(self) -> bool {
        matches!(self, Kind::Drive | Kind::Dir)
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default, serde::Serialize, serde::Deserialize)]
pub enum SortKey {
    #[default]
    Name,
    Modified,
    Type,
    Size,
}

impl SortKey {
    pub const ALL: [SortKey; 4] = [SortKey::Name, SortKey::Modified, SortKey::Type, SortKey::Size];
    pub fn column(self) -> usize {
        self as usize
    }
    pub fn from_column(c: usize) -> SortKey {
        Self::ALL.get(c).copied().unwrap_or_default()
    }
    /// Lenient parse for settings written by the old .NET app ("Name", "DateModified", ...).
    pub fn parse_loose(s: &str) -> SortKey {
        let s = s.to_lowercase();
        if s.contains("mod") || s.contains("date") {
            SortKey::Modified
        } else if s.contains("type") {
            SortKey::Type
        } else if s.contains("size") {
            SortKey::Size
        } else {
            SortKey::Name
        }
    }
}

/// A listing entry produced by the background loader.
#[derive(Clone, Debug)]
pub struct Entry {
    pub name: String,
    pub path: String,
    pub kind: Kind,
    pub attrs: u32,
    pub size: u64,
    /// FILETIME as u64 (100ns since 1601), 0 = unknown.
    pub modified: u64,
    pub type_name: Arc<str>,
    pub icon: i32,
    /// True if the icon was resolved for this exact path (not a per-extension guess).
    pub icon_final: bool,
    pub has_children: Option<bool>,
}

#[derive(Clone, Debug)]
pub struct Node {
    pub kind: Kind,
    pub name: String,
    pub path: String,
    /// For zip entries: (archive path, inner path with '/' separators).
    pub zip: Option<(Arc<str>, String)>,
    pub attrs: u32,
    pub size: u64,
    pub modified: u64,
    pub type_name: Arc<str>,
    pub icon: i32,
    pub icon_final: bool,
    pub parent: Option<NodeId>,
    pub children: Option<Vec<NodeId>>,
    pub expanded: bool,
    pub loading: bool,
    pub has_children: Option<bool>,
    pub depth: u16,
    pub selected: bool,
    /// Expand all descendants as they load.
    pub expand_all: bool,
    /// The children came from a subfolder-search scan (no exact icons; dropped when it ends).
    pub scanned: bool,
}

pub const ATTR_HIDDEN: u32 = 0x2;
pub const ATTR_SYSTEM: u32 = 0x4;

impl Node {
    pub fn from_entry(e: Entry, parent: Option<NodeId>, depth: u16) -> Node {
        Node {
            kind: e.kind,
            name: e.name,
            path: e.path,
            zip: None,
            attrs: e.attrs,
            size: e.size,
            modified: e.modified,
            type_name: e.type_name,
            icon: e.icon,
            icon_final: e.icon_final,
            parent,
            children: None,
            expanded: false,
            loading: false,
            has_children: e.has_children,
            depth,
            selected: false,
            expand_all: false,
            scanned: false,
        }
    }
    /// Hidden-attribute items and dot-files/folders (Unix-style hidden), drawn faded.
    pub fn is_hidden(&self) -> bool {
        self.attrs & ATTR_HIDDEN != 0 || (self.kind != Kind::Drive && self.name.starts_with('.'))
    }
    /// Whether to draw an expand chevron.
    pub fn expandable(&self) -> bool {
        if !self.kind.is_container() {
            return false;
        }
        match &self.children {
            Some(c) => !c.is_empty(),
            None => self.has_children != Some(false),
        }
    }
}

struct Slot {
    gen: u32,
    node: Option<Node>,
}

#[derive(Default)]
pub struct Tree {
    slots: Vec<Slot>,
    free: Vec<u32>,
    pub roots: Vec<NodeId>,
    pub sort: SortKey,
    pub desc: bool,
    /// Roots keep their insertion order (This PC drive list).
    pub fixed_root_order: bool,
}

/// One entry from a zip archive's central directory.
#[derive(Clone, Debug)]
pub struct ZipItem {
    pub inner: String,
    pub is_dir: bool,
    pub size: u64,
    pub modified: u64,
    pub type_name: Arc<str>,
    pub icon: i32,
}

impl Tree {
    pub fn new(sort: SortKey, desc: bool) -> Tree {
        Tree { sort, desc, ..Default::default() }
    }

    /// Removes all nodes. Slots keep their generation so stale ids never match new nodes.
    pub fn clear(&mut self) {
        self.free.clear();
        for (i, slot) in self.slots.iter_mut().enumerate().rev() {
            slot.node = None;
            self.free.push(i as u32);
        }
        self.roots.clear();
    }

    pub fn alloc(&mut self, node: Node) -> NodeId {
        if let Some(idx) = self.free.pop() {
            let slot = &mut self.slots[idx as usize];
            slot.gen = slot.gen.wrapping_add(1);
            slot.node = Some(node);
            NodeId { idx, gen: slot.gen }
        } else {
            self.slots.push(Slot { gen: 0, node: Some(node) });
            NodeId { idx: (self.slots.len() - 1) as u32, gen: 0 }
        }
    }

    pub fn get(&self, id: NodeId) -> Option<&Node> {
        let slot = self.slots.get(id.idx as usize)?;
        if slot.gen == id.gen {
            slot.node.as_ref()
        } else {
            None
        }
    }

    pub fn get_mut(&mut self, id: NodeId) -> Option<&mut Node> {
        let slot = self.slots.get_mut(id.idx as usize)?;
        if slot.gen == id.gen {
            slot.node.as_mut()
        } else {
            None
        }
    }

    pub fn exists(&self, id: NodeId) -> bool {
        self.get(id).is_some()
    }

    fn free_subtree(&mut self, id: NodeId) {
        let mut stack = vec![id];
        while let Some(cur) = stack.pop() {
            let Some(slot) = self.slots.get_mut(cur.idx as usize) else { continue };
            if slot.gen != cur.gen {
                continue;
            }
            if let Some(node) = slot.node.take() {
                if let Some(ch) = node.children {
                    stack.extend(ch);
                }
                self.free.push(cur.idx);
            }
        }
    }

    pub fn add_root(&mut self, e: Entry) -> NodeId {
        let id = self.alloc(Node::from_entry(e, None, 0));
        self.roots.push(id);
        id
    }

    pub fn children(&self, id: NodeId) -> &[NodeId] {
        self.get(id).and_then(|n| n.children.as_deref()).unwrap_or(&[])
    }

    /// Items in the same container as `id` (or the roots).
    pub fn siblings(&self, id: NodeId) -> &[NodeId] {
        match self.get(id).and_then(|n| n.parent) {
            Some(p) => self.children(p),
            None => &self.roots,
        }
    }

    pub fn compare(&self, a: &Node, b: &Node) -> Ordering {
        let fa = a.kind.is_folder();
        let fb = b.kind.is_folder();
        // Date order mixes folders and files (matches the original app).
        if fa != fb && self.sort != SortKey::Modified {
            return if fa { Ordering::Less } else { Ordering::Greater };
        }
        let by_name = || natural_cmp(&a.name, &b.name);
        let ord = match self.sort {
            SortKey::Name => by_name(),
            SortKey::Modified => a.modified.cmp(&b.modified).then_with(by_name),
            SortKey::Type => natural_cmp(&a.type_name, &b.type_name).then_with(by_name),
            SortKey::Size => a.size.cmp(&b.size).then_with(by_name),
        };
        if self.desc {
            ord.reverse()
        } else {
            ord
        }
    }

    fn sort_ids(&self, ids: &mut [NodeId]) {
        ids.sort_by(|&x, &y| match (self.get(x), self.get(y)) {
            (Some(a), Some(b)) => self.compare(a, b),
            _ => Ordering::Equal,
        });
    }

    /// Re-sorts every loaded level.
    pub fn resort(&mut self) {
        if !self.fixed_root_order {
            let mut roots = std::mem::take(&mut self.roots);
            self.sort_ids(&mut roots);
            self.roots = roots;
        }
        let mut stack: Vec<NodeId> = self.roots.clone();
        while let Some(id) = stack.pop() {
            let Some(mut ch) = self.get_mut(id).and_then(|n| n.children.take()) else { continue };
            self.sort_ids(&mut ch);
            stack.extend(ch.iter().copied());
            if let Some(n) = self.get_mut(id) {
                n.children = Some(ch);
            }
        }
    }

    /// Replaces/merges the children of `parent` with a fresh listing.
    /// Existing nodes (matched case-insensitively by name) keep their expansion, selection and children.
    pub fn merge_children(&mut self, parent: NodeId, entries: Vec<Entry>) {
        let Some(pnode) = self.get(parent) else { return };
        let depth = pnode.depth + 1;
        let old = pnode.children.clone().unwrap_or_default();
        let mut by_name: HashMap<String, NodeId> =
            old.iter().filter_map(|&id| self.get(id).map(|n| (n.name.to_lowercase(), id))).collect();
        let mut result = Vec::with_capacity(entries.len());
        for e in entries {
            let key = e.name.to_lowercase();
            if let Some(id) = by_name.remove(&key) {
                let kind_changed = self.get(id).map(|n| n.kind != e.kind).unwrap_or(true);
                if kind_changed {
                    self.free_subtree(id);
                    result.push(self.alloc(Node::from_entry(e, Some(parent), depth)));
                    continue;
                }
                self.update_from_entry(id, e);
                result.push(id);
            } else {
                result.push(self.alloc(Node::from_entry(e, Some(parent), depth)));
            }
        }
        for (_, gone) in by_name {
            self.free_subtree(gone);
        }
        self.sort_ids(&mut result);
        if let Some(p) = self.get_mut(parent) {
            p.has_children = Some(!result.is_empty());
            p.children = Some(result);
            p.loading = false;
        }
    }

    fn update_from_entry(&mut self, id: NodeId, e: Entry) {
        let Some(n) = self.get_mut(id) else { return };
        n.name = e.name;
        n.path = e.path;
        n.attrs = e.attrs;
        n.size = e.size;
        n.modified = e.modified;
        n.type_name = e.type_name;
        if e.icon_final || !n.icon_final {
            n.icon = e.icon;
            n.icon_final = e.icon_final;
        }
        if e.has_children.is_some() {
            n.has_children = e.has_children;
        }
    }

    /// Replaces the roots with `entries` (e.g. a refreshed drive list), keeping the state of roots
    /// whose path is unchanged. Root order follows `entries` when `fixed_root_order` is set.
    pub fn merge_roots(&mut self, entries: Vec<Entry>) {
        let mut by_path: HashMap<String, NodeId> =
            self.roots.iter().filter_map(|&id| self.get(id).map(|n| (path_key(&n.path), id))).collect();
        let mut result = Vec::with_capacity(entries.len());
        for e in entries {
            match by_path.remove(&path_key(&e.path)) {
                Some(id) if self.get(id).is_some_and(|n| n.kind == e.kind) => {
                    self.update_from_entry(id, e);
                    result.push(id);
                }
                old => {
                    if let Some(id) = old {
                        self.free_subtree(id);
                    }
                    result.push(self.alloc(Node::from_entry(e, None, 0)));
                }
            }
        }
        for (_, gone) in by_path {
            self.free_subtree(gone);
        }
        if !self.fixed_root_order {
            self.sort_ids(&mut result);
        }
        self.roots = result;
    }

    /// Updates per-item details (icons, has-children) delivered by the second loading phase.
    pub fn apply_details(&mut self, parent: NodeId, details: &[(String, Option<i32>, Option<bool>)]) {
        let map: HashMap<String, NodeId> = self
            .children(parent)
            .iter()
            .filter_map(|&id| self.get(id).map(|n| (n.name.to_lowercase(), id)))
            .collect();
        for (name, icon, has) in details {
            if let Some(&id) = map.get(&name.to_lowercase()) {
                let n = self.get_mut(id).unwrap();
                if let Some(i) = icon {
                    n.icon = *i;
                    n.icon_final = true;
                }
                if has.is_some() && n.children.is_none() {
                    n.has_children = *has;
                }
            }
        }
    }

    /// Builds the virtual folder hierarchy for a zip archive node.
    pub fn set_zip_children(&mut self, zip: NodeId, items: Vec<ZipItem>, folder_type: Arc<str>, folder_icon: i32) {
        let Some(znode) = self.get(zip) else { return };
        let archive: Arc<str> = Arc::from(znode.path.as_str());
        let base_depth = znode.depth;
        if let Some(old) = self.get_mut(zip).and_then(|n| n.children.take()) {
            for id in old {
                self.free_subtree(id);
            }
        }
        // inner dir path (lowercase, no trailing '/') -> node
        let mut dirs: HashMap<String, NodeId> = HashMap::new();
        dirs.insert(String::new(), zip);
        if let Some(n) = self.get_mut(zip) {
            n.children = Some(Vec::new());
        }
        let mut sorted = items;
        sorted.sort_by(|a, b| a.inner.cmp(&b.inner));
        for item in sorted {
            let inner = item.inner.trim_end_matches('/').to_string();
            if inner.is_empty() {
                continue;
            }
            let parts: Vec<&str> = inner.split('/').filter(|p| !p.is_empty()).collect();
            if parts.is_empty() {
                continue;
            }
            // Ensure parent dirs exist.
            let mut parent = zip;
            let mut acc = String::new();
            for (i, part) in parts.iter().enumerate() {
                let last = i == parts.len() - 1;
                if !acc.is_empty() {
                    acc.push('/');
                }
                acc.push_str(part);
                let key = acc.to_lowercase();
                if last && !item.is_dir {
                    let node = Node {
                        kind: Kind::ZipFile,
                        name: part.to_string(),
                        path: format!("{}\\{}", archive, acc.replace('/', "\\")),
                        zip: Some((archive.clone(), acc.clone())),
                        attrs: 0,
                        size: item.size,
                        modified: item.modified,
                        type_name: item.type_name.clone(),
                        icon: item.icon,
                        icon_final: true,
                        parent: Some(parent),
                        children: None,
                        expanded: false,
                        loading: false,
                        has_children: Some(false),
                        depth: base_depth + 1 + i as u16,
                        selected: false,
                        expand_all: false,
                        scanned: false,
                    };
                    let id = self.alloc(node);
                    self.get_mut(parent).unwrap().children.get_or_insert_with(Vec::new).push(id);
                    continue;
                }
                if let Some(&existing) = dirs.get(&key) {
                    parent = existing;
                    if last {
                        if let Some(n) = self.get_mut(existing) {
                            n.modified = n.modified.max(item.modified);
                        }
                    }
                    continue;
                }
                let node = Node {
                    kind: Kind::ZipDir,
                    name: part.to_string(),
                    path: format!("{}\\{}", archive, acc.replace('/', "\\")),
                    zip: Some((archive.clone(), format!("{acc}/"))),
                    attrs: 0,
                    size: 0,
                    modified: if last { item.modified } else { 0 },
                    type_name: folder_type.clone(),
                    icon: folder_icon,
                    icon_final: true,
                    parent: Some(parent),
                    children: Some(Vec::new()),
                    expanded: false,
                    loading: false,
                    has_children: Some(false),
                    depth: base_depth + 1 + i as u16,
                    selected: false,
                    expand_all: false,
                    scanned: false,
                };
                let id = self.alloc(node);
                self.get_mut(parent).unwrap().children.get_or_insert_with(Vec::new).push(id);
                dirs.insert(key, id);
                parent = id;
            }
        }
        // Sort every level and fix has_children.
        let mut stack = vec![zip];
        while let Some(id) = stack.pop() {
            let Some(mut ch) = self.get_mut(id).and_then(|n| n.children.take()) else { continue };
            self.sort_ids(&mut ch);
            stack.extend(ch.iter().copied());
            if let Some(n) = self.get_mut(id) {
                n.has_children = Some(!ch.is_empty());
                n.children = Some(ch);
                n.loading = false;
            }
        }
    }

    /// Visible rows in display order.
    pub fn visible(&self) -> Vec<NodeId> {
        let mut out = Vec::new();
        let mut stack: Vec<NodeId> = self.roots.iter().rev().copied().collect();
        while let Some(id) = stack.pop() {
            let Some(n) = self.get(id) else { continue };
            out.push(id);
            if n.expanded {
                if let Some(ch) = &n.children {
                    stack.extend(ch.iter().rev().copied());
                }
            }
        }
        out
    }

    /// Every loaded node in display order, including the contents of collapsed folders.
    pub fn all_loaded(&self) -> Vec<NodeId> {
        let mut out = Vec::new();
        let mut stack: Vec<NodeId> = self.roots.iter().rev().copied().collect();
        while let Some(id) = stack.pop() {
            let Some(n) = self.get(id) else { continue };
            out.push(id);
            if let Some(ch) = &n.children {
                stack.extend(ch.iter().rev().copied());
            }
        }
        out
    }

    /// Visible rows narrowed to names containing `query` (case-insensitive). Ancestors of matches
    /// stay as context, as do expanded roots (so a folder tab keeps its header row).
    /// Without `deep` only rows that are already visible are searched; with it every loaded node is,
    /// and collapsed folders holding matches are shown open (listed in `opened`).
    pub fn filtered(&self, query: &str, deep: bool) -> Filtered {
        let order = if deep { self.all_loaded() } else { self.visible() };
        let mut f = Filtered { total: order.len(), ..Default::default() };
        let mut kept = HashSet::new();
        let matcher = Matcher::new(query);
        // Reverse pre-order visits children before their parents.
        for &id in order.iter().rev() {
            let Some(n) = self.get(id) else { continue };
            let hit = matcher.is_match(&n.name);
            let open_root = n.parent.is_none() && n.expanded;
            if hit || open_root || kept.contains(&id) {
                if hit {
                    f.matched += 1;
                } else {
                    f.context.insert(id);
                }
                kept.insert(id);
                if let Some(p) = n.parent {
                    kept.insert(p);
                }
            }
        }
        self.emit_filtered(&self.roots, 0, &kept, deep, &mut f);
        f
    }

    fn emit_filtered(&self, ids: &[NodeId], depth: u16, kept: &HashSet<NodeId>, deep: bool, f: &mut Filtered) {
        let mut hidden = 0;
        for &id in ids {
            let Some(n) = self.get(id) else { continue };
            if !kept.contains(&id) {
                hidden += 1;
                continue;
            }
            f.rows.push(id);
            let Some(ch) = &n.children else { continue };
            let open = n.expanded || (deep && ch.iter().any(|c| kept.contains(c)));
            if open {
                if !n.expanded {
                    f.opened.insert(id);
                }
                self.emit_filtered(ch, depth + 1, kept, deep, f);
            }
        }
        if hidden > 0 {
            f.rows.push(NodeId::hidden_marker(f.hidden.len() as u32));
            f.hidden.push((depth, hidden));
        }
    }

    /// Type-to-search scope: the children of an expanded focused folder, otherwise its siblings
    /// (the roots without focus) — limited to the rows actually `shown`.
    pub fn search_scope(&self, focus: Option<NodeId>, shown: &[NodeId]) -> Vec<NodeId> {
        let shown: HashSet<NodeId> = shown.iter().copied().collect();
        let pick = |ids: &[NodeId]| ids.iter().copied().filter(|id| shown.contains(id)).collect::<Vec<_>>();
        let Some((f, n)) = focus.and_then(|f| self.get(f).map(|n| (f, n))) else { return pick(&self.roots) };
        if n.expanded {
            let ch = pick(self.children(f));
            if !ch.is_empty() {
                return ch;
            }
        }
        pick(self.siblings(f))
    }

    /// Finds a loaded node by full path (case-insensitive). When the path is loaded more than once
    /// (a favorite root also reachable through its drive), a visible copy is preferred.
    pub fn find_path(&self, path: &str) -> Option<NodeId> {
        let all = self.find_paths(path);
        all.iter().copied().find(|&id| self.is_shown(id)).or_else(|| all.first().copied())
    }

    /// Every loaded node with this path, those under the most specific root first.
    pub fn find_paths(&self, path: &str) -> Vec<NodeId> {
        let key = path_key(path);
        let mut starts: Vec<(usize, NodeId)> = self
            .roots
            .iter()
            .filter_map(|&id| {
                let nk = path_key(&self.get(id)?.path);
                covers(&nk, &key).then_some((nk.len(), id))
            })
            .collect();
        starts.sort_by(|a, b| b.0.cmp(&a.0));
        starts.into_iter().filter_map(|(_, id)| self.descend(id, &key)).collect()
    }

    /// Walks down from `id`, which covers `key`, through loaded children.
    fn descend(&self, mut id: NodeId, key: &str) -> Option<NodeId> {
        'outer: loop {
            let n = self.get(id)?;
            if path_key(&n.path) == key {
                return Some(id);
            }
            for &c in n.children.as_deref()? {
                if self.get(c).is_some_and(|cn| covers(&path_key(&cn.path), key)) {
                    id = c;
                    continue 'outer;
                }
            }
            return None;
        }
    }

    /// True if every ancestor of the node is expanded.
    pub fn is_shown(&self, mut id: NodeId) -> bool {
        while let Some(p) = self.get(id).and_then(|n| n.parent) {
            if !self.get(p).is_some_and(|n| n.expanded) {
                return false;
            }
            id = p;
        }
        true
    }

    pub fn is_ancestor(&self, ancestor: NodeId, mut id: NodeId) -> bool {
        while let Some(p) = self.get(id).and_then(|n| n.parent) {
            if p == ancestor {
                return true;
            }
            id = p;
        }
        false
    }

    pub fn clear_selection(&mut self) {
        for slot in self.slots.iter_mut() {
            if let Some(n) = slot.node.as_mut() {
                n.selected = false;
            }
        }
    }

    pub fn set_selected(&mut self, id: NodeId, sel: bool) {
        if let Some(n) = self.get_mut(id) {
            n.selected = sel;
        }
    }

    pub fn is_selected(&self, id: NodeId) -> bool {
        self.get(id).map(|n| n.selected).unwrap_or(false)
    }

    /// Selected nodes in visible order.
    pub fn selection(&self, visible: &[NodeId]) -> Vec<NodeId> {
        visible.iter().copied().filter(|&id| self.is_selected(id)).collect()
    }

    /// Collapses a node; descendants that were selected get deselected.
    pub fn collapse(&mut self, id: NodeId) {
        if let Some(n) = self.get_mut(id) {
            n.expanded = false;
            n.expand_all = false;
        }
        let mut stack: Vec<NodeId> = self.children(id).to_vec();
        while let Some(c) = stack.pop() {
            if let Some(n) = self.get_mut(c) {
                n.selected = false;
                if let Some(ch) = &n.children {
                    stack.extend(ch.iter().copied());
                }
            }
        }
    }

    /// Paths of expanded loaded containers (for session persistence).
    pub fn expanded_paths(&self) -> Vec<String> {
        let mut out = Vec::new();
        let mut stack: Vec<NodeId> = self.roots.clone();
        while let Some(id) = stack.pop() {
            let Some(n) = self.get(id) else { continue };
            if n.expanded {
                out.push(normalize_path(&n.path));
                if let Some(ch) = &n.children {
                    stack.extend(ch.iter().copied());
                }
            }
        }
        out
    }

    /// Loaded file-system directories (for change-notification refreshes). Folders filled by a
    /// subfolder-search scan are left out (they are rescanned instead).
    pub fn loaded_dirs(&self) -> Vec<(NodeId, String)> {
        let mut out = Vec::new();
        let mut stack: Vec<NodeId> = self.roots.clone();
        while let Some(id) = stack.pop() {
            let Some(n) = self.get(id) else { continue };
            if n.kind.is_fs_dir() && n.children.is_some() {
                if !n.scanned {
                    out.push((id, n.path.clone()));
                }
                stack.extend(n.children.as_ref().unwrap().iter().copied());
            }
        }
        out
    }

    /// Forgets the contents of collapsed folders that were filled by a subfolder-search scan,
    /// so they load normally (with exact icons and details) when next expanded.
    pub fn drop_scanned(&mut self) {
        let mut stack: Vec<NodeId> = self.roots.clone();
        while let Some(id) = stack.pop() {
            let Some(n) = self.get(id) else { continue };
            if n.scanned && !n.expanded {
                for c in n.children.clone().unwrap_or_default() {
                    self.free_subtree(c);
                }
                let n = self.get_mut(id).unwrap();
                n.children = None;
                n.scanned = false;
                n.loading = false;
                n.expand_all = false;
            } else if let Some(ch) = &n.children {
                stack.extend(ch.iter().copied());
            }
        }
    }
}

/// True if the node with path key `nk` is `key` or one of its ancestors.
fn covers(nk: &str, key: &str) -> bool {
    key == nk || key.strip_prefix(nk).is_some_and(|rest| nk.ends_with('\\') || rest.starts_with('\\'))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(name: &str, kind: Kind, size: u64) -> Entry {
        Entry {
            name: name.into(),
            path: format!("C:\\r\\{name}"),
            kind,
            attrs: 0,
            size,
            modified: size * 7 % 5,
            type_name: Arc::from(if kind == Kind::Dir { "File folder" } else { "Text" }),
            icon: 0,
            icon_final: false,
            has_children: None,
        }
    }

    fn root_tree() -> (Tree, NodeId) {
        let mut t = Tree::new(SortKey::Name, false);
        let mut r = entry("r", Kind::Dir, 0);
        r.path = "C:\\r".into();
        let root = t.add_root(r);
        t.get_mut(root).unwrap().expanded = true;
        (t, root)
    }

    fn names(t: &Tree, ids: &[NodeId]) -> Vec<String> {
        ids.iter().map(|&i| t.get(i).unwrap().name.clone()).collect()
    }

    #[test]
    fn dot_names_count_as_hidden() {
        let mut hidden = entry("plain", Kind::File, 0);
        hidden.attrs = ATTR_HIDDEN;
        let node = |e: Entry| Node::from_entry(e, None, 0);
        assert!(node(hidden).is_hidden());
        assert!(node(entry(".git", Kind::Dir, 0)).is_hidden());
        assert!(!node(entry("a.txt", Kind::File, 0)).is_hidden());
    }

    #[test]
    fn sort_folders_first_natural() {
        let (mut t, root) = root_tree();
        t.merge_children(root, vec![entry("b10.txt", Kind::File, 3), entry("b2.txt", Kind::File, 1), entry("zdir", Kind::Dir, 0), entry("adir", Kind::Dir, 0)]);
        assert_eq!(names(&t, t.children(root)), vec!["adir", "zdir", "b2.txt", "b10.txt"]);
        t.desc = true;
        t.resort();
        assert_eq!(names(&t, t.children(root)), vec!["zdir", "adir", "b10.txt", "b2.txt"]);
        t.sort = SortKey::Size;
        t.desc = false;
        t.resort();
        assert_eq!(names(&t, t.children(root)), vec!["adir", "zdir", "b2.txt", "b10.txt"]);
    }

    #[test]
    fn sort_by_date_mixes_folders_and_files() {
        let (mut t, root) = root_tree();
        let mut old_dir = entry("old", Kind::Dir, 0);
        old_dir.modified = 10;
        let mut new_dir = entry("new", Kind::Dir, 0);
        new_dir.modified = 30;
        let mut mid = entry("mid.txt", Kind::File, 1);
        mid.modified = 20;
        t.sort = SortKey::Modified;
        t.merge_children(root, vec![new_dir, mid, old_dir]);
        assert_eq!(names(&t, t.children(root)), vec!["old", "mid.txt", "new"]);
        t.desc = true;
        t.resort();
        assert_eq!(names(&t, t.children(root)), vec!["new", "mid.txt", "old"]);
    }

    #[test]
    fn merge_keeps_state() {
        let (mut t, root) = root_tree();
        t.merge_children(root, vec![entry("a", Kind::Dir, 0), entry("b.txt", Kind::File, 1)]);
        let a = t.children(root)[0];
        t.get_mut(a).unwrap().expanded = true;
        t.merge_children(a, vec![entry("inner", Kind::File, 1)]);
        t.set_selected(a, true);
        // Refresh: "b.txt" deleted, "c.txt" added, "A" renamed case-only.
        t.merge_children(root, vec![entry("A", Kind::Dir, 0), entry("c.txt", Kind::File, 1)]);
        let ch = t.children(root).to_vec();
        assert_eq!(names(&t, &ch), vec!["A", "c.txt"]);
        assert_eq!(ch[0], a);
        let n = t.get(a).unwrap();
        assert!(n.expanded && n.selected && n.children.as_ref().unwrap().len() == 1);
        assert_eq!(t.visible().len(), 4);
    }

    #[test]
    fn freed_ids_are_stale() {
        let (mut t, root) = root_tree();
        t.merge_children(root, vec![entry("x", Kind::File, 1)]);
        let x = t.children(root)[0];
        t.merge_children(root, vec![]);
        assert!(t.get(x).is_none());
        t.merge_children(root, vec![entry("y", Kind::File, 1)]);
        assert!(t.get(x).is_none());
        assert_eq!(t.get(root).unwrap().has_children, Some(true));
    }

    #[test]
    fn merge_roots_keeps_existing() {
        let mut t = Tree::new(SortKey::Name, false);
        t.fixed_root_order = true;
        t.merge_roots(vec![entry("c", Kind::Drive, 0), entry("d", Kind::Drive, 0)]);
        let c = t.roots[0];
        let d = t.roots[1];
        t.get_mut(c).unwrap().expanded = true;
        t.merge_roots(vec![entry("c", Kind::Drive, 0), entry("e", Kind::Drive, 0)]);
        assert_eq!(names(&t, &t.roots.clone()), vec!["c", "e"]);
        assert_eq!(t.roots[0], c);
        assert!(t.get(c).unwrap().expanded);
        assert!(t.get(d).is_none());
    }

    #[test]
    fn find_and_expanded() {
        let (mut t, root) = root_tree();
        t.merge_children(root, vec![entry("a", Kind::Dir, 0)]);
        let a = t.children(root)[0];
        assert_eq!(t.find_path("c:\\R\\A\\"), Some(a));
        assert_eq!(t.find_path("C:\\r"), Some(root));
        assert_eq!(t.find_path("C:\\r\\zz"), None);
        assert_eq!(t.expanded_paths(), vec!["C:\\r".to_string()]);
        assert!(t.is_ancestor(root, a));
    }

    #[test]
    fn find_path_with_favorite_root() {
        let mut t = Tree::new(SortKey::Name, false);
        t.fixed_root_order = true;
        let at = |name: &str, path: &str, kind: Kind| Entry { path: path.into(), ..entry(name, kind, 0) };
        t.merge_roots(vec![at("C:", "C:\\", Kind::Drive), at("a", "C:\\r\\a", Kind::Dir)]);
        let (drive, fav) = (t.roots[0], t.roots[1]);
        assert_eq!(t.find_path("C:\\r\\a"), Some(fav));
        // A favorite's contents resolve through it even while the drive is not loaded.
        t.merge_children(fav, vec![at("x", "C:\\r\\a\\x", Kind::File)]);
        let fav_x = t.children(fav)[0];
        assert_eq!(t.find_path("c:\\R\\a\\X"), Some(fav_x));
        // Loaded through the drive as well: both copies are found, the visible one preferred.
        t.merge_children(drive, vec![at("r", "C:\\r", Kind::Dir)]);
        let r = t.children(drive)[0];
        t.merge_children(r, vec![at("a", "C:\\r\\a", Kind::Dir)]);
        let a = t.children(r)[0];
        t.merge_children(a, vec![at("x", "C:\\r\\a\\x", Kind::File)]);
        let drive_x = t.children(a)[0];
        for id in [drive, r, a] {
            t.get_mut(id).unwrap().expanded = true;
        }
        assert_eq!(t.find_paths("C:\\r\\a\\x"), vec![fav_x, drive_x]);
        assert_eq!(t.find_path("C:\\r\\a\\x"), Some(drive_x));
        t.get_mut(fav).unwrap().expanded = true;
        assert_eq!(t.find_path("C:\\r\\a\\x"), Some(fav_x));
        assert_eq!(t.find_path("C:\\rr"), None);
    }

    #[test]
    fn zip_tree() {
        let mut t = Tree::new(SortKey::Name, false);
        let z = t.add_root(Entry { path: "C:\\a.zip".into(), ..entry("a.zip", Kind::Zip, 10) });
        let item = |inner: &str, is_dir: bool| ZipItem { inner: inner.into(), is_dir, size: 1, modified: 5, type_name: Arc::from("T"), icon: 1 };
        t.set_zip_children(z, vec![item("docs/readme.txt", false), item("docs/", true), item("b.txt", false), item("x/y/z.bin", false)], Arc::from("Folder"), 3);
        t.get_mut(z).unwrap().expanded = true;
        let ch = t.children(z).to_vec();
        assert_eq!(names(&t, &ch), vec!["docs", "x", "b.txt"]);
        let x = ch[1];
        let y = t.children(x)[0];
        let zbin = t.children(y)[0];
        let n = t.get(zbin).unwrap();
        assert_eq!(n.path, "C:\\a.zip\\x\\y\\z.bin");
        assert_eq!(n.zip.as_ref().unwrap().1, "x/y/z.bin");
        assert_eq!(n.depth, 3);
        assert_eq!(t.get(y).unwrap().zip.as_ref().unwrap().1, "x/y/");
        assert_eq!(t.find_path("C:\\a.zip\\docs\\readme.txt").map(|i| t.get(i).unwrap().name.clone()), Some("readme.txt".into()));
    }

    #[test]
    fn stale_ids_after_clear() {
        let (mut t, root) = root_tree();
        t.clear();
        assert!(t.get(root).is_none());
        let fresh = t.add_root(entry("n", Kind::Dir, 0));
        assert!(t.get(root).is_none());
        assert!(t.get(fresh).is_some());
    }

    /// r/{docs/{report.txt, notes.md, old/{report_old.txt}}, pics/{report.png}, readme.txt, todo.txt}
    /// with r and docs expanded; old and pics collapsed but loaded.
    fn filter_tree() -> (Tree, NodeId) {
        let (mut t, root) = root_tree();
        t.merge_children(root, vec![entry("docs", Kind::Dir, 0), entry("pics", Kind::Dir, 0), entry("readme.txt", Kind::File, 1), entry("todo.txt", Kind::File, 2)]);
        let ch = t.children(root).to_vec();
        let (docs, pics) = (ch[0], ch[1]);
        t.merge_children(docs, vec![entry("report.txt", Kind::File, 1), entry("notes.md", Kind::File, 2), entry("old", Kind::Dir, 0)]);
        let old = t.children(docs).iter().copied().find(|&i| t.get(i).unwrap().name == "old").unwrap();
        t.merge_children(old, vec![entry("report_old.txt", Kind::File, 1)]);
        t.merge_children(pics, vec![entry("report.png", Kind::File, 1)]);
        t.get_mut(docs).unwrap().expanded = true;
        (t, root)
    }

    fn by_name(t: &Tree, name: &str) -> NodeId {
        t.visible().into_iter().find(|&i| t.get(i).unwrap().name == name).unwrap()
    }

    /// Row labels with hidden markers as "~depth:count".
    fn labels(t: &Tree, f: &Filtered) -> Vec<String> {
        f.rows
            .iter()
            .map(|&id| match id.as_hidden_marker() {
                Some(i) => format!("~{}:{}", f.hidden[i].0, f.hidden[i].1),
                None => t.get(id).unwrap().name.clone(),
            })
            .collect()
    }

    #[test]
    fn filter_keeps_matches_and_their_ancestors() {
        let (t, root) = filter_tree();
        let f = t.filtered("REPORT", false);
        // pics is collapsed, so its report.png is not searched; old likewise.
        assert_eq!(labels(&t, &f), vec!["r", "docs", "report.txt", "~2:2", "~1:3"]);
        assert_eq!((f.matched, f.total), (1, 8));
        assert!(f.context.contains(&root) && f.context.contains(&by_name(&t, "docs")));
        assert!(!f.context.contains(&by_name(&t, "report.txt")));
        assert!(f.rows.iter().all(|&id| id.is_placeholder() == id.as_hidden_marker().is_some()));
    }

    #[test]
    fn filter_matching_folder_is_not_context() {
        let (t, _) = filter_tree();
        let f = t.filtered("doc", false);
        assert_eq!(labels(&t, &f), vec!["r", "docs", "~2:3", "~1:3"]);
        assert!(!f.context.contains(&by_name(&t, "docs")));
    }

    #[test]
    fn filter_without_matches_leaves_open_root_and_marker() {
        let (mut t, _) = filter_tree();
        t.add_root(entry("zz", Kind::Dir, 0));
        let f = t.filtered("nothing", false);
        assert_eq!(labels(&t, &f), vec!["r", "~1:4", "~0:1"]);
        assert_eq!(f.matched, 0);
    }

    #[test]
    fn type_ahead_scope_follows_shown_rows() {
        let (t, _) = filter_tree();
        let pick = |scope: &[NodeId], focus: Option<NodeId>, buf: &str| {
            let names: Vec<&str> = scope.iter().map(|&i| t.get(i).unwrap().name.as_str()).collect();
            let cur = focus.and_then(|f| scope.iter().position(|&i| i == f));
            crate::util::type_search(&names, cur, buf).map(|i| t.get(scope[i]).unwrap().name.clone())
        };
        let docs = by_name(&t, "docs");
        let readme = by_name(&t, "readme.txt");
        // Unfiltered: an expanded focused folder searches its children, a file its siblings.
        let all = t.visible();
        assert_eq!(names(&t, &t.search_scope(Some(docs), &all)), vec!["old", "notes.md", "report.txt"]);
        let scope = t.search_scope(Some(readme), &all);
        assert_eq!(names(&t, &scope), vec!["docs", "pics", "readme.txt", "todo.txt"]);
        assert_eq!(pick(&scope, Some(readme), "p").as_deref(), Some("pics"));
        assert_eq!(pick(&scope, Some(readme), "t").as_deref(), Some("todo.txt"));
        // Filtered: hidden rows can't be jumped to.
        let f = t.filtered(".txt", false);
        let scope = t.search_scope(Some(readme), &f.rows);
        assert_eq!(names(&t, &scope), vec!["docs", "readme.txt", "todo.txt"]);
        assert_eq!(pick(&scope, Some(readme), "p"), None);
        assert_eq!(pick(&scope, Some(readme), "t").as_deref(), Some("todo.txt"));
        assert_eq!(names(&t, &t.search_scope(Some(docs), &f.rows)), vec!["report.txt"]);
        // No focus: shown roots.
        assert_eq!(names(&t, &t.search_scope(None, &f.rows)), vec!["r"]);
    }

    #[test]
    fn deep_filter_looks_into_collapsed_folders() {
        let (t, _) = filter_tree();
        let f = t.filtered("REPORT", true);
        assert_eq!(labels(&t, &f), vec!["r", "docs", "old", "report_old.txt", "report.txt", "~2:1", "pics", "report.png", "~1:2"]);
        assert_eq!((f.matched, f.total), (3, 10));
        let (old, pics) = (by_name(&t, "old"), t.children(t.roots[0])[1]);
        assert_eq!(f.opened, HashSet::from([old, pics]));
        assert!(f.context.contains(&old) && f.context.contains(&pics));
        // Folders without matches stay closed and are not counted as opened.
        let f = t.filtered("notes", true);
        assert_eq!(labels(&t, &f), vec!["r", "docs", "notes.md", "~2:2", "~1:3"]);
        assert!(f.opened.is_empty());
    }

    #[test]
    fn drop_scanned_forgets_collapsed_scan_results() {
        let (mut t, root) = filter_tree();
        let pics = t.children(root)[1];
        let old = by_name(&t, "old");
        let png = t.children(pics)[0];
        t.get_mut(pics).unwrap().scanned = true;
        t.get_mut(old).unwrap().scanned = true;
        t.get_mut(old).unwrap().expanded = true;
        assert!(t.loaded_dirs().iter().all(|&(id, _)| id != pics && id != old));
        t.drop_scanned();
        let p = t.get(pics).unwrap();
        assert!(p.children.is_none() && !p.scanned && p.has_children == Some(true));
        assert!(t.get(png).is_none());
        // An expanded scanned folder keeps its contents.
        assert_eq!(t.children(old).len(), 1);
        assert!(NodeId::SCAN_NOTE.is_placeholder() && NodeId::SCAN_NOTE.as_hidden_marker().is_none());
    }
}
