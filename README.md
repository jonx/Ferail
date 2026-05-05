# Feraille

Feraille is a blazing-fast macOS file explorer written in Rust. It is the
Mac port and UI rewrite of the Windows project `../Ferail`, but it is not a
skin of the old app: the UI is rebuilt around Feraille's own renderer/control
stack, with the same feature ambition and stricter responsiveness rules.

The prime directive is simple:

> The UI must never stop. Paint, hit-testing, scrolling, hover, and text input
> must never perform filesystem, shell, database, network, thumbnail, preview,
> or magic-sniffing I/O.

## Current State

The app already has a usable Finder-style shell:

- Home and `/Volumes` navigation through tree, breadcrumb, tabs, and list.
- Virtualized file list with columns, sorting, hover, status bar, and scrollbar.
- Breadcrumb edit mode with `Cmd+L` / `Ctrl+L`.
- Open file, refresh, show hidden files, delete to Trash fallback.
- macOS system icons through `NSWorkspace`.
- macOS chrome inset, context menu slice, drag-out slice, reveal in Finder,
  copy path, rename dialog, new-folder dialog, search/filter dialog, preview
  info pane, and in-memory Ant Trail heat.
- Magic sniffing is off the UI thread and feeds the Magic column when ready.
- Headless screenshot CLI for visual verification.

The feature parity ledger lives in [docs/FEATURE_LEDGER.md](docs/FEATURE_LEDGER.md).
The Ferail source-doc mapping lives in [docs/porting/FERAIL_DOCS_MAP.md](docs/porting/FERAIL_DOCS_MAP.md).

## Architecture

```text
feraille-app                  binary; owns window, event loop, app state
   |-- feraille-controls      primitives and explorer controls
   |-- feraille-design        tokens and visual constants
   |-- feraille-render        renderer trait and soft renderer
   |-- feraille-core          shared model types, NodeId, FileEntry, AntTrail
   |-- feraille-fs-native     native filesystem and metadata helpers
   |-- feraille-shell-mac     Cocoa/AppKit shell integrations
   `-- feraille-shell-win32   placeholder/future Windows shell integrations
```

Read [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md) before moving feature
boundaries. Read [docs/UI_NONBLOCKING.md](docs/UI_NONBLOCKING.md) before
touching paint, navigation, enumeration, preview, icons, magic, context menus,
or drag/drop.

## Running

```sh
cargo run --bin feraille
```

Useful bindings:

- `Enter`: open selected folder/file.
- `Backspace`: parent folder.
- `Cmd+L` / `Ctrl+L`: edit path.
- `Cmd+F` / `Ctrl+F`: filter current folder.
- `Cmd+P` / `Ctrl+P`: toggle preview info pane.
- `Cmd+I` / `Ctrl+I`: Get Info panel.
- `F2`: rename dialog.
- `Cmd+Shift+N` / `Ctrl+Shift+N`: new folder dialog.
- `Cmd+Shift+C` / `Ctrl+Shift+C`: copy path.
- `Cmd+R` / `Ctrl+R`: reveal in Finder.
- `F5`: refresh.
- `Cmd+Shift+.` / `Ctrl+H`: show/hide hidden files.

## Screenshot CLI

The binary can render deterministic PNGs without opening a window:

```sh
cargo run --bin feraille -- \
  --screenshot /tmp/feraille.png \
  --navigate ~/Source/Feraille \
  --width 1400 --height 900 \
  --select-name Cargo.toml \
  --preview
```

Run:

```sh
cargo run --bin feraille -- --help
```

The CLI is the preferred way to verify visual changes in this repo. See
[docs/TESTING_OVERLAYS.md](docs/TESTING_OVERLAYS.md) for planned debug states
and visual overlays.

## Specs

- [specs/ux](specs/ux) covers product behavior and performance.
- [specs/controls](specs/controls) covers tokens, primitives, controls, and
  state machines.
- [docs/features](docs/features) contains cleaned, Mac-aware reconstructions
  of the major Ferail feature notes.

## License

Dual-licensed under MIT or Apache-2.0, at your option.
