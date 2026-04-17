using System.Collections.Specialized;
using System.Windows;
using System.Windows.Controls;
using System.Windows.Controls.Primitives;
using System.Windows.Media;

namespace FileBrowser;

// A user control that renders a SplitPane as a nested Grid with GridSplitters,
// and rebuilds itself whenever the model's Children or Orientation change.
public sealed class SplitPaneView : ContentControl
{
    private SplitPane? _pane;

    public SplitPaneView()
    {
        DataContextChanged += (_, e) =>
        {
            Detach();
            _pane = e.NewValue as SplitPane;
            Attach();
            Rebuild();
        };
    }

    private void Attach()
    {
        if (_pane == null) return;
        _pane.Children.CollectionChanged += Children_Changed;
        _pane.PropertyChanged += Pane_PropertyChanged;
    }

    private void Detach()
    {
        if (_pane == null) return;
        _pane.Children.CollectionChanged -= Children_Changed;
        _pane.PropertyChanged -= Pane_PropertyChanged;
    }

    private void Pane_PropertyChanged(object? s, System.ComponentModel.PropertyChangedEventArgs e)
    {
        if (e.PropertyName == nameof(SplitPane.Orientation)) Rebuild();
    }

    private void Children_Changed(object? s, NotifyCollectionChangedEventArgs e) => Rebuild();

    private Grid? _grid;
    private readonly System.Collections.Generic.Dictionary<object, ContentPresenter> _presenters = new();

    private void Rebuild()
    {
        if (_pane == null) { Content = null; _grid = null; _presenters.Clear(); return; }

        // Reuse the existing Grid and ContentPresenters where possible so that
        // panes whose identity hasn't changed keep their visual subtree (and
        // therefore their TreeView state: scroll offset, expanded containers,
        // selection, focus) intact across inserts/removes of siblings.
        var grid = _grid ?? new Grid();
        grid.Children.Clear();
        grid.ColumnDefinitions.Clear();
        grid.RowDefinitions.Clear();

        bool horizontal = _pane.Orientation != Orientation.Vertical;

        int n = _pane.Children.Count;
        while (_pane.Sizes.Count < n) _pane.Sizes.Add(1.0);

        for (int i = 0; i < n; i++)
        {
            if (horizontal)
            {
                grid.ColumnDefinitions.Add(new ColumnDefinition
                {
                    Width = new GridLength(_pane.Sizes[i], GridUnitType.Star),
                    MinWidth = 120,
                });
                if (i < n - 1)
                    grid.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(6) });
            }
            else
            {
                grid.RowDefinitions.Add(new RowDefinition
                {
                    Height = new GridLength(_pane.Sizes[i], GridUnitType.Star),
                    MinHeight = 80,
                });
                if (i < n - 1)
                    grid.RowDefinitions.Add(new RowDefinition { Height = new GridLength(6) });
            }
        }

        // Drop cached presenters for children that no longer exist.
        var current = new System.Collections.Generic.HashSet<object>(_pane.Children);
        var stale = new System.Collections.Generic.List<object>();
        foreach (var kv in _presenters) if (!current.Contains(kv.Key)) stale.Add(kv.Key);
        foreach (var k in stale) _presenters.Remove(k);

        for (int i = 0; i < n; i++)
        {
            var child = _pane.Children[i];
            if (!_presenters.TryGetValue(child, out var cc))
            {
                cc = new ContentPresenter { Content = child };
                _presenters[child] = cc;
            }
            if (horizontal)
            {
                Grid.SetColumn(cc, i * 2);
                Grid.SetRow(cc, 0);
            }
            else
            {
                Grid.SetRow(cc, i * 2);
                Grid.SetColumn(cc, 0);
            }
            grid.Children.Add(cc);

            if (i < n - 1)
            {
                var splitter = new GridSplitter
                {
                    ShowsPreview = false,
                    Background = Brushes.Transparent,
                    Focusable = false,
                };
                if (horizontal)
                {
                    splitter.Width = 6;
                    splitter.HorizontalAlignment = HorizontalAlignment.Stretch;
                    splitter.VerticalAlignment = VerticalAlignment.Stretch;
                    splitter.ResizeDirection = GridResizeDirection.Columns;
                    splitter.Cursor = System.Windows.Input.Cursors.SizeWE;
                    Grid.SetColumn(splitter, i * 2 + 1);
                    Grid.SetRow(splitter, 0);
                }
                else
                {
                    splitter.Height = 6;
                    splitter.HorizontalAlignment = HorizontalAlignment.Stretch;
                    splitter.VerticalAlignment = VerticalAlignment.Stretch;
                    splitter.ResizeDirection = GridResizeDirection.Rows;
                    splitter.Cursor = System.Windows.Input.Cursors.SizeNS;
                    Grid.SetRow(splitter, i * 2 + 1);
                    Grid.SetColumn(splitter, 0);
                }
                var capturedGrid = grid;
                splitter.DragCompleted += (_, _) =>
                {
                    if (_pane == null) return;
                    // Pull star weights back from the grid so Sizes stays in sync.
                    if (horizontal)
                    {
                        for (int j = 0; j < _pane.Children.Count; j++)
                            _pane.Sizes[j] = capturedGrid.ColumnDefinitions[j * 2].Width.Value;
                    }
                    else
                    {
                        for (int j = 0; j < _pane.Children.Count; j++)
                            _pane.Sizes[j] = capturedGrid.RowDefinitions[j * 2].Height.Value;
                    }
                };
                grid.Children.Add(splitter);
            }
        }

        _grid = grid;
        if (!ReferenceEquals(Content, grid)) Content = grid;
    }
}
