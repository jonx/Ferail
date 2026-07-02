# Feraille

**The power-user file manager that never freezes.** Native, fast, written
in Rust — with the tools you usually install separately built in: a
duplicate finder, a disk-usage treemap you can export as HTML, magic-byte
file identification, bulk rename with regex, a media viewer with sticky
zoom, and a command palette. macOS today, Windows close behind, Linux
underway.

![Feraille main window](docs/images/tour-shell.png)

## Why try it

- **It refuses to beachball.** One rule shapes the whole codebase — *the
  UI must never stop*. Paint, scroll, hover, and typing never touch the
  filesystem, network, or database; a dead network mount slows a
  background task, never your pointer.
  ([how that's enforced](docs/ARCHITECTURE.md#prime-directive))
- **It reads bytes, not extensions.** Every file is identified by
  content — a `.jpg` that's secretly a `.zip` is flagged in the list,
  with real facts (architecture, dimensions, duration) in a Description
  column no other file manager has.
- **The power tools are built in.** Duplicate finder (hash-funnel,
  APFS-clone-aware), disk-usage treemap (multi-select, act on squares,
  **export the picture as embeddable HTML**), regex bulk rename with live
  preview, sticky-zoom media viewer, Cmd+K palette — zero extra downloads.
- **It learns your habits.** The Ant Trail heat-tints the folders you
  actually use and keeps them one click away.
- **Everything is undoable.** Rename, bulk rename, move, copy, trash —
  Cmd+Z, with guards so an undo never overwrites newer work.

## Quick start

```sh
git clone https://github.com/jonx/Feraille && cd Feraille
cargo run --release --bin feraille-gpui
```

On macOS, build the signed bundle instead to get the normal folder-access
prompts: `scripts/bundle-mac.sh && open target/Feraille.app` — details and
per-platform prerequisites in **[GETTING_STARTED.md](GETTING_STARTED.md)**.

## Feature tour

The full tour with a picture per feature lives in
**[docs/FEATURES.md](docs/FEATURES.md)**. A taste:

| Disk-usage treemap → HTML export | Duplicate finder | Bulk rename |
|---|---|---|
| ![Disk Usage](docs/images/tour-disk-usage.png) | ![Duplicates](docs/images/tour-dupes.png) | ![Bulk rename](docs/images/tour-bulk-rename.png) |
| **Icon grid** | **Media viewer + live grading** | **Command palette** |
| ![Icon grid](docs/images/tour-grid.png) | ![Viewer](docs/images/tour-viewer.png) | ![Palette](docs/images/tour-palette.png) |

And the ones a screenshot can't show: sticky zoom that carries your zoom
and pan to the next file; chroma-keyed transparent video windows you can
stack over your desktop (optional mpv backend); a dock drawer that parks
the window against a screen edge and slides in on an edge-slam; streaming
search; quarantine "where from" provenance; a homoglyph-aware deceptive-
filename detector. **[Read the tour →](docs/FEATURES.md)**

## How Feraille compares

Feraille optimizes for what the default managers treat as afterthoughts:
**responsiveness under load** and **content intelligence**. It reuses the
good OS plumbing where it exists — Quick Look, Spotlight, tags, quarantine
on macOS — and builds in the rest. The ❌ rows are real gaps, not modesty.

| Capability | Feraille | Finder (macOS) | File Explorer (Windows) | Dolphin / Nautilus (Linux) |
|---|---|---|---|---|
| **Never-block UI on slow / network I/O** | ✅ design invariant | ❌ beachballs | ❌ hangs | ⚠️ varies |
| **Content / magic detection** (sniff bytes, not extension) | ✅ | ❌ | ❌ | ⚠️ MIME |
| **Built-in disk usage** (treemap, HTML export) | ✅ | ❌ (3rd-party) | ❌ (3rd-party) | ⚠️ separate app |
| **Built-in duplicate finder** | ✅ hash funnel, clone/hard-link aware | ❌ (3rd-party) | ❌ (3rd-party) | ❌ (3rd-party) |
| **Bulk rename** (regex + live preview) | ✅ | ❌ | ⚠️ PowerRename (PowerToys) | ✅ KRename etc. |
| **Predictive navigation** (visit heat) | ✅ Ant Trail | ❌ | ❌ | ❌ |
| **Previews** | ✅ Quick Look + inline highlighted code | ✅ Quick Look | ⚠️ preview pane | ✅ Dolphin strong |
| **Tabs + multi-window** | ✅ | ⚠️ tabs, no split | ⚠️ tabs (recent) | ✅ Dolphin best-in-class |
| **Command palette** | ✅ Cmd+K | ❌ | ❌ | ❌ |
| **Global / indexed search** | ⚠️ Spotlight-backed, single query box | ✅ rich | ✅ indexed | ✅ Tracker / Baloo |
| **Cloud integration** (iCloud / OneDrive) | ⚠️ detects placeholders | ✅ deep | ✅ deep | ⚠️ |
| **3rd-party shell-extension verbs** | ❌ (built-ins + Open With) | ✅ Services | ✅ large ecosystem | ✅ service menus |
| **Accessibility / localization / maturity** | ❌ young, single-dev | ✅ decades | ✅ decades | ✅ mature |

✅ first-class · ⚠️ partial / varies · ❌ absent

## Platform status

| Platform | Status |
|---|---|
| **macOS** | Primary, daily-driver. Feature-complete for everyday use. |
| **Windows** | Active port, broad native parity (clipboard, Recycle Bin, thumbnails, Open With, Media Foundation video). Verified on real hardware; elevation and shell-extension verbs still stubbed. ([details](docs/features/windows-port.md)) |
| **Linux** | Early port — builds and runs; volumes, Trash, Open With real; clipboard/thumbnails/video still stubbed. ([details](docs/features/linux-port.md)) |

Open work lives in [TODO.md](TODO.md).

## macOS permissions (access prompts)

macOS gates Desktop/Documents/Downloads (and removable/network volumes)
behind its privacy system. The familiar consent prompt only appears for a
**code-signed `.app` bundle** with the matching usage strings — the loose
`cargo run` binary can't prompt and shows the in-app "Access required"
screen instead. To get real prompts:

```sh
scripts/bundle-mac.sh && open target/Feraille.app
```

Caveats (stale terminal grants, ad-hoc signing, re-prompts) are covered in
[GETTING_STARTED.md](GETTING_STARTED.md#macos-permissions); the short
version: sign with a real identity for grants that stick, and if a prompt
never shows, `tccutil reset` the stale state.

## Project layout

The active app is `feraille-gpui`. Domain logic lives in UI-free crates.

| Crate | Responsibility |
|---|---|
| `feraille-gpui` | Active GPUI app + CLI entry points; views, actions, scheduling, shell state |
| `feraille-core` | Platform-neutral domain types, command catalogue, `NodeId`, `FileEntry` |
| `feraille-fs-native` | Native filesystem: enumeration, metadata, magic, volumes, trash |
| `feraille-shell-mac` | AppKit/Cocoa integration — no painting |
| `feraille-shell-win32` | Windows shell integration |
| `feraille-shell-linux` | Linux (freedesktop) shell integration |
| `feraille-meta` | SQLite-backed metadata, layout, and Ant Trail persistence |
| `feraille-disk-usage` | Pure disk-usage model, aggregation, treemap layout, HTML export |
| `feraille-design` | Shared design tokens (color, spacing, typography) |

Crate-boundary rules:
[docs/ARCHITECTURE.md → Crate Boundaries](docs/ARCHITECTURE.md#crate-boundaries).

## Documentation

| Document | Purpose |
|---|---|
| [docs/FEATURES.md](docs/FEATURES.md) | **The feature tour** — what the app does, with screenshots. |
| [GETTING_STARTED.md](GETTING_STARTED.md) | Zero-to-running setup, per platform. |
| [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md) | Source of truth for crate boundaries, data model, scheduling. |
| [docs/features/](docs/features/README.md) | Deep design notes per feature. |
| [TODO.md](TODO.md) | Open work and roadmap. |
| [CLAUDE.md](CLAUDE.md) | Operating manual for AI/human contributors. |

## Contributing

See [CONTRIBUTING.md](CONTRIBUTING.md). Feraille is, by design, mostly
**AI pair-programmed** — that's the workflow, not a caveat — and
**AI-generated pull requests are welcome**: point your agent at
[CLAUDE.md](CLAUDE.md) and [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md),
keep the prime directive, and run `cargo check` / `cargo test` before
opening the PR. Human review gates every merge. In short:

- New product work belongs in `crates/feraille-gpui`; keep domain code
  UI-free in the core crates.
- Before finishing: `cargo check`, `cargo test`, and a fresh screenshot
  for UI changes. See [CLAUDE.md](CLAUDE.md#verification).

## License

Dual-licensed under either of

- MIT license ([LICENSE-MIT](LICENSE-MIT))
- Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE))

at your option.

Third-party components incorporated into a built binary (GPUI,
gpui-component, and the bundled Lucide / Bootstrap icon artwork) are
credited in [THIRD-PARTY-NOTICES.md](THIRD-PARTY-NOTICES.md).

> **Note:** Feraille depends on `gpui` / `gpui-component` as git
> dependencies (not on crates.io), so there is no `cargo install` — build
> from source; reproducible builds come from the committed `Cargo.lock`.
