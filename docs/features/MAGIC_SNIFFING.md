# Magic Sniffing

Purpose: identify real file types from content without trusting extensions, then
cache the result so the Format and Description columns stay cheap to paint.

## Status

Shipped with follow-ups.

Implemented:

- `feraille_fs_native::detect_magic` for legacy label-only callers.
- `feraille_fs_native::detect_magic_info` for structured facts.
- First-4-KB bounded header reads for the main detector.
- A ZIP-family tail read, also bounded to 4 KB, for central-directory facts.
- Structured parsers for executables, ZIP/Office/JAR/APK, images, audio, video,
  text/scripts, and signature-table formats.
- `MagicInfo::description()` for the Description column.
- `display_magic` and `display_description` on `FileEntry`.
- Format/Description columns, sorting, icon tinting, search filtering, mismatch
  cues, and preview display.
- Background prefetch fused with quarantine lookup.
- SQLite write-through/read-through cache via `MetadataDb`.
- Reset scope for magic metadata (`--reset-db magic`).

Remaining work is long-tail format coverage, cloud-volume policy, stale-result
tests, and debug visibility into queued/running jobs.

## Detector

The detector reads a bounded prefix into memory and dispatches in this order:

1. Executables.
2. ZIP-based formats.
3. Images.
4. Audio.
5. Video containers.
6. Fixed signature table.
7. Text/script heuristics.
8. Binary/unknown fallback.

For ZIP-family formats, `detect_magic_info` performs a second bounded tail read
when possible. That lets the detector refine generic ZIP into Office/JAR/APK
types and fill central-directory facts such as entry count and single-root
folder.

The public split is intentional:

- `detect_magic(path)` returns only a display label.
- `detect_magic_info(path)` returns `MagicInfo` with type-specific facts.

## Cached Fields

`MagicInfo` can populate:

- magic type label,
- description text,
- executable bitness, architecture, subsystem, and .NET flag,
- Office macro/encryption flags,
- ZIP entry count and root,
- image dimensions and alpha,
- audio channels, sample rate, bitrate, and duration,
- video/audio stream presence,
- script interpreter,
- text encoding/subtype.

The UI stores the display-ready strings on `FileEntry`; paint never formats or
sniffs file content.

## Prefetch Flow

`crates/feraille-gpui/src/prefetch.rs` starts after directory enumeration. It:

1. Snapshots rows into sendable seeds (`path`, row index, mtime, size, current
   cached flags).
2. Registers a `TaskKind::MagicPrefetch` task.
3. Runs on the background executor.
4. Reads `MetadataDb` first.
5. Falls back to `detect_magic_info` and quarantine xattr lookup.
6. Writes fresh data back to SQLite.
7. Applies one foreground batch to the live `FileEntry` slice.

Row application is bounds-checked so a re-enumerated directory does not panic on
stale row indexes.

## Nonblocking Contract

Magic sniffing reads file bytes. It must never run from paint, navigation
commit, selection, hover, scroll, or row drawing.

Allowed trigger points:

- After navigation commits.
- When a file row enters or nears the viewport.
- During idle prefetch.
- When a preview provider needs type data.

Every request must be cancellable or ignorable by generation id.

## Mac Notes

- Some cloud files may fault in content on read. Treat magic sniffing as
  speculative and low priority.
- Extended attributes and Uniform Type Identifiers can complement magic, but
  asking the OS for them may also block. Cache and worker-boundary them.
- Quarantine provenance is intentionally fused into the same prefetch pass so
  the app pays one worker/apply cycle for both derived metadata streams.

## Remaining Work

Tracked in [TODO.md](../../TODO.md):

- Expand long-tail signatures and structured parsers.
- Add per-volume/cloud skip rules.
- Add tests for stale result dropping.
- Add a debug overlay showing queued/running magic jobs.
- Add CLI output modes for JSON, CSV, recursive scans, mismatch-only, and
  result limits.
