# Built-in Image Editor (Redact / Annotate)

A deliberately small "black it out, circle it, save" editor for images —
not a paint program. Two modes (Redact: opaque black; Annotate: coloured),
two tools (rectangle, brush), undo, and two save gestures: Cmd+S writes an
"edited" copy beside the original, Cmd+Shift+S overwrites the original
after a confirmation.

← Back to [feature notes](README.md) · Source:
`crates/ferail-gpui/src/image_edit.rs`

## What ships

- **Entry points.** Row context menu **Edit Image** (single file whose
  extension the bundled codecs can round-trip: png, jpg/jpeg/jpe, bmp,
  tif/tiff, webp — GIF is deliberately excluded because re-encoding would
  silently drop animation), plus `file.edit_image` in the File menu and
  command palette. The action safely declines an incompatible selection.
- **The window.** Spiral-cascaded standalone window; toolbar with
  mode (Redact / Annotate), tool (Rectangle / Brush), a seven-colour
  annotation palette, S/M/L brush sizes, Undo, Save Copy, Overwrite….
  Cmd+Z undoes stroke by stroke; Esc / Cmd+W / the OS close button all
  route through an unsaved-edits guard (Save Copy / Discard / Cancel).
  The location button returns to the exact Ferail source tab and reselects the
  image, falling back to an ordinary Ferail reveal if that tab was closed.
  Command-icon tooltips show their Cmd/Ctrl shortcuts, and open editor windows
  are listed in the app's Window menu.
- **Drawing.** Strokes are stored in full-image pixel coordinates.
  Rectangles preview as a live element overlay and composite on release;
  brush strokes composite live. Redact rectangles are filled opaque
  black, annotate rectangles are outlines whose width scales with the
  image; the brush stamps interpolated discs so fast drags stay
  continuous.
- **Saving.** The copy path claims "<stem> edited[ n].<ext>" beside the
  original with `create_new` (race-safe, never clobbers, localized
  suffix), keeps the source format, flattens alpha for JPEG (quality 90),
  and toasts the new name. Overwrite rewrites the original through
  `safe_write` — backup sibling first, then in place (same inode: tags,
  permissions, creation date survive); a midway failure leaves the backup
  behind and the error names it.

## Pixel discipline (prime directive)

The window keeps only a **display-resolution copy** (longest edge
2048 px) of the decoded image. Interactive repaints composite the strokes
over that copy off-thread — single-flight, latest-wins, reconverging on
completion (the viewer's `schedule_process` shape) — so a fast brush drag
coalesces instead of queueing. The **full-resolution** image is re-decoded
from the file only inside the save worker and never rides the UI thread;
saving from the preview buffer would silently downsample, which is why the
cached viewer frames are not reused. Images past 64 MP are refused (a
full-res RGBA pass is 4 B/px), as are files the bundled `image` crate
cannot decode; a refused file is never touched. In **Private Mode** the
stage blanks fail-closed and the toolbar hides, same stance as the viewer.

The stroke compositor (`apply_strokes` + helpers) is pure and
unit-tested: rect fill/outline coverage, brush interpolation and
scale-invariance, copy naming, JPEG alpha flattening, and the
overwrite-leaves-no-temp invariant.

## Relationship to the bug-reporter redaction modal

TODO's Diagnostics follow-up (a) — drag-to-black-box over the issue
screenshot before bundling — is a strict subset of this editor (Redact
mode, rectangle tool, in-memory image). Point that modal at this module's
stroke/composite core rather than building a second canvas.

## Deliberate non-features (v1)

No zoom/pan (the stage fits the window), no layers, no redo, no text or
arrow shapes, no EXIF editing (see the separate "Remove location data"
TODO item), no HEIC/AVIF/RAW (no bundled encoder). Note that redacting a
JPEG rewrites the whole file — location EXIF is currently preserved
verbatim; combining redaction with metadata stripping is the EXIF item's
scope.

## Follow-ups if wanted

- Arrow + text annotation shapes; a highlighter (multiply) brush.
- Zoom/pan for precise work on large screenshots.
- Redo, and stroke-level hit-testing to move/delete a shape.
- Feed the diagnostics redaction modal from this module.
