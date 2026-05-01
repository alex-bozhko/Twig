using System;
using System.Collections.Generic;
using System.Collections.ObjectModel;
using System.ComponentModel;
using System.IO;
using System.Linq;
using System.Runtime.CompilerServices;

namespace FileBrowser;

public enum SortKey { Name, Modified, Type, Size }

public sealed class FileSystemItem : INotifyPropertyChanged
{
    public static SortKey CurrentSortKey { get; set; } = SortKey.Name;
    public static bool SortDescending { get; set; }

    private bool _hasUnrealizedChildren;
    private bool _isExpanded;
    private bool _isSelected;
    private bool _childrenLoaded;
    private ObservableCollection<FileSystemItem> _children = new();

    private DateTime? _lastWriteTime;
    private long _length = -1;
    private string? _typeDescription;

    public FileSystemItem(string path, bool isDirectory, string? displayName = null)
    {
        Path = path;
        IsDirectory = isDirectory;
        Name = displayName ?? GetFriendlyName(path, isDirectory);
        // Files with a .zip extension are expandable in the tree.
        if (!isDirectory && IsZipPath(path))
            IsZip = true;
        HasUnrealizedChildren = (isDirectory && HasAnyChildren(path)) || IsZip;

        if (HasUnrealizedChildren)
        {
            // Placeholder so WPF's TreeView shows the expand chevron before
            // we've actually enumerated the folder / archive.
            _children.Add(new FileSystemItem("__placeholder__", isDirectory: false, "(loading...)"));
        }

        try
        {
            if (isDirectory)
            {
                var info = new DirectoryInfo(path);
                if (info.Exists) _lastWriteTime = info.LastWriteTime;
            }
            else
            {
                var info = new FileInfo(path);
                if (info.Exists)
                {
                    _lastWriteTime = info.LastWriteTime;
                    _length = info.Length;
                }
            }
        }
        catch
        {
        }
    }

    // Constructor for virtual zip-entry nodes.
    private FileSystemItem(string virtualPath, bool isDirectory, string displayName,
        string zipArchivePath, string zipEntryPath, long length, DateTime? modified)
    {
        Path = virtualPath;
        IsDirectory = isDirectory;
        Name = displayName;
        ZipArchivePath = zipArchivePath;
        ZipEntryPath = zipEntryPath;
        IsZipEntry = true;
        _length = length;
        _lastWriteTime = modified;
        HasUnrealizedChildren = isDirectory;
        if (isDirectory)
        {
            _children.Add(new FileSystemItem("__placeholder__", isDirectory: false, "(loading...)"));
        }
    }

    public bool IsZip { get; }
    public bool IsZipEntry { get; }
    public string? ZipArchivePath { get; }
    public string? ZipEntryPath { get; }
    public bool IsExpandable => HasUnrealizedChildren || (ChildrenLoaded && _children.Count > 0) || IsZip;
    private static bool IsZipPath(string path) =>
        !string.IsNullOrEmpty(path) &&
        string.Equals(System.IO.Path.GetExtension(path), ".zip", StringComparison.OrdinalIgnoreCase);

    // Quick check whether a directory has at least one child entry.
    private static bool HasAnyChildren(string path)
    {
        try
        {
            return Directory.EnumerateFileSystemEntries(path).Any();
        }
        catch { return true; } // If we can't check, assume non-empty.
    }

    private bool IsPlaceholder => string.Equals(Path, "__placeholder__", StringComparison.Ordinal);

    public string Path { get; }
    public string Name { get; }
    public bool IsDirectory { get; }

    public string Glyph => IsDirectory ? "\uE8B7" : "\uE7C3";

    public System.Windows.Media.Imaging.BitmapSource? Icon => ShellIcon.GetIcon(Path, IsDirectory);

    public DateTime? LastWriteTime => _lastWriteTime;
    public long Length => _length;

    public string ModifiedText => _lastWriteTime?.ToString("g") ?? string.Empty;

    public string TypeDescription => _typeDescription ??= ComputeType();

    public string SizeText
    {
        get
        {
            if (IsDirectory || _length < 0) return string.Empty;
            return FormatSize(_length);
        }
    }

    public ObservableCollection<FileSystemItem> Children
    {
        get => _children;
        private set
        {
            _children = value;
            OnPropertyChanged();
            OnPropertyChanged(nameof(IsExpandable));
        }
    }

    public bool HasUnrealizedChildren
    {
        get => _hasUnrealizedChildren;
        set
        {
            if (_hasUnrealizedChildren == value) return;
            _hasUnrealizedChildren = value;
            OnPropertyChanged();
            OnPropertyChanged(nameof(IsExpandable));
        }
    }

    public bool IsExpanded
    {
        get => _isExpanded;
        set
        {
            if (_isExpanded == value) return;
            _isExpanded = value;
            OnPropertyChanged();
        }
    }

    public bool IsSelected
    {
        get => _isSelected;
        set
        {
            if (_isSelected == value) return;
            _isSelected = value;
            OnPropertyChanged();
        }
    }

    private bool _isDropTarget;
    public bool IsDropTarget
    {
        get => _isDropTarget;
        set
        {
            if (_isDropTarget == value) return;
            _isDropTarget = value;
            OnPropertyChanged();
        }
    }

    private bool _isMultiSelected;
    public bool IsMultiSelected
    {
        get => _isMultiSelected;
        set
        {
            if (_isMultiSelected == value) return;
            _isMultiSelected = value;
            OnPropertyChanged();
        }
    }

    public bool ChildrenLoaded
    {
        get => _childrenLoaded;
        private set
        {
            _childrenLoaded = value;
            OnPropertyChanged();
        }
    }

    public void LoadChildren()
    {
        if (IsZip && !IsZipEntry)
        {
            LoadZipArchiveChildren();
            return;
        }
        if (!IsDirectory) return;

        var gathered = new List<FileSystemItem>();
        try
        {
            foreach (var dir in Directory.EnumerateDirectories(Path))
            {
                try
                {
                    var info = new DirectoryInfo(dir);
                    if ((info.Attributes & FileAttributes.System) != 0 &&
                        (info.Attributes & FileAttributes.Hidden) != 0)
                    {
                        continue;
                    }
                    gathered.Add(new FileSystemItem(dir, isDirectory: true));
                }
                catch
                {
                }
            }

            foreach (var file in Directory.EnumerateFiles(Path))
            {
                try
                {
                    gathered.Add(new FileSystemItem(file, isDirectory: false));
                }
                catch
                {
                }
            }
        }
        catch (UnauthorizedAccessException)
        {
            gathered.Add(new FileSystemItem(Path, false, "(Access denied)"));
        }
        catch (Exception ex)
        {
            gathered.Add(new FileSystemItem(Path, false, $"(Error: {ex.Message})"));
        }

        var sorted = Sort(gathered).ToList();

        if (_childrenLoaded && _children != null && _children.Count > 0)
        {
            // Preserve existing FileSystemItem instances (with their IsExpanded / loaded
            // children) for paths that still exist, so subfolders don't fold on refresh.
            var existing = new Dictionary<string, FileSystemItem>(StringComparer.OrdinalIgnoreCase);
            foreach (var c in _children)
                existing[c.Path] = c;
            for (int i = 0; i < sorted.Count; i++)
            {
                if (existing.TryGetValue(sorted[i].Path, out var keep) && keep.IsDirectory == sorted[i].IsDirectory)
                    sorted[i] = keep;
            }
        }

        Children = new ObservableCollection<FileSystemItem>(sorted);
        ChildrenLoaded = true;
        HasUnrealizedChildren = false;
    }

    private void LoadZipArchiveChildren()
    {
        var gathered = new List<FileSystemItem>();
        try
        {
            using var archive = System.IO.Compression.ZipFile.OpenRead(Path);
            // Build a virtual directory tree from flat entry list.
            // Each folder node holds its immediate children.
            var rootFolder = new ZipFolder("");
            foreach (var entry in archive.Entries)
            {
                var full = entry.FullName.Replace('\\', '/');
                if (string.IsNullOrEmpty(full)) continue;
                bool isDir = full.EndsWith("/");
                var parts = full.TrimEnd('/').Split('/');
                if (parts.Length == 0) continue;
                var cur = rootFolder;
                for (int i = 0; i < parts.Length - 1; i++)
                {
                    var name = parts[i];
                    if (!cur.SubFolders.TryGetValue(name, out var sub))
                    {
                        sub = new ZipFolder(Combine(cur.EntryPath, name));
                        cur.SubFolders[name] = sub;
                    }
                    cur = sub;
                }
                var leafName = parts[^1];
                if (isDir)
                {
                    if (!cur.SubFolders.ContainsKey(leafName))
                        cur.SubFolders[leafName] = new ZipFolder(Combine(cur.EntryPath, leafName));
                }
                else
                {
                    cur.Files.Add((leafName, entry));
                }
            }
            foreach (var fsi in BuildZipNodes(rootFolder, Path))
                gathered.Add(fsi);
        }
        catch (Exception ex)
        {
            gathered.Add(new FileSystemItem(Path, false, $"(Zip error: {ex.Message})"));
        }

        Children = new ObservableCollection<FileSystemItem>(Sort(gathered));
        ChildrenLoaded = true;
        HasUnrealizedChildren = false;
    }

    private sealed class ZipFolder
    {
        public string EntryPath;
        public Dictionary<string, ZipFolder> SubFolders = new(StringComparer.OrdinalIgnoreCase);
        public List<(string name, System.IO.Compression.ZipArchiveEntry entry)> Files = new();
        public ZipFolder(string entryPath) { EntryPath = entryPath; }
    }

    private static string Combine(string parent, string child)
        => string.IsNullOrEmpty(parent) ? child : parent + "/" + child;

    private static IEnumerable<FileSystemItem> BuildZipNodes(ZipFolder folder, string archivePath)
    {
        foreach (var kv in folder.SubFolders)
        {
            var sub = kv.Value;
            var virtualPath = archivePath + "\\" + sub.EntryPath.Replace('/', '\\');
            var item = new FileSystemItem(virtualPath, isDirectory: true, displayName: kv.Key,
                zipArchivePath: archivePath, zipEntryPath: sub.EntryPath, length: -1, modified: null);
            item.AssignZipChildren(BuildZipNodes(sub, archivePath).ToList());
            yield return item;
        }
        foreach (var (name, entry) in folder.Files)
        {
            var entryPath = Combine(folder.EntryPath, name);
            var virtualPath = archivePath + "\\" + entryPath.Replace('/', '\\');
            DateTime? modified = null;
            try { modified = entry.LastWriteTime.LocalDateTime; } catch { }
            yield return new FileSystemItem(virtualPath, isDirectory: false, displayName: name,
                zipArchivePath: archivePath, zipEntryPath: entryPath, length: entry.Length, modified: modified);
        }
    }

    private void AssignZipChildren(List<FileSystemItem> children)
    {
        Children = new ObservableCollection<FileSystemItem>(Sort(children));
        ChildrenLoaded = true;
        HasUnrealizedChildren = false;
    }

    public void ResortRecursively(SortKey sortKey, bool descending)
    {
        if (!_childrenLoaded) return;
        var sorted = Sort(_children.ToList(), sortKey, descending).ToList();
        _children.Clear();
        foreach (var c in sorted)
        {
            _children.Add(c);
            c.ResortRecursively(sortKey, descending);
        }
    }

    // Overload for backward compatibility (uses global statics).
    public void ResortRecursively() => ResortRecursively(CurrentSortKey, SortDescending);

    private static IEnumerable<FileSystemItem> Sort(IEnumerable<FileSystemItem> items, SortKey sortKey, bool descending)
    {
        Comparison<FileSystemItem> cmp = sortKey switch
        {
            SortKey.Modified => (a, b) => Nullable.Compare(a.LastWriteTime, b.LastWriteTime),
            SortKey.Type => (a, b) => string.Compare(a.TypeDescription, b.TypeDescription, StringComparison.OrdinalIgnoreCase),
            SortKey.Size => (a, b) => a.Length.CompareTo(b.Length),
            _ => (a, b) => string.Compare(a.Name, b.Name, StringComparison.OrdinalIgnoreCase),
        };

        var list = items.ToList();
        list.Sort(cmp);
        if (descending) list.Reverse();
        return list;
    }

    private static IEnumerable<FileSystemItem> Sort(IEnumerable<FileSystemItem> items)
        => Sort(items, CurrentSortKey, SortDescending);

    private string ComputeType()
    {
        if (IsDirectory) return "File folder";
        var ext = System.IO.Path.GetExtension(Path);
        return string.IsNullOrEmpty(ext) ? "File" : ext.TrimStart('.').ToUpperInvariant() + " file";
    }

    private static string FormatSize(long bytes)
    {
        string[] units = { "B", "KB", "MB", "GB", "TB", "PB" };
        double len = bytes;
        int order = 0;
        while (len >= 1024 && order < units.Length - 1)
        {
            order++;
            len /= 1024;
        }
        return $"{len:0.##} {units[order]}";
    }

    private static string GetFriendlyName(string path, bool isDirectory)
    {
        try
        {
            if (isDirectory)
            {
                var name = System.IO.Path.GetFileName(path.TrimEnd(System.IO.Path.DirectorySeparatorChar,
                                                                    System.IO.Path.AltDirectorySeparatorChar));
                return string.IsNullOrEmpty(name) ? path : name;
            }
            return System.IO.Path.GetFileName(path);
        }
        catch
        {
            return path;
        }
    }

    public event PropertyChangedEventHandler? PropertyChanged;

    private void OnPropertyChanged([CallerMemberName] string? propertyName = null)
    {
        PropertyChanged?.Invoke(this, new PropertyChangedEventArgs(propertyName));
    }
}

