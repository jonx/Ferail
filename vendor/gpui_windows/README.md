# gpui_windows (Ferail patch)

This is Zed's Apache-2.0 `gpui_windows` crate at commit
`38ca9106c5306ef93e52c35643df015a27f15b72`, copied verbatim except for the
small outbound file-drag patch documented below and a standalone Cargo
manifest.

GPUI core already promotes an internal drag through
`PlatformWindow::{can_start_external_drag,start_external_drag}`, and its macOS
and Wayland backends implement that contract. The Windows backend does not, so
the resolver is never called when the pointer leaves a GPUI window.

Ferail's delta:

- `src/external_drag.rs` creates the canonical Shell data object from PIDLs
  and starts the native OLE session through `SHDoDragDrop`.
- `src/window.rs` enables that implementation and limits OLE `DragOver`
  forwarding into GPUI to a display-relevant cadence instead of repainting at
  the mouse report rate. Once the gesture leaves Ferail, the Shell image stays
  the sole visual owner through any re-entry; Ferail's shared badge flag hides
  its in-window image while preserving the typed payload for internal drops.
- `src/events.rs` defers the synchronous OLE loop through a private window
  message, after GPUI has released its mutable application borrow, and closes
  GPUI's platform-owned drag state when the OLE session ends.
- `src/gpui_windows.rs` declares the helper module.

The data-object and drag sequence follows the production pattern used by
Simon Mourier's ShellBat/ShellN: `SHCreateDataObject`, then `SHDoDragDrop` with
the Shell's default `IDropSource` and copy/move/link effects.

Keep this fork narrow. When updating GPUI, first check whether upstream has
implemented these two methods; if so, remove this patch and return to the
upstream `gpui_windows` package.

Upstream and this vendored crate are licensed under Apache-2.0; see
`LICENSE-APACHE`.
