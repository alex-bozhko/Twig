using System.ComponentModel;
using System.IO;
using System.Runtime.CompilerServices;
using System.Windows.Media.Imaging;

namespace FileBrowser;

public sealed class Favorite : INotifyPropertyChanged
{
    private string _path = string.Empty;
    public string Path
    {
        get => _path;
        set
        {
            if (_path == value) return;
            _path = value;
            Raise();
            Raise(nameof(Name));
            Raise(nameof(Icon));
        }
    }

    public string Name
    {
        get
        {
            if (string.IsNullOrEmpty(_path)) return string.Empty;
            var trimmed = _path.TrimEnd('\\', '/');
            var leaf = System.IO.Path.GetFileName(trimmed);
            return string.IsNullOrEmpty(leaf) ? trimmed : leaf;
        }
    }

    public BitmapSource? Icon =>
        !string.IsNullOrEmpty(_path) && Directory.Exists(_path)
            ? ShellIcon.GetIcon(_path, isDirectory: true)
            : null;

    public event PropertyChangedEventHandler? PropertyChanged;
    private void Raise([CallerMemberName] string? p = null) =>
        PropertyChanged?.Invoke(this, new PropertyChangedEventArgs(p));
}
