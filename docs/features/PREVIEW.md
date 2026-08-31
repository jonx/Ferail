# Preview

Ferail's preview pane is an async, cache-backed inspector for the current
selection. It combines Finder-style Get Info details with lightweight content
preview, and it never reads file content from paint.

## Status

Shipped with follow-ups.

Live providers:

- Selection metadata / Get Info.
- Quick Look thumbnails for images, PDFs, videos, and other renderable file
  types.
- Inline text, Markdown, and syntax-highlighted source preview.
- CP437/ANSI scene NFO and structured Kodi NFO preview.
- Memory-only folder sidecar cards for NFO and checksum manifests.
- Quarantine/provenance details with clear-quarantine action.

Remaining providers are tracked in [TODO.md](../../TODO.md): audio waveforms,
video strips beyond the Quick Look poster, archive/package summaries, and true
live Windows preview hosting. Selection-driven image work now carries a
cancellation token through the native provider chain; metadata/text providers
remain bounded by their latest-wins queues.

## User Surface

- Toggle with `Cmd+P` / `Ctrl+P`.
- Shows selected item name, kind, path, size, modified date, magic label,
  description, and quarantine/provenance when available.
- Filesystem paths in the embedded Get Info sections are constrained to the
  pane and use the shared `beginning…middle…end` elision for very long values;
  hovering reveals the complete path.
- Filesystem creation, modification, and last-access dates have an **Edit**
  action where the host can write them. The editor accepts local
  `YYYY-MM-DD HH:MM:SS`, rejects invalid/nonexistent daylight-saving times,
  writes on the background executor, then reloads the listing and re-gathers
  the inspector. Windows supports all three dates; Unix supports modification
  and access while creation/birth time remains read-only.
- Clickable Quick Look thumbnail opens the viewer window.
- A drag grip under the thumbnail resizes the thumbnail box (120–600 DIPs);
  the height persists across restarts (`app_state::preview_thumb_height`, on
  the same debounced save as the splitter widths).
- Inline text/code preview appears before the detailed metadata for text-like
  files. Text is detected by content, not extension: a bounded shared decoder
  handles BOM UTF-8/UTF-16, strict UTF-8, confident CP437 artwork, then a
  printable-ratio-gated Latin-1 fallback. Scene NFO cursor layout and safe SGR
  colours are rendered through a bounded inert ANSI canvas, with a terminal
  font chosen for connected CP437 box/block glyphs; Kodi NFO shows a readable
  local summary and its source without fetching scraper URLs.
- Selecting a folder schedules one bounded background discovery of immediate
  NFO/checksum candidates. A small **Sidecar files** card can preview an NFO
  without changing folder selection or open a manifest Verify surface. The
  cache is memory-only and stores no sidecar body or hash.
- With nothing selected and the tab parked at a volume's mount root: where a
  sidebar volume click lands: the pane previews the volume itself: name plus
  the Get Info volume section (capacity, available, used, format, a read-only
  Access row when the mount is, mount point, device). The mount-root check reads the cached volume list only; the record
  gathers on the background executor like every other selection.
- The preview pane has its own scroll state and can be resized without stealing
  width from the sidebar.

## Provider Flow

Selection change schedules preview work through the shared preview request path.
Providers run off the UI thread and publish compact cached results back to the
shell. Selection-driven image and text providers each use a constant-space,
latest-wins queue: one request may run and only the newest request behind it is
retained. Holding an arrow key therefore cannot create one native provider or
file read per crossed row.

Paint reads only:

- the selected `FileEntry`,
- `PreviewCache`,
- `TextPreviewCache`,
- `FolderSidecarCache`,
- cached quarantine and magic metadata.

Provider results are path-keyed and never mutate table rows or geometry. A
result that finishes after the selection moved may remain useful in the small
cache, but only the currently selected path is rendered. Superseding a native
image request now cancels the active chain. On Windows that also terminates and
waits for the disposable preview broker, so a hung in-process shell extension
cannot keep consuming the single preview slot.

## Content Thumbnail Provider

`preview.rs` requests a 512 px thumbnail on a worker and stores it in an LRU
cache. The fetch is `video_poster::fetch_content_thumbnail`: Quick Look
first, then embedded cover art for audio, then an mpv poster frame for
videos Quick Look refuses. It is used for:

- images,
- PDFs,
- videos,
- audio files with embedded cover art,
- other file types Quick Look can render.

Quick Look only thumbnails what AVFoundation decodes; whole container
families (AVI, WMV/ASF, MKV, …) come back empty instantly. When the user has
selected the mpv video provider (Settings → Plugins), the fallback decodes
one frame with libmpv on a **dedicated poster worker thread**: pooled
tasks await the result rather than blocking, so a folder of 90 rips can
never starve the background pool (prefetch, folder sizes, timers) the way
a blocking convoy would. The queue drains newest-first: the folder being
looked at thumbnails first, and jobs for rows browsed away from still
finish and stay cached for the next visit. Files that don't decode cost
one bounded deadline and are negative-cached. Audio cover art is read with
`lofty` (`ferail_fs_native::media::read_cover_art`), which is what makes
album art work on Windows/Linux where there is no Quick Look. Every
thumbnail surface (list rows, icon grid, this pane, `ferail thumb`) rides
the same fetch.

The fetch carries a **tier** (`video_poster::Tier`): the grid, list rows,
viewer fallback and `ferail thumb` ask for `Thumbnail`; this pane (and
`ferail thumb --preview`) asks for `Preview`. macOS and Linux answer both
identically. On Windows the two differ, because the shell does:

- `Thumbnail` = what Explorer would show: `IShellItemImageFactory`
  (`SIIGBF_THUMBNAILONLY`), then for PDFs page 1 rendered natively by
  `Windows.Data.Pdf` (`ferail-shell-win32/src/pdf_render.rs`: no window,
  no third-party code). Open, parse, render and stream read share one
  five-second deadline and are cancelled on expiry.
- `Preview` = the same chain, plus, when both come up empty, a brokered
  `IPreviewHandler` capture (Word/Excel/PowerPoint, RTF, text). A preview
  handler is a live viewer, so its capture includes the handler's own
  chrome (scrollbars, toolbars); that is acceptable in the pane and
  exactly why the grid never gets it. Explorer's real answer: the handler
  hosted live in a child window over the pane: is tracked in `TODO.md`.
  The capture loads in the disposable broker first so killing that process
  owns the provider lifetime; `prevhost.exe` is compatibility fallback only.
  A newer selection cancels and kills the active broker immediately.

Neither shell tier ever returns a type icon: the icon is the *caller's*
last tier (`platform_shell::fetch_type_icon`, `SIIGBF_RESIZETOFIT`),
requested by `fetch_content` only after the bundled raster decoder, cover
art, and the poster gate all had their turn. An icon returned any earlier
would mask a decodable image: the shell declines a 512 px extraction for
some files (OneDrive placeholders, for one) whose grid-size thumbnail
works, and the pane must then decode the image itself rather than show
the Photos icon. macOS/Linux return `None` from `fetch_type_icon` and
keep drawing their own type glyphs.

The thumbnail is intentionally a preview-pane poster, not the full viewer. The
viewer has its own loader and playback path.

## Text And Code Provider

`text_preview.rs` reads the first ~128 KB, capped at 500 lines. The worker
rejects binary content before the render layer sees it:

- NUL byte means binary.
- invalid UTF-8 means binary.

Markdown files (`.md`, `.markdown`, `.mdx`) render as formatted Markdown through
gpui-component's `TextView`.

Other text files are wrapped in a fenced code block tagged with the extension
and rendered with syntax highlighting. The fence grows longer than any backtick
run in the file so source content cannot break out of the fence.

Ferail also registers extra highlight queries for gpui-component grammars that
ship without highlight query text, including C, C++, C#, Bash, Swift, and CMake.

## Scrolling And Layout

Inline preview content lives in a bounded scroll box so long files do not bury
the rest of the inspector.

Scroll chaining works in two stages:

1. The inner text preview consumes wheel input while it can scroll.
2. Residual wheel delta at the top/bottom forwards to the outer preview pane.

Code preview does not wrap lines. It gets a definite width based on the widest
line estimate (`PREVIEW_CODE_*`) so horizontal scrolling remains available.

Rendered Markdown uses a fixed reading column (`PREVIEW_MD_MIN_W`) because
gpui-component wraps Markdown prose.

The box carries its own vertical scrollbar (`scrolled_preview_box`), set to
`ScrollbarMode::Always` rather than the theme's fade-out default: because the
box is deliberately capped, the bar is the only thing telling the reader the
file continues past the last visible line. It draws nothing when the content
fits, and it rides a 16px strip on the right edge instead of covering the box,
since the scrollbar element claims the hitbox of whatever bounds it is given
and the text underneath has to stay selectable. The folder sidecar box has its
own handle for the same treatment.

## Provider Rules

- Selection change schedules preview work. That includes the automatic
  selection of the first row when a listing completes: it is not a gesture, so
  no click or keyboard handler runs, and the request is issued next to the
  selection itself (active tab only).
- At most one selection preview runs per provider; only the newest waiting
  request survives.
- Worker returns compact preview data.
- Paint draws placeholder, loading state, cached result, or error.
- No provider reads file content on the UI thread.

## Metadata Editing

The timestamp editor changes filesystem facts only. It does not rewrite file
contents or embedded media/document metadata. On Windows the writer opens the
item with `FILE_WRITE_ATTRIBUTES` plus delete/read/write sharing and passes
null pointers for the untouched timestamps to `SetFileTime`; this keeps the
operation valid for directories and read-only files without toggling their
attributes. Reparse-point timestamps remain read-only in the UI so a displayed
link cannot silently update its target.

Future embedded-metadata work is deliberately separate: location scrubbing,
audio-tag editing, and writable document properties each need format-specific
atomic replacement, cache invalidation, and privacy/error contracts. They are
tracked in `TODO.md`, not implied by the filesystem-date editor.

## Mac Notes

- Quick Look can provide rich previews, but it must be isolated from paint and
  handled carefully around AppKit threading.
- Some files may be cloud placeholders. Preview should not accidentally download
  large content without a user-visible state.

## Remaining Work

Tracked in [TODO.md](../../TODO.md):

- Audio waveform / metadata provider.
- Video thumbnail strip beyond the Quick Look poster.
- Archive/package summary provider.
- Per-provider cancellation tokens.
- More explicit cloud-placeholder state before reads that may fault content in.
- Embedded metadata editing: remove location data, edit common audio tags, and
  write supported Windows document properties (see `TODO.md`).
