using System;
using System.Collections.Concurrent;
using System.IO;
using System.Runtime.InteropServices;
using System.Windows;
using System.Windows.Interop;
using System.Windows.Media.Imaging;

namespace FileBrowser;

/// <summary>
/// Fetches the same small icon Explorer would show for a given path, using
/// SHGetFileInfo. Results are cached per-extension for regular files (plus
/// one shared folder icon), and per-path for items that carry their own
/// icon (exe, lnk, ico, cur, ani).
/// </summary>
internal static class ShellIcon
{
    private static readonly ConcurrentDictionary<string, BitmapSource> _extCache = new(StringComparer.OrdinalIgnoreCase);
    private static readonly ConcurrentDictionary<string, BitmapSource> _pathCache = new(StringComparer.OrdinalIgnoreCase);
    private static BitmapSource? _folderIcon;
    private static BitmapSource? _genericFileIcon;

    private static readonly string[] _perPathExts = { ".exe", ".lnk", ".ico", ".cur", ".ani", ".scr" };

    public static BitmapSource? GetIcon(string path, bool isDirectory)
    {
        try
        {
            if (isDirectory)
            {
                // Drive roots (e.g. "C:\") should show the actual drive icon, not the generic folder icon.
                if (IsDriveRoot(path))
                {
                    if (_pathCache.TryGetValue(path, out var cached)) return cached;
                    var icon = LoadForPath(path);
                    if (icon != null) _pathCache[path] = icon;
                    return icon;
                }
                return _folderIcon ??= LoadGeneric(FILE_ATTRIBUTE_DIRECTORY);
            }

            var ext = Path.GetExtension(path);
            if (Array.Exists(_perPathExts, e => string.Equals(e, ext, StringComparison.OrdinalIgnoreCase)))
            {
                if (_pathCache.TryGetValue(path, out var cached)) return cached;
                var icon = LoadForPath(path);
                if (icon != null) _pathCache[path] = icon;
                return icon;
            }

            if (string.IsNullOrEmpty(ext))
            {
                return _genericFileIcon ??= LoadGeneric(FILE_ATTRIBUTE_NORMAL);
            }

            if (_extCache.TryGetValue(ext, out var byExt)) return byExt;
            // Use USEFILEATTRIBUTES so we don't touch the file system.
            var iconByExt = LoadByExtension(ext);
            if (iconByExt != null) _extCache[ext] = iconByExt;
            return iconByExt;
        }
        catch
        {
            return null;
        }
    }

    private static bool IsDriveRoot(string path)
    {
        if (string.IsNullOrEmpty(path)) return false;
        try
        {
            var root = Path.GetPathRoot(path);
            if (string.IsNullOrEmpty(root)) return false;
            return string.Equals(root.TrimEnd('\\', '/'), path.TrimEnd('\\', '/'),
                StringComparison.OrdinalIgnoreCase);
        }
        catch { return false; }
    }

    private static BitmapSource? LoadForPath(string path)
    {
        var shfi = new SHFILEINFO();
        var flags = SHGFI_ICON | SHGFI_SMALLICON;
        IntPtr result = SHGetFileInfo(path, 0, ref shfi, (uint)Marshal.SizeOf<SHFILEINFO>(), flags);
        return HIconToBitmap(result, shfi.hIcon);
    }

    private static BitmapSource? LoadByExtension(string ext)
    {
        var shfi = new SHFILEINFO();
        var flags = SHGFI_ICON | SHGFI_SMALLICON | SHGFI_USEFILEATTRIBUTES;
        IntPtr result = SHGetFileInfo("file" + ext, FILE_ATTRIBUTE_NORMAL, ref shfi,
            (uint)Marshal.SizeOf<SHFILEINFO>(), flags);
        return HIconToBitmap(result, shfi.hIcon);
    }

    private static BitmapSource? LoadGeneric(uint attrs)
    {
        var shfi = new SHFILEINFO();
        var flags = SHGFI_ICON | SHGFI_SMALLICON | SHGFI_USEFILEATTRIBUTES;
        var dummy = attrs == FILE_ATTRIBUTE_DIRECTORY ? "folder" : "file";
        IntPtr result = SHGetFileInfo(dummy, attrs, ref shfi, (uint)Marshal.SizeOf<SHFILEINFO>(), flags);
        return HIconToBitmap(result, shfi.hIcon);
    }

    private static BitmapSource? HIconToBitmap(IntPtr result, IntPtr hIcon)
    {
        if (result == IntPtr.Zero || hIcon == IntPtr.Zero) return null;
        try
        {
            var bs = Imaging.CreateBitmapSourceFromHIcon(
                hIcon, Int32Rect.Empty, BitmapSizeOptions.FromEmptyOptions());
            bs.Freeze();
            return bs;
        }
        finally
        {
            DestroyIcon(hIcon);
        }
    }

    private const uint SHGFI_ICON = 0x000000100;
    private const uint SHGFI_SMALLICON = 0x000000001;
    private const uint SHGFI_USEFILEATTRIBUTES = 0x000000010;
    private const uint FILE_ATTRIBUTE_DIRECTORY = 0x00000010;
    private const uint FILE_ATTRIBUTE_NORMAL = 0x00000080;

    [StructLayout(LayoutKind.Sequential, CharSet = CharSet.Unicode)]
    private struct SHFILEINFO
    {
        public IntPtr hIcon;
        public int iIcon;
        public uint dwAttributes;
        [MarshalAs(UnmanagedType.ByValTStr, SizeConst = 260)]
        public string szDisplayName;
        [MarshalAs(UnmanagedType.ByValTStr, SizeConst = 80)]
        public string szTypeName;
    }

    [DllImport("shell32.dll", CharSet = CharSet.Unicode)]
    private static extern IntPtr SHGetFileInfo(string pszPath, uint dwFileAttributes,
        ref SHFILEINFO psfi, uint cbFileInfo, uint uFlags);

    [DllImport("user32.dll")]
    private static extern bool DestroyIcon(IntPtr hIcon);
}
