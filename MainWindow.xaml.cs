using System;
using System.Collections.Generic;
using System.Collections.ObjectModel;
using System.ComponentModel;
using System.IO;
using System.Linq;
using System.Runtime.CompilerServices;
using System.Text.Json;
using System.Windows;
using System.Windows.Controls;
using System.Windows.Controls.Primitives;
using System.Windows.Input;
using System.Windows.Interop;
using System.Windows.Media;

namespace FileBrowser;

public partial class MainWindow : Window, INotifyPropertyChanged
{
    private readonly LeafPane _rootLeaf = new();

    private PaneNode _root;
    public PaneNode Root
    {
        get => _root;
        private set { if (!ReferenceEquals(_root, value)) { _root = value; Raise(); } }
    }

    private LeafPane _activePane;
    public LeafPane ActivePane
    {
        get => _activePane;
        set
        {
            if (ReferenceEquals(_activePane, value) || value == null) return;
            _activePane = value;
            foreach (var p in AllLeaves()) p.IsActive = ReferenceEquals(p, value);
            Raise();
            Raise(nameof(Tabs));
            Raise(nameof(ActiveTab));
            OnActiveTabChanged();
        }
    }

    // Facade — always reflects the active pane's tabs.
    public ObservableCollection<BrowserTab> Tabs => _activePane.Tabs;
    public ObservableCollection<Favorite> Favorites { get; } = new();

    public BrowserTab? ActiveTab
    {
        get => _activePane.ActiveTab;
        set
        {
            if (ReferenceEquals(_activePane.ActiveTab, value)) return;
            // Capture outgoing tab's scroll offset before switching.
            if (_activePane.ActiveTab != null)
            {
                var sv = FindActiveScrollViewer();
                if (sv != null) _activePane.ActiveTab.ScrollOffset = sv.VerticalOffset;
            }
            _activePane.ActiveTab = value;
            Raise();
            OnActiveTabChanged();
        }
    }

    // --- Tab maximize state ---
    // When a tab is "maximized" we replace Root with a temporary single-leaf pane
    // that shows only that tab. The tab instance is shared with its original pane
    // (which is preserved in _savedRoot but not rendered). On restore we swap Root
    // back and make the pane containing the tab active.
    private BrowserTab? _maximizedTab;
    private PaneNode? _savedRoot;
    private LeafPane? _savedActivePane;
    private LeafPane? _maxLeaf;

    public bool IsTabMaximized => _maximizedTab != null;

    public void ToggleMaximizeTab(BrowserTab tab)
    {
        if (ReferenceEquals(_maximizedTab, tab)) RestoreMaximizedTab();
        else MaximizeTab(tab);
    }

    private void MaximizeTab(BrowserTab tab)
    {
        if (_maximizedTab != null) RestoreMaximizedTab();
        _savedRoot = Root;
        _savedActivePane = ActivePane;
        _maximizedTab = tab;
        _maxLeaf = new LeafPane();
        _maxLeaf.Tabs.Add(tab);
        _maxLeaf.ActiveTab = tab;
        Root = _maxLeaf;
        ActivePane = _maxLeaf;
    }

    private void RestoreMaximizedTab()
    {
        if (_maximizedTab == null || _savedRoot == null) return;
        var tab = _maximizedTab;
        var oldRoot = _savedRoot;
        var oldPane = _savedActivePane;
        _maximizedTab = null;
        _savedRoot = null;
        _savedActivePane = null;
        if (_maxLeaf != null) { _maxLeaf.Tabs.Clear(); _maxLeaf = null; }
        Root = oldRoot;
        // Find the pane that currently holds the tab in the restored tree.
        LeafPane? owner = null;
        foreach (var leaf in EnumerateLeaves(oldRoot))
        {
            if (leaf.Tabs.Contains(tab)) { owner = leaf; break; }
        }
        owner ??= oldPane;
        if (owner != null)
        {
            ActivePane = owner;
            owner.ActiveTab = tab;
        }
    }

    public IEnumerable<LeafPane> AllLeaves() => EnumerateLeaves(Root);
    private static IEnumerable<LeafPane> EnumerateLeaves(PaneNode node)
    {
        if (node is LeafPane l) { yield return l; yield break; }
        if (node is SplitPane s)
            foreach (var c in s.Children)
                foreach (var x in EnumerateLeaves(c)) yield return x;
    }
    public IEnumerable<BrowserTab> AllTabs() => AllLeaves().SelectMany(p => p.Tabs);

    private bool _restoring;
    private bool _bulkLoading;
    private bool _syncingToolbar;

    private static readonly string SettingsPath = Path.Combine(
        Environment.GetFolderPath(Environment.SpecialFolder.LocalApplicationData),
        "FileBrowser",
        "settings.json");

    public MainWindow()
    {
        _root = _rootLeaf;
        _activePane = _rootLeaf;
        _rootLeaf.IsActive = true;
        InitializeComponent();
        DataContext = this;

        var settings = LoadSettings();
        ApplySettings(settings);
        SortState.Instance.Refresh();

        // Try to restore the full pane tree.
        LeafPane? restoredActive = null;
        if (settings?.Root != null)
        {
            var holder = new LeafPane[1];
            var restored = DeserializePane(settings.Root, holder);
            if (restored != null)
            {
                Root = restored;
                restored.Parent = null;
                restoredActive = holder[0];
                _activePane = restoredActive ?? AllLeaves().FirstOrDefault() ?? _rootLeaf;
                foreach (var p in AllLeaves()) p.IsActive = ReferenceEquals(p, _activePane);
            }
        }

        if (restoredActive == null && Root is LeafPane rootLeafSingle && rootLeafSingle.Tabs.Count == 0)
        {
            // Fall back to flat tab restore on the root leaf.
            var tabEntries = settings?.TabEntries?
            .Where(e => e.IsDrives || (!string.IsNullOrEmpty(e.Path) && Directory.Exists(e.Path!)))
            .Select(e => new TabEntry
            {
                Path = e.Path,
                AsRoot = e.AsRoot,
                IsDrives = e.IsDrives,
                RootPath = e.RootPath,
                ExpandedPaths = e.ExpandedPaths,
                SelectedPath = e.SelectedPath,
                ScrollOffset = e.ScrollOffset,
            })
            .ToList() ?? new List<TabEntry>();
        if (tabEntries.Count == 0 && settings?.Tabs != null)
        {
            tabEntries = settings.Tabs
                .Where(p => !string.IsNullOrEmpty(p) && Directory.Exists(p))
                .Select(p => new TabEntry { Path = p, AsRoot = false })
                .ToList();
        }
        if (tabEntries.Count == 0 && !string.IsNullOrEmpty(settings?.LastPath) && Directory.Exists(settings.LastPath))
        {
            tabEntries.Add(new TabEntry { Path = settings.LastPath!, AsRoot = false });
        }
        if (tabEntries.Count == 0)
            tabEntries.Add(new TabEntry { IsDrives = true });

        foreach (var e in tabEntries)
            Tabs.Add(new BrowserTab
            {
                Path = e.Path ?? string.Empty,
                AsRoot = e.AsRoot,
                IsDrives = e.IsDrives,
                RootPath = e.RootPath ?? string.Empty,
                ExpandedPaths = e.ExpandedPaths ?? new List<string>(),
                SelectedPath = e.SelectedPath ?? string.Empty,
                ScrollOffset = e.ScrollOffset,
                SortKey = !string.IsNullOrEmpty(e.SortKey) && Enum.TryParse<SortKey>(e.SortKey, true, out var sk2) ? sk2 : SortKey.Name,
                SortDescending = e.SortDescending ?? false,
            });
        }

        // Favorites (load BEFORE setting ActiveTab — the ActiveTab setter fires
        // OnActiveTabChanged -> SaveSettings, which would otherwise write an
        // empty Favorites list over the loaded settings).
        if (settings?.FavoriteEntries != null && settings.FavoriteEntries.Count > 0)
        {
            foreach (var fe in settings.FavoriteEntries)
            {
                if (!string.IsNullOrWhiteSpace(fe.Path))
                    Favorites.Add(new Favorite
                    {
                        Path = fe.Path,
                        SortKey = !string.IsNullOrEmpty(fe.SortKey) && Enum.TryParse<SortKey>(fe.SortKey, true, out var fsk) ? fsk : SortKey.Name,
                        SortDescending = fe.SortDescending,
                    });
            }
        }
        else if (settings?.Favorites != null)
        {
            foreach (var fp in settings.Favorites)
            {
                if (!string.IsNullOrWhiteSpace(fp))
                    Favorites.Add(new Favorite { Path = fp });
            }
        }
        Favorites.CollectionChanged += (_, _) => SaveSettings();

        if (restoredActive != null)
        {
            Raise(nameof(Tabs));
            Raise(nameof(ActiveTab));
        }
        else
        {
            var activeIdx = Math.Clamp(settings?.ActiveTabIndex ?? 0, 0, Tabs.Count - 1);
            ActiveTab = Tabs[activeIdx];
        }

        Loaded += (_, _) =>
        {
            TryEnableDragDropFromLowerIL();
        };

        Loaded += async (_, _) =>
        {
            _bulkLoading = true;
            try
            {
                foreach (var t in AllTabs().ToList())
                {
                    string navPath;
                    if (t.IsDrives)
                        navPath = string.Empty;
                    else if (t.AsRoot && !string.IsNullOrEmpty(t.RootPath))
                        navPath = t.RootPath;
                    else
                        navPath = t.Path;
                    await NavigateTabAsync(t, navPath, expandToPath: true, rootAtTarget: t.AsRoot);
                    await RestoreTabStateAsync(t);
                }
            }
            finally { _bulkLoading = false; }
        };

        Closing += (_, _) => SaveSettings();
    }

    // ---------- Settings ----------

    private sealed class Settings
    {
        public string? LastPath { get; set; }
        public List<string>? Tabs { get; set; }
        public List<TabEntry>? TabEntries { get; set; }
        public int? ActiveTabIndex { get; set; }
        public List<string>? Favorites { get; set; }
        public List<FavoriteEntry>? FavoriteEntries { get; set; }
        public double? SidebarWidth { get; set; }
        public double? NameWidth { get; set; }
        public double? ModifiedWidth { get; set; }
        public double? TypeWidth { get; set; }
        public double? SizeWidth { get; set; }
        public string? SortKey { get; set; }
        public bool? SortDescending { get; set; }
        public PaneEntry? Root { get; set; }
    }

    private sealed class FavoriteEntry
    {
        public string Path { get; set; } = string.Empty;
        public string? SortKey { get; set; }
        public bool SortDescending { get; set; }
    }

    private sealed class PaneEntry
    {
        public string Kind { get; set; } = "leaf"; // "leaf" | "split"
        // Leaf
        public List<TabEntry>? Tabs { get; set; }
        public int? ActiveTabIndex { get; set; }
        public bool IsActive { get; set; }
        // Split
        public string? Orientation { get; set; } // "Horizontal" | "Vertical"
        public List<double>? Sizes { get; set; }
        public List<PaneEntry>? Children { get; set; }
    }

    private sealed class TabEntry
    {
        public string? Path { get; set; }
        public bool AsRoot { get; set; }
        public bool IsDrives { get; set; }
        public string? RootPath { get; set; }
        public List<string>? ExpandedPaths { get; set; }
        public string? SelectedPath { get; set; }
        public double ScrollOffset { get; set; }
        public string? SortKey { get; set; }
        public bool? SortDescending { get; set; }
    }

    private static Settings? LoadSettings()
    {
        try
        {
            if (!File.Exists(SettingsPath)) return null;
            return JsonSerializer.Deserialize<Settings>(File.ReadAllText(SettingsPath),
                new JsonSerializerOptions { PropertyNameCaseInsensitive = true });
        }
        catch { return null; }
    }

    private static void ApplySettings(Settings? s)
    {
        if (s == null) return;
        if (s.NameWidth.HasValue) ColumnLayout.Instance.NamePixels = s.NameWidth.Value;
        if (s.ModifiedWidth.HasValue) ColumnLayout.Instance.ModifiedPixels = s.ModifiedWidth.Value;
        if (s.TypeWidth.HasValue) ColumnLayout.Instance.TypePixels = s.TypeWidth.Value;
        if (s.SizeWidth.HasValue) ColumnLayout.Instance.SizePixels = s.SizeWidth.Value;
        if (!string.IsNullOrEmpty(s.SortKey) &&
            Enum.TryParse<SortKey>(s.SortKey, true, out var k)) FileSystemItem.CurrentSortKey = k;
        if (s.SortDescending.HasValue) FileSystemItem.SortDescending = s.SortDescending.Value;
    }

    private void SaveSettings()
    {
        if (_bulkLoading) return;
        try
        {
            CaptureAllTabStates();
            var tabs = AllTabs().Select(t => t.Path).Where(p => !string.IsNullOrEmpty(p)).ToList();
            var tabEntries = AllTabs().Select(t => new TabEntry
                {
                    Path = t.Path,
                    AsRoot = t.AsRoot,
                    IsDrives = t.IsDrives,
                    RootPath = t.RootPath,
                    ExpandedPaths = t.ExpandedPaths,
                    SelectedPath = t.SelectedPath,
                    ScrollOffset = t.ScrollOffset,
                    SortKey = t.SortKey.ToString(),
                    SortDescending = t.SortDescending,
                })
                .Where(e => e.IsDrives || !string.IsNullOrEmpty(e.Path)).ToList();
            var activeIdx = ActiveTab != null ? Math.Max(0, AllTabs().ToList().IndexOf(ActiveTab)) : 0;
            var payload = new Settings
            {
                LastPath = ActiveTab?.Path,
                Tabs = tabs,
                TabEntries = tabEntries,
                ActiveTabIndex = activeIdx,
                NameWidth = ColumnLayout.Instance.NamePixels,
                ModifiedWidth = ColumnLayout.Instance.ModifiedPixels,
                TypeWidth = ColumnLayout.Instance.TypePixels,
                SizeWidth = ColumnLayout.Instance.SizePixels,
                SortKey = FileSystemItem.CurrentSortKey.ToString(),
                SortDescending = FileSystemItem.SortDescending,
                Favorites = Favorites.Select(f => f.Path).Where(p => !string.IsNullOrEmpty(p)).ToList(),
                FavoriteEntries = Favorites.Where(f => !string.IsNullOrEmpty(f.Path)).Select(f => new FavoriteEntry
                {
                    Path = f.Path,
                    SortKey = f.SortKey.ToString(),
                    SortDescending = f.SortDescending,
                }).ToList(),
                Root = SerializePane(_savedRoot ?? Root),
            };
            var dir = Path.GetDirectoryName(SettingsPath);
            if (!string.IsNullOrEmpty(dir)) Directory.CreateDirectory(dir);
            File.WriteAllText(SettingsPath, JsonSerializer.Serialize(payload,
                new JsonSerializerOptions { WriteIndented = true }));
        }
        catch { }
    }

    private static PaneEntry SerializePane(PaneNode node)
    {
        if (node is LeafPane l)
        {
            return new PaneEntry
            {
                Kind = "leaf",
                IsActive = l.IsActive,
                ActiveTabIndex = l.ActiveTab != null ? Math.Max(0, l.Tabs.IndexOf(l.ActiveTab)) : 0,
                Tabs = l.Tabs.Select(t => new TabEntry
                {
                    Path = t.Path,
                    AsRoot = t.AsRoot,
                    IsDrives = t.IsDrives,
                    RootPath = t.RootPath,
                    ExpandedPaths = t.ExpandedPaths,
                    SelectedPath = t.SelectedPath,
                    ScrollOffset = t.ScrollOffset,
                    SortKey = t.SortKey.ToString(),
                    SortDescending = t.SortDescending,
                }).ToList(),
            };
        }
        if (node is SplitPane s)
        {
            return new PaneEntry
            {
                Kind = "split",
                Orientation = s.Orientation.ToString(),
                Sizes = s.Sizes.ToList(),
                Children = s.Children.Select(SerializePane).ToList(),
            };
        }
        return new PaneEntry { Kind = "leaf", Tabs = new List<TabEntry>() };
    }

    private PaneNode? DeserializePane(PaneEntry? e, LeafPane[] outActive)
    {
        if (e == null) return null;
        if (string.Equals(e.Kind, "split", StringComparison.OrdinalIgnoreCase))
        {
            var sp = new SplitPane
            {
                Orientation = string.Equals(e.Orientation, "Vertical", StringComparison.OrdinalIgnoreCase)
                    ? Orientation.Vertical : Orientation.Horizontal,
            };
            foreach (var c in e.Children ?? Enumerable.Empty<PaneEntry>())
            {
                var child = DeserializePane(c, outActive);
                if (child == null) continue;
                child.Parent = sp;
                sp.Children.Add(child);
            }
            if (e.Sizes != null)
                foreach (var w in e.Sizes) sp.Sizes.Add(w > 0 ? w : 1.0);
            while (sp.Sizes.Count < sp.Children.Count) sp.Sizes.Add(1.0);
            if (sp.Children.Count == 0) return null;
            if (sp.Children.Count == 1) { sp.Children[0].Parent = null; return sp.Children[0]; }
            return sp;
        }
        // Leaf
        var leaf = new LeafPane();
        foreach (var t in e.Tabs ?? Enumerable.Empty<TabEntry>())
        {
            if (!t.IsDrives && (string.IsNullOrEmpty(t.Path) || !Directory.Exists(t.Path!))) continue;
            leaf.Tabs.Add(new BrowserTab
            {
                Path = t.Path ?? string.Empty,
                AsRoot = t.AsRoot,
                IsDrives = t.IsDrives,
                RootPath = t.RootPath ?? string.Empty,
                ExpandedPaths = t.ExpandedPaths ?? new List<string>(),
                SelectedPath = t.SelectedPath ?? string.Empty,
                ScrollOffset = t.ScrollOffset,
                SortKey = !string.IsNullOrEmpty(t.SortKey) && Enum.TryParse<SortKey>(t.SortKey, true, out var sk) ? sk : SortKey.Name,
                SortDescending = t.SortDescending ?? false,
            });
        }
        if (leaf.Tabs.Count == 0) return null;
        var idx = Math.Clamp(e.ActiveTabIndex ?? 0, 0, leaf.Tabs.Count - 1);
        leaf.ActiveTab = leaf.Tabs[idx];
        if (e.IsActive) outActive[0] = leaf;
        return leaf;
    }

    // ---------- Tab state capture / restore ----------

    private void CaptureAllTabStates()
    {
        foreach (var t in AllTabs()) CaptureTabState(t);
        if (ActiveTab != null)
        {
            var sv = FindActiveScrollViewer();
            if (sv != null) ActiveTab.ScrollOffset = sv.VerticalOffset;
        }
    }

    private static void CaptureTabState(BrowserTab tab)
    {
        // If the tree hasn't been built yet (e.g., during startup before NavigateTabAsync
        // has run), don't overwrite saved state with empty values.
        if (tab.Roots.Count == 0) return;
        if (!tab.Roots.Any(r => r.ChildrenLoaded || r.IsExpanded)) return;
        var expanded = new List<string>();
        string selected = string.Empty;
        foreach (var r in tab.Roots) CollectStateFrom(r, expanded, ref selected);
        tab.ExpandedPaths = expanded;
        tab.SelectedPath = selected;
    }

    private static void CollectStateFrom(FileSystemItem node, List<string> expanded, ref string selected)
    {
        if (node.IsSelected && !string.IsNullOrEmpty(node.Path) && node.Path != "__placeholder__")
        {
            selected = node.Path;
        }
        if (node.IsDirectory && node.IsExpanded && node.ChildrenLoaded &&
            !string.IsNullOrEmpty(node.Path) && node.Path != "__placeholder__")
        {
            expanded.Add(node.Path);
        }
        if (node.ChildrenLoaded)
        {
            foreach (var c in node.Children) CollectStateFrom(c, expanded, ref selected);
        }
    }

    private ScrollViewer? FindActiveScrollViewer()
    {
        var tree = ActivePaneTree();
        return tree != null ? FindDescendant<ScrollViewer>(tree) : null;
    }

    private async System.Threading.Tasks.Task RestoreTabStateAsync(BrowserTab tab)
    {
        if (tab.Roots.Count == 0) return;

        // Expand saved folders (parents before children so LoadChildren cascades cleanly).
        foreach (var p in tab.ExpandedPaths.Distinct().OrderBy(s => s.Length))
        {
            var root = FindRootFor(tab, p);
            if (root == null) continue;
            var node = WalkAndLoad(root, p);
            if (node != null && node.IsDirectory)
            {
                if (!node.ChildrenLoaded) node.LoadChildren();
                node.IsExpanded = true;
            }
        }

        await Dispatcher.InvokeAsync(() => { }, System.Windows.Threading.DispatcherPriority.Background);

        // Restore selection.
        if (!string.IsNullOrEmpty(tab.SelectedPath))
        {
            var root = FindRootFor(tab, tab.SelectedPath);
            if (root != null)
            {
                var sel = WalkAndLoad(root, tab.SelectedPath);
                if (sel != null) sel.IsSelected = true;
            }
        }

        // Restore scroll offset (only for the active tab — non-active tabs have no live TreeView).
        if (ReferenceEquals(tab, ActiveTab))
        {
            await Dispatcher.InvokeAsync(() => { }, System.Windows.Threading.DispatcherPriority.Background);
            var sv = FindActiveScrollViewer();
            sv?.ScrollToVerticalOffset(tab.ScrollOffset);
            await Dispatcher.InvokeAsync(() => { }, System.Windows.Threading.DispatcherPriority.Background);
            sv?.ScrollToVerticalOffset(tab.ScrollOffset);
        }
    }

    private static FileSystemItem? FindRootFor(BrowserTab tab, string path)
    {
        var norm = path.TrimEnd('\\', '/');
        foreach (var r in tab.Roots)
        {
            var rn = r.Path.TrimEnd('\\', '/');
            if (string.Equals(rn, norm, StringComparison.OrdinalIgnoreCase)) return r;
            if (norm.StartsWith(rn, StringComparison.OrdinalIgnoreCase) &&
                (norm.Length == rn.Length || norm[rn.Length] is '\\' or '/'))
            {
                return r;
            }
        }
        return null;
    }

    private static FileSystemItem? WalkAndLoad(FileSystemItem root, string targetPath)
    {
        var norm = targetPath.TrimEnd('\\', '/');
        var current = root;
        while (!string.Equals(current.Path.TrimEnd('\\', '/'), norm, StringComparison.OrdinalIgnoreCase))
        {
            if (current.IsDirectory && !current.ChildrenLoaded) current.LoadChildren();
            if (!current.IsDirectory) return null;
            var next = current.Children.FirstOrDefault(c =>
            {
                var cn = c.Path.TrimEnd('\\', '/');
                return norm.StartsWith(cn, StringComparison.OrdinalIgnoreCase) &&
                       (norm.Length == cn.Length || norm[cn.Length] is '\\' or '/');
            });
            if (next == null) return null;
            current = next;
        }
        return current;
    }

    // ---------- Drives ----------

    private static List<string> GetReadyDrivePaths()
    {
        var list = new List<string>();
        try
        {
            foreach (var d in DriveInfo.GetDrives())
            {
                if (!d.IsReady) continue;
                list.Add(d.RootDirectory.FullName);
            }
        }
        catch { }
        return list;
    }

    private static bool IsDriveRoot(string path)
    {
        if (string.IsNullOrEmpty(path)) return false;
        try
        {
            var root = Path.GetPathRoot(path);
            if (string.IsNullOrEmpty(root)) return false;
            return string.Equals(
                path.TrimEnd('\\', '/'),
                root.TrimEnd('\\', '/'),
                StringComparison.OrdinalIgnoreCase);
        }
        catch { return false; }
    }

    // ---------- Navigation ----------

    private void SetActiveTabRoot(string path)
    {
        if (ActiveTab == null) return;
        _ = NavigateTabAsync(ActiveTab, path, expandToPath: false);
    }

    private async System.Threading.Tasks.Task NavigateTabAsync(BrowserTab tab, string targetPath, bool expandToPath, bool rootAtTarget = false)
    {
        // Ensure LoadChildren (which uses the global sort statics) applies this
        // tab's sort order for any children loaded during navigation.
        FileSystemItem.CurrentSortKey = tab.SortKey;
        FileSystemItem.SortDescending = tab.SortDescending;

        // Special mode: drives tab (no path). Populate all ready drives as roots.
        if (string.IsNullOrEmpty(targetPath))
        {
            _restoring = true;
            try
            {
                // Preserve any existing drive-root FileSystemItem instances so their
                // expanded/loaded subtrees survive the re-root operation.
                var existing = tab.Roots
                    .Where(r => IsDriveRoot(r.Path))
                    .ToDictionary(r => r.Path.TrimEnd('\\', '/'), StringComparer.OrdinalIgnoreCase);
                tab.IsDrives = true;
                tab.AsRoot = false;
                tab.Path = string.Empty;
                tab.Roots.Clear();
                foreach (var d in GetReadyDrivePaths())
                {
                    var key = d.TrimEnd('\\', '/');
                    if (existing.TryGetValue(key, out var keep))
                        tab.Roots.Add(keep);
                    else
                        tab.Roots.Add(new FileSystemItem(d, isDirectory: true));
                }
                if (ReferenceEquals(tab, ActiveTab)) UpdateToolbarFromActiveTab();
                StatusText.Text = "This PC";
            }
            finally { _restoring = false; }
            return;
        }

        if (!Directory.Exists(targetPath))
        {
            StatusText.Text = $"Folder not found: {targetPath}";
            return;
        }

        _restoring = true;
        try
        {
            tab.IsDrives = false;
            tab.AsRoot = rootAtTarget;
            if (rootAtTarget) tab.RootPath = targetPath;
            else tab.RootPath = string.Empty;
            string driveRoot;
            if (rootAtTarget)
            {
                driveRoot = targetPath;
            }
            else
            {
                driveRoot = Path.GetPathRoot(targetPath) ?? string.Empty;
                if (string.IsNullOrEmpty(driveRoot) || !Directory.Exists(driveRoot)) return;
            }

            // Preserve existing FileSystemItem instances for the drive root (and any
            // other roots currently shown) so their expanded subtrees survive re-rooting.
            var existingRoots = tab.Roots
                .Where(r => !string.IsNullOrEmpty(r.Path))
                .GroupBy(r => r.Path.TrimEnd('\\', '/'), StringComparer.OrdinalIgnoreCase)
                .ToDictionary(g => g.Key, g => g.First(), StringComparer.OrdinalIgnoreCase);
            tab.Roots.Clear();
            var driveKey = driveRoot.TrimEnd('\\', '/');
            if (existingRoots.TryGetValue(driveKey, out var keepRoot))
                tab.Roots.Add(keepRoot);
            else
                tab.Roots.Add(new FileSystemItem(driveRoot, isDirectory: true));
            tab.Path = targetPath;

            if (ReferenceEquals(tab, ActiveTab)) UpdateToolbarFromActiveTab();

            if (rootAtTarget)
            {
                var rootNode = tab.Roots[0];
                if (!rootNode.ChildrenLoaded) rootNode.LoadChildren();
                rootNode.IsExpanded = true;
                if (ReferenceEquals(tab, ActiveTab))
                {
                    await System.Threading.Tasks.Task.Delay(50);
                    await System.Windows.Threading.Dispatcher.Yield(System.Windows.Threading.DispatcherPriority.ApplicationIdle);
                    UpdateLayout();
                    var tree = ActivePaneTree();
                    var sv = tree != null ? FindDescendant<ScrollViewer>(tree) : null;
                    sv?.ScrollToTop();
                }
                return;
            }

            if (!expandToPath && string.Equals(targetPath.TrimEnd('\\', '/'), driveRoot.TrimEnd('\\', '/'),
                                                StringComparison.OrdinalIgnoreCase))
            {
                return;
            }

            // Expand tree down to targetPath.
            var segments = new List<string>();
            var rootTrim = driveRoot.TrimEnd('\\', '/');
            string? current = targetPath;
            while (!string.IsNullOrEmpty(current))
            {
                var t = current.TrimEnd('\\', '/');
                if (string.Equals(t, rootTrim, StringComparison.OrdinalIgnoreCase)) break;
                segments.Add(t);
                current = Path.GetDirectoryName(current);
            }
            segments.Reverse();

            FileSystemItem? node = tab.Roots.FirstOrDefault();
            if (node == null) return;

            foreach (var seg in segments)
            {
                if (!node.ChildrenLoaded) node.LoadChildren();
                node.IsExpanded = true;
                await System.Windows.Threading.Dispatcher.Yield(System.Windows.Threading.DispatcherPriority.Background);

                var next = node.Children.FirstOrDefault(c =>
                    string.Equals(c.Path.TrimEnd('\\', '/'), seg, StringComparison.OrdinalIgnoreCase));
                if (next == null) break;
                node = next;
            }

            if (node != null)
            {
                tab.Path = node.Path;
                if (node.IsDirectory)
                {
                    if (!node.ChildrenLoaded) node.LoadChildren();
                    node.IsExpanded = true;
                }
                node.IsSelected = true;
                await System.Windows.Threading.Dispatcher.Yield(System.Windows.Threading.DispatcherPriority.Background);
                await BringItemIntoViewAsync(tab, node);
            }
            StatusText.Text = $"Tab: {tab.Path}";
        }
        finally
        {
            _restoring = false;
        }
    }

    private async void OnActiveTabChanged()
    {
        UpdateToolbarFromActiveTab();
        // Sync the global sort statics and column header arrows to this tab's sort.
        if (ActiveTab != null)
        {
            FileSystemItem.CurrentSortKey = ActiveTab.SortKey;
            FileSystemItem.SortDescending = ActiveTab.SortDescending;
            SortState.Instance.Refresh();
        }
        SaveSettings();
        // Restore scroll position of the newly active tab (its TreeView is realized now).
        var tab = ActiveTab;
        if (tab != null)
        {
            await Dispatcher.InvokeAsync(() => { }, System.Windows.Threading.DispatcherPriority.Background);
            var sv = FindActiveScrollViewer();
            if (sv != null && ReferenceEquals(tab, ActiveTab)) sv.ScrollToVerticalOffset(tab.ScrollOffset);
        }
    }

    private void UpdateToolbarFromActiveTab()
    {
        if (ActiveTab == null) return;
        _syncingToolbar = true;
        try
        {
            PathBox.Text = ActiveTab.Path;
        }
        finally { _syncingToolbar = false; }
    }

    // ---------- Tab lifecycle ----------

    private void NewTab_Click(object sender, RoutedEventArgs e)
    {
        // Resolve the pane that owns the + button.
        var pane = ActivePane;
        if (sender is FrameworkElement fe)
        {
            DependencyObject? o = fe;
            while (o != null)
            {
                if (o is FrameworkElement f && f.DataContext is LeafPane lp) { pane = lp; break; }
                o = System.Windows.Media.VisualTreeHelper.GetParent(o);
            }
        }
        var tab = new BrowserTab { IsDrives = true };
        pane.Tabs.Add(tab);
        ActivePane = pane;
        pane.ActiveTab = tab;
        Raise(nameof(ActiveTab));
        OnActiveTabChanged();
        _ = NavigateTabAsync(tab, string.Empty, expandToPath: false);
        SaveSettings();
    }

    private void CloseTab_Click(object sender, RoutedEventArgs e)
    {
        if (sender is Button btn && btn.Tag is BrowserTab tab)
            CloseTabCore(tab);
        e.Handled = true;
    }

    private void CloseTabCore(BrowserTab tab)
    {
        // If we are closing the currently maximized tab, restore layout first so
        // we can delete it cleanly from its real owning pane.
        if (ReferenceEquals(_maximizedTab, tab)) RestoreMaximizedTab();
        var pane = FindPaneOfTab(tab);
        if (pane == null) return;
        // Require at least one tab overall.
        if (AllTabs().Count() <= 1) return;
        var idx = pane.Tabs.IndexOf(tab);
        if (idx < 0) return;
        var wasActive = ReferenceEquals(tab, pane.ActiveTab);
        pane.Tabs.Remove(tab);
        if (pane.Tabs.Count == 0)
        {
            CollapseIfEmpty(pane);
        }
        else if (wasActive)
        {
            pane.ActiveTab = pane.Tabs[Math.Max(0, Math.Min(idx, pane.Tabs.Count - 1))];
            if (ReferenceEquals(pane, ActivePane)) { Raise(nameof(ActiveTab)); OnActiveTabChanged(); }
        }
        SaveSettings();
    }

    private static BrowserTab? TabFromSender(object sender) =>
        (sender as FrameworkElement)?.Tag as BrowserTab;

    private void TabMenuClose_Click(object sender, RoutedEventArgs e)
    {
        if (TabFromSender(sender) is BrowserTab t) CloseTabCore(t);
    }

    private void TabMenuCloseOthers_Click(object sender, RoutedEventArgs e)
    {
        if (TabFromSender(sender) is not BrowserTab keep) return;
        foreach (var t in Tabs.Where(x => !ReferenceEquals(x, keep)).ToList())
            CloseTabCore(t);
    }

    private void TabMenuCloseLeft_Click(object sender, RoutedEventArgs e)
    {
        if (TabFromSender(sender) is not BrowserTab pivot) return;
        var idx = Tabs.IndexOf(pivot);
        if (idx <= 0) return;
        foreach (var t in Tabs.Take(idx).ToList())
            CloseTabCore(t);
    }

    private void TabMenuCloseRight_Click(object sender, RoutedEventArgs e)
    {
        if (TabFromSender(sender) is not BrowserTab pivot) return;
        var idx = Tabs.IndexOf(pivot);
        if (idx < 0 || idx >= Tabs.Count - 1) return;
        foreach (var t in Tabs.Skip(idx + 1).ToList())
            CloseTabCore(t);
    }

    private void LeafTabControl_SelectionChanged(object sender, SelectionChangedEventArgs e)
    {
        if (sender is not TabControl tc) return;
        if (tc.DataContext is not LeafPane pane) return;
        bool activeChanged = !ReferenceEquals(ActivePane, pane);
        if (activeChanged) ActivePane = pane;
        if (tc.SelectedItem is BrowserTab t)
        {
            if (!ReferenceEquals(pane.ActiveTab, t) && pane.ActiveTab != null)
            {
                var oldTree = TreeForTab(pane.ActiveTab);
                var sv = oldTree != null ? FindDescendant<ScrollViewer>(oldTree) : null;
                if (sv != null) pane.ActiveTab.ScrollOffset = sv.VerticalOffset;
            }
            pane.ActiveTab = t;
            if (!activeChanged) { Raise(nameof(ActiveTab)); OnActiveTabChanged(); }
        }
    }

    private void LeafTabControl_GotFocus(object sender, RoutedEventArgs e)
    {
        if (sender is TabControl tc && tc.DataContext is LeafPane pane && !ReferenceEquals(ActivePane, pane))
            ActivePane = pane;
    }

    private void LeafPaneHost_PreviewMouseDown(object sender, MouseButtonEventArgs e)
    {
        if (sender is FrameworkElement fe && fe.DataContext is LeafPane pane && !ReferenceEquals(ActivePane, pane))
            ActivePane = pane;
    }

    // ---------- Toolbar ----------

    private void Refresh_Click(object sender, RoutedEventArgs e)
    {
        if (ActiveTab == null) return;
        // Reload children of every expanded node under every root, without
        // re-rooting the tab. Re-rooting would change which folder is shown as
        // the top-level — which is not what "refresh" should do.
        foreach (var root in ActiveTab.Roots)
            RefreshLoadedSubtree(root);
    }

    private static void RefreshLoadedSubtree(FileSystemItem node)
    {
        if (node.IsDirectory && node.ChildrenLoaded)
        {
            var wasExpanded = node.IsExpanded;
            node.LoadChildren();
            node.IsExpanded = wasExpanded;
            foreach (var c in node.Children)
                RefreshLoadedSubtree(c);
        }
    }

    private void PathBox_KeyDown(object sender, KeyEventArgs e)
    {
        if (e.Key == Key.Enter && ActiveTab != null)
        {
            _ = NavigateTabAsync(ActiveTab, PathBox.Text?.Trim() ?? string.Empty, expandToPath: true);
            e.Handled = true;
        }
    }

    private void Browse_Click(object sender, RoutedEventArgs e)
    {
        var dlg = new Microsoft.Win32.OpenFolderDialog();
        if (dlg.ShowDialog(this) == true && ActiveTab != null)
        {
            _ = NavigateTabAsync(ActiveTab, dlg.FolderName, expandToPath: true);
        }
    }

    // ---------- Tree events ----------

    private bool _allowBringIntoView;
    private void TreeViewItem_RequestBringIntoView(object sender, RequestBringIntoViewEventArgs e)
    {
        if (!_allowBringIntoView) e.Handled = true;
    }

    private void TreeViewItem_Expanded(object sender, RoutedEventArgs e)
    {
        if (sender is TreeViewItem tvi && tvi.DataContext is FileSystemItem item && !item.ChildrenLoaded)
        {
            item.LoadChildren();
        }
    }

    private void Tree_SelectedItemChanged(object sender, RoutedPropertyChangedEventArgs<object> e)
    {
        if (_restoring) return;
        if (sender is not TreeView tv || tv.DataContext is not BrowserTab tab) return;
        if (e.NewValue is not FileSystemItem item) return;

        var savePath = item.IsDirectory ? item.Path : Path.GetDirectoryName(item.Path);
        if (string.IsNullOrEmpty(savePath)) return;

        tab.Path = savePath;
        if (ReferenceEquals(tab, ActiveTab)) UpdateToolbarFromActiveTab();
        SaveSettings();
    }

    // ---------- Shell context menu ----------

    private async void Tree_PreviewMouseRightButtonUp(object sender, MouseButtonEventArgs e)
    {
        var tvi = FindAncestor<TreeViewItem>(e.OriginalSource as DependencyObject);
        if (tvi?.DataContext is not FileSystemItem item) return;
        tvi.IsSelected = true;

        var hwnd = new WindowInteropHelper(this).Handle;
        var screen = PointToScreen(e.GetPosition(this));
        var created = ShellContextMenu.Show(hwnd, item.Path, (int)screen.X, (int)screen.Y,
            extendedVerbs: (Keyboard.Modifiers & ModifierKeys.Shift) != 0,
            isDirectory: item.IsDirectory);
        e.Handled = true;

        if (!string.IsNullOrEmpty(created))
        {
            if (created == "__expand_all__")
            {
                ExpandAllRecursively(item);
                return;
            }
            if (created == "__collapse_all__")
            {
                CollapseAllRecursively(item);
                return;
            }
            PushUndo(new UndoEntry
            {
                Kind = UndoKind.Create,
                DestPath = created,
                IsDirectory = Directory.Exists(created),
                Label = $"Create {Path.GetFileName(created)}",
            });
            // New item was created inside item.Path; refresh that folder, select the new child.
            await RefreshParentOfAsync(item.Path);
            if (ActiveTab != null && ActiveTab.Roots.Count > 0)
            {
                var newItem = FindNodeByPath(ActiveTab.Roots[0], created);
                if (newItem != null)
                {
                    newItem.IsSelected = true;
                    await FocusContainerAsync(ActiveTab, newItem);
                    TryRename(newItem);
                }
            }
        }
    }

    private static T? FindAncestor<T>(DependencyObject? d) where T : DependencyObject
    {
        while (d != null)
        {
            if (d is T t) return t;
            d = System.Windows.Media.VisualTreeHelper.GetParent(d);
        }
        return null;
    }

    private static void ExpandAllRecursively(FileSystemItem item)
    {
        if (!item.IsDirectory) return;
        if (!item.ChildrenLoaded) item.LoadChildren();
        item.IsExpanded = true;
        foreach (var c in item.Children)
            ExpandAllRecursively(c);
    }

    private static void CollapseAllRecursively(FileSystemItem item)
    {
        if (!item.IsDirectory) return;
        item.IsExpanded = false;
        if (!item.ChildrenLoaded) return;
        foreach (var c in item.Children)
            CollapseAllRecursively(c);
    }

    // ---------- Tab drag-hover switch ----------

    private System.Windows.Threading.DispatcherTimer? _tabHoverTimer;
    private BrowserTab? _tabHoverTarget;

    private void TabItem_DragEnter(object sender, DragEventArgs e)
    {
        if (sender is not TabItem ti) return;
        if (ti.DataContext is not BrowserTab tab) return;

        bool isFile = e.Data.GetDataPresent(DataFormats.FileDrop);
        bool isTab = e.Data.GetDataPresent(TabDragFormat);
        if (!isFile && !isTab) return;

        if (isTab)
        {
            // While dragging a tab over another tab header: move/reorder. Mark effect.
            e.Effects = DragDropEffects.Move;
            e.Handled = true;
            return;
        }

        if (ReferenceEquals(tab, ActiveTab)) return;
        if (ReferenceEquals(_tabHoverTarget, tab)) return;

        _tabHoverTarget = tab;
        _tabHoverTimer?.Stop();
        _tabHoverTimer = new System.Windows.Threading.DispatcherTimer
        {
            Interval = TimeSpan.FromMilliseconds(500),
        };
        _tabHoverTimer.Tick += (_, _) =>
        {
            _tabHoverTimer?.Stop();
            if (_tabHoverTarget != null)
            {
                // Find which pane hosts this tab and activate it.
                foreach (var p in AllLeaves())
                {
                    if (p.Tabs.Contains(_tabHoverTarget))
                    {
                        ActivePane = p;
                        p.ActiveTab = _tabHoverTarget;
                        Raise(nameof(ActiveTab));
                        break;
                    }
                }
            }
            _tabHoverTarget = null;
        };
        _tabHoverTimer.Start();
    }

    private void TabItem_DragLeave(object sender, DragEventArgs e)
    {
        if (sender is not TabItem ti) return;
        if (!ReferenceEquals(ti.DataContext, _tabHoverTarget)) return;
        _tabHoverTimer?.Stop();
        _tabHoverTimer = null;
        _tabHoverTarget = null;
    }

    // ---------- Tab drag (reorder / move between panes / split) ----------

    private const string TabDragFormat = "FileBrowser.AppTab";

    private sealed class TabDragPayload
    {
        public BrowserTab Tab { get; init; } = null!;
        public LeafPane SourcePane { get; init; } = null!;
    }

    private Point _tabDragStart;
    private BrowserTab? _tabDragCandidate;

    private void TabItem_MouseDoubleClick(object sender, MouseButtonEventArgs e)
    {
        if (sender is not TabItem ti) return;
        if (ti.DataContext is not BrowserTab tab) return;
        // Only left-button double-clicks toggle maximize. Ignore middle-click (close).
        if (e.ChangedButton != MouseButton.Left) return;
        ToggleMaximizeTab(tab);
        // Cancel any drag candidate initialized by the preceding MouseDown so the
        // double-click gesture does not also fire a drag.
        _tabDragCandidate = null;
        e.Handled = true;
    }

    private void TabItem_PreviewMouseLeftButtonDown(object sender, MouseButtonEventArgs e)
    {
        if (sender is not TabItem ti) return;
        if (ti.DataContext is not BrowserTab tab) return;
        _tabDragStart = e.GetPosition(this);
        _tabDragCandidate = tab;
    }

    private void TabItem_PreviewMouseMove(object sender, MouseEventArgs e)
    {
        if (_tabDragCandidate == null) return;
        if (e.LeftButton != MouseButtonState.Pressed) { _tabDragCandidate = null; return; }
        var pos = e.GetPosition(this);
        if (Math.Abs(pos.X - _tabDragStart.X) < SystemParameters.MinimumHorizontalDragDistance &&
            Math.Abs(pos.Y - _tabDragStart.Y) < SystemParameters.MinimumVerticalDragDistance)
            return;
        if (sender is not TabItem ti) return;

        var draggedTab = _tabDragCandidate;
        _tabDragCandidate = null;

        // If a tab is currently maximized, un-maximize for the duration of the
        // drag so the user can see (and drop into) the other panes, then re-
        // maximize the same tab once the drag completes.
        bool wasMaximized = ReferenceEquals(_maximizedTab, draggedTab);
        if (wasMaximized) RestoreMaximizedTab();

        var sourcePane = FindPaneOfTab(draggedTab);
        if (sourcePane == null)
        {
            if (wasMaximized) MaximizeTab(draggedTab);
            return;
        }

        var payload = new TabDragPayload { Tab = draggedTab, SourcePane = sourcePane };
        var data = new DataObject();
        data.SetData(TabDragFormat, payload);
        try { DragDrop.DoDragDrop(ti, data, DragDropEffects.Move); }
        catch { }

        if (wasMaximized) MaximizeTab(draggedTab);
    }

    private enum DropZone { None, Center, Left, Right, Top, Bottom, TabStrip }

    private DropZone _dropZone = DropZone.None;
    private LeafPane? _dropLeaf;
    private Border? _activeOverlay;

    private void LeafPane_DragOver(object sender, DragEventArgs e)
    {
        if (!e.Data.GetDataPresent(TabDragFormat)) return;
        if (sender is not TabControl tc) return;
        if (tc.DataContext is not LeafPane pane) return;

        var payload = e.Data.GetData(TabDragFormat) as TabDragPayload;
        if (payload == null) return;

        var contentArea = FindDescendant<ContentPresenter>(tc);
        var tabStrip = FindDescendant<TabPanel>(tc);
        var overlay = FindDescendant<Border>(tc, b => b.Name == "DropOverlay");
        if (contentArea == null) return;

        var tcPos = e.GetPosition(tc);
        DropZone zone;
        Rect overlayRect;

        // TabStrip zone test
        if (tabStrip != null)
        {
            var stripRect = TransformBounds(tabStrip, tc);
            if (stripRect.Contains(tcPos))
            {
                zone = DropZone.TabStrip;
                overlayRect = stripRect;
                goto apply;
            }
        }

        var contentRect = TransformBounds(contentArea, tc);
        if (!contentRect.Contains(tcPos))
        {
            ClearDropOverlay();
            return;
        }

        double rx = (tcPos.X - contentRect.X) / Math.Max(1, contentRect.Width);
        double ry = (tcPos.Y - contentRect.Y) / Math.Max(1, contentRect.Height);
        const double edge = 0.25;

        if (rx < edge)      { zone = DropZone.Left;   overlayRect = new Rect(contentRect.X, contentRect.Y, contentRect.Width * 0.5, contentRect.Height); }
        else if (rx > 1-edge){ zone = DropZone.Right;  overlayRect = new Rect(contentRect.X + contentRect.Width * 0.5, contentRect.Y, contentRect.Width * 0.5, contentRect.Height); }
        else if (ry < edge) { zone = DropZone.Top;    overlayRect = new Rect(contentRect.X, contentRect.Y, contentRect.Width, contentRect.Height * 0.5); }
        else if (ry > 1-edge){ zone = DropZone.Bottom; overlayRect = new Rect(contentRect.X, contentRect.Y + contentRect.Height * 0.5, contentRect.Width, contentRect.Height * 0.5); }
        else                { zone = DropZone.Center; overlayRect = contentRect; }

    apply:
        _dropZone = zone;
        _dropLeaf = pane;
        if (overlay != null)
        {
            overlay.Visibility = Visibility.Visible;
            overlay.Width = overlayRect.Width;
            overlay.Height = overlayRect.Height;
            overlay.HorizontalAlignment = HorizontalAlignment.Left;
            overlay.VerticalAlignment = VerticalAlignment.Top;
            overlay.Margin = new Thickness(overlayRect.X, overlayRect.Y, 0, 0);
            _activeOverlay = overlay;
        }
        e.Effects = DragDropEffects.Move;
        e.Handled = true;
    }

    private void LeafPane_DragLeave(object sender, DragEventArgs e)
    {
        // Only clear when leaving the TabControl entirely.
        if (sender is TabControl tc)
        {
            var pos = e.GetPosition(tc);
            if (pos.X < 0 || pos.Y < 0 || pos.X > tc.ActualWidth || pos.Y > tc.ActualHeight)
                ClearDropOverlay();
        }
    }

    private void ClearDropOverlay()
    {
        if (_activeOverlay != null) _activeOverlay.Visibility = Visibility.Collapsed;
        _activeOverlay = null;
        _dropZone = DropZone.None;
        _dropLeaf = null;
    }

    private void LeafPane_Drop(object sender, DragEventArgs e)
    {
        if (!e.Data.GetDataPresent(TabDragFormat)) return;
        e.Handled = true;

        var payload = e.Data.GetData(TabDragFormat) as TabDragPayload;
        var zone = _dropZone;
        var targetPane = _dropLeaf;
        ClearDropOverlay();
        if (payload == null || targetPane == null || zone == DropZone.None) return;

        var tab = payload.Tab;
        var source = payload.SourcePane;

        if (ReferenceEquals(source, targetPane) && (zone == DropZone.Center || zone == DropZone.TabStrip))
            return; // no-op

        if (zone == DropZone.Center || zone == DropZone.TabStrip)
        {
            MoveTabToPane(tab, source, targetPane);
            return;
        }

        // Split-based drops
        Orientation ori = (zone == DropZone.Left || zone == DropZone.Right) ? Orientation.Horizontal : Orientation.Vertical;
        bool before = zone == DropZone.Left || zone == DropZone.Top;
        SplitPaneWithTab(tab, source, targetPane, ori, before);
    }

    private LeafPane? FindPaneOfTab(BrowserTab tab)
    {
        foreach (var p in AllLeaves()) if (p.Tabs.Contains(tab)) return p;
        return null;
    }

    private void MoveTabToPane(BrowserTab tab, LeafPane source, LeafPane target)
    {
        if (ReferenceEquals(source, target)) return;
        source.Tabs.Remove(tab);
        target.Tabs.Add(tab);
        target.ActiveTab = tab;
        ActivePane = target;
        Raise(nameof(ActiveTab));
        OnActiveTabChanged();
        CollapseIfEmpty(source);
        SaveSettings();
    }

    private void SplitPaneWithTab(BrowserTab tab, LeafPane source, LeafPane target, Orientation ori, bool placeNewBefore)
    {
        // If the drag would leave the source empty, pre-populate it with a placeholder so the split is visible.
        bool sourceWillEmpty = ReferenceEquals(source, target)
            ? source.Tabs.Count <= 1
            : source.Tabs.Count <= 1;
        BrowserTab? placeholder = null;
        if (sourceWillEmpty)
        {
            placeholder = new BrowserTab { IsDrives = true };
            source.Tabs.Insert(0, placeholder);
            source.ActiveTab = placeholder;
            _ = NavigateTabAsync(placeholder, string.Empty, expandToPath: false);
        }

        var newLeaf = new LeafPane();
        source.Tabs.Remove(tab);
        newLeaf.Tabs.Add(tab);
        newLeaf.ActiveTab = tab;

        // Insert newLeaf next to target, creating/reusing a SplitPane of matching orientation.
        var parent = target.Parent;
        if (parent != null && parent.Orientation == ori)
        {
            int idx = parent.Children.IndexOf(target);
            int insertAt = placeNewBefore ? idx : idx + 1;
            newLeaf.Parent = parent;
            parent.Children.Insert(insertAt, newLeaf);
            parent.Sizes.Insert(insertAt, 1.0);
        }
        else
        {
            var split = new SplitPane { Orientation = ori };
            // Replace target with split in its parent (or as Root).
            if (parent == null)
            {
                // target == Root
                Root = split;
            }
            else
            {
                int idx = parent.Children.IndexOf(target);
                parent.Children[idx] = split;
                split.Parent = parent;
            }
            target.Parent = split;
            newLeaf.Parent = split;
            if (placeNewBefore)
            {
                split.Children.Add(newLeaf);
                split.Children.Add(target);
                split.Sizes.Add(1.0);
                split.Sizes.Add(1.0);
            }
            else
            {
                split.Children.Add(target);
                split.Children.Add(newLeaf);
                split.Sizes.Add(1.0);
                split.Sizes.Add(1.0);
            }
        }

        ActivePane = newLeaf;
        Raise(nameof(ActiveTab));
        OnActiveTabChanged();
        CollapseIfEmpty(source);
        SaveSettings();
    }

    private void CollapseIfEmpty(LeafPane leaf)
    {
        if (leaf.Tabs.Count > 0) return;
        var parent = leaf.Parent;
        if (parent == null)
        {
            // Root leaf became empty: add a placeholder drives tab so the app never has zero tabs.
            leaf.Tabs.Add(new BrowserTab { IsDrives = true });
            leaf.ActiveTab = leaf.Tabs[0];
            _ = NavigateTabAsync(leaf.Tabs[0], string.Empty, expandToPath: false);
            return;
        }
        int idx = parent.Children.IndexOf(leaf);
        if (idx < 0) return;
        parent.Children.RemoveAt(idx);
        if (idx < parent.Sizes.Count) parent.Sizes.RemoveAt(idx);

        // If parent split now has only 1 child, collapse it into that child.
        if (parent.Children.Count == 1)
        {
            var only = parent.Children[0];
            parent.Children.Clear();
            var grand = parent.Parent;
            only.Parent = grand;
            if (grand == null)
            {
                Root = only;
            }
            else
            {
                int gidx = grand.Children.IndexOf(parent);
                if (gidx >= 0) grand.Children[gidx] = only;
            }
        }
        else if (parent.Children.Count == 0)
        {
            // Shouldn't happen, but cleanup anyway.
            var grand = parent.Parent;
            if (grand != null) { int gidx = grand.Children.IndexOf(parent); if (gidx >= 0) { grand.Children.RemoveAt(gidx); if (gidx < grand.Sizes.Count) grand.Sizes.RemoveAt(gidx); } }
        }

        // If we just removed the active pane, pick another.
        if (ReferenceEquals(ActivePane, leaf))
        {
            var first = AllLeaves().FirstOrDefault();
            if (first != null) ActivePane = first;
        }
    }

    private static Rect TransformBounds(FrameworkElement element, Visual relativeTo)
    {
        var origin = element.TransformToAncestor(relativeTo).Transform(new Point(0, 0));
        return new Rect(origin.X, origin.Y, element.ActualWidth, element.ActualHeight);
    }

    private static Border? FindDescendant<T>(DependencyObject root, System.Func<Border, bool> filter) where T : Border
    {
        foreach (var b in FindVisualChildren<Border>(root))
            if (filter(b)) return b;
        return null;
    }

    // Tab context menu: split. The invoked tab stays exactly where it is; a new
    // pane with a "This PC" tab is added to the right / below.
    private void TabMenuSplitRight_Click(object sender, RoutedEventArgs e)
    {
        if (sender is MenuItem mi && mi.Tag is BrowserTab tab)
        {
            var src = FindPaneOfTab(tab);
            if (src != null) AddNewPaneBeside(src, Orientation.Horizontal);
        }
    }

    private void TabMenuSplitDown_Click(object sender, RoutedEventArgs e)
    {
        if (sender is MenuItem mi && mi.Tag is BrowserTab tab)
        {
            var src = FindPaneOfTab(tab);
            if (src != null) AddNewPaneBeside(src, Orientation.Vertical);
        }
    }

    private void AddNewPaneBeside(LeafPane target, Orientation ori)
    {
        var newTab = new BrowserTab { IsDrives = true };
        var newLeaf = new LeafPane();
        newLeaf.Tabs.Add(newTab);
        newLeaf.ActiveTab = newTab;

        var parent = target.Parent;
        if (parent != null && parent.Orientation == ori)
        {
            // Parent already splits in the right direction — just insert the
            // new leaf next to target. SplitPaneView preserves target's visual
            // subtree across this insert.
            int idx = parent.Children.IndexOf(target);
            newLeaf.Parent = parent;
            parent.Children.Insert(idx + 1, newLeaf);
            parent.Sizes.Insert(idx + 1, 1.0);
        }
        else
        {
            // Need to wrap target in a new SplitPane. To keep target's visual
            // subtree intact, build the new split OFF-TREE first (populated
            // with target + newLeaf), then attach it in a single swap.
            var split = new SplitPane { Orientation = ori };
            // Detach target from its current parent (if any) without causing a
            // visual rebuild of target itself. Since we are about to re-attach
            // it inside `split`, its ContentPresenter will be re-parented by
            // SplitPaneView's presenter cache.
            target.Parent = split;
            newLeaf.Parent = split;
            split.Children.Add(target);
            split.Children.Add(newLeaf);
            split.Sizes.Add(1.0);
            split.Sizes.Add(1.0);

            if (parent == null)
            {
                // target was Root.
                Root = split;
            }
            else
            {
                int idx = parent.Children.IndexOf(target);
                split.Parent = parent;
                // Replace target with split in-place. Because target is now a
                // child of split, and split's presenter cache reuses target's
                // existing ContentPresenter, target's TreeView is not rebuilt.
                parent.Children[idx] = split;
            }
        }

        // Keep focus on the original pane so the current tab's selection/scroll
        // are not disturbed. The new "This PC" tab is created but not activated.
        ActivePane = target;
        _ = NavigateTabAsync(newTab, string.Empty, expandToPath: false);
        SaveSettings();
    }

    // ---------- Drag & drop ----------

    // Allow drag-drop from lower-IL processes (e.g. Explorer) when our app runs elevated.
    [System.Runtime.InteropServices.DllImport("user32.dll")]
    private static extern bool ChangeWindowMessageFilterEx(
        IntPtr hWnd, uint msg, uint action, IntPtr extra);
    private const uint MSGFLT_ALLOW = 1;
    private const uint WM_DROPFILES = 0x0233;
    private const uint WM_COPYDATA = 0x004A;
    private const uint WM_COPYGLOBALDATA = 0x0049;

    private void TryEnableDragDropFromLowerIL()
    {
        try
        {
            var h = new System.Windows.Interop.WindowInteropHelper(this).Handle;
            if (h == IntPtr.Zero) return;
            ChangeWindowMessageFilterEx(h, WM_DROPFILES, MSGFLT_ALLOW, IntPtr.Zero);
            ChangeWindowMessageFilterEx(h, WM_COPYDATA, MSGFLT_ALLOW, IntPtr.Zero);
            ChangeWindowMessageFilterEx(h, WM_COPYGLOBALDATA, MSGFLT_ALLOW, IntPtr.Zero);
        }
        catch { /* non-fatal */ }
    }

    private System.Windows.Point _dragStart;
    private FileSystemItem? _dragItem;
    private FileSystemItem? _dropHighlight;

    private void Tree_PreviewMouseLeftButtonDown(object sender, MouseButtonEventArgs e)
    {
        _dragStart = e.GetPosition(null);
        var tvi = FindAncestor<TreeViewItem>(e.OriginalSource as DependencyObject);
        var item = tvi?.DataContext as FileSystemItem;
        _dragItem = item;
        if (item == null) return;

        // Ignore clicks on the expander chevron so it can still toggle.
        if (FindAncestor<System.Windows.Controls.Primitives.ToggleButton>(e.OriginalSource as DependencyObject) != null)
            return;

        var tab = (sender as TreeView)?.DataContext as BrowserTab;
        if (tab == null) return;
        var mods = Keyboard.Modifiers;

        if ((mods & ModifierKeys.Shift) != 0)
        {
            var anchor = tab.SelectionAnchor ?? (sender as TreeView)?.SelectedItem as FileSystemItem;
            if (anchor != null)
            {
                tab.SelectionAnchor = anchor;
                ExtendMultiSelection(tab, item);
                e.Handled = true;
                tvi!.Focus();
                return;
            }
        }
        else if ((mods & ModifierKeys.Control) != 0)
        {
            // Seed multi-selection with the currently TreeView-selected item so that
            // Ctrl+click after a normal click preserves both items as selected.
            if (tab.MultiSelection.Count == 0 && (sender as TreeView)?.SelectedItem is FileSystemItem cur)
            {
                cur.IsMultiSelected = true;
                tab.MultiSelection.Add(cur);
                if (tab.SelectionAnchor == null) tab.SelectionAnchor = cur;
            }
            ToggleMultiSelection(tab, item);
            tab.SelectionAnchor = item;
            // Suppress WPF's built-in TreeViewItem selection which fires on the
            // bubbling MouseLeftButtonDown and would re-select the item we just
            // toggled off. We need to mark the event Handled AND manually clear
            // the TreeViewItem.IsSelected so the TwoWay binding doesn't override
            // our IsMultiSelected state.
            e.Handled = true;
            if (!item.IsMultiSelected)
            {
                tvi!.IsSelected = false;
                item.IsSelected = false;
            }
            tvi!.Focus();
        }
        else
        {
            // If the clicked item is already part of a multi-selection, don't collapse
            // the selection yet — wait for mouse-up without drag (so drag-drop of
            // multi-selection continues to work). We just set the anchor.
            if (!item.IsMultiSelected)
            {
                ClearMultiSelection(tab);
            }
            tab.SelectionAnchor = item;
            // Let default TreeView single-select continue.
        }
    }

    private void Tree_PreviewMouseLeftButtonUp(object sender, MouseButtonEventArgs e)
    {
        // Complete the "click on already-multi-selected item without drag" gesture:
        // collapse to single selection.
        if (_dragItem == null) return;
        var pos = e.GetPosition(null);
        if (Math.Abs(pos.X - _dragStart.X) >= SystemParameters.MinimumHorizontalDragDistance ||
            Math.Abs(pos.Y - _dragStart.Y) >= SystemParameters.MinimumVerticalDragDistance)
        {
            _dragItem = null;
            return;
        }
        var mods = Keyboard.Modifiers;
        if ((mods & (ModifierKeys.Shift | ModifierKeys.Control)) != 0) { _dragItem = null; return; }
        var tab = (sender as TreeView)?.DataContext as BrowserTab;
        if (tab != null && _dragItem.IsMultiSelected && tab.MultiSelection.Count > 1)
        {
            ClearMultiSelection(tab);
            _dragItem.IsMultiSelected = true;
            tab.MultiSelection.Add(_dragItem);
            tab.SelectionAnchor = _dragItem;
        }
        _dragItem = null;
    }

    private void ClearMultiSelection(BrowserTab tab)
    {
        foreach (var i in tab.MultiSelection) i.IsMultiSelected = false;
        tab.MultiSelection.Clear();
    }

    private void ToggleMultiSelection(BrowserTab tab, FileSystemItem item)
    {
        if (item.IsMultiSelected)
        {
            item.IsMultiSelected = false;
            tab.MultiSelection.Remove(item);
        }
        else
        {
            item.IsMultiSelected = true;
            tab.MultiSelection.Add(item);
        }
    }

    private void ExtendMultiSelection(BrowserTab tab, FileSystemItem to)
    {
        var flat = FlattenVisible(tab);
        int a = tab.SelectionAnchor == null ? -1 : flat.IndexOf(tab.SelectionAnchor);
        int b = flat.IndexOf(to);
        if (a < 0 || b < 0) return;
        int lo = Math.Min(a, b), hi = Math.Max(a, b);
        ClearMultiSelection(tab);
        for (int i = lo; i <= hi; i++)
        {
            flat[i].IsMultiSelected = true;
            tab.MultiSelection.Add(flat[i]);
        }
    }

    private static List<FileSystemItem> FlattenVisible(BrowserTab tab)
    {
        var acc = new List<FileSystemItem>();
        foreach (var root in tab.Roots) Walk(root, acc);
        return acc;
        static void Walk(FileSystemItem node, List<FileSystemItem> acc)
        {
            acc.Add(node);
            if (!node.IsExpanded || !node.ChildrenLoaded) return;
            foreach (var c in node.Children) Walk(c, acc);
        }
    }

    private List<FileSystemItem> GetEffectiveSelection(BrowserTab tab, FileSystemItem fallback)
    {
        if (tab.MultiSelection.Count > 0 && tab.MultiSelection.Contains(fallback))
            return tab.MultiSelection.ToList();
        return new List<FileSystemItem> { fallback };
    }

    private async System.Threading.Tasks.Task<TreeViewItem?> LocateContainerAsync(BrowserTab tab, string targetPath)
    {
        if (!ReferenceEquals(ActiveTab, tab)) return null;
        UpdateLayout();
        var tree = ActivePaneTree();
        if (tree == null || tab.Roots.Count == 0) return null;

        var targetNorm = targetPath.TrimEnd('\\', '/');
        var root = FindRootFor(tab, targetPath) ?? tab.Roots[0];
        var chain = new List<FileSystemItem> { root };
        var current = root;
        while (!string.Equals(current.Path.TrimEnd('\\', '/'), targetNorm, StringComparison.OrdinalIgnoreCase))
        {
            var next = current.Children.FirstOrDefault(c =>
            {
                var cNorm = c.Path.TrimEnd('\\', '/');
                return targetNorm.StartsWith(cNorm, StringComparison.OrdinalIgnoreCase) &&
                       (targetNorm.Length == cNorm.Length ||
                        targetNorm[cNorm.Length] is '\\' or '/');
            });
            if (next == null) break;
            chain.Add(next);
            current = next;
        }

        ItemsControl container = tree;
        TreeViewItem? tvi = null;
        foreach (var node in chain)
        {
            tvi = await GetContainerAsync(container, node);
            if (tvi == null) return null;
            container = tvi;
        }
        return tvi;
    }

    private async void Tree_MouseDoubleClick(object sender, MouseButtonEventArgs e)
    {
        var tvi = FindAncestor<TreeViewItem>(e.OriginalSource as DependencyObject);
        var item = tvi?.DataContext as FileSystemItem;
        if (item == null || string.IsNullOrEmpty(item.Path)) return;

        // Ignore double-clicks that land on the expander chevron. Expanding and
        // collapsing a folder quickly would otherwise register as a double-click
        // and re-root into the folder, which is not what the user wants.
        if (FindAncestor<System.Windows.Controls.Primitives.ToggleButton>(e.OriginalSource as DependencyObject) != null)
            return;

        if (!item.IsDirectory)
        {
            if (item.IsZipEntry && !string.IsNullOrEmpty(item.ZipArchivePath) && !string.IsNullOrEmpty(item.ZipEntryPath))
            {
                try
                {
                    var extracted = ExtractZipEntryToTemp(item.ZipArchivePath!, item.ZipEntryPath!);
                    if (!string.IsNullOrEmpty(extracted))
                    {
                        var psi = new System.Diagnostics.ProcessStartInfo(extracted) { UseShellExecute = true };
                        System.Diagnostics.Process.Start(psi);
                    }
                }
                catch (Exception ex) { StatusText.Text = $"Open failed: {ex.Message}"; }
                e.Handled = true;
                return;
            }
            if (File.Exists(item.Path))
            {
                try
                {
                    var psi = new System.Diagnostics.ProcessStartInfo(item.Path) { UseShellExecute = true };
                    System.Diagnostics.Process.Start(psi);
                }
                catch (Exception ex) { StatusText.Text = $"Open failed: {ex.Message}"; }
                e.Handled = true;
            }
            return;
        }

        if (ActiveTab == null) return;
        e.Handled = true;

        bool isCurrentRoot = ActiveTab.AsRoot &&
            !string.IsNullOrEmpty(ActiveTab.RootPath) &&
            string.Equals(
                ActiveTab.RootPath.TrimEnd('\\', '/'),
                item.Path.TrimEnd('\\', '/'),
                StringComparison.OrdinalIgnoreCase);

        // If double-clicking a drive root while that drive is the current root,
        // jump up to the "all drives" (This PC) view instead of just un-rooting
        // into the same drive again.
        bool isDriveRoot = IsDriveRoot(item.Path);
        if (isCurrentRoot && isDriveRoot)
        {
            await NavigateTabAsync(ActiveTab, string.Empty, expandToPath: false);
            return;
        }

        // Capture the item's current visual Y within the ScrollViewer so we can
        // preserve its on-screen position across the re-root operation.
        // Only anchor when rooting (not when un-rooting) — when un-rooting, the
        // old position is the top of the tree, which isn't useful to anchor to;
        // we'd rather let the centering logic in BringItemIntoView place it
        // nicely in the middle of the viewport.
        var sv = FindActiveScrollViewer();
        double? anchorY = null;
        if (!isCurrentRoot && sv != null && tvi != null)
        {
            try { anchorY = tvi.TransformToAncestor(sv).Transform(new System.Windows.Point(0, 0)).Y; }
            catch { anchorY = null; }
        }
        var targetPath = item.Path;

        await NavigateTabAsync(ActiveTab, targetPath, expandToPath: true, rootAtTarget: !isCurrentRoot);

        if (sv == null || anchorY == null) return;
        // After navigation completes (and its own scroll calls have run), anchor
        // the same item back to its original visual Y.
        await System.Windows.Threading.Dispatcher.Yield(System.Windows.Threading.DispatcherPriority.Background);
        var newTvi = await LocateContainerAsync(ActiveTab, targetPath);
        if (newTvi == null) return;
        double newY;
        try { newY = newTvi.TransformToAncestor(sv).Transform(new System.Windows.Point(0, 0)).Y; }
        catch { return; }
        var delta = newY - anchorY.Value;
        if (Math.Abs(delta) < 0.5) return;
        var newOffset = sv.VerticalOffset + delta;
        newOffset = Math.Max(0, Math.Min(newOffset, sv.ScrollableHeight));
        sv.ScrollToVerticalOffset(newOffset);
    }

    private const double TreeDragThresholdMultiplier = 4.0;

    private void Tree_PreviewMouseMove(object sender, MouseEventArgs e)
    {
        if (e.LeftButton != MouseButtonState.Pressed || _dragItem == null) return;
        var pos = e.GetPosition(null);
        double threshX = SystemParameters.MinimumHorizontalDragDistance * TreeDragThresholdMultiplier;
        double threshY = SystemParameters.MinimumVerticalDragDistance * TreeDragThresholdMultiplier;
        if (Math.Abs(pos.X - _dragStart.X) < threshX &&
            Math.Abs(pos.Y - _dragStart.Y) < threshY)
            return;
        var item = _dragItem;
        _dragItem = null;
        if (item == null || string.IsNullOrEmpty(item.Path)) return;

        var tab = (sender as TreeView)?.DataContext as BrowserTab;
        var selectionItems = tab != null
            ? GetEffectiveSelection(tab, item)
            : new List<FileSystemItem> { item };

        var paths = new System.Collections.Specialized.StringCollection();
        foreach (var it in selectionItems)
        {
            string? p = null;
            if (it.IsZipEntry && !string.IsNullOrEmpty(it.ZipArchivePath) && !string.IsNullOrEmpty(it.ZipEntryPath))
                p = ExtractZipEntryToTemp(it.ZipArchivePath!, it.ZipEntryPath!);
            else if (File.Exists(it.Path) || Directory.Exists(it.Path))
                p = it.Path;
            if (!string.IsNullOrEmpty(p)) paths.Add(p);
        }
        if (paths.Count == 0) return;
        try
        {
            var data = new DataObject();
            data.SetFileDropList(paths);
            DragDrop.DoDragDrop((DependencyObject)sender, data,
                DragDropEffects.Copy | DragDropEffects.Move | DragDropEffects.Link);
        }
        catch (Exception ex) { StatusText.Text = $"Drag failed: {ex.Message}"; }
        finally { ClearDropHighlight(); }
    }

    private string? ExtractZipEntryToTemp(string archivePath, string entryPath)
    {
        try
        {
            var norm = entryPath.Replace('\\', '/');
            using var archive = System.IO.Compression.ZipFile.OpenRead(archivePath);
            var entry = archive.GetEntry(norm) ?? archive.Entries
                .FirstOrDefault(en => string.Equals(en.FullName.Replace('\\', '/'), norm, StringComparison.OrdinalIgnoreCase));
            if (entry == null) { StatusText.Text = $"Entry not found: {entryPath}"; return null; }
            var baseDir = Path.Combine(Path.GetTempPath(), "FileBrowserZip",
                Path.GetFileNameWithoutExtension(archivePath) + "_" + Math.Abs(archivePath.GetHashCode()).ToString("X"));
            var destPath = Path.Combine(baseDir, entry.FullName.Replace('/', Path.DirectorySeparatorChar));
            var destDir = Path.GetDirectoryName(destPath);
            if (!string.IsNullOrEmpty(destDir)) Directory.CreateDirectory(destDir);
            // Re-extract if archive or entry is newer.
            bool needExtract = !File.Exists(destPath) ||
                new FileInfo(destPath).LastWriteTime < entry.LastWriteTime.LocalDateTime ||
                new FileInfo(destPath).Length != entry.Length;
            if (needExtract)
            {
                using var es = entry.Open();
                using var fs = File.Create(destPath);
                es.CopyTo(fs);
            }
            return destPath;
        }
        catch (Exception ex) { StatusText.Text = $"Extract failed: {ex.Message}"; return null; }
    }

    private void SetDropHighlight(FileSystemItem? item)
    {
        if (ReferenceEquals(_dropHighlight, item)) return;
        if (_dropHighlight != null) _dropHighlight.IsDropTarget = false;
        _dropHighlight = item;
        if (_dropHighlight != null) _dropHighlight.IsDropTarget = true;
    }

    private void ClearDropHighlight()
    {
        if (_dropHighlight != null)
        {
            _dropHighlight.IsDropTarget = false;
            _dropHighlight = null;
        }
    }

    private FileSystemItem? ResolveDropTargetItem(DragEventArgs e)
    {
        var tvi = FindAncestor<TreeViewItem>(e.OriginalSource as DependencyObject);
        var item = tvi?.DataContext as FileSystemItem;
        if (item == null) return null;
        if (item.IsDirectory) return item;
        // File: highlight its parent folder container if we can find it.
        var parentTvi = FindAncestor<TreeViewItem>(
            System.Windows.Media.VisualTreeHelper.GetParent(tvi!));
        return parentTvi?.DataContext as FileSystemItem;
    }

    private string? ResolveDropTargetDir(DragEventArgs e, FileSystemItem? item)
    {
        if (item != null)
            return item.IsDirectory ? item.Path : Path.GetDirectoryName(item.Path);
        if (ActiveTab?.Roots.Count > 0) return ActiveTab.Roots[0].Path;
        return null;
    }

    private static bool AllSourcesAlreadyInDir(DragEventArgs e, string? targetDir)
    {
        if (string.IsNullOrEmpty(targetDir)) return false;
        if (e.Data.GetData(DataFormats.FileDrop) is not string[] files || files.Length == 0) return false;
        var norm = targetDir!.TrimEnd('\\', '/');
        foreach (var src in files)
        {
            if (string.IsNullOrEmpty(src)) return false;
            var srcDir = Path.GetDirectoryName(src.TrimEnd('\\', '/'));
            if (srcDir == null) return false;
            if (!string.Equals(srcDir.TrimEnd('\\', '/'), norm, StringComparison.OrdinalIgnoreCase))
                return false;
        }
        return true;
    }

    private void Tree_DragOver(object sender, DragEventArgs e)
    {
        if (!e.Data.GetDataPresent(DataFormats.FileDrop))
        {
            e.Effects = DragDropEffects.None;
            ClearDropHighlight();
            e.Handled = true;
            return;
        }
        var targetItem = ResolveDropTargetItem(e);
        var targetDir = ResolveDropTargetDir(e, targetItem);
        if (AllSourcesAlreadyInDir(e, targetDir))
        {
            e.Effects = DragDropEffects.None;
            ClearDropHighlight();
            e.Handled = true;
            return;
        }
        // Default = Move; Ctrl = Copy.
        bool ctrl = (e.KeyStates & DragDropKeyStates.ControlKey) != 0;
        e.Effects = ctrl ? DragDropEffects.Copy : DragDropEffects.Move;
        SetDropHighlight(targetItem);
        e.Handled = true;
    }

    private void Tree_DragLeave(object sender, DragEventArgs e)
    {
        // Only clear if we're actually leaving the TreeView, not a child.
        var tv = sender as TreeView;
        if (tv == null) { ClearDropHighlight(); return; }
        var pos = e.GetPosition(tv);
        if (pos.X < 0 || pos.Y < 0 || pos.X > tv.ActualWidth || pos.Y > tv.ActualHeight)
            ClearDropHighlight();
    }

    private async void Tree_Drop(object sender, DragEventArgs e)
    {
        if (!e.Data.GetDataPresent(DataFormats.FileDrop)) return;
        e.Handled = true;
        ClearDropHighlight();
        _tabHoverTimer?.Stop();
        _tabHoverTarget = null;

        var tvi = FindAncestor<TreeViewItem>(e.OriginalSource as DependencyObject);
        var item = tvi?.DataContext as FileSystemItem;
        string targetDir;
        if (item != null)
        {
            targetDir = item.IsDirectory ? item.Path : Path.GetDirectoryName(item.Path) ?? string.Empty;
        }
        else if (ActiveTab?.Roots.Count > 0)
        {
            targetDir = ActiveTab.Roots[0].Path;
        }
        else return;

        if (string.IsNullOrEmpty(targetDir) || !Directory.Exists(targetDir))
        {
            StatusText.Text = "Invalid drop target";
            return;
        }

        if (e.Data.GetData(DataFormats.FileDrop) is not string[] files || files.Length == 0) return;
        bool ctrl = (e.KeyStates & DragDropKeyStates.ControlKey) != 0;
        bool move = !ctrl;

        try
        {
            var batch = new List<UndoEntry>();
            foreach (var src in files)
            {
                if (string.IsNullOrEmpty(src)) continue;
                var name = Path.GetFileName(src.TrimEnd('\\', '/'));
                if (string.IsNullOrEmpty(name)) continue;
                var dest = Path.Combine(targetDir, name);
                // Same-folder no-op (for both move and copy).
                var srcDir = Path.GetDirectoryName(src.TrimEnd('\\', '/'));
                if (srcDir != null &&
                    string.Equals(srcDir.TrimEnd('\\', '/'), targetDir.TrimEnd('\\', '/'),
                        StringComparison.OrdinalIgnoreCase))
                {
                    continue;
                }
                if (File.Exists(src))
                {
                    if (move)
                    {
                        Microsoft.VisualBasic.FileIO.FileSystem.MoveFile(src, dest,
                            Microsoft.VisualBasic.FileIO.UIOption.AllDialogs,
                            Microsoft.VisualBasic.FileIO.UICancelOption.DoNothing);
                        if (File.Exists(dest))
                            batch.Add(new UndoEntry { Kind = UndoKind.Move, SrcPath = src, DestPath = dest, IsDirectory = false, Label = $"Move {name}" });
                    }
                    else
                    {
                        Microsoft.VisualBasic.FileIO.FileSystem.CopyFile(src, dest,
                            Microsoft.VisualBasic.FileIO.UIOption.AllDialogs,
                            Microsoft.VisualBasic.FileIO.UICancelOption.DoNothing);
                        if (File.Exists(dest))
                            batch.Add(new UndoEntry { Kind = UndoKind.Copy, DestPath = dest, IsDirectory = false, Label = $"Copy {name}" });
                    }
                }
                else if (Directory.Exists(src))
                {
                    // Prevent moving a folder into itself or its own child.
                    if (IsSameOrSubPath(src, targetDir)) continue;
                    if (move)
                    {
                        Microsoft.VisualBasic.FileIO.FileSystem.MoveDirectory(src, dest,
                            Microsoft.VisualBasic.FileIO.UIOption.AllDialogs,
                            Microsoft.VisualBasic.FileIO.UICancelOption.DoNothing);
                        if (Directory.Exists(dest))
                            batch.Add(new UndoEntry { Kind = UndoKind.Move, SrcPath = src, DestPath = dest, IsDirectory = true, Label = $"Move {name}" });
                    }
                    else
                    {
                        Microsoft.VisualBasic.FileIO.FileSystem.CopyDirectory(src, dest,
                            Microsoft.VisualBasic.FileIO.UIOption.AllDialogs,
                            Microsoft.VisualBasic.FileIO.UICancelOption.DoNothing);
                        if (Directory.Exists(dest))
                            batch.Add(new UndoEntry { Kind = UndoKind.Copy, DestPath = dest, IsDirectory = true, Label = $"Copy {name}" });
                    }
                }
            }
            PushBatchUndo(batch, move ? "Move" : "Copy");
            await RefreshParentOfAsync(targetDir);
            // If files were moved, also refresh their original parent directories.
            if (move)
            {
                var srcParents = files
                    .Where(f => !string.IsNullOrEmpty(f))
                    .Select(f => Path.GetDirectoryName(f.TrimEnd('\\', '/')))
                    .Where(p => !string.IsNullOrEmpty(p))
                    .Distinct(StringComparer.OrdinalIgnoreCase)
                    .ToList();
                foreach (var p in srcParents)
                    if (!string.Equals(p!.TrimEnd('\\', '/'), targetDir.TrimEnd('\\', '/'),
                        StringComparison.OrdinalIgnoreCase))
                        await RefreshParentOfAsync(p!);
            }
            StatusText.Text = $"{(move ? "Moved" : "Copied")} {files.Length} item(s) to {targetDir}";
        }
        catch (Exception ex) { StatusText.Text = $"Drop failed: {ex.Message}"; }
    }

    private static bool IsSameOrSubPath(string src, string target)
    {
        var s = src.TrimEnd('\\', '/');
        var t = target.TrimEnd('\\', '/');
        if (string.Equals(s, t, StringComparison.OrdinalIgnoreCase)) return true;
        return t.StartsWith(s + System.IO.Path.DirectorySeparatorChar, StringComparison.OrdinalIgnoreCase);
    }

    // ---------- Undo stack ----------

    private enum UndoKind { Rename, Move, Create, Copy, Delete, Batch }
    private sealed class UndoEntry
    {
        public UndoKind Kind;
        public string SrcPath = "";   // where it was before the operation
        public string DestPath = "";  // where it is after the operation
        public bool IsDirectory;
        public string Label = "";
        public List<UndoEntry>? Children;  // only used when Kind == Batch
    }

    private readonly Stack<UndoEntry> _undoStack = new();

    private void PushUndo(UndoEntry entry) => _undoStack.Push(entry);

    // Collapse a list of sub-operations into a single batch undo entry so that the
    // user can undo a whole multi-item operation (move/copy/delete N items) with a
    // single Ctrl+Z. A batch with a single child is pushed as that single entry.
    private void PushBatchUndo(List<UndoEntry> children, string verb)
    {
        if (children == null || children.Count == 0) return;
        if (children.Count == 1) { _undoStack.Push(children[0]); return; }
        _undoStack.Push(new UndoEntry
        {
            Kind = UndoKind.Batch,
            Children = children,
            Label = $"{verb} {children.Count} items",
        });
    }

    private async System.Threading.Tasks.Task DoUndoAsync()
    {
        if (_undoStack.Count == 0) { StatusText.Text = "Nothing to undo"; return; }
        var e = _undoStack.Pop();
        var refreshDirs = new HashSet<string>(StringComparer.OrdinalIgnoreCase);
        string? selectPath = null;
        try
        {
            if (e.Kind == UndoKind.Batch && e.Children != null)
            {
                // Undo in reverse of the order the sub-ops were performed so that, for
                // example, moves are reversed after the copies they depended on.
                for (int i = e.Children.Count - 1; i >= 0; i--)
                {
                    try { ApplyUndoSingle(e.Children[i], refreshDirs, ref selectPath); }
                    catch (Exception ex) { StatusText.Text = $"Undo failed for '{e.Children[i].Label}': {ex.Message}"; }
                }
            }
            else
            {
                ApplyUndoSingle(e, refreshDirs, ref selectPath);
            }

            foreach (var d in refreshDirs)
                await RefreshParentOfAsync(d);

            if (selectPath != null && ActiveTab != null && ActiveTab.Roots.Count > 0)
            {
                foreach (var root in ActiveTab.Roots)
                {
                    var node = FindNodeByPath(root, selectPath);
                    if (node != null)
                    {
                        node.IsSelected = true;
                        await FocusContainerAsync(ActiveTab, node);
                        break;
                    }
                }
            }

            StatusText.Text = $"Undone: {e.Label}";
        }
        catch (Exception ex)
        {
            StatusText.Text = $"Undo failed: {ex.Message}";
            // Put it back so user can try again or inspect.
            _undoStack.Push(e);
        }
    }

    private static void ApplyUndoSingle(UndoEntry e, HashSet<string> refreshDirs, ref string? selectPath)
    {
        switch (e.Kind)
        {
            case UndoKind.Rename:
            case UndoKind.Move:
                {
                    if (e.IsDirectory && Directory.Exists(e.DestPath))
                        Microsoft.VisualBasic.FileIO.FileSystem.MoveDirectory(
                            e.DestPath, e.SrcPath,
                            Microsoft.VisualBasic.FileIO.UIOption.AllDialogs,
                            Microsoft.VisualBasic.FileIO.UICancelOption.DoNothing);
                    else if (!e.IsDirectory && File.Exists(e.DestPath))
                        Microsoft.VisualBasic.FileIO.FileSystem.MoveFile(
                            e.DestPath, e.SrcPath,
                            Microsoft.VisualBasic.FileIO.UIOption.AllDialogs,
                            Microsoft.VisualBasic.FileIO.UICancelOption.DoNothing);
                    var dDir = Path.GetDirectoryName(e.DestPath.TrimEnd('\\', '/'));
                    var sDir = Path.GetDirectoryName(e.SrcPath.TrimEnd('\\', '/'));
                    if (!string.IsNullOrEmpty(dDir)) refreshDirs.Add(dDir);
                    if (!string.IsNullOrEmpty(sDir)) refreshDirs.Add(sDir);
                    selectPath = e.SrcPath;
                    break;
                }
            case UndoKind.Create:
            case UndoKind.Copy:
                {
                    if (Directory.Exists(e.DestPath))
                        Microsoft.VisualBasic.FileIO.FileSystem.DeleteDirectory(
                            e.DestPath,
                            Microsoft.VisualBasic.FileIO.UIOption.OnlyErrorDialogs,
                            Microsoft.VisualBasic.FileIO.RecycleOption.SendToRecycleBin);
                    else if (File.Exists(e.DestPath))
                        Microsoft.VisualBasic.FileIO.FileSystem.DeleteFile(
                            e.DestPath,
                            Microsoft.VisualBasic.FileIO.UIOption.OnlyErrorDialogs,
                            Microsoft.VisualBasic.FileIO.RecycleOption.SendToRecycleBin);
                    var dDir = Path.GetDirectoryName(e.DestPath.TrimEnd('\\', '/'));
                    if (!string.IsNullOrEmpty(dDir)) refreshDirs.Add(dDir);
                    break;
                }
            case UndoKind.Delete:
                {
                    if (!TryRestoreFromRecycleBin(e.SrcPath))
                        throw new InvalidOperationException($"Could not restore '{Path.GetFileName(e.SrcPath)}' from recycle bin");
                    var sDir = Path.GetDirectoryName(e.SrcPath.TrimEnd('\\', '/'));
                    if (!string.IsNullOrEmpty(sDir)) refreshDirs.Add(sDir);
                    selectPath = e.SrcPath;
                    break;
                }
        }
    }

    // Restore a file/folder from the recycle bin whose original path matched `originalPath`.
    // Uses Shell.Application COM automation (ProgID Shell.Application, namespace 10 = recycle bin).
    private static bool TryRestoreFromRecycleBin(string originalPath)
    {
        try
        {
            var shellType = Type.GetTypeFromProgID("Shell.Application");
            if (shellType == null) return false;
            dynamic shell = Activator.CreateInstance(shellType)!;
            dynamic recycle = shell.NameSpace(10);
            if (recycle == null) return false;

            var targetDir = (Path.GetDirectoryName(originalPath) ?? "").TrimEnd('\\', '/');
            var targetName = Path.GetFileName(originalPath.TrimEnd('\\', '/'));

            var items = recycle.Items();
            int count = (int)items.Count;
            for (int i = count - 1; i >= 0; i--)
            {
                dynamic item = items.Item(i);
                string itemName = (string)item.Name;
                string origLoc = (string)recycle.GetDetailsOf(item, 1);
                bool nameMatches = string.Equals(itemName, targetName, StringComparison.OrdinalIgnoreCase);
                bool dirMatches = string.Equals(origLoc.TrimEnd('\\', '/'), targetDir, StringComparison.OrdinalIgnoreCase);
                if (!nameMatches || !dirMatches) continue;

                var verbs = item.Verbs();
                int vc = (int)verbs.Count;
                for (int v = 0; v < vc; v++)
                {
                    dynamic verb = verbs.Item(v);
                    string vname = ((string)verb.Name).Replace("&", "");
                    if (vname.Equals("Restore", StringComparison.OrdinalIgnoreCase) ||
                        vname.Equals("Put Back", StringComparison.OrdinalIgnoreCase))
                    {
                        verb.DoIt();
                        return true;
                    }
                }
            }
            return false;
        }
        catch { return false; }
    }

    // ---------- Type-to-search (Explorer-style) ----------

    private string _searchBuffer = string.Empty;
    private DateTime _lastSearchKeyTime = DateTime.MinValue;
    private const int SearchResetMs = 1000;

    private void Tree_PreviewTextInput(object sender, TextCompositionEventArgs e)
    {
        if (sender is not TreeView tv) return;
        if (ActiveTab == null || ActiveTab.Roots.Count == 0) return;
        var text = e.Text;
        if (string.IsNullOrEmpty(text)) return;
        // Ignore control chars (Enter, Esc, backspace, etc.).
        if (text[0] < ' ') return;
        // Ignore modifier combinations with Ctrl/Alt (but Shift is fine).
        if ((Keyboard.Modifiers & (ModifierKeys.Control | ModifierKeys.Alt)) != 0) return;

        var now = DateTime.UtcNow;
        bool timedOut = (now - _lastSearchKeyTime).TotalMilliseconds > SearchResetMs;
        var before = timedOut ? string.Empty : _searchBuffer;
        _searchBuffer = before + text;
        _lastSearchKeyTime = now;

        // Cycle mode: repeated same single character within the timeout.
        bool cycle = before.Length >= 1 &&
                     text.Length == 1 &&
                     AllSame(_searchBuffer) &&
                     char.ToLowerInvariant(before[0]) == char.ToLowerInvariant(text[0]);
        string prefix = cycle ? text : _searchBuffer;

        // Scope search to the children of the currently selected folder (or
        // the parent folder of the currently selected file). This matches the
        // user expectation: if FolderC is selected and expanded, typing
        // searches only within FolderC's children.
        var current = tv.SelectedItem as FileSystemItem;
        IList<FileSystemItem> searchScope;
        if (current != null && current.IsDirectory && current.IsExpanded && current.ChildrenLoaded)
        {
            // Selected item is an expanded folder — search its children.
            searchScope = current.Children;
        }
        else if (current != null)
        {
            // Selected item is a file (or collapsed folder). Find its parent
            // and search among its siblings.
            var parent = FindParentItem(ActiveTab.Roots, current);
            searchScope = parent != null && parent.ChildrenLoaded
                ? parent.Children
                : ActiveTab.Roots;
        }
        else
        {
            searchScope = ActiveTab.Roots;
        }

        if (searchScope.Count == 0) return;
        int startIdx = 0;
        if (cycle && current != null)
        {
            var ci = IndexOfItem(searchScope, current);
            if (ci >= 0) startIdx = ci + 1;
        }

        FileSystemItem? match = null;
        for (int offset = 0; offset < searchScope.Count; offset++)
        {
            var idx = (startIdx + offset) % searchScope.Count;
            if (searchScope[idx].Name.StartsWith(prefix, StringComparison.OrdinalIgnoreCase))
            {
                match = searchScope[idx];
                break;
            }
        }

        if (match != null)
        {
            match.IsSelected = true;
            _ = FocusContainerAsync(ActiveTab, match);
            StatusText.Text = $"Find: {prefix}";
        }
        else
        {
            StatusText.Text = $"Find: {prefix} (no match)";
        }
        e.Handled = true;
    }

    private static int IndexOfItem(IList<FileSystemItem> list, FileSystemItem item)
    {
        for (int i = 0; i < list.Count; i++)
            if (ReferenceEquals(list[i], item)) return i;
        return -1;
    }

    // Walk the tree to find the parent of a given item.
    private static FileSystemItem? FindParentItem(IEnumerable<FileSystemItem> roots, FileSystemItem target)
    {
        foreach (var root in roots)
        {
            var result = FindParentItemRecursive(root, target);
            if (result != null) return result;
        }
        return null;
    }

    private static FileSystemItem? FindParentItemRecursive(FileSystemItem node, FileSystemItem target)
    {
        if (!node.ChildrenLoaded) return null;
        foreach (var child in node.Children)
        {
            if (ReferenceEquals(child, target)) return node;
            var r = FindParentItemRecursive(child, target);
            if (r != null) return r;
        }
        return null;
    }

    private static bool AllSame(string s)
    {
        if (s.Length <= 1) return true;
        var c = char.ToLowerInvariant(s[0]);
        for (int i = 1; i < s.Length; i++)
            if (char.ToLowerInvariant(s[i]) != c) return false;
        return true;
    }

    private static IEnumerable<FileSystemItem> EnumerateVisibleItems(IEnumerable<FileSystemItem> roots)
    {
        foreach (var item in roots)
        {
            yield return item;
            if (item.IsExpanded && item.ChildrenLoaded)
            {
                foreach (var child in EnumerateVisibleItems(item.Children))
                    yield return child;
            }
        }
    }

    // ---------- Sort headers ----------

    private void HeaderName_Click(object sender, RoutedEventArgs e) => ApplySort(SortKey.Name);
    private void HeaderModified_Click(object sender, RoutedEventArgs e) => ApplySort(SortKey.Modified);
    private void HeaderType_Click(object sender, RoutedEventArgs e) => ApplySort(SortKey.Type);
    private void HeaderSize_Click(object sender, RoutedEventArgs e) => ApplySort(SortKey.Size);

    private void ApplySort(SortKey key)
    {
        var tab = ActiveTab;
        if (tab == null) return;

        if (tab.SortKey == key) tab.SortDescending = !tab.SortDescending;
        else { tab.SortKey = key; tab.SortDescending = false; }

        // Also update the global statics so LoadChildren (which runs during
        // expand) uses the current tab's sort order.
        FileSystemItem.CurrentSortKey = tab.SortKey;
        FileSystemItem.SortDescending = tab.SortDescending;

        foreach (var root in tab.Roots)
            root.ResortRecursively(tab.SortKey, tab.SortDescending);

        SortState.Instance.Refresh();
        SaveSettings();
    }

    // ---------- Column splitter drag ----------

    private void Splitter_DragDelta(object sender, DragDeltaEventArgs e)
    {
        if (sender is not GridSplitter s || s.Tag is not string key) return;
        switch (key)
        {
            case "Name": ColumnLayout.Instance.NamePixels += e.HorizontalChange; break;
            case "Modified": ColumnLayout.Instance.ModifiedPixels += e.HorizontalChange; break;
            case "Type": ColumnLayout.Instance.TypePixels += e.HorizontalChange; break;
        }
    }

    private async System.Threading.Tasks.Task<TreeViewItem?> GetContainerAsync(ItemsControl parent, object item)
    {
        parent.ApplyTemplate();
        parent.UpdateLayout();
        if (parent is TreeViewItem tviParent && !tviParent.IsExpanded)
        {
            tviParent.IsExpanded = true;
        }
        var generator = parent.ItemContainerGenerator;
        Log($"    GetContainer: parent={parent.GetType().Name} items={parent.Items.Count} status={generator.Status}");

        var container = generator.ContainerFromItem(item) as TreeViewItem;
        if (container != null) return container;

        // Try by index if we find the item in parent.Items.
        int idx = -1;
        for (int k = 0; k < parent.Items.Count; k++)
        {
            if (ReferenceEquals(parent.Items[k], item)) { idx = k; break; }
        }
        Log($"    GetContainer: index of item = {idx}");

        for (int attempt = 0; attempt < 15 && container == null; attempt++)
        {
            await System.Threading.Tasks.Task.Delay(50);
            await System.Windows.Threading.Dispatcher.Yield(System.Windows.Threading.DispatcherPriority.Background);
            parent.UpdateLayout();
            container = generator.ContainerFromItem(item) as TreeViewItem;
            if (container == null && idx >= 0)
                container = generator.ContainerFromIndex(idx) as TreeViewItem;
        }
        Log($"    GetContainer: result={(container != null ? "found" : "null")} finalStatus={generator.Status}");
        return container;
    }

    private static readonly string LogPath = Path.Combine(
        Environment.GetFolderPath(Environment.SpecialFolder.LocalApplicationData),
        "FileBrowser", "debug.log");

    private static void Log(string msg)
    {
        try
        {
            var dir = Path.GetDirectoryName(LogPath);
            if (!string.IsNullOrEmpty(dir)) Directory.CreateDirectory(dir);
            File.AppendAllText(LogPath, $"{DateTime.Now:HH:mm:ss.fff} {msg}\r\n");
        }
        catch { }
    }

    private async System.Threading.Tasks.Task BringItemIntoViewAsync(BrowserTab tab, FileSystemItem target)
    {
        Log($"=== BringItemIntoViewAsync target={target.Path}");
        if (!ReferenceEquals(ActiveTab, tab)) { Log("bring: tab not active"); return; }
        UpdateLayout();
        var tree = ActivePaneTree();
        if (tree == null) { Log("bring: no TreeView"); return; }
        if (tab.Roots.Count == 0) { Log("bring: no roots"); return; }
        _allowBringIntoView = true;
        try
        {

        var targetNorm = target.Path.TrimEnd('\\', '/');
        var chain = new List<FileSystemItem> { tab.Roots[0] };
        var current = tab.Roots[0];
        while (!string.Equals(current.Path.TrimEnd('\\', '/'), targetNorm, StringComparison.OrdinalIgnoreCase))
        {
            var next = current.Children.FirstOrDefault(c =>
            {
                var cNorm = c.Path.TrimEnd('\\', '/');
                return targetNorm.StartsWith(cNorm, StringComparison.OrdinalIgnoreCase) &&
                       (targetNorm.Length == cNorm.Length ||
                        targetNorm[cNorm.Length] is '\\' or '/');
            });
            if (next == null) break;
            chain.Add(next);
            current = next;
        }
        Log($"bring: chain len={chain.Count}");

        ItemsControl container = tree;
        for (int i = 0; i < chain.Count; i++)
        {
            var node = chain[i];
            Log($"  level {i}: looking for {node.Path}");
            var tvi = await GetContainerAsync(container, node);
            if (tvi == null)
            {
                Log($"  level {i}: NO CONTAINER");
                return;
            }
            Log($"  level {i}: got container");
            if (i == chain.Count - 1)
            {
                tvi.IsSelected = true;
                tvi.Focus();
                tvi.UpdateLayout();
                await System.Threading.Tasks.Task.Delay(50);
                await System.Windows.Threading.Dispatcher.Yield(System.Windows.Threading.DispatcherPriority.ApplicationIdle);
                Log($"  level {i}: scrolling (1st)");
                ScrollTreeViewItemIntoView(tree, tvi);
                await System.Threading.Tasks.Task.Delay(30);
                await System.Windows.Threading.Dispatcher.Yield(System.Windows.Threading.DispatcherPriority.ApplicationIdle);
                Log($"  level {i}: scrolling (2nd)");
                ScrollTreeViewItemIntoView(tree, tvi);
            }
            else
            {
                tvi.IsExpanded = true;
                tvi.UpdateLayout();
                await System.Threading.Tasks.Task.Delay(30);
                await System.Windows.Threading.Dispatcher.Yield(System.Windows.Threading.DispatcherPriority.Background);
            }
            container = tvi;
        }
        }
        finally { _allowBringIntoView = false; }
    }

    private void ScrollTreeViewItemIntoView(TreeView tree, TreeViewItem tvi)
    {
        var scrollViewer = FindDescendant<ScrollViewer>(tree);
        if (scrollViewer == null)
        {
            Log("scroll: no ScrollViewer found");
            tvi.BringIntoView();
            return;
        }
        try
        {
            var transform = tvi.TransformToAncestor(scrollViewer);
            var offsetY = transform.Transform(new System.Windows.Point(0, 0)).Y + scrollViewer.VerticalOffset;
            // Center the target row vertically in the viewport.
            // Use a fixed row height (not tvi.ActualHeight, which includes the
            // whole expanded subtree).
            const double rowHeight = 24;
            var target = offsetY - (scrollViewer.ViewportHeight - rowHeight) / 2.0;
            target = Math.Max(0, Math.Min(target, scrollViewer.ScrollableHeight));
            Log($"scroll: tviY={offsetY:F0} view={scrollViewer.ViewportHeight:F0} scrollable={scrollViewer.ScrollableHeight:F0} target={target:F0} cur={scrollViewer.VerticalOffset:F0}");
            scrollViewer.ScrollToVerticalOffset(target);
        }
        catch (Exception ex)
        {
            Log($"scroll exception: {ex.Message}");
            tvi.BringIntoView();
        }
    }

    private async System.Threading.Tasks.Task FocusContainerAsync(BrowserTab tab, FileSystemItem target)
    {
        if (!ReferenceEquals(ActiveTab, tab)) return;
        var tree = ActivePaneTree();
        if (tree == null || tab.Roots.Count == 0) return;

        var targetNorm = target.Path.TrimEnd('\\', '/');
        var chain = new List<FileSystemItem> { tab.Roots[0] };
        var current = tab.Roots[0];
        while (!string.Equals(current.Path.TrimEnd('\\', '/'), targetNorm, StringComparison.OrdinalIgnoreCase))
        {
            var next = current.Children.FirstOrDefault(c =>
            {
                var cNorm = c.Path.TrimEnd('\\', '/');
                return targetNorm.StartsWith(cNorm, StringComparison.OrdinalIgnoreCase) &&
                       (targetNorm.Length == cNorm.Length || targetNorm[cNorm.Length] is '\\' or '/');
            });
            if (next == null) return;
            chain.Add(next);
            current = next;
        }

        ItemsControl container = tree;
        for (int i = 0; i < chain.Count; i++)
        {
            var tvi = await GetContainerAsync(container, chain[i]);
            if (tvi == null) return;
            if (i == chain.Count - 1)
            {
                tvi.Focus();
                Keyboard.Focus(tvi);
            }
            else
            {
                if (!tvi.IsExpanded) tvi.IsExpanded = true;
                tvi.UpdateLayout();
                container = tvi;
            }
        }
    }

    private static T? FindDescendant<T>(DependencyObject root) where T : DependencyObject
    {
        if (root == null) return null;
        int n = System.Windows.Media.VisualTreeHelper.GetChildrenCount(root);
        for (int i = 0; i < n; i++)
        {
            var child = System.Windows.Media.VisualTreeHelper.GetChild(root, i);
            if (child is T t) return t;
            var deeper = FindDescendant<T>(child);
            if (deeper != null) return deeper;
        }
        return null;
    }

    private static IEnumerable<T> FindVisualChildren<T>(DependencyObject root) where T : DependencyObject
    {
        if (root == null) yield break;
        int n = System.Windows.Media.VisualTreeHelper.GetChildrenCount(root);
        for (int i = 0; i < n; i++)
        {
            var child = System.Windows.Media.VisualTreeHelper.GetChild(root, i);
            if (child is T t) yield return t;
            foreach (var d in FindVisualChildren<T>(child)) yield return d;
        }
    }

    // Tree in the active pane hosting ActiveTab's content.
    private TreeView? ActivePaneTree()
    {
        var at = ActiveTab;
        if (at == null) return null;
        foreach (var t in FindVisualChildren<TreeView>(this))
            if (ReferenceEquals(t.DataContext, at)) return t;
        return null;
    }

    // Tree hosting a specific tab across any pane.
    private TreeView? TreeForTab(BrowserTab tab)
    {
        foreach (var t in FindVisualChildren<TreeView>(this))
            if (ReferenceEquals(t.DataContext, tab)) return t;
        return null;
    }

    private void Window_PreviewKeyDown(object sender, KeyEventArgs e)
    {
        if (e.Handled) return;
        // Ignore when focus is in a text input (rename/path box) so we don't
        // hijack the browser undo there.
        if (Keyboard.FocusedElement is TextBox) return;
        bool ctrl = (Keyboard.Modifiers & ModifierKeys.Control) != 0;
        if (ctrl && e.Key == Key.Z)
        {
            _ = DoUndoAsync();
            e.Handled = true;
        }
    }

    private void Tree_PreviewKeyDown(object sender, KeyEventArgs e)
    {
        if (sender is not TreeView tv) return;
        if (tv.SelectedItem is not FileSystemItem item) return;
        var tab = tv.DataContext as BrowserTab;
        var selection = tab != null ? GetEffectiveSelection(tab, item) : new List<FileSystemItem> { item };

        bool ctrl = (Keyboard.Modifiers & ModifierKeys.Control) != 0;

        if (ctrl && e.Key == Key.C)
        {
            TryCopyToClipboard(selection);
            e.Handled = true;
        }
        else if (ctrl && e.Key == Key.V)
        {
            _ = TryPasteAsync(item);
            e.Handled = true;
        }
        else if (e.Key == Key.Delete && Keyboard.Modifiers == ModifierKeys.Control)
        {
            TryDeleteMany(selection);
            e.Handled = true;
        }
        else if (e.Key == Key.Enter || e.Key == Key.F2)
        {
            TryRename(item);
            e.Handled = true;
        }
    }

    private void TryCopyToClipboard(FileSystemItem item) => TryCopyToClipboard(new List<FileSystemItem> { item });

    private void TryCopyToClipboard(IList<FileSystemItem> items)
    {
        try
        {
            var col = new System.Collections.Specialized.StringCollection();
            foreach (var it in items)
            {
                if (it.IsZipEntry && !string.IsNullOrEmpty(it.ZipArchivePath) && !string.IsNullOrEmpty(it.ZipEntryPath))
                {
                    var p = ExtractZipEntryToTemp(it.ZipArchivePath!, it.ZipEntryPath!);
                    if (!string.IsNullOrEmpty(p)) col.Add(p);
                }
                else if (File.Exists(it.Path) || Directory.Exists(it.Path))
                {
                    col.Add(it.Path);
                }
            }
            if (col.Count == 0) { StatusText.Text = "Nothing to copy"; return; }
            Clipboard.SetFileDropList(col);
            StatusText.Text = $"Copied {col.Count} item(s)";
        }
        catch (Exception ex) { StatusText.Text = $"Copy failed: {ex.Message}"; }
    }

    private void TryDeleteMany(IList<FileSystemItem> items)
    {
        if (items.Count == 1) { TryDelete(items[0]); return; }
        var parents = new HashSet<string>(StringComparer.OrdinalIgnoreCase);
        var batch = new List<UndoEntry>();
        foreach (var it in items.ToList())
        {
            var parentDir = Path.GetDirectoryName(it.Path.TrimEnd('\\', '/'));
            try
            {
                bool isDir = it.IsDirectory && Directory.Exists(it.Path);
                bool isFile = !isDir && File.Exists(it.Path);
                if (!isDir && !isFile) continue;
                var deletedPath = it.Path;
                if (isDir)
                {
                    Microsoft.VisualBasic.FileIO.FileSystem.DeleteDirectory(
                        it.Path,
                        Microsoft.VisualBasic.FileIO.UIOption.AllDialogs,
                        Microsoft.VisualBasic.FileIO.RecycleOption.SendToRecycleBin);
                }
                else
                {
                    Microsoft.VisualBasic.FileIO.FileSystem.DeleteFile(
                        it.Path,
                        Microsoft.VisualBasic.FileIO.UIOption.AllDialogs,
                        Microsoft.VisualBasic.FileIO.RecycleOption.SendToRecycleBin);
                }
                if (!File.Exists(deletedPath) && !Directory.Exists(deletedPath))
                {
                    batch.Add(new UndoEntry
                    {
                        Kind = UndoKind.Delete,
                        SrcPath = deletedPath,
                        DestPath = string.Empty,
                        IsDirectory = isDir,
                        Label = $"Delete {Path.GetFileName(deletedPath)}",
                    });
                }
                if (!string.IsNullOrEmpty(parentDir)) parents.Add(parentDir);
            }
            catch (Exception ex) { StatusText.Text = $"Delete failed: {ex.Message}"; }
        }
        PushBatchUndo(batch, "Delete");
        foreach (var p in parents) _ = RefreshParentOfAsync(p);
        StatusText.Text = $"Deleted {items.Count} item(s)";
    }

    private async System.Threading.Tasks.Task TryPasteAsync(FileSystemItem item)
    {
        try
        {
            if (!Clipboard.ContainsFileDropList()) { StatusText.Text = "Clipboard has no files"; return; }
            var destDir = item.IsDirectory ? item.Path : Path.GetDirectoryName(item.Path);
            if (string.IsNullOrEmpty(destDir)) return;
            var files = Clipboard.GetFileDropList();
            var batch = new List<UndoEntry>();
            foreach (string? src in files)
            {
                if (string.IsNullOrEmpty(src)) continue;
                var name = Path.GetFileName(src.TrimEnd('\\', '/'));
                var dest = Path.Combine(destDir, name);
                try
                {
                    if (Directory.Exists(src))
                    {
                        Microsoft.VisualBasic.FileIO.FileSystem.CopyDirectory(
                            src, dest, Microsoft.VisualBasic.FileIO.UIOption.AllDialogs);
                        if (Directory.Exists(dest))
                            batch.Add(new UndoEntry { Kind = UndoKind.Copy, DestPath = dest, IsDirectory = true, Label = $"Paste {name}" });
                    }
                    else if (File.Exists(src))
                    {
                        Microsoft.VisualBasic.FileIO.FileSystem.CopyFile(
                            src, dest, Microsoft.VisualBasic.FileIO.UIOption.AllDialogs);
                        if (File.Exists(dest))
                            batch.Add(new UndoEntry { Kind = UndoKind.Copy, DestPath = dest, IsDirectory = false, Label = $"Paste {name}" });
                    }
                }
                catch (Exception ex) { StatusText.Text = $"Paste failed for {name}: {ex.Message}"; }
            }
            PushBatchUndo(batch, "Paste");
            await RefreshParentOfAsync(destDir);
            StatusText.Text = $"Pasted into {destDir}";
        }
        catch (Exception ex) { StatusText.Text = $"Paste failed: {ex.Message}"; }
    }

    private void TryDelete(FileSystemItem item)
    {
        try
        {
            var parentDir = Path.GetDirectoryName(item.Path.TrimEnd('\\', '/'));
            bool isDir = item.IsDirectory && Directory.Exists(item.Path);
            bool isFile = !isDir && File.Exists(item.Path);
            if (!isDir && !isFile) return;
            var deletedPath = item.Path;
            if (isDir)
            {
                Microsoft.VisualBasic.FileIO.FileSystem.DeleteDirectory(
                    item.Path,
                    Microsoft.VisualBasic.FileIO.UIOption.AllDialogs,
                    Microsoft.VisualBasic.FileIO.RecycleOption.SendToRecycleBin);
            }
            else
            {
                Microsoft.VisualBasic.FileIO.FileSystem.DeleteFile(
                    item.Path,
                    Microsoft.VisualBasic.FileIO.UIOption.AllDialogs,
                    Microsoft.VisualBasic.FileIO.RecycleOption.SendToRecycleBin);
            }
            // Only push if the item is actually gone (user may have cancelled the dialog).
            if (!File.Exists(deletedPath) && !Directory.Exists(deletedPath))
            {
                PushUndo(new UndoEntry
                {
                    Kind = UndoKind.Delete,
                    SrcPath = deletedPath,
                    DestPath = string.Empty,
                    IsDirectory = isDir,
                    Label = $"Delete {Path.GetFileName(deletedPath)}",
                });
            }
            if (!string.IsNullOrEmpty(parentDir)) _ = RefreshParentOfAsync(parentDir);
            StatusText.Text = $"Deleted: {item.Path}";
        }
        catch (Exception ex) { StatusText.Text = $"Delete failed: {ex.Message}"; }
    }

    private async void TryRename(FileSystemItem item)
    {
        Log($"TryRename start: path='{item.Path}' name='{item.Name}' isDir={item.IsDirectory}");
        var parentDir = Path.GetDirectoryName(item.Path.TrimEnd('\\', '/'));
        Log($"TryRename parentDir='{parentDir}'");
        if (string.IsNullOrEmpty(parentDir)) { StatusText.Text = "Cannot rename root"; return; }
        var dlg = new RenameDialog(item.Name) { Owner = this };
        var result = dlg.ShowDialog();
        Log($"TryRename dialog result={result} newName='{dlg.NewName}'");
        if (result != true) return;
        var newName = dlg.NewName;
        if (string.IsNullOrWhiteSpace(newName) || newName == item.Name) { Log("TryRename no-op"); return; }
        var newPath = Path.Combine(parentDir, newName);
        Log($"TryRename moving '{item.Path}' -> '{newPath}'");
        try
        {
            if (File.Exists(newPath) || Directory.Exists(newPath))
            {
                StatusText.Text = "A file or folder with that name already exists";
                return;
            }
            if (item.IsDirectory)
                Microsoft.VisualBasic.FileIO.FileSystem.MoveDirectory(
                    item.Path, newPath,
                    Microsoft.VisualBasic.FileIO.UIOption.AllDialogs,
                    Microsoft.VisualBasic.FileIO.UICancelOption.DoNothing);
            else
                Microsoft.VisualBasic.FileIO.FileSystem.MoveFile(
                    item.Path, newPath,
                    Microsoft.VisualBasic.FileIO.UIOption.AllDialogs,
                    Microsoft.VisualBasic.FileIO.UICancelOption.DoNothing);
            Log("TryRename move OK, refreshing");
            PushUndo(new UndoEntry
            {
                Kind = UndoKind.Rename,
                SrcPath = item.Path,
                DestPath = newPath,
                IsDirectory = item.IsDirectory,
                Label = $"Rename to {newName}",
            });
            var tree = ActivePaneTree();
            var sv = tree != null ? FindDescendant<ScrollViewer>(tree) : null;
            var savedOffset = sv?.VerticalOffset ?? 0;
            await RefreshParentOfAsync(parentDir);
            StatusText.Text = $"Renamed to {newName}";
            if (ActiveTab != null && ActiveTab.Roots.Count > 0)
            {
                var newItem = FindNodeByPath(ActiveTab.Roots[0], newPath);
                if (newItem != null) newItem.IsSelected = true;
            }
            await Dispatcher.InvokeAsync(() => { }, System.Windows.Threading.DispatcherPriority.Background);
            sv?.ScrollToVerticalOffset(savedOffset);
            if (ActiveTab != null && ActiveTab.Roots.Count > 0)
            {
                var newItem = FindNodeByPath(ActiveTab.Roots[0], newPath);
                if (newItem != null) await FocusContainerAsync(ActiveTab, newItem);
            }
            sv?.ScrollToVerticalOffset(savedOffset);
        }
        catch (Exception ex) { Log($"TryRename move FAILED: {ex}"); StatusText.Text = $"Rename failed: {ex.Message}"; }
    }

    private async System.Threading.Tasks.Task RefreshParentOfAsync(string dirPath)
    {
        var norm = dirPath.TrimEnd('\\', '/');
        bool any = false;
        foreach (var tab in AllTabs())
        {
            foreach (var root in tab.Roots)
            {
                var node = FindNodeByPath(root, norm);
                if (node == null) continue;
                var wasExpanded = node.IsExpanded;
                node.LoadChildren();
                node.IsExpanded = wasExpanded || true;
                any = true;
            }
        }
        _ = any; // avoid unused warning
        await System.Threading.Tasks.Task.CompletedTask;
    }

    private static FileSystemItem? FindNodeByPath(FileSystemItem root, string path)
    {
        if (string.Equals(root.Path.TrimEnd('\\', '/'), path, StringComparison.OrdinalIgnoreCase))
            return root;
        if (!root.ChildrenLoaded) return null;
        foreach (var c in root.Children)
        {
            var r = FindNodeByPath(c, path);
            if (r != null) return r;
        }
        return null;
    }

    // ---------- Favorites ----------

    private void AddFavorite_Click(object sender, RoutedEventArgs e)
    {
        var tab = ActiveTab;
        if (tab == null) return;
        var p = tab.Path;
        if (string.IsNullOrEmpty(p) || !Directory.Exists(p)) return;

        // If the folder is already a favorite, update its sort state.
        var existing = Favorites.FirstOrDefault(f => string.Equals(f.Path, p, StringComparison.OrdinalIgnoreCase));
        if (existing != null)
        {
            existing.SortKey = tab.SortKey;
            existing.SortDescending = tab.SortDescending;
            SaveSettings();
            StatusText.Text = $"Updated sort for favorite: {existing.Name}";
            return;
        }

        Favorites.Add(new Favorite
        {
            Path = p,
            SortKey = tab.SortKey,
            SortDescending = tab.SortDescending,
        });
    }

    private void RemoveFavorite_Click(object sender, RoutedEventArgs e)
    {
        if (FavoritesList.SelectedItem is Favorite f) Favorites.Remove(f);
    }

    private void FavoritesList_MouseDoubleClick(object sender, MouseButtonEventArgs e)
    {
        OpenSelectedFavorite(newTab: true, asRoot: true);
    }

    private void FavoritesList_KeyDown(object sender, KeyEventArgs e)
    {
        if (e.Key == Key.Enter) { OpenSelectedFavorite(newTab: true, asRoot: true); e.Handled = true; }
        else if (e.Key == Key.Delete) { RemoveFavorite_Click(sender, e); e.Handled = true; }
    }

    private void FavoriteOpen_Click(object sender, RoutedEventArgs e) => OpenSelectedFavorite(newTab: false, asRoot: false);
    private void FavoriteOpenNewTab_Click(object sender, RoutedEventArgs e) => OpenSelectedFavorite(newTab: true, asRoot: true);

    private void FavoritesToggle_Click(object sender, RoutedEventArgs e)
    {
        FavoritesPopup.IsOpen = !FavoritesPopup.IsOpen;
        if (FavoritesPopup.IsOpen)
        {
            Dispatcher.BeginInvoke(new Action(() =>
            {
                FavoritesList.Focus();
                if (FavoritesList.SelectedItem == null && FavoritesList.Items.Count > 0)
                    FavoritesList.SelectedIndex = 0;
            }), System.Windows.Threading.DispatcherPriority.Input);
        }
    }

    private void OpenSelectedFavorite(bool newTab, bool asRoot)
    {
        if (FavoritesList.SelectedItem is not Favorite f) return;
        if (!Directory.Exists(f.Path))
        {
            StatusText.Text = $"Folder not found: {f.Path}";
            return;
        }
        FavoritesPopup.IsOpen = false;
        if (newTab)
        {
            var tab = new BrowserTab
            {
                Path = f.Path,
                SortKey = f.SortKey,
                SortDescending = f.SortDescending,
            };
            Tabs.Add(tab);
            ActiveTab = tab;
            _ = NavigateTabAsync(tab, f.Path, expandToPath: true, rootAtTarget: asRoot);
        }
        else if (ActiveTab != null)
        {
            ActiveTab.SortKey = f.SortKey;
            ActiveTab.SortDescending = f.SortDescending;
            FileSystemItem.CurrentSortKey = f.SortKey;
            FileSystemItem.SortDescending = f.SortDescending;
            SortState.Instance.Refresh();
            _ = NavigateTabAsync(ActiveTab, f.Path, expandToPath: true, rootAtTarget: asRoot);
        }
    }

    // ---------- INotifyPropertyChanged ----------

    public event PropertyChangedEventHandler? PropertyChanged;
    private void Raise([CallerMemberName] string? p = null) =>
        PropertyChanged?.Invoke(this, new PropertyChangedEventArgs(p));
}
