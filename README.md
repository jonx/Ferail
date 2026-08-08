# Ferail

**A power-user file manager that never freezes.** Native, fast, written in
Rust — with the tools you normally install separately built in: a duplicate
finder, a disk-usage treemap you can export as HTML, an archive browser that
opens `.zip` / `.7z` / `.tar.*` / `.lha` in place, magic-byte file identification,
regex bulk rename, a media viewer, and a command palette.

![Ferail main window](docs/images/tour-shell.png)

## What is Ferail?

Ferail is a desktop file manager built around a single conviction: **the UI
must never stop.** Painting, scrolling, hovering, hit-testing, and typing are
read-only and nonblocking — they never touch the filesystem, network, or
database. A dead network mount slows a background task, never your pointer.
Everything expensive happens off the UI thread and reports back when it's
ready; results that arrive after you've moved on are dropped.

On top of that responsive core, Ferail folds in the utilities power users
otherwise juggle as separate apps, and adds content intelligence the stock
managers lack: every file is identified by its **bytes, not its extension**, so
a `.jpg` that's secretly a `.zip` is flagged in the list, with real facts —
architecture, dimensions, duration — surfaced in a Description column.

It targets the gap the default managers leave: **responsiveness under load** and
**content intelligence**, without a pile of third-party add-ons.

## What makes it different

- **It refuses to beachball.** One invariant shapes the whole codebase.
  ([how it's enforced](docs/ARCHITECTURE.md#prime-directive))
- **It reads bytes, not extensions.** Content/magic detection identifies every
  file and reports real metadata no other manager shows.
- **The power tools are built in.** Duplicate finder (hash-funnel,
  clone/hard-link aware), disk-usage treemap (multi-select, act on squares,
  **export the picture as embeddable HTML**), regex bulk rename with live
  preview, sticky-zoom media viewer, Cmd+K command palette — zero extra
  downloads.
- **Archives open like folders.** Browse inside a `.zip`, `.7z`, `.tar.*` or
  `.lha`/`.lzh` (the Amiga/Aminet format, read-only)
  without extracting first — a real sortable list with expandable folders and a
  filter box, so a 5000-file archive opens as one folder to drill into. Extract
  just what you selected, drag files in to add them, drag entries out to extract
  them, and compress to ZIP / 7-Zip / TAR through a built-in engine that behaves
  the same on every platform. It works on anything that *is* an archive
  underneath, even without the extension — `.docx`, `.jar`, `.apk`.
- **It learns your habits.** The Ant Trail heat-tints the folders you actually
  use and keeps them one click away.
- **Everything is undoable.** Rename, bulk rename, move, copy, trash — Cmd+Z,
  with guards so an undo never overwrites newer work.

## Feature tour

The full tour with a picture per feature lives in
**[docs/FEATURES.md](docs/FEATURES.md)**. A taste:

| Disk-usage treemap → HTML export | Duplicate finder | Bulk rename |
|---|---|---|
| ![Disk Usage](docs/images/tour-disk-usage.png) | ![Duplicates](docs/images/tour-dupes.png) | ![Bulk rename](docs/images/tour-bulk-rename.png) |
| **Icon grid** | **Media viewer + live grading** | **Command palette** |
| ![Icon grid](docs/images/tour-grid.png) | ![Viewer](docs/images/tour-viewer.png) | ![Palette](docs/images/tour-palette.png) |
| **Archives, browsed in place** | **Ant Trail visit heat** | **Content-derived descriptions** |
| ![Archive workbench](docs/images/tour-archive.png) | ![Ant Trail](docs/images/tour-ant-trail.png) | ![Magic descriptions](docs/images/tour-magic.png) |

**Deceptive filenames, exposed** — zero-widths, bidi overrides, homoglyphs and
disguised whitespace are rendered as visible chips instead of letting a name lie:

![Filename hazards](docs/images/tour-filename-hazards.png)

**The same binary is a command-line toolbox** — `ferail magic` prints what
files really are (bytes, not extensions), `ferail du` a disk-usage summary,
`ferail thumb` extracts any file's thumbnail to a PNG, and `ferail doctor`
runs a config/storage/dependency health check:

![Ferail CLI](docs/images/tour-cli.png)

And the ones a screenshot can't show: sticky zoom that carries your zoom and pan
to the next file; chroma-keyed transparent video windows you can stack over your
desktop (optional mpv backend); a dock drawer that parks against a screen edge
and slides in on an edge-slam; streaming search; quarantine "where from"
provenance.
**[Read the tour →](docs/FEATURES.md)**

## Vision & roadmap

One file manager that is responsive, content-aware, and equally at home on every
desktop — with the power tools first-class instead of bolted on.

- **Cross-platform parity.** macOS is the daily driver today; the roadmap is
  full native parity on Windows and Linux (clipboard, trash, thumbnails, "open
  with", native video), reusing OS plumbing where it's good and building in the
  rest.
- **Deeper content intelligence.** File-level frecency feeding search ranking,
  smart folders / saved searches that re-run live, and richer magic/metadata.
- **Sharper everyday flow.** Clipboard history with a paste picker, command-
  palette navigation polish, and a unified selection/hover design language.

The prioritized list of open work is in **[TODO.md](TODO.md)**.

## Current status

| Platform | Status |
|---|---|
| **macOS** | **Primary, daily-driver.** Feature-complete for everyday use. |
| **Windows** | **Active port**, broad native parity — clipboard, Recycle Bin, thumbnails, Open With, Media Foundation video, UAC elevation + Restart Manager lock diagnostics. Builds, runs, and screenshots on real hardware. Still missing: third-party shell-extension verbs, WSL, an indexed search engine, and the window-docking feature. ([details](docs/features/windows-port.md)) |
| **Linux** | **Early port** — builds and runs; volumes, Trash, and Open With are real; clipboard, thumbnails, and video are still stubbed. ([details](docs/features/linux-port.md)) |
| **AROS** | **Research port** — Ferail boots and runs as a browsable, themed file manager on [AROS](https://aros.org) (the open-source AmigaOS) via a from-scratch GPUI platform backend. Not at parity: some features are gated pending native shell integration. ([details](docs/features/aros-port.md)) |

### How Ferail compares

The ❌ rows are real gaps, not modesty.

| Capability | Ferail | Finder (macOS) | File Explorer (Windows) | Dolphin / Nautilus (Linux) |
|---|---|---|---|---|
| **Never-block UI on slow / network I/O** | ✅ design invariant | ❌ beachballs | ❌ hangs | ⚠️ varies |
| **Content / magic detection** (sniff bytes, not extension) | ✅ | ❌ | ❌ | ⚠️ MIME |
| **Built-in disk usage** (treemap, HTML export) | ✅ | ❌ (3rd-party) | ❌ (3rd-party) | ⚠️ separate app |
| **Built-in duplicate finder** | ✅ hash funnel, clone/hard-link aware | ❌ (3rd-party) | ❌ (3rd-party) | ❌ (3rd-party) |
| **Browse inside archives** (no extract-first) | ✅ zip, 7z, tar.*, lha — browse, add, extract selected | ❌ extract only | ⚠️ zip only | ✅ Ark / File Roller |
| **Bulk rename** (regex + live preview) | ✅ | ❌ | ⚠️ PowerRename (PowerToys) | ✅ KRename etc. |
| **Predictive navigation** (visit heat) | ✅ Ant Trail | ❌ | ❌ | ❌ |
| **Previews** | ✅ Quick Look + inline highlighted code | ✅ Quick Look | ⚠️ preview pane | ✅ Dolphin strong |
| **Tabs + multi-window** | ✅ | ⚠️ tabs, no split | ⚠️ tabs (recent) | ✅ Dolphin best-in-class |
| **Command palette** | ✅ Cmd+K | ❌ | ❌ | ❌ |
| **Global / indexed search** | ⚠️ Spotlight-backed, single query box | ✅ rich | ✅ indexed | ✅ Tracker / Baloo |
| **Accessibility / localization / maturity** | ❌ young, single-dev | ✅ decades | ✅ decades | ✅ mature |

✅ first-class · ⚠️ partial / varies · ❌ absent

## Download

Prebuilt builds are published on the
[Releases](https://github.com/jonx/Ferail/releases) page.

| Platform | Download | Signed |
|---|---|---|
| **macOS** (Apple silicon) | `Ferail-<version>.dmg` — open it and drag Ferail to Applications | ✅ Developer ID signed **and notarized**, so it opens without warnings |
| **Windows** (x64) | `Ferail-<version>-win-x64.zip` — unzip anywhere and run `Ferail.exe`. No installation, no admin rights. An Inno Setup installer ships alongside it when built. | ❌ unsigned — see below |
| **Linux** (Ubuntu 22.04+ / Debian, Intel & ARM) | `ferail_<version>-1_amd64.deb` or `…_arm64.deb` — install with `sudo apt install ./ferail_*.deb`, run `ferail` | — (unsigned, like most out-of-repo `.deb`s; built and smoke-tested by CI) |

The platforms are not always released in lockstep: the newest downloads may
sit on different release tags. Check the
[Releases](https://github.com/jonx/Ferail/releases) list for the latest of
each.

> ### ⚠️ The Windows download is not code-signed yet
>
> Windows will show **"Windows protected your PC"** when you run it. That is
> SmartScreen reporting that the file carries no Authenticode signature — it is
> not a malware detection. Click **More info → Run anyway** to proceed.
>
> Code-signing certificates cost a few hundred euros a year and, since 2023,
> require a hardware token, so this project ships unsigned for now. **Verify the
> download with its SHA-256 instead** — that catches a corrupted or tampered
> download, though, unlike a signature, it cannot prove *who* built the file:
>
> ```pwsh
> Get-FileHash .\Ferail-0.3.0-win-x64.zip -Algorithm SHA256
> ```
>
> ```
> 451CEF7F00EF3C92D8A2B961E583AD79231DE6059CF0A7B10FCC285CF8E1F404
> ```
>
> Each release publishes its own checksum in its notes. If it matches, the file
> is exactly what was built here. If it doesn't, delete it.

The macOS download carries no such warning — it is signed with a Developer ID
and notarized by Apple.

## Getting started

```sh
git clone https://github.com/jonx/Ferail && cd Ferail
cargo run --release --bin ferail-gpui
```

On macOS, build the signed bundle instead to get the normal folder-access
prompts (Desktop / Documents / Downloads and removable/network volumes are
gated behind the OS privacy system, which only prompts a **code-signed `.app`
bundle** — the loose `cargo run` binary shows an in-app "Access required" screen
instead):

```sh
scripts/bundle-mac.sh && open target/Ferail.app
```

On Windows, build a distributable (portable ZIP, plus an installer when
[Inno Setup](https://jrsoftware.org/isinfo.php) is present). Licence notices are
staged next to the binaries, and Authenticode signing is wired but optional —
an unsigned build runs fine locally, but every downloader of one gets a
SmartScreen warning, so a public release wants a certificate:

```pwsh
./scripts/package-win.ps1                              # unsigned, local testing
./scripts/package-win.ps1 -SignCert C:\certs\ferail.pfx  # release
```

Per-platform prerequisites, permission caveats, and packaging live in
**[GETTING_STARTED.md](GETTING_STARTED.md)**.

## Project layout

The active app is `ferail-gpui`. Domain logic lives in UI-free crates.

| Crate | Responsibility |
|---|---|
| `ferail-gpui` | Active GPUI app + CLI entry points; views, actions, scheduling, shell state |
| `ferail-core` | Platform-neutral domain types, command catalogue, `NodeId`, `FileEntry` |
| `ferail-fs-native` | Native filesystem: enumeration, metadata, magic, volumes, trash |
| `ferail-shell-mac` | AppKit/Cocoa integration — no painting |
| `ferail-shell-win32` | Windows shell integration |
| `ferail-shell-linux` | Linux (freedesktop) shell integration |
| `ferail-shell-aros` | AROS shell integration (research port) |
| `ferail-meta` | SQLite-backed metadata, layout, and Ant Trail persistence |
| `ferail-disk-usage` | Pure disk-usage model, aggregation, treemap layout, HTML export |
| `ferail-archive` | Pure archive model — format identity, capability matrix, TOC entries (codecs live in `ferail-fs-native`) |
| `ferail-video-mpv` | Optional libmpv video provider for the viewer (`--features mpv`) |
| `ferail-aros-app` | Ferail as an AROS `C:` command (research port) |
| `ferail-design` | Shared design tokens (color, spacing, typography) |

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
| [CHANGELOG.md](CHANGELOG.md) | What changed, newest first. |
| [CLAUDE.md](CLAUDE.md) | Operating manual for AI/human contributors. |

## Contributing

See [CONTRIBUTING.md](CONTRIBUTING.md). Ferail is, by design, mostly **AI
pair-programmed** — that's the workflow, not a caveat — and **AI-generated pull
requests are welcome**: point your agent at [CLAUDE.md](CLAUDE.md) and
[docs/ARCHITECTURE.md](docs/ARCHITECTURE.md), keep the prime directive, and run
`cargo check` / `cargo test` before opening the PR. Human review gates every
merge.

- New product work belongs in `crates/ferail-gpui`; keep domain code UI-free
  in the core crates.
- Before finishing: `cargo check`, `cargo test`, and a fresh screenshot for UI
  changes. See [CLAUDE.md](CLAUDE.md#verification).

## License

Dual-licensed under either of

- MIT license ([LICENSE-MIT](LICENSE-MIT))
- Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE))

at your option.

Third-party components incorporated into a built binary (GPUI, gpui-component,
and the bundled Lucide / Bootstrap icon artwork) are credited in
[THIRD-PARTY-NOTICES.md](THIRD-PARTY-NOTICES.md).

> **Note:** Ferail depends on `gpui` / `gpui-component` as git dependencies
> (not on crates.io), so there is no `cargo install`. Use a
> [prebuilt download](#download) or build from source; reproducible builds come
> from the committed `Cargo.lock`.
