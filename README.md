# Branch

A Finder-style, tree-first file browser for Windows, built on WPF / .NET 9.

Branch shows your file system as a real collapsible tree (not a flat list), with
multiple tabs, split panes, and Explorer-grade file operations.

## Features

### Tree view

- Full-file-system tree with per-folder expand / collapse chevrons.
- Double-click a folder to re-root the tree on it; double-click the root again
  to jump back up.
- Type-to-search within a folder jumps to the next matching child.

### Tabs

- Multiple tabs per pane
- Double-click a tab header to temporarily **maximize** it fullscreen; double-
  click again (or start dragging it) to restore the previous layout. Drag-and-
  drop of a maximized tab auto-restores the layout for the duration of the drag
  and re-maximizes after drop.
- Tab context menu: **Close**, **Close Others**, **Close to the Right**,
  **Duplicate**, **Split Right**, **Split Down**.

### Split panes

- **Split Right** / **Split Down** adds a new pane next to the current one
  with a fresh "This PC" tab. 

### File operations

- **Drag and drop** with Windows semantics (hold `Ctrl` to copy). 
- **Ctrl+C** — copy selected paths to the clipboard.
- **Ctrl+V** — paste clipboard file list into the selected folder.
- **Ctrl+Delete** — move selected items to the Recycle Bin.
- **F2 / Enter** — rename (single-selection only).
- **Ctrl+Z** — undo move / copy / paste / delete / rename. Multi-item
  operations are undone as a single action.
- Full **Windows Shell context menu** on right-click, including extended
  verbs when holding `Shift`. Creating a new item via the menu refreshes the
  parent folder and starts a rename on the new item.
- `Refresh` button re-reads the loaded subtree without re-rooting.

### Multi-selection

- **Ctrl+click** toggles an item in the selection.
- **Shift+click** selects the range between the anchor and the clicked item
  (following the visible tree order).

### Zip archives

- Zip files show an expand chevron alongside folders. Expanding a zip reads
  its directory virtually — the archive is **not** unpacked on disk.
- Entries inside a zip appear as regular tree nodes (folders and files).
- Opening a zip entry (double-click) extracts just that entry to
  `%TEMP%\FileBrowserZip\…` and opens it with the default handler.
- Dragging a zip entry out of Branch extracts on-demand to a temp path and
  hands the extracted file to the drop target.

### Favorites

- ★ button in the toolbar opens a **popup** Favorites panel (context-menu
  style, auto-dismisses on click outside or when a favorite is chosen).
- Add the current folder to Favorites; remove via context menu.
- Open a favorite in the current tab or as a new tab.

### Persistence

- On exit, Branch saves: window size, column widths, sort state, favorites,
  full pane tree (orientation + sizes), per-tab roots, per-tab expanded paths,
  per-tab selection, per-tab scroll offset, and which tab is active in each
  pane. All of this is restored on the next launch.

## Build

```powershell
# Debug
dotnet build

# Release
dotnet build -c Release

# Self-contained single-file executable
dotnet publish -c Release -r win-x64 --self-contained true /p:PublishSingleFile=true
```

Requires .NET 9 SDK. Targets `net9.0-windows`.
