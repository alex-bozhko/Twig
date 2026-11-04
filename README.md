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
- A clickable breadcrumb bar sits along the bottom of every pane (just the PC
  icon on **This PC**).
- Click the empty part of the breadcrumb bar (or press `Ctrl+L`) to type or
  paste a path. Quotes and `%VARS%` are fine, a file
  path opens its folder with the file selected, `Enter` goes there, and `Esc`
  or clicking away cancels. Starting the text with `:` filters the tab instead
  (see Filter below).
- Each tab has its own back/forward history (the small arrows at the right end
  of the breadcrumb bar, `Alt+←`/`Alt+→`, `Backspace`, or the mouse side
  buttons). Going back restores the previous folder, what was expanded and
  scrolled, and re-selects what was selected.
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

### Command line

- `Twig.exe <path>` opens the folder (or a file's folder with the file
  selected) in a new tab. A zip archive is treated as a file here: its folder
  opens with the archive selected.
- Single instance: launching Twig again forwards the path to the running
  window as a new tab and brings it to the front.

### Settings

- Open with the gear at the right of the status bar or `Ctrl+,`.
- **Default file manager**: "Make Twig the default" registers Twig (for your
  Windows account only, under `HKCU\Software\Classes`) as the default verb for
  folders and drives, so opening a folder from the desktop, apps or the Run
  dialog opens a Twig tab. Double-clicking a folder in an Explorer window
  also opens Twig. "Show in folder" (browsers, IDEs...) opens the folder and
  selects the file. `Win+E` and Explorer's own windows are unchanged.
  "Restore Explorer" removes the registration. The window shows which
  program currently opens folders and drives. The registration stores the
  exe path, so re-register after moving `Twig.exe`.
- About: version, program path and settings folder.
- Updates: **Check for updates** in Settings → About looks up the latest GitHub release. If it's newer, **Update to X** downloads its `Twig.exe`, replaces the running exe and restarts Twig with the session restored. Needs public releases and write access to Twig's folder.

### File operations

- Drag and drop with Windows semantics (`Ctrl` copy, `Shift` move).
- `Ctrl+C` / `Ctrl+X` / `Ctrl+V` clipboard (interoperates with Explorer).
- `Delete` recycles, `Shift+Delete` deletes permanently; both ask for
  confirmation first.
- `F2` / `Enter` rename, `Alt+Enter` properties.
- **New → Folder / Text Document** in the context menu of a folder, a drive or
  the empty space below the rows (the current folder); `Ctrl+Shift+N` creates
  a folder inside the focused folder (or next to a focused file). The new item
  starts in rename mode.
- `Ctrl+Z` undo of copy / move / rename / recycle / new item.
- Full Windows Shell context menu (`Shift` for extended verbs).
- All operations run on a background thread via `IFileOperation`; the UI never
  blocks.

### Action log

- Every copy, move, rename, recycle, permanent delete, new folder / file and
  undo is recorded, including failures with their error.
- Open with the clock button left of the gear or `Ctrl+H`. Newest first,
  with time and location; "Clear log" empties it.
- Click an entry (or `Enter`) to jump there: the active tab opens the folder
  and selects the resulting items (or the originals, or the nearest folder
  that still exists).
- Kept across restarts (last 500 entries) in `%LOCALAPPDATA%\Twig\log.json`.

### Quick Look

- `Space` previews the selected item in a Finder-style pop-up; `Space` or
  `Esc` closes it. Arrow keys keep working and the preview follows the
  selection.
- Text and source files (with line numbers, long lines wrapped) and images (PNG, JPEG, GIF, BMP,
  TIFF, ICO, WebP…) are rendered, including entries inside zip archives;
  anything else gets an info card with icon, size and dates.
- Documents open at 80% of the screen; the open button launches the item; the
  window can be moved and resized and remembers its document size for the
  session.

### Zip archives

- Zip files expand like folders without unpacking on disk.
- Opening, copying or dragging out an entry extracts just that entry to a
  unique folder under `%TEMP%\Twig\` (cleaned up on start).

### Network shares

- UNC paths work anywhere a path does: `\\server\share\folder`, or just
  `\\server` to list its disk shares (hidden `$` shares are skipped).
- Going up from a share root shows the server with that share selected; up
  from a server goes to This PC.
- A server that doesn't answer within 5 seconds reports "Not found" and is
  skipped for 30 seconds, so a dead server can't stall the UI.

### Favorites

- Right-click a folder → **Add to favorites** / **Remove from favorites**
  (its own section in the menu). A tab rooted at a favorite uses the
  favorite's sort; changing the sort there updates the favorite (e.g. keep
  Downloads sorted by date modified). A new favorite takes the current tab's sort.
- **This PC** lists favorites below the drives, under a ★ Favorites header.
  They expand in place. They can't be renamed, deleted, cut
  or dragged away from there.

### Filter (experimental)

- The filter lives in each tab's breadcrumb bar: press `Ctrl+F` (or type `:`
  in the path editor) and type, e.g. `:report`, to narrow the tab to rows whose
  names contain the text (case-insensitive), as you type. Only rows already
  shown are searched; collapsed folders are not opened. `::report` also
  searches subfolders (see below).
- Matches are highlighted. Folders kept only because they contain a match are
  dimmed, and an "N items hidden by the filter" row marks each folder that lost
  items.
- Each tab keeps its own filter. While it's set, the breadcrumb bar shows it as
  a chip (click to edit it, × to clear it), the tab shows a funnel in its
  title, and the status bar shows the match/hidden counts.
- `Enter` or clicking away keeps the filter and closes the editor; `Esc` (in
  the editor or the grid) clears it, as does deleting the `:`. Navigating the
  tab to another folder also clears it.
- `Ctrl+A` and Shift-range selection skip the dimmed folders.
- **Search subfolders** (`::text`, the **Subfolders** checkbox that appears in
  the breadcrumb bar while filtering, or `Alt+S`)
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
