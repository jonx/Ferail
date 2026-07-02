# Feraille — Feature Tour

← [README](../README.md) · [Getting started](../GETTING_STARTED.md) ·
[Architecture](ARCHITECTURE.md) · [Design notes per feature](features/README.md)

Everything the app does today, with pictures. Each section links to the
deeper design note where one exists. If you only read one thing: Feraille
is a file manager that **never blocks** — every feature below is built on
that rule.

---

## The shell — a file table that keeps up with you

![The shell](images/tour-shell.png)

- **Virtualized table** over directories of any size: sort, resize, and
  reorder columns (your layout persists across launches), filter as you
  type, and stream huge folders in without a frozen frame.
- **Format column reads bytes, not extensions** — ~67 magic signatures
  with structured parsers (executables, archives, images, audio, video),
  so a `.jpg` that's secretly a `.zip` is flagged inline, and a
  Description column fills with real facts (bitness/arch, dimensions,
  channels/kHz/duration). ([magic notes](features/MAGIC_DESCRIPTION.md))
- **Inspector-grade preview pane**: Quick Look media or syntax-highlighted
  text, plus the full Get Info surface — dates, tags, POSIX permission
  grid, volume — all editable, all gathered off-thread.
  ([preview](features/PREVIEW.md))
- **Finder-parity essentials**: color tags, quarantine badges with
  "where from" provenance, hidden-file toggle, per-tab history,
  drag-and-drop in and out, undoable operations (Cmd+Z), and a
  deceptive-filename highlighter that catches `раypal.exe`-style
  homoglyph tricks without flagging normal Cyrillic/Greek names.

## Icon grid — the same folder, visual

![Icon grid](images/tour-grid.png)

Any tab flips between the dense table and a Finder-style grid
(per-folder, like Finder). Real thumbnails, size slider, and the same
adornments as the list: tag dots, favorite stars, visit-heat tint, cut
dimming. Multi-select with the keyboard or Cmd-click; the same context
menu and drag-out everywhere.

## Media viewer — compare files like frames

![Media viewer](images/tour-viewer.png)

A built-in image *and* video viewer whose party trick is **sticky
zoom**: zoom to 4× on one photo's corner and the next file opens at 4×
on the *same* corner — purpose-built for comparing scans, renders, or
frames. Per-file rotation, slideshow, frame stepping, and a live
adjustment panel (brightness/contrast/saturation, one-click
auto-enhance, SIMD 1×/2×/4× upscale).

With the optional **mpv backend** it plays virtually any container and
grades it live with no re-encode — plus the showpiece: **chroma-keyed,
stackable transparent video windows** (pick a color, key it out, float
the clip over your desktop; stack several). ([viewer](features/VIEWER.md))

## Disk Usage — see the bytes, act on them, share the picture

![Disk usage](images/tour-disk-usage.png)

A squarified treemap with async scanning that streams in live, colored
by content category, with a Top-N largest-files panel. It counts
*honestly*: hardlinks once, `du -x` filesystem boundaries with
macOS-firmlink awareness, real rolled-up sizes for `.app` bundles, and
an apparent-vs-on-disk toggle.

It's not just a picture: **multi-select squares and act on them** —
Open, Reveal, Get Info, Copy, Move to Trash (auto-rescans) — with full
keyboard support. And when you want to *show* someone: **Export as
HTML** renders the exact same treemap as a self-contained snippet
(no JavaScript) you can paste into any document, wiki, or web page.
([disk usage](features/DISK_USAGE.md))

## Duplicate finder — built in, clone-aware

![Duplicate finder](images/tour-dupes.png)

A size→partial-hash→full-hash funnel (BLAKE3, results cached in SQLite
so re-scans are fast) with an optional byte-for-byte paranoid pass that
streams in constant memory. It knows APFS clones and hardlinks don't
free space, so "reclaimable" is a number you can trust. Review groups
in a dedicated panel, mark keepers, trash the rest — or deduplicate
in place with APFS clonefile. ([duplicates](features/DUPLICATES.md))

## Bulk rename — regex power with a live preview

![Bulk rename](images/tour-bulk-rename.png)

Select the files, right-click → *Rename N Items…*: literal or regex
find/replace (with `$1` captures), case transforms, and a template
stage with `{name}` `{ext}` `{n}` `{date}` tokens for numbering. The
before→after preview updates live, conflicts block the button before
anything touches disk, renumbering chains and swaps execute in
dependency order, and the whole batch is one Cmd+Z away from undone.
([bulk rename](features/BULK_RENAME.md))

## Search — streaming results, Spotlight-backed

![Search](images/tour-search.png)

Recursive search of the current folder or the whole volume, streaming
results into a live tab as they're found. Rides Spotlight when
available, with a walker fallback that works anywhere (and is the
Windows/Linux path today). ([search](features/SEARCH.md))

## Command palette & shortcuts

![Command palette](images/tour-palette.png)

Cmd+K opens a searchable overlay of every command and its shortcut —
one identity layer (`feraille-core`'s command catalogue) drives the
palette, the macOS menu bar, and the keybindings, so they can't drift
apart.

## Settings & themes

![Settings](images/tour-settings.png)

Searchable settings with light/dark/system themes, selection and
heat-tint accent colors, UI zoom (Cmd+= / Cmd+-), thumbnail and
recents toggles, search/duplicate-finder tuning, and a diagnostics
page with a privacy-redacted "copy report" for bug reports.

## The quiet features you'll feel

- **Ant Trail** — the app learns the folders you visit; heat tints them
  in the list and a Recents section keeps them one click away.
  ([ant trail](features/ANT_TRAIL.md))
- **Dock drawer** — park the whole window against a screen edge; it
  slides away and comes back on an edge-slam, floating over every Space.
  ([dock](features/DOCK.md))
- **Resilient file ops** — copy/move/trash batches continue past
  failures and report "N of M · why" with copy/retry (and
  retry-as-administrator on macOS). ([file ops](features/FILE_OPS.md))
- **Undo that means it** — rename, bulk rename, move, copy, trash, and
  favorites edits are all reversible, with guards so an undo never
  overwrites something newer.
- **CLI + headless screenshots** — `feraille magic`, `feraille du`, and
  a screenshot harness that renders any surface off-screen (it produced
  every image on this page).

## Platform status

macOS is the daily-driver build. Windows has broad native parity
(clipboard, Recycle Bin, thumbnails, Open With, Media Foundation
video). Linux builds and runs with the basics real and the rest
stubbed. Details: [windows port](features/windows-port.md) ·
[linux port](features/linux-port.md) · [roadmap](../TODO.md)
