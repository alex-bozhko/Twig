using System;
using System.Runtime.InteropServices;

namespace FileBrowser;

/// <summary>
/// Displays the native Windows Explorer shell context menu for a file system
/// item. Uses <c>IContextMenu</c>/<c>IContextMenu2</c>/<c>IContextMenu3</c>
/// under the hood and forwards <c>WM_INITMENUPOPUP</c>/<c>WM_DRAWITEM</c>/
/// <c>WM_MEASUREITEM</c>/<c>WM_MENUCHAR</c> via a window subclass so that
/// owner-drawn items and dynamic submenus (Send To, Open With, ...) populate
/// correctly.
/// </summary>
internal static class ShellContextMenu
{
    /// <summary>
    /// Shows the native shell context menu, plus our custom "Open File Explorer here" item.
    /// When <paramref name="isDirectory"/> is true, also prepends a "New" submenu
    /// allowing the user to create a new folder or file inside the target.
    /// Returns the path of a newly created file/folder (if the user picked "New ▸ …"),
    /// otherwise null.
    /// </summary>
    public static string? Show(IntPtr hwnd, string path, int screenX, int screenY, bool extendedVerbs, bool isDirectory = false)
    {
        if (string.IsNullOrEmpty(path)) return null;

        IntPtr pidlFull = IntPtr.Zero;
        IntPtr hMenu = IntPtr.Zero;
        IntPtr hNewMenu = IntPtr.Zero;
        IShellFolder? parentFolder = null;
        IContextMenu? ctxMenu = null;
        SubclassProc? subclassProc = null;
        const nuint subclassId = 0x5C4E;
        bool subclassed = false;

        try
        {
            uint sfgao = 0;
            if (SHParseDisplayName(path, IntPtr.Zero, out pidlFull, 0, ref sfgao) != 0 || pidlFull == IntPtr.Zero)
            {
                return null;
            }

            var iidShellFolder = typeof(IShellFolder).GUID;
            if (SHBindToParent(pidlFull, ref iidShellFolder, out var parentObj, out var pidlChild) != 0 ||
                parentObj is not IShellFolder pf)
            {
                return null;
            }
            parentFolder = pf;

            IntPtr[] pidls = { pidlChild };
            var iidContextMenu = typeof(IContextMenu).GUID;
            if (parentFolder.GetUIObjectOf(IntPtr.Zero, 1, pidls, ref iidContextMenu, IntPtr.Zero, out var ctxMenuObj) != 0 ||
                ctxMenuObj is not IContextMenu cm)
            {
                return null;
            }
            ctxMenu = cm;

            hMenu = CreatePopupMenu();
            if (hMenu == IntPtr.Zero) return null;

            const uint CMF_NORMAL = 0x00000000;
            const uint CMF_EXTENDEDVERBS = 0x00000100;
            const uint CMF_CANRENAME = 0x00000010;
            uint flags = CMF_NORMAL | CMF_CANRENAME;
            if (extendedVerbs) flags |= CMF_EXTENDEDVERBS;

            const uint idCmdFirst = 1;
            const uint idCmdLast = 0x7000;
            const uint idOpenInExplorer = 0x7FFE;
            const uint idCopyFullPath = 0x7FFD;
            const uint idNewFolder = 0x7FF0;
            const uint idNewFile = 0x7FF1;
            const uint idExpandAll = 0x7FFC;
            const uint idCollapseAll = 0x7FFB;
            if (ctxMenu.QueryContextMenu(hMenu, 0, idCmdFirst, idCmdLast, flags) < 0)
            {
                return null;
            }

            // Prepend our custom "Open File Explorer here" + "Copy full path" + separator.
            const uint MF_BYPOSITION = 0x0400;
            const uint MF_STRING = 0x0000;
            const uint MF_SEPARATOR = 0x0800;
            const uint MF_POPUP = 0x0010;
            InsertMenuW(hMenu, 0, MF_BYPOSITION | MF_SEPARATOR, UIntPtr.Zero, null);
            InsertMenuW(hMenu, 0, MF_BYPOSITION | MF_STRING, (UIntPtr)idCopyFullPath, "Copy full path");
            InsertMenuW(hMenu, 0, MF_BYPOSITION | MF_STRING, (UIntPtr)idOpenInExplorer, "Open File Explorer here");

            // "Expand all" / "Collapse all" for directories, below "Copy full path".
            // After the inserts above: 0=Explorer, 1=Copy path, 2=separator, 3+=shell items.
            // We insert between Copy path (1) and separator (2).
            if (isDirectory)
            {
                // After original inserts: 0=Explorer, 1=CopyPath, 2=separator.
                // Insert after separator (pos 3+), then add another separator after.
                InsertMenuW(hMenu, 3, MF_BYPOSITION | MF_STRING, (UIntPtr)idExpandAll, "Expand all");
                InsertMenuW(hMenu, 4, MF_BYPOSITION | MF_STRING, (UIntPtr)idCollapseAll, "Collapse all");
                InsertMenuW(hMenu, 5, MF_BYPOSITION | MF_SEPARATOR, UIntPtr.Zero, null);
            }

            // Prepend "New ▸" submenu for directories.
            if (isDirectory)
            {
                hNewMenu = CreatePopupMenu();
                if (hNewMenu != IntPtr.Zero)
                {
                    InsertMenuW(hNewMenu, 0, MF_BYPOSITION | MF_STRING, (UIntPtr)idNewFile, "File");
                    InsertMenuW(hNewMenu, 0, MF_BYPOSITION | MF_STRING, (UIntPtr)idNewFolder, "Folder");
                    InsertMenuW(hMenu, 0, MF_BYPOSITION | MF_SEPARATOR, UIntPtr.Zero, null);
                    InsertMenuW(hMenu, 0, MF_BYPOSITION | MF_POPUP | MF_STRING,
                        (UIntPtr)(ulong)hNewMenu.ToInt64(), "New");
                }
            }

            // Install subclass so IContextMenu2/3 messages are forwarded.
            var cm2 = ctxMenu as IContextMenu2;
            var cm3 = ctxMenu as IContextMenu3;
            if (cm2 != null || cm3 != null)
            {
                _current = new MenuState(cm2, cm3);
                subclassProc = SubclassWndProc;
                if (SetWindowSubclass(hwnd, subclassProc, subclassId, IntPtr.Zero))
                {
                    subclassed = true;
                }
            }

            const uint TPM_RETURNCMD = 0x0100;
            const uint TPM_RIGHTBUTTON = 0x0002;
            uint cmd = TrackPopupMenuEx(hMenu, TPM_RETURNCMD | TPM_RIGHTBUTTON, screenX, screenY, hwnd, IntPtr.Zero);
            if (cmd == 0) return null;

            if (cmd == idOpenInExplorer)
            {
                OpenInExplorer(path);
                return null;
            }

            if (cmd == idCopyFullPath)
            {
                try { System.Windows.Clipboard.SetText(path); } catch { }
                return null;
            }

            if (cmd == idExpandAll) return "__expand_all__";
            if (cmd == idCollapseAll) return "__collapse_all__";

            if (cmd == idNewFolder)
            {
                return CreateNewFolder(path);
            }
            if (cmd == idNewFile)
            {
                return CreateNewFile(path);
            }

            var info = new CMINVOKECOMMANDINFOEX
            {
                cbSize = (uint)Marshal.SizeOf<CMINVOKECOMMANDINFOEX>(),
                fMask = CMIC_MASK_UNICODE | CMIC_MASK_PTINVOKE,
                hwnd = hwnd,
                lpVerb = (IntPtr)(cmd - idCmdFirst),
                lpVerbW = (IntPtr)(cmd - idCmdFirst),
                nShow = SW_SHOWNORMAL,
                ptInvoke = new POINT { X = screenX, Y = screenY },
            };
            ctxMenu.InvokeCommand(ref info);
            return null;
        }
        catch
        {
            // Best-effort: don't crash the app if the shell extension misbehaves.
            return null;
        }
        finally
        {
            if (subclassed && subclassProc != null)
            {
                RemoveWindowSubclass(hwnd, subclassProc, subclassId);
            }
            _current = null;

            if (hMenu != IntPtr.Zero) DestroyMenu(hMenu);
            // hNewMenu is owned by hMenu once inserted; DestroyMenu(hMenu) frees it too.
            if (ctxMenu != null) Marshal.FinalReleaseComObject(ctxMenu);
            if (parentFolder != null) Marshal.FinalReleaseComObject(parentFolder);
            if (pidlFull != IntPtr.Zero) Marshal.FreeCoTaskMem(pidlFull);
        }
    }

    private static string? CreateNewFolder(string parent)
    {
        try
        {
            var name = UniqueName(parent, "New folder", isDirectory: true);
            var full = System.IO.Path.Combine(parent, name);
            System.IO.Directory.CreateDirectory(full);
            return full;
        }
        catch { return null; }
    }

    private static string? CreateNewFile(string parent)
    {
        try
        {
            var name = UniqueName(parent, "New Text Document.txt", isDirectory: false);
            var full = System.IO.Path.Combine(parent, name);
            using (System.IO.File.Create(full)) { }
            return full;
        }
        catch { return null; }
    }

    private static string UniqueName(string parent, string desired, bool isDirectory)
    {
        string candidate = desired;
        if (!Exists(parent, candidate, isDirectory)) return candidate;
        var stem = isDirectory ? desired : System.IO.Path.GetFileNameWithoutExtension(desired);
        var ext = isDirectory ? string.Empty : System.IO.Path.GetExtension(desired);
        for (int i = 2; i < 10000; i++)
        {
            candidate = $"{stem} ({i}){ext}";
            if (!Exists(parent, candidate, isDirectory)) return candidate;
        }
        return $"{stem} ({Guid.NewGuid():N}){ext}";
    }

    private static bool Exists(string parent, string name, bool isDirectory)
    {
        var full = System.IO.Path.Combine(parent, name);
        return isDirectory ? System.IO.Directory.Exists(full) : System.IO.File.Exists(full);
    }

    private static void OpenInExplorer(string path)
    {
        try
        {
            string args;
            if (System.IO.Directory.Exists(path))
            {
                args = $"\"{path}\"";
            }
            else
            {
                args = $"/select,\"{path}\"";
            }
            System.Diagnostics.Process.Start(new System.Diagnostics.ProcessStartInfo
            {
                FileName = "explorer.exe",
                Arguments = args,
                UseShellExecute = true,
            });
        }
        catch
        {
            // Best effort.
        }
    }

    // --- Subclass proc forwarding menu messages to IContextMenu2/3 ---

    private sealed record MenuState(IContextMenu2? Cm2, IContextMenu3? Cm3);

    [ThreadStatic]
    private static MenuState? _current;

    private const uint WM_INITMENUPOPUP = 0x0117;
    private const uint WM_DRAWITEM = 0x002B;
    private const uint WM_MEASUREITEM = 0x002C;
    private const uint WM_MENUCHAR = 0x0120;
    private const uint WM_MENUSELECT = 0x011F;

    private static IntPtr SubclassWndProc(IntPtr hWnd, uint uMsg, IntPtr wParam, IntPtr lParam, nuint uIdSubclass, IntPtr dwRefData)
    {
        var state = _current;
        if (state != null)
        {
            switch (uMsg)
            {
                case WM_MENUCHAR:
                    if (state.Cm3 != null && state.Cm3.HandleMenuMsg2(uMsg, wParam, lParam, out var menuCharResult) == 0)
                    {
                        return menuCharResult;
                    }
                    break;
                case WM_INITMENUPOPUP:
                case WM_DRAWITEM:
                case WM_MEASUREITEM:
                case WM_MENUSELECT:
                    if (state.Cm3 != null)
                    {
                        state.Cm3.HandleMenuMsg2(uMsg, wParam, lParam, out _);
                    }
                    else
                    {
                        state.Cm2?.HandleMenuMsg(uMsg, wParam, lParam);
                    }
                    if (uMsg == WM_DRAWITEM || uMsg == WM_MEASUREITEM)
                    {
                        return (IntPtr)1;
                    }
                    return IntPtr.Zero;
            }
        }
        return DefSubclassProc(hWnd, uMsg, wParam, lParam);
    }

    // --- Constants & structs ---

    private const uint CMIC_MASK_UNICODE = 0x00004000;
    private const uint CMIC_MASK_PTINVOKE = 0x20000000;
    private const int SW_SHOWNORMAL = 1;

    // A thread-static discriminator for SetWindowSubclass IDs.

    [StructLayout(LayoutKind.Sequential)]
    private struct POINT { public int X; public int Y; }

    [StructLayout(LayoutKind.Sequential, CharSet = CharSet.Auto)]
    private struct CMINVOKECOMMANDINFOEX
    {
        public uint cbSize;
        public uint fMask;
        public IntPtr hwnd;
        public IntPtr lpVerb;
        [MarshalAs(UnmanagedType.LPStr)] public string? lpParameters;
        [MarshalAs(UnmanagedType.LPStr)] public string? lpDirectory;
        public int nShow;
        public uint dwHotKey;
        public IntPtr hIcon;
        [MarshalAs(UnmanagedType.LPStr)] public string? lpTitle;
        public IntPtr lpVerbW;
        [MarshalAs(UnmanagedType.LPWStr)] public string? lpParametersW;
        [MarshalAs(UnmanagedType.LPWStr)] public string? lpDirectoryW;
        [MarshalAs(UnmanagedType.LPWStr)] public string? lpTitleW;
        public POINT ptInvoke;
    }

    // --- COM interfaces ---

    [ComImport, Guid("000214E6-0000-0000-C000-000000000046"),
     InterfaceType(ComInterfaceType.InterfaceIsIUnknown)]
    private interface IShellFolder
    {
        [PreserveSig] int ParseDisplayName(IntPtr hwnd, IntPtr pbc, [MarshalAs(UnmanagedType.LPWStr)] string pszDisplayName, ref uint pchEaten, out IntPtr ppidl, ref uint pdwAttributes);
        [PreserveSig] int EnumObjects(IntPtr hwnd, int grfFlags, out IntPtr ppenumIDList);
        [PreserveSig] int BindToObject(IntPtr pidl, IntPtr pbc, ref Guid riid, out IntPtr ppv);
        [PreserveSig] int BindToStorage(IntPtr pidl, IntPtr pbc, ref Guid riid, out IntPtr ppv);
        [PreserveSig] int CompareIDs(IntPtr lParam, IntPtr pidl1, IntPtr pidl2);
        [PreserveSig] int CreateViewObject(IntPtr hwndOwner, ref Guid riid, out IntPtr ppv);
        [PreserveSig] int GetAttributesOf(uint cidl, [MarshalAs(UnmanagedType.LPArray)] IntPtr[] apidl, ref uint rgfInOut);
        [PreserveSig] int GetUIObjectOf(IntPtr hwndOwner, uint cidl, [MarshalAs(UnmanagedType.LPArray)] IntPtr[] apidl, ref Guid riid, IntPtr rgfReserved, [MarshalAs(UnmanagedType.Interface)] out object ppv);
        [PreserveSig] int GetDisplayNameOf(IntPtr pidl, uint uFlags, IntPtr pName);
        [PreserveSig] int SetNameOf(IntPtr hwnd, IntPtr pidl, [MarshalAs(UnmanagedType.LPWStr)] string pszName, uint uFlags, out IntPtr ppidlOut);
    }

    [ComImport, Guid("000214e4-0000-0000-c000-000000000046"),
     InterfaceType(ComInterfaceType.InterfaceIsIUnknown)]
    private interface IContextMenu
    {
        [PreserveSig] int QueryContextMenu(IntPtr hmenu, uint iMenu, uint idCmdFirst, uint idCmdLast, uint uFlags);
        [PreserveSig] int InvokeCommand(ref CMINVOKECOMMANDINFOEX pici);
        [PreserveSig] int GetCommandString(IntPtr idCmd, uint uType, IntPtr pReserved, IntPtr pszName, uint cchMax);
    }

    [ComImport, Guid("000214f4-0000-0000-c000-000000000046"),
     InterfaceType(ComInterfaceType.InterfaceIsIUnknown)]
    private interface IContextMenu2
    {
        [PreserveSig] int QueryContextMenu(IntPtr hmenu, uint iMenu, uint idCmdFirst, uint idCmdLast, uint uFlags);
        [PreserveSig] int InvokeCommand(ref CMINVOKECOMMANDINFOEX pici);
        [PreserveSig] int GetCommandString(IntPtr idCmd, uint uType, IntPtr pReserved, IntPtr pszName, uint cchMax);
        [PreserveSig] int HandleMenuMsg(uint uMsg, IntPtr wParam, IntPtr lParam);
    }

    [ComImport, Guid("bcfce0a0-ec17-11d0-8d10-00a0c90f2719"),
     InterfaceType(ComInterfaceType.InterfaceIsIUnknown)]
    private interface IContextMenu3
    {
        [PreserveSig] int QueryContextMenu(IntPtr hmenu, uint iMenu, uint idCmdFirst, uint idCmdLast, uint uFlags);
        [PreserveSig] int InvokeCommand(ref CMINVOKECOMMANDINFOEX pici);
        [PreserveSig] int GetCommandString(IntPtr idCmd, uint uType, IntPtr pReserved, IntPtr pszName, uint cchMax);
        [PreserveSig] int HandleMenuMsg(uint uMsg, IntPtr wParam, IntPtr lParam);
        [PreserveSig] int HandleMenuMsg2(uint uMsg, IntPtr wParam, IntPtr lParam, out IntPtr plResult);
    }

    // --- P/Invoke ---

    [DllImport("shell32.dll", CharSet = CharSet.Unicode)]
    private static extern int SHParseDisplayName([MarshalAs(UnmanagedType.LPWStr)] string pszName, IntPtr pbc, out IntPtr ppidl, uint sfgaoIn, ref uint psfgaoOut);

    [DllImport("shell32.dll")]
    private static extern int SHBindToParent(IntPtr pidl, ref Guid riid, [MarshalAs(UnmanagedType.Interface)] out object ppv, out IntPtr ppidlLast);

    [DllImport("user32.dll", CharSet = CharSet.Unicode, EntryPoint = "InsertMenuW")]
    private static extern bool InsertMenuW(IntPtr hMenu, uint uPosition, uint uFlags, UIntPtr uIDNewItem, [MarshalAs(UnmanagedType.LPWStr)] string? lpNewItem);

    [DllImport("user32.dll")]
    private static extern IntPtr CreatePopupMenu();

    [DllImport("user32.dll")]
    private static extern bool DestroyMenu(IntPtr hMenu);

    [DllImport("user32.dll")]
    private static extern uint TrackPopupMenuEx(IntPtr hmenu, uint fuFlags, int x, int y, IntPtr hwnd, IntPtr lptpm);

    private delegate IntPtr SubclassProc(IntPtr hWnd, uint uMsg, IntPtr wParam, IntPtr lParam, nuint uIdSubclass, IntPtr dwRefData);

    [DllImport("comctl32.dll", SetLastError = true)]
    private static extern bool SetWindowSubclass(IntPtr hWnd, SubclassProc pfnSubclass, nuint uIdSubclass, IntPtr dwRefData);

    [DllImport("comctl32.dll", SetLastError = true)]
    private static extern bool RemoveWindowSubclass(IntPtr hWnd, SubclassProc pfnSubclass, nuint uIdSubclass);

    [DllImport("comctl32.dll")]
    private static extern IntPtr DefSubclassProc(IntPtr hWnd, uint uMsg, IntPtr wParam, IntPtr lParam);
}
