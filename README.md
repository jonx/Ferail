# Feraille

A blazing-fast Windows file explorer, written in Rust.

> **Status:** iter-2 shipped. Real macOS browser of $HOME with Zed-aligned
> tokens, glyph-cached soft renderer, Selection model, virtualized list +
> scrollbar + splitter + focus ring, multi-tab state, breadcrumb (read-only
> segments), and a minimal lazy-loading FileTree. Drag/drop, context menu,
> native macOS chrome, GPU renderer, and shell-side features land in
> iter-3+ per [CLAUDE.md](CLAUDE.md).

## Why

Existing options bottom out somewhere:

- **Windows Explorer** is fast on huge folders but visually stagnant and
  closed.
- **Files App** (WinUI 3) looks great but is *slower* than Explorer at
  scale.
- **Total Commander / Directory Opus** are fast and powerful but show their
  age visually.

Feraille is the "and": Explorer's speed, Files App's polish, an open codebase
to extend.

## Architecture (one-line)

UI talks to a **filesystem trait**, not to platform APIs. Shell glue (Win32
COM, IShellFolder, IContextMenu, IFileOperation) lives in one crate behind
that trait. Controls render via a **renderer trait** with a software backend
for dev iteration on macOS and a Direct2D backend for production on Windows.

```
feraille-app                  binary; owns window + state
   ├── feraille-controls      primitives + explorer controls (this crate is the "spec implemented")
   │     ├── feraille-design  design tokens (no rendering deps)
   │     └── feraille-render  Renderer trait + soft backend (D2D backend lands later)
   ├── feraille-core          NodeId, FileEntry, FsBackend trait — UI knows nothing about Win32
   ├── feraille-fs-native     std::fs implementation of FsBackend (cross-platform)
   └── feraille-shell-win32   Windows shell glue (no-op on macOS)
```

Direction of dependency is one-way. `feraille-controls` does not know what
a path is; `feraille-shell-win32` does not know what a control is.

## Specs

The project is **spec-driven**. Read these before changing UI code:

- [specs/controls/](specs/controls/) — design tokens, primitives, explorer
  controls, the interactive state machine.
- [specs/ux/](specs/ux/) — navigation, selection, keyboard, drag-drop,
  performance budgets, error/empty states.

If you find yourself wanting to add a UI behavior that contradicts a spec,
edit the spec first.

## Running

You'll need Rust stable (1.78+).

```sh
cd Feraille
cargo run --bin feraille
```

A window opens with header / tabstrip / [tree | breadcrumb / list] / status.
$HOME is enumerated on launch; the FileTree shows it as a root alongside
any `/Volumes/*` mounts.

- Click a tree row → expand/select; chevron → expand without selecting.
- Click a breadcrumb segment → navigate to that ancestor.
- Click "+" in tabstrip → new tab; click X on tab hover → close.
- Drag the splitter → resize tree pane.
- Drag the scrollbar thumb → scroll the file pane.
- ↑/↓ / PgUp/PgDn / Home/End — keyboard navigation in the file pane.
- Enter on a directory row → open it. Backspace → parent.
- `FERAILLE_THEME=dark cargo run` → dark mode.
- Esc → quit.

## Screenshot CLI

For UI verification without manual interaction, the binary supports a
headless screenshot mode:

```sh
cargo run --bin feraille -- \
    --screenshot out.png \
    --width 1180 --height 760 --scale 2.0 \
    --theme dark \
    --navigate ~/Source/Feraille \
    --expand ~/Source --expand ~/Source/Feraille \
    --select-name Cargo.toml \
    --splitter 240
```

Run `cargo run --bin feraille -- --help` for the full flag list. Mouse
drags and animations aren't scriptable — anything visual that depends on
those needs the GUI binary.

## Targets

| Target | Status |
|---|---|
| `aarch64-apple-darwin` (dev mode, soft renderer) | working |
| `x86_64-apple-darwin` (dev mode, soft renderer) | should work, untested |
| `x86_64-pc-windows-msvc` (production, D2D renderer) | rustup target installed; `cargo-xwin` cross-compile + D2D backend in iteration 3 |

## Inspiration vs. dependency

We studied the structure of [Zed's GPUI](https://github.com/zed-industries/zed/tree/main/crates/gpui)
— its `uniform_list`, scrollbar, and DirectX renderer — but copied no code
from it. Feraille has zero GPUI dependency. The reasons are in the
conversation log and `specs/controls/00-overview.md`: GPUI is pre-1.0,
Zed-coupled, and gives us the easy half (rendering primitives) while not
solving the hard half (shell integration).

## License

Dual-licensed under MIT or Apache-2.0, at your option.
