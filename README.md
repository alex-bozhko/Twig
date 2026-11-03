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
  up (a drive root goes back to **This PC**).
- When zoomed into a folder, the tab shows an open-folder badge and the root row stays
  pinned at the top.
- A clickable breadcrumb bar sits above the columns on every tab (just the PC
  icon on **This PC**).
- Click the empty part of the breadcrumb bar (or press `Ctrl+L`) to type or
  paste a path. Quotes and `%VARS%` are fine, a file
  path opens its folder with the file selected, `Enter` goes there, and `Esc`
  or clicking away cancels.
- Folder context menu adds **Expand all** / **Collapse all**.
- Click a column header to sort, drag header edges to resize.
- Type-to-search jumps to the next matching item in the current folder.
- The status bar shows the active tab's item count, and the number and total
  size of selected items.
- Hidden items and dot-files/folders (`.git`, `.vscode`) are drawn faded.
- Hovering a cut-off name, date or type shows the full text in place.
- Keyboard: arrows, `Home`/`End`, `PgUp`/`PgDn`, `←`/`→` collapse/expand,
  `Ctrl+A`, `Ctrl+Space`, `F5` refresh.

### Tabs and panes

- `Ctrl+T` new tab, `Ctrl+W` close, `Ctrl+Tab` / `Ctrl+Shift+Tab` cycle.
- Drag a tab onto another pane's strip to move it, or onto a pane edge to
  dock it into a new split.
- Double-click a tab (or the empty tab strip) to maximize that pane; again
  to restore.
- Tab context menu: Close, Close Others, Close to the Left / Right,
  Split Right, Split Down, Maximize / Restore pane.

### File operations

- Drag and drop with Windows semantics (`Ctrl` copy, `Shift` move).
- `Ctrl+C` / `Ctrl+X` / `Ctrl+V` clipboard (interoperates with Explorer).
- `Delete` recycles, `Shift+Delete` deletes permanently; both ask for
  confirmation first.
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
- **This PC** lists favorites below the drives, under a ★ Favorites header.
  They expand in place. Right-click one to remove it. They can't be renamed, deleted, cut
  or dragged away from there.

### Filter (experimental)

- The filter box in the toolbar (`Ctrl+F`) narrows the focused tab to rows
  whose names contain the text (case-insensitive). Only rows already shown are
  searched; collapsed folders are not opened.
- Matches are highlighted. Folders kept only because they contain a match are
  dimmed, and an "N items hidden by the filter" row marks each folder that lost
  items.
- Each tab keeps its own filter; switching tabs puts that tab's text back in
  the box. A filtered tab shows a funnel in its title, and the status bar shows
  the match/hidden counts.
- `Esc` (in the box or the grid) or the × button clears it; `Enter`/`Down`
  moves to the list. Navigating the tab to another folder also clears it.
- `Ctrl+A` and Shift-range selection skip the dimmed folders.
- **Search subfolders** (the **Subfolders** checkbox that appears in the filter
  box once you type, or `Alt+S`)
  also searches inside collapsed folders. It is off by default, highlighted when
  on, and unavailable in This PC. Subfolders are read in the background (a
  "Searching subfolders…" row shows progress) with no item limit; turn it off
  (`Alt+S`) to stop a long search. Clearing the filter (`Esc`, ×), navigating,
  or a new tab turns it off; folders opened only by the search close again.

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
