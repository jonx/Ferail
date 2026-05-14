# Feraille

Feraille is a fast macOS file manager written in Rust with GPUI and
gpui-component. It is the macOS successor to the Windows project
`../Ferail`, but it is not a skin of the old app: the UI, native shell
integration, and responsiveness model are macOS-first.

The prime directive is simple:

> The UI must never stop. Render, hit testing, scrolling, hover, and text
> input must not perform filesystem, shell, database, network, thumbnail,
> preview, or magic-sniffing I/O.

## Current App

The active app is `feraille-gpui`.

It currently provides:

- Native macOS chrome with a GPUI title bar.
- Favorites, Browse, Volumes, and Trash in the sidebar.
- Virtualized file table with sortable, resizable, reorderable columns.
- Magic-first `Format` column with extension fallback and mismatch cues.
- Real file/folder icons, Finder tags, quarantine cues, and preview metadata.
- Tabs, breadcrumbs, filtering, history navigation, context menus, and
  resizable panels.
- Settings built with gpui-component settings primitives.
- Disk Usage window with async scanning and treemap/top-list views.
- Status/task feedback and crash diagnostics.
- Headless screenshot support for visual verification.

## Architecture

Read [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md) before changing crate
boundaries, UI-thread behavior, filesystem access, native shell work, or
worker scheduling.

Open work lives in [TODO.md](TODO.md). Deeper feature notes live under
[docs/features](docs/features).

## Running

```sh
cargo run --bin feraille-gpui
```

Useful non-GUI commands:

```sh
cargo run --bin feraille -- magic <path>...
cargo run --bin feraille -- du [--top N] [--packages] <path>
```

Screenshot CLI:

```sh
cargo run --bin feraille-gpui -- \
  --screenshot screenshots/feraille.png \
  --navigate ~/Source/Feraille \
  --width 1400 --height 900 \
  --select-name Cargo.toml \
  --preview
```

Reset local metadata:

```sh
cargo run --bin feraille-gpui -- --reset-db <scope>
```

## Development Notes

- New product work belongs in `crates/feraille-gpui`.
- Domain code belongs in `feraille-core`, `feraille-fs-native`,
  `feraille-meta`, or `feraille-disk-usage` when it can stay UI-free.
- The old soft-rendered crates are reference/fallback code until the GPUI
  shell fully replaces them.
- Do not run broad formatters casually; this repo often has local work in
  progress.

## License

Dual-licensed under MIT or Apache-2.0, at your option.
