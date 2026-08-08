# Changelog

Notable changes to Ferail, newest first. This tracks what you'd notice as a
user; the full detail lives in the git history. Dependency-pin bumps are logged
separately in [CHANGELOG-DEPS.md](CHANGELOG-DEPS.md).

**Unreleased** collects work not yet in a tagged build.

## Unreleased

- **Dragging files out of Ferail into other apps now works.** Dragging rows,
  grid cells, or sidebar folders to Finder, the Desktop, an editor, or any
  other app never actually worked — the drag ghost just stopped at the edge
  of the window and nothing was handed to the system, because the UI
  framework's drag was in-window only and the code wrongly believed
  otherwise. The framework has since gained real drag-out support; Ferail now
  promotes a drag to a native macOS drag session the moment the pointer
  leaves the window, so dropping files elsewhere copies them like a Finder
  drag would. Entries inside an archive are the one exception — they have no
  on-disk files until extracted, so they still only drag within the window.
- **The status bar shows what Ferail itself costs.** A quiet readout on the
  right — "up 3d 4h · CPU 0.2% · MEM 184.0 MB · 0 rps" — reports how long the
  app has been running, its CPU share, its memory footprint, and how many
  times per second the window redrew. All figures are about Ferail, not the
  machine: the last figure is deliberately labelled *rps* (redraws per
  second), not "fps" — the app only draws when something changes, so an idle
  window honestly reads 0, and any number it shows is a plain count of real
  redraws rather than a claim about animation smoothness. A nonzero value
  while you aren't doing anything means something is wastefully repainting.
  CPU is counted the way Activity Monitor does, so it can exceed 100% on
  multi-core work. Each number sits in a fixed-width slot, so the bar
  doesn't shift around as values update. The readout appears a few seconds
  after launch, once the first reliable sample exists.
- **Ferail can be packaged as an Ubuntu/Debian `.deb`.** The repo now carries
  a freedesktop desktop entry, the app icon in the standard hicolor location,
  and `cargo deb` packaging metadata; every window identifies itself to the
  desktop environment as `ferail`, so docks and taskbars show the right icon
  and group Ferail windows together. Verified end to end on Ubuntu 24.04
  (arm64): the package builds, installs, and the installed app launches and
  browses folders. CI builds both the Intel (amd64) and ARM (arm64)
  packages against Ubuntu 22.04, so they run on 22.04 and later — and on
  release tags they are attached to the GitHub release as public downloads,
  starting with the next release. Opening a *specific folder* from the
  desktop ("Open with Ferail") isn't wired yet — the binary doesn't take a
  directory argument.
- **Fixed a crash-on-build for ARM Linux.** Owner/group name lookup used a
  buffer type that only compiles where C's `char` is signed — fine on
  Intel/macOS, a build failure on ARM Linux (Raspberry Pi class machines,
  ARM servers, Apple-Silicon VMs).
- **Choose your terminal.** Settings → Files → Terminal picks which terminal
  "Open Terminal Here" launches — an app name or `.app` bundle on macOS, a
  program path, or a command on `PATH` — with your own launch arguments
  (`{dir}` expands to the folder) and a Standard/Administrator mode. Blank
  keeps the platform default: Terminal.app, Windows Terminal, or the usual
  Linux emulator hunt. Administrator means a UAC prompt on Windows and a root
  shell in the terminal on macOS and Linux.
- **Paste, move, rename, and the result is selected for you.** When an
  operation finishes in the folder you're still looking at, what it produced is
  selected and scrolled into view — pasted and moved files, a renamed item, a
  new folder, a duplicate, an alias. Previously a paste into a long listing
  gave no sign of where the file landed.
- **Hidden files are easier to reason about.** With *Show hidden* on, hidden
  entries render dimmed so they read as distinct from your real files. With it
  off, the status bar quietly reports what's out of sight — "3 hidden ·
  12.1 KB" — so you know hidden content exists, and how much space it takes,
  without unhiding it.
- **Diagnostics can take you to the files it talks about.** Settings →
  Diagnostics now shows the full path of the running app, and every row about a
  location on disk — the app itself, the config folder, the settings file, the
  metadata database, mpv — has a Reveal button that opens that spot in Ferail
  with the item selected.
- **Read-only volumes are detected on Windows and Linux.** The status bar says
  "{volume} is read-only" instead of reporting "0 B free", which is true on a
  CD but buries the actual story.
- **Text previews handle legacy single-byte files.** Old README and `.nfo`
  files written in ISO-8859-1 (Amiga/DOS-era exports) now preview as text
  instead of being rejected as binary.
- **Folder sizes stop re-measuring themselves.** Returning to the app used to
  re-walk every visible folder tree from scratch, so on a big folder the Size
  column could never settle. It now answers from cache and only recomputes what
  actually went stale; Refresh is still the gesture that forces a fresh measure.
- **Fixed: folders labelled as archives.** Some folders showed a file format
  such as "ZIP archive · 3 files" in the Format and Description columns,
  inherited from a stale cache entry for a path that used to be a file. Folders
  are now guarded from format detection at every layer, and existing bad
  entries clear themselves on next launch.
- **Delete Immediately is findable.** The permanent-delete command (Shift+Delete,
  or Option+Cmd+Delete on macOS) was missing from the Cmd+K command palette and
  the keyboard-shortcut sheet, so it could only be reached from a menu.

## 0.2.2 — 2026-07-31

First release with a **Windows download**. Also the release that unbroke the
Windows build — which had been failing to compile on `main` for two weeks
without anyone noticing, because nothing in CI ever built it.

- **Windows builds are back, and now ship.** A single `IMFMediaEngine::SetMuted`
  call written from a Mac on 2026-07-14 could not compile under the `windows`
  crate's type inference, and took the whole Windows app down with it. Fixed —
  and with it the app builds, runs, passes its tests, and screenshots on
  Windows again.
- **Windows packaging** — `scripts/package-win.ps1` produces a portable ZIP
  (`Ferail.exe`, the `ferail` CLI, and the licence notices), plus an installer
  with a Start Menu entry and an uninstaller when Inno Setup is present.
  Authenticode signing is wired but this release is **unsigned**, so Windows
  shows a SmartScreen warning — verify the download instead:
  `Ferail-0.2.2-win-x64.zip` is SHA-256
  `9993AF0EF53DE617C255BF9EBBA7FF53DFB3EDDC80866FF5D969715A78F30E6B`.
- **Fixed: the viewer's "Stay on Top" did nothing on Windows.** The toggle never
  reached the OS. It now really does keep the window above other apps.
- Note that nothing automatically builds Windows yet, so the class of breakage
  above can still recur — it is caught only by someone building on a Windows
  machine.
- **Fixed: a fresh `git clone` could not build on any platform.** The workspace
  manifest referenced checkouts that only exist on one developer's machine, so
  `cargo` failed before it compiled anything.
- **The shipped binary carries no GPL-3.0 code.** A single non-optional
  dependency edge (`gpui → sum_tree → ztracing`) pulled GPL-3.0-or-later crates
  into every build, which is incompatible with distributing a binary under
  MIT/Apache-2.0. Ferail now vendors that one Apache-2.0 crate with the edge
  removed. Related correction: THIRD-PARTY-NOTICES.md previously said the edge
  was already gone upstream — it was not; the lockfile that suggested so had
  been generated with an unrelated local override active.
- No user-visible macOS changes.

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
