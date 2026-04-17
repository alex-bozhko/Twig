using System.Collections.Generic;
using System.Collections.ObjectModel;
using System.ComponentModel;
using System.Runtime.CompilerServices;

namespace FileBrowser;

/// <summary>
/// State for a single browser tab: its own root stack and current path.
/// </summary>
public sealed class BrowserTab : INotifyPropertyChanged
{
    public ObservableCollection<FileSystemItem> Roots { get; } = new();
    public List<FileSystemItem> MultiSelection { get; } = new();
    public FileSystemItem? SelectionAnchor { get; set; }

    public bool AsRoot { get; set; }
    public bool IsDrives { get; set; }
    public string RootPath { get; set; } = string.Empty;
    public List<string> ExpandedPaths { get; set; } = new();
    public string SelectedPath { get; set; } = string.Empty;
    public double ScrollOffset { get; set; }

    private string _path = string.Empty;
    public string Path
    {
        get => _path;
        set
        {
            if (_path == value) return;
            _path = value;
            Raise(nameof(Path));
            Raise(nameof(Title));
        }
    }

    public string Title
    {
        get
        {
            if (IsDrives) return "This PC";
            if (string.IsNullOrEmpty(_path)) return "(empty)";
            var trimmed = _path.TrimEnd('\\', '/');
            if (trimmed.Length == 0) return _path;
            var name = System.IO.Path.GetFileName(trimmed);
            return string.IsNullOrEmpty(name) ? trimmed : name;
        }
    }

    public event PropertyChangedEventHandler? PropertyChanged;
    private void Raise([CallerMemberName] string? p = null) =>
        PropertyChanged?.Invoke(this, new PropertyChangedEventArgs(p));
}
