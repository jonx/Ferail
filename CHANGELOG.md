# Changelog

Notable changes to Feraille, newest first. This tracks what you'd notice as a
user; the full detail lives in the git history. Dependency-pin bumps are logged
separately in [CHANGELOG-DEPS.md](CHANGELOG-DEPS.md).

**Unreleased** collects work not yet in a tagged build.

## Unreleased

- **Archive support (compress & extract)** — right-click **Extract** on any
  `.zip`, `.tar`, `.tar.gz`/`.tgz`, `.tar.bz2`, `.tar.xz`, `.gz`, `.bz2`, `.xz`,
  or `.7z`, then **Extract Here** (into the current folder) or **Extract To…**
  (pick a destination). Extraction lands in place when the archive holds a
  single top-level folder, or in a new folder named after the archive
  otherwise — and it's safe against malicious archive paths (no writing outside
  the destination). **Compress** is now a submenu offering **ZIP**, **7-Zip**,
  and **TAR** (Gzip / Bzip2 / XZ / uncompressed), powered by a new built-in
  archive engine (no more shelling out to `ditto`), so it works the same on
  every platform.
- **Browse inside archives** — right-click an archive → **Open as Archive** to
  open a contents view (like Disk Usage): see every file and folder without
  extracting, then **Extract Selected** (cherry-pick) or **Extract All**.
  Formats that can't be edited in place (tar, 7z) are marked read-only.
- **Richer 7-Zip descriptions** — `.7z` files now show their file count, root
  folder, and whether they're encrypted in the Description column, the same way
  ZIPs already did.

## 0.1.0 — 2026-07-24

First signed & notarized macOS build.

- **Folder contents at a glance** — folders now show their recursive item counts
  ("1,204 files · 88 folders") in the Description column.
- **Easier favorites** — "Add to Favorites" is now in the File menu, and you can
  drag a folder straight onto an empty Favorites list (it's a proper drop zone
  now).
- **Filenames truncate in the middle** — long names in the list keep their start
  and their extension visible ("Annual Board Meeting…approved).pdf"), Finder-style,
  instead of losing the end.
- **Tidier viewer** — the viewer toolbar folds into a "…" menu when the window is
  too narrow to fit every button.
- **Fixed:** typing a name into the New Folder / Rename dialogs now works — those
  fields were silently ignoring keystrokes.
- **Fixed:** resizing a column while a folder is still loading now sticks — the
  width no longer snapped back while background work was running.

## 2026-07 — More platforms, steadier on slow drives

- **Runs on AROS** — Feraille now boots on AROS (aarch64) with menus, previews,
  and disk usage.
- **Windows & Linux caught up** — resilient file operations with clear errors
  when a file is busy, OneDrive/Trash/Open-With integration, native video, and
  Finder-style "Eject All" everywhere.
- **Image previews without macOS** — a built-in thumbnail renderer means previews
  work off macOS too.
- **Calmer on slow media** — spun-down drives and network mounts no longer freeze
  the window.

## 2026-06 — Viewer, video, search & disk usage

- **Media viewer** — images and video with zoom/pan, rotation, slideshow,
  In/Out cues, one-click enhance, and transparent stacking windows.
- **Icon (grid) view** and a Finder-style drag with real thumbnails and
  spring-loaded folders.
- **Find things** — recursive + Spotlight search and a duplicate finder, each in
  its own tab.
- **Disk Usage** — treemap with a Top-N panel and HTML export.
- **Richer previews** — inline text/code with syntax highlighting and formatted
  markdown.
- **Command palette** (Cmd+K) and a keyboard-shortcuts overlay.

## 2026-05 — The core explorer

- Rebuilt on a new rendering foundation, then filled in the essentials:
  **multi-window tabs, a curated Favorites sidebar, sortable columns, copy /
  move / trash with progress and undo, Get Info, and background folder sizes.**
- Started as a native macOS file explorer with real icons, magic-byte file
  detection, quarantine badges, and drag-out to Finder.
