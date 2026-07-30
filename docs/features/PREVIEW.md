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
- Quarantine/provenance details with clear-quarantine action.

Remaining providers are tracked in [TODO.md](../../TODO.md): audio waveforms,
video strips beyond the Quick Look poster, archive/package summaries, and true
per-provider cancellation tokens.

## User Surface

- Toggle with `Cmd+P` / `Ctrl+P`.
- Shows selected item name, kind, path, size, modified date, magic label,
  description, and quarantine/provenance when available.
- Clickable Quick Look thumbnail opens the viewer window.
- A drag grip under the thumbnail resizes the thumbnail box (120–600 DIPs);
  the height persists across restarts (`app_state::preview_thumb_height`, on
  the same debounced save as the splitter widths).
- Inline text/code preview appears before the detailed metadata for text-like
  files.
- The preview pane has its own scroll state and can be resized without stealing
  width from the sidebar.

## Provider Flow

Selection change schedules preview work through the shared preview request path.
Providers run off the UI thread and publish compact cached results back to the
shell.

Paint reads only:

- the selected `FileEntry`,
- `PreviewCache`,
- `TextPreviewCache`,
- cached quarantine and magic metadata.

Stale provider results are dropped at apply time by checking the active
selection/request. Some providers still run to completion after they become
stale; cooperative cancellation is the remaining architecture gap.

## Content Thumbnail Provider

`preview.rs` requests a 512 px thumbnail on a worker and stores it in an LRU
cache. The fetch is `video_poster::fetch_content_thumbnail` — Quick Look
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
one frame with libmpv on a **dedicated poster worker thread** — pooled
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

## Provider Rules

- Selection change schedules preview work.
- Previous request is cancelled or ignored.
- Worker returns compact preview data.
- Paint draws placeholder, loading state, cached result, or error.
- No provider reads file content on the UI thread.

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
