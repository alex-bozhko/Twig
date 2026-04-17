using System.IO;
using System.Windows;
using System.Windows.Controls;
using System.Windows.Input;

namespace FileBrowser;

public sealed class RenameDialog : Window
{
    private readonly TextBox _textBox;
    public string NewName => _textBox.Text;

    public RenameDialog(string initial)
    {
        Title = "Rename";
        Width = 420;
        Height = 140;
        WindowStartupLocation = WindowStartupLocation.CenterOwner;
        ResizeMode = ResizeMode.NoResize;
        ShowInTaskbar = false;

        var stack = new StackPanel { Margin = new Thickness(12) };
        stack.Children.Add(new TextBlock { Text = "New name:", Margin = new Thickness(0, 0, 0, 6) });
        _textBox = new TextBox { Text = initial, Padding = new Thickness(4) };
        stack.Children.Add(_textBox);

        var buttons = new StackPanel
        {
            Orientation = Orientation.Horizontal,
            HorizontalAlignment = HorizontalAlignment.Right,
            Margin = new Thickness(0, 12, 0, 0)
        };
        var ok = new Button { Content = "OK", Width = 80, Margin = new Thickness(0, 0, 6, 0), IsDefault = true };
        var cancel = new Button { Content = "Cancel", Width = 80, IsCancel = true };
        ok.Click += (_, _) => { DialogResult = true; };
        buttons.Children.Add(ok);
        buttons.Children.Add(cancel);
        stack.Children.Add(buttons);
        Content = stack;

        Loaded += (_, _) =>
        {
            _textBox.Focus();
            var stem = Path.GetFileNameWithoutExtension(initial);
            _textBox.Select(0, string.IsNullOrEmpty(stem) ? initial.Length : stem.Length);
        };

        _textBox.KeyDown += (_, e) =>
        {
            if (e.Key == Key.Enter) { DialogResult = true; e.Handled = true; }
        };
    }
}
