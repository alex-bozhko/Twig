using System;
using System.ComponentModel;
using System.Windows;

namespace FileBrowser;

/// <summary>
/// Shared bindable source for the four data columns in the tree. Used by
/// both the column-header grid and the per-row grid inside the TreeView
/// item template so that drag-resizing the header updates every row.
/// </summary>
public sealed class ColumnLayout : INotifyPropertyChanged
{
    public static ColumnLayout Instance { get; private set; } = new();

    public const double MinColumnWidth = 50;

    private double _name = 320;
    private double _modified = 170;
    private double _type = 140;
    private double _size = 110;

    public ColumnLayout()
    {
        Instance = this;
    }

    public double NamePixels
    {
        get => _name;
        set { _name = Math.Max(MinColumnWidth, value); Raise(nameof(NameWidth)); Raise(nameof(NamePixels)); }
    }

    public double ModifiedPixels
    {
        get => _modified;
        set { _modified = Math.Max(MinColumnWidth, value); Raise(nameof(ModifiedWidth)); Raise(nameof(ModifiedPixels)); }
    }

    public double TypePixels
    {
        get => _type;
        set { _type = Math.Max(MinColumnWidth, value); Raise(nameof(TypeWidth)); Raise(nameof(TypePixels)); }
    }

    public double SizePixels
    {
        get => _size;
        set { _size = Math.Max(MinColumnWidth, value); Raise(nameof(SizeWidth)); Raise(nameof(SizePixels)); }
    }

    public GridLength NameWidth => new(_name);
    public GridLength ModifiedWidth => new(_modified);
    public GridLength TypeWidth => new(_type);
    public GridLength SizeWidth => new(_size);

    public event PropertyChangedEventHandler? PropertyChanged;
    private void Raise(string name) => PropertyChanged?.Invoke(this, new PropertyChangedEventArgs(name));
}
