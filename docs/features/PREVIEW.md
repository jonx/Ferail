# Preview

Feraille currently has a preview information pane. The full Ferail preview
vision is an async provider system.

## Status

Partial.

Current pane:

- Toggle with `Cmd+P` / `Ctrl+P`.
- Shows selected item name, kind, path, size, modified date, and magic label.
- Reads only already-enumerated/cached data during paint.

## Target

Preview providers:

- Text and Markdown.
- Images.
- PDF / Quick Look.
- Audio waveform or metadata.
- Video thumbnail strip.
- Archives and packages summary.

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
