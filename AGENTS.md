# Agent instructions -- Twig (Rust / Win32)

Twig is a native Windows file browser written in Rust using raw Win32 through
`windows-rs` (pinned `=0.58.0`). There is no UI framework, no XAML, no .NET.

## Build & test

```powershell
cargo build                 # must produce 0 warnings
cargo test                  # pure-logic tests (util, layout, model, settings)
cargo build --release       # target\release\Twig.exe (LTO, stripped)
```

A running `Twig.exe` locks `target\release\Twig.exe`; close it before a
release rebuild.

## Modules (`src/`)

| Module | Responsibility |
|---|---|
| `main.rs` | Entry point, COM/DPI init, message loop |
| `app.rs` | `App` state: panes, tabs, navigation, loader results, undo, favorites |
| `mainwin.rs` | Main window: toolbar, path box, tab strips, splitters, tab drag/dock, shortcuts |
| `grid.rs` | Custom-drawn virtualized tree grid (one window per pane), rename edit |
| `model.rs` | Per-tab generational node arena: sort, merge, flatten |
| `layout.rs` | N-ary split tree (pure logic) |
| `loader.rs` | Background listing / icon worker pool |
| `watch.rs` | Recursive `ReadDirectoryChangesW` watchers |
| `fileops.rs` | `IFileOperation` on an STA worker, undo records |
| `shell.rs` | PIDLs, context menus, data objects, clipboard, Recycle Bin restore |
| `dragdrop.rs` | OLE drop targets |
| `zipfs.rs` | Zip entry extraction to `%TEMP%\Twig\` |
| `settings.rs` | JSON persistence + legacy .NET settings import |
| `theme.rs`, `win.rs`, `util.rs` | Colors/fonts/dark mode, Win32 helpers, pure helpers |

## Critical rules

- **App state** lives in a thread-local `RefCell` accessed via `with_app`.
  `with_app` returns `None` when re-entered. Never run anything that pumps
  messages inside it: modal menus, dialogs, `TrackPopupMenu`, `DoDragDrop`,
  `SetFocus`, `MessageBox`. Return a `Deferred` value and run it after the
  borrow ends (see `run_deferred` in `grid.rs`/`mainwin.rs`).
- Focus handlers only post `WM_FOCUS_CHANGED`; `WM_PAINT` arriving while the
  app is borrowed validates and re-posts a repaint.
- Worker threads never touch UI or `App`; they post results to the main
  window.
- Node ids are generational; always re-check with `tree.get(id)` /
  `tree.exists(id)` after async boundaries.
- Keep pure logic (util, layout, model, settings) free of Win32 so it stays
  unit-testable; add tests there for behavior changes.
- Settings are written atomically (`.tmp` + rename) on close.
