using System.Collections.ObjectModel;
using System.ComponentModel;
using System.Runtime.CompilerServices;
using System.Windows.Controls;

namespace FileBrowser;

public abstract class PaneNode : INotifyPropertyChanged
{
    public SplitPane? Parent { get; set; }

    public event PropertyChangedEventHandler? PropertyChanged;
    protected void OnPropertyChanged([CallerMemberName] string? name = null)
        => PropertyChanged?.Invoke(this, new PropertyChangedEventArgs(name));
}

public sealed class LeafPane : PaneNode
{
    public ObservableCollection<BrowserTab> Tabs { get; } = new();

    private BrowserTab? _activeTab;
    public BrowserTab? ActiveTab
    {
        get => _activeTab;
        set
        {
            if (ReferenceEquals(_activeTab, value)) return;
            _activeTab = value;
            OnPropertyChanged();
        }
    }

    private bool _isActive;
    public bool IsActive
    {
        get => _isActive;
        set
        {
            if (_isActive == value) return;
            _isActive = value;
            OnPropertyChanged();
        }
    }
}

public sealed class SplitPane : PaneNode
{
    private Orientation _orientation;
    public Orientation Orientation
    {
        get => _orientation;
        set { if (_orientation != value) { _orientation = value; OnPropertyChanged(); } }
    }
    public ObservableCollection<PaneNode> Children { get; } = new();
    public ObservableCollection<double> Sizes { get; } = new(); // star weights
}
