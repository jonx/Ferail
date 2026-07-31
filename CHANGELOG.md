# Changelog

Notable changes to Ferail, newest first. This tracks what you'd notice as a
user; the full detail lives in the git history. Dependency-pin bumps are logged
separately in [CHANGELOG-DEPS.md](CHANGELOG-DEPS.md).

**Unreleased** collects work not yet in a tagged build.

## Unreleased

## 0.2.1 — 2026-07-31

- **Fixed: the 0.2.0 app would not launch on a Mac without Homebrew.** It quit
  immediately with a dyld *"Library not loaded:
  /opt/homebrew/opt/xz/lib/liblzma.5.dylib"* error. The xz/LZMA support added
  in 0.2.0 linked against whatever liblzma the build machine happened to have
  instead of bundling its own, so the app only ran on machines that already had
  Homebrew's `xz` installed. liblzma is now compiled into the binary, and the
  macOS packaging script refuses to build a release that references any library
  outside `/usr/lib` and `/System`.
- The macOS app bundle now carries `LICENSE-MIT`, `LICENSE-APACHE` and
  `THIRD-PARTY-NOTICES.md` in `Contents/Resources/licenses`.

## 0.2.0 — 2026-07-30

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
- **New Archive dialog** — "New Archive…" in the Compress menu opens a dialog to
  pick the format (ZIP / 7-Zip / TAR.GZ / TAR.BZ2 / TAR.XZ / TAR), the
  compression level (Store / Fast / Normal / Maximum), and an optional password,
  instead of taking the one-click defaults.
- **Add files to a zip by dropping them in** — drag files from Finder or the file
  list onto an open archive to add them in place (ZIP only; formats that can't be
  edited show no drop target). Names already in the archive are reported rather
  than silently duplicated.
- **Browse inside archives** — right-click a file → **Open as Archive** to open
  its contents (like Disk Usage): a real, sortable file list with the usual
  columns, expandable folders, and a filter box — so a 5000-file archive opens
  as one folder to drill into, not 5000 rows. Then **Extract Selected**
  (a selected folder brings its whole subtree) or **Extract All**. It works on
  anything that *is* an archive underneath, even without the extension —
  `.docx`, `.xlsx`, `.pptx`, `.jar`, `.apk` — and says so plainly when a file
  isn't one. Formats that can't be edited in place (tar, 7z) are marked
  read-only. The workbench can also be **popped out into its own window** (and
  docked back), like Disk Usage — handy for dragging files into an archive with
  Finder open beside it. You can also **drag entries out of an archive** onto a
  folder row or another Ferail window to extract them there (dragging to
  Finder itself is still to come).
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

- **Runs on AROS** — Ferail now boots on AROS (aarch64) with menus, previews,
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
