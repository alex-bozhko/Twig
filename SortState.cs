using System.ComponentModel;
using System.Runtime.CompilerServices;

namespace FileBrowser;

/// <summary>
/// Observable sort-state singleton used by column headers across all tabs.
/// Header buttons bind to these labels so the active-column arrow updates
/// everywhere when sort changes.
/// </summary>
public sealed class SortState : INotifyPropertyChanged
{
    public static SortState Instance { get; private set; } = new();

    public SortState()
    {
        Instance = this;
    }

    public string NameLabel => "Name" + Arrow(SortKey.Name);
    public string ModifiedLabel => "Date modified" + Arrow(SortKey.Modified);
    public string TypeLabel => "Type" + Arrow(SortKey.Type);
    public string SizeLabel => "Size" + Arrow(SortKey.Size);

    private static string Arrow(SortKey key) =>
        FileSystemItem.CurrentSortKey == key
            ? (FileSystemItem.SortDescending ? " ▼" : " ▲")
            : string.Empty;

    public void Refresh()
    {
        Raise(nameof(NameLabel));
        Raise(nameof(ModifiedLabel));
        Raise(nameof(TypeLabel));
        Raise(nameof(SizeLabel));
    }

    public event PropertyChangedEventHandler? PropertyChanged;
    private void Raise([CallerMemberName] string? p = null) =>
        PropertyChanged?.Invoke(this, new PropertyChangedEventArgs(p));
}
