# Twig

A fast, tree-first file browser for Windows, written in Rust on raw Win32
(via [`windows-rs`](https://github.com/microsoft/windows-rs)). Single ~1 MB
executable, no runtime dependencies.

Twig shows your file system as a real collapsible tree with columns (Name /
Date modified / Type / Size), multiple tabs, split panes and Explorer-grade
file operations.

## Features

### Tree

- Virtualized, custom-drawn tree grid with per-folder expand chevrons; folders
  load in the background and update live as files change on disk.
- Double-click a folder to re-root the tab on it; double-click the root to go
  up (a drive root goes back to **This PC**). `Backspace` goes up.
- Folder context menu adds **Expand all** / **Collapse all**.
- Click a column header to sort, drag header edges to resize.
- Type-to-search jumps to the next matching item in the current folder.
- Keyboard: arrows, `Home`/`End`, `PgUp`/`PgDn`, `←`/`→` collapse/expand,
  `Ctrl+A`, `Ctrl+Space`, `F5` refresh.

### Tabs and panes

- `Ctrl+T` new tab, `Ctrl+W` close, `Ctrl+Tab` / `Ctrl+Shift+Tab` cycle,
  `Ctrl+L` focus the path box.
- Drag a tab onto another pane's strip to move it, or onto a pane edge to
  dock it into a new split.
- Double-click a tab (or the empty tab strip) to maximize that pane; again
  to restore.
- Tab context menu: Close, Close Others, Close to the Left / Right,
  Split Right, Split Down, Maximize / Restore pane.

### File operations

- Drag and drop with Windows semantics (`Ctrl` copy, `Shift` move).
- `Ctrl+C` / `Ctrl+X` / `Ctrl+V` clipboard (interoperates with Explorer).
- `Delete` recycles, `Shift+Delete` deletes permanently.
- `F2` / `Enter` rename, `Alt+Enter` properties.
- `Ctrl+Z` undo of copy / move / rename / recycle / new item.
- Full Windows Shell context menu (`Shift` for extended verbs).
- All operations run on a background thread via `IFileOperation`; the UI never
  blocks.

### Zip archives

- Zip files expand like folders without unpacking on disk.
- Opening, copying or dragging out an entry extracts just that entry to a
  unique folder under `%TEMP%\Twig\` (cleaned up on start).

### Favorites

- ★ menu: add the current folder, open in current or new tab, right-click to
  remove.

### Persistence

- Window placement, pane layout, tabs (root, expanded folders, selection,
  scroll, sort), column widths and favorites are saved to
  `%LOCALAPPDATA%\Twig\settings.json`.
- On first run, settings from the previous .NET version
  (`%LOCALAPPDATA%\FileBrowser\settings.json`) are imported, including the
  full tab/pane layout.

### Appearance

- Per-monitor DPI aware; follows the system light/dark theme.

## Build

Requires the Rust MSVC toolchain (`rustup default stable-msvc`).

```powershell
cargo build --release   # target\release\Twig.exe
cargo test
```
