# Feraille

**A fast, native cross-platform file manager written in Rust — macOS and
Windows, with Linux possibly to follow.**

Feraille is built on [GPUI](https://www.gpui.rs/) and
[gpui-component](https://github.com/longbridge/gpui-component). It began as a
successor to the Windows project `Ferail`, but it is not a reskin: the UI,
native shell integration, and responsiveness model are designed for each
platform. The macOS app is the furthest along; the Windows port is in active
development (see [docs/features/windows-port.md](docs/features/windows-port.md)),
and Linux may come later (a starting orientation lives in
[docs/features/linux-port.md](docs/features/linux-port.md)).

> ### 🤖 Built with AI — and accepting AI pull requests
>
> Feraille is, by design, **mostly "vibe-coded"**: the bulk of it was written
> through AI pair-programming rather than typed by hand. That's not a caveat,
> it's the workflow.
>
> **AI-generated pull requests are welcome and encouraged.** Point your agent
> at [CLAUDE.md](CLAUDE.md) (the operating manual for AI/human contributors)
> and [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md), keep the prime directive
> below, and run `cargo check` / `cargo test` before opening the PR. Human
> review still gates every merge.

One rule shapes the whole design — **the UI must never stop:**

> Render, hit testing, scrolling, hover, and text input never perform
> filesystem, shell, database, network, thumbnail, preview, or
> magic-sniffing I/O. Expensive work is scheduled off the hot path and
> dropped if the user has already moved on.

See [Architecture → Prime Directive](docs/ARCHITECTURE.md#prime-directive)
for how this is enforced across the codebase.

![Feraille main window](docs/images/readme-shell.png)

## Features

- **Native window chrome** — GPUI title bar, transparent titlebar, system
  theme (macOS today; matching Windows chrome is in progress).
- **Virtualized file table** — sortable, resizable, reorderable columns over
  large directories without jank.
- **Magic-first `Format` column** — content sniffing with extension fallback
  and mismatch / quarantine cues.
  ([spec](docs/features/MAGIC_DESCRIPTION.md))
- **Rich sidebar** — Favorites, Browse tree, Volumes with capacity bars, and
  Trash, with drag-and-drop. ([spec](docs/features/FAVORITES.md))
- **Tabs & windows** — per-tab directory, history, filter, sort, and selection;
  multi-window with closed-tab undo.
  ([spec](docs/features/feraille-windows-instances-tabs-spec.md))
- **Preview pane** — dense, async metadata surface for the current selection.
  ([spec](docs/features/PREVIEW.md))
- **Disk Usage window** — async scanning with squarified treemap and top-list
  views. ([spec](docs/features/DISK_USAGE.md))
- **Persistent metadata** — SQLite-backed store for derived metadata, layout,
  and Ant Trail history. ([spec](docs/features/METADATA_DB.md))
- **CLI utilities** and **headless screenshots** for scripting and visual
  verification.

| Preview pane | Disk Usage | Settings |
|---|---|---|
| ![Preview pane](docs/images/readme-preview.png) | ![Disk Usage](docs/images/readme-disk-usage.png) | ![Settings](docs/images/readme-settings.png) |

## How Feraille Compares

Feraille is a **power-user file manager** that optimizes for two things the
default managers treat as afterthoughts: **responsiveness under load** (the
prime directive above) and **content intelligence** (magic detection, built-in
disk usage, predictive navigation, a duplicate finder, and search that rides
the OS index). It reuses the good OS plumbing where it exists — Quick Look,
Spotlight, system tags, quarantine on macOS — and builds in the power tools
you'd otherwise install separately.

It is also young and single-developer, so it trades maturity for focus. The
table below is an honest scorecard, not a victory lap — the ❌ rows are real
gaps. Status reflects
the furthest-along platform; the Windows port is
[in progress](docs/features/windows-port.md) and there is no Linux build yet
([orientation only](docs/features/linux-port.md)).

| Capability | Feraille | Finder (macOS) | File Explorer (Windows) | Dolphin / Nautilus (Linux) |
|---|---|---|---|---|
| **Never-block UI on slow / network I/O** | ✅ design invariant | ❌ beachballs | ❌ hangs | ⚠️ varies |
| **Content / magic detection** (sniff bytes, not extension) | ✅ | ❌ | ❌ | ⚠️ MIME |
| **Built-in disk usage** (treemap) | ✅ | ❌ (3rd-party) | ❌ (3rd-party) | ⚠️ separate app |
| **Built-in duplicate finder** | ✅ size→hash funnel, cached, hard-link aware | ❌ | ❌ | ❌ |
| **Predictive prewarming** (navigation heat / hover) | ✅ Ant Trail | ❌ | ❌ | ❌ |
| **Previews** | ✅ Quick Look + inline syntax-highlighted text/code/markdown | ✅ Quick Look | ⚠️ preview pane | ✅ Dolphin strong |
| **Tabs + multi-window + split** | ✅ tabs + multi-window | ⚠️ tabs, no split | ⚠️ tabs (recent) | ✅ Dolphin best-in-class |
| **Command palette** | ✅ Cmd+K | ❌ | ❌ | ❌ |
| **Global / indexed search** | ✅ rides Spotlight, walker fallback | ✅ Spotlight | ✅ indexed | ✅ Tracker / Baloo |
| **Cloud integration** (iCloud / OneDrive / Drive) | ⚠️ detects placeholders | ✅ deep | ✅ deep | ⚠️ |
| **Network / SMB browse & mount** | ⚠️ mounted shares only | ✅ | ✅ | ✅ Dolphin KIO |
| **3rd-party shell-extension verbs** | ❌ (built-ins + Open With) | ✅ Services | ✅ large ecosystem | ✅ service menus |
| **Accessibility / localization / maturity** | ❌ young, single-dev | ✅ decades | ✅ decades | ✅ mature |

✅ first-class · ⚠️ partial / varies · ❌ absent

**Where it leads everyone:** non-blocking responsiveness and *built-in* power
tools (magic detection, disk-usage treemap, predictive navigation, a duplicate
finder, Spotlight-backed search) — no default ships all of that in one app.
**Where it's behind:** cloud/network/ecosystem depth and the maturity tax. In
short: a tool you'd *choose* for the power features, not yet the one that ships
free with the OS.

## Quick Start

```sh
# Run the app
cargo run --bin feraille-gpui

# CLI utilities
cargo run --bin feraille -- magic <path>...                    # identify file types
cargo run --bin feraille -- du [--top N] [--packages] <path>   # disk usage

# Headless screenshot (used for visual verification)
cargo run --bin feraille-gpui -- \
  --screenshot screenshots/feraille.png \
  --navigate ~/Source/Feraille \
  --width 1400 --height 900 \
  --select-name Cargo.toml --preview

# Reset local metadata
cargo run --bin feraille-gpui -- --reset-db <scope>
```

## Project Layout

The active app is `feraille-gpui`. Domain logic lives in UI-free crates.

| Crate | Responsibility |
|---|---|
| `feraille-gpui` | Active GPUI app + CLI entry points; views, actions, scheduling, shell state |
| `feraille-core` | Platform-neutral domain types, command catalogue, `NodeId`, `FileEntry` |
| `feraille-fs-native` | Native filesystem: enumeration, metadata, magic, volumes, trash |
| `feraille-shell-mac` | AppKit/Cocoa integration (drag-drop, menus, tags, icons) — no painting |
| `feraille-meta` | SQLite-backed metadata, layout, and Ant Trail persistence |
| `feraille-disk-usage` | Pure disk-usage model, aggregation, and treemap layout |
| `feraille-design` | Shared design tokens (color, spacing, typography) |
| `feraille-shell-win32` | Windows shell reference crate (not macOS v1 UI) |

The full crate-boundary rules are in
[docs/ARCHITECTURE.md → Crate Boundaries](docs/ARCHITECTURE.md#crate-boundaries).

## Documentation

| Document | Purpose |
|---|---|
| [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md) | **Source of truth** for crate boundaries, data model, scheduling, and UI structure. Read before changing those. |
| [TODO.md](TODO.md) | Open work and roadmap. |
| [docs/features/](docs/features/README.md) | Deeper design notes per feature — start at the [feature index](docs/features/README.md). |
| [NOTES.md](NOTES.md) | Architecture and decision log for in-progress spec work. |
| [CLAUDE.md](CLAUDE.md) | Operating manual for AI/human contributors. |

## Contributing

See [CONTRIBUTING.md](CONTRIBUTING.md) for the full guide. **AI-generated PRs
are welcome** — point your agent at [CLAUDE.md](CLAUDE.md) first. In short:

- New product work belongs in `crates/feraille-gpui`.
- Domain code belongs in `feraille-core`, `feraille-fs-native`,
  `feraille-meta`, or `feraille-disk-usage` whenever it can stay UI-free.
- Before finishing changes: `cargo check`, `cargo test`, and for UI changes a
  fresh screenshot in `screenshots/`. See [CLAUDE.md](CLAUDE.md#verification).
- Do not run broad formatters casually; this repo often has local work in
  progress.

## License

Dual-licensed under either of

- MIT license ([LICENSE-MIT](LICENSE-MIT))
- Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE))

at your option.

> **Note:** Feraille depends on `gpui` / `gpui-component` as git
> dependencies (they are not published to crates.io), so the crates are not
> on crates.io and there is no `cargo install`. Build from source with
> `cargo run --bin feraille-gpui`; reproducible builds come from the
> committed `Cargo.lock`.
