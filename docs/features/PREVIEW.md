# Preview

Feraille currently has a preview information pane. The full Ferail preview
vision is an async provider system.

## Status

Partial — two providers live (image/PDF/media via Quick Look, and
inline text/code), plus the info metadata.

Current pane:

- Toggle with `Cmd+P` / `Ctrl+P`.
- Shows selected item name, kind, path, size, modified date, and magic label.
- **Quick Look thumbnail** for images / PDF / video / anything QL can
  render (`preview.rs`, 512 px, async + LRU-cached). Clicking it opens
  the viewer window.
- **Inline text/code preview** for text files (`text_preview.rs`):
  the first ~128 KB (capped at 500 lines), rendered through
  gpui-component's `TextView`. Markdown files (`.md`/`.markdown`/
  `.mdx`) render *formatted* (headings, lists, links); every other
  text file is wrapped in a fenced code block tagged with its
  extension and rendered **syntax-highlighted** (tree-sitter, the
  full `tree-sitter-languages` grammar set). gpui-component only
  highlights a language when its `LanguageConfig` carries a query;
  several grammars it ships (C#, C, C++, Bash, Swift, CMake) come with
  an empty query, so `crate::syntax_extra` registers vendored queries
  (the grammars' own `queries/highlights.scm`, under
  `src/syntax_queries/`) against the already-compiled grammars — no
  extra grammar deps. The fence is grown
  longer than any backtick run in the file so content containing
  ``` can't break out. Text-vs-binary is decided in the worker — NUL
  byte or invalid UTF-8 ⇒ not text ⇒ the thumbnail shows instead.
  Both providers ride the one `preview::request` selection event.
- Reads only already-cached provider results during paint; the file
  read is off the UI thread and `TextView` parses off-thread too.

## Target (remaining)

- Audio waveform or metadata; video thumbnail strip (beyond the QL
  poster).
- Archives and packages summary.
- Per-provider cancellation tokens (today a stale result is dropped at
  apply time, not cancelled mid-read).

Provider rules:

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

## Todo

- Preview request id/generation.
- Text preview provider.
- Image preview provider.
- Quick Look investigation.
- Preview cache.
- Status progress and cancellation UI.
