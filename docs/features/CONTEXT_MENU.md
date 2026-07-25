# Context Menu

Ferail used Win32 shell context menus and prewarming to avoid the first
right-click feeling stuck. Feraille rebuilds the same outcome with Mac-native
building blocks. macOS does not expose Finder's actual menu through any public
API, so we compose a faithful equivalent from `NSWorkspace`,
`NSSharingServicePicker`, `URLTagNamesKey`, and `qlmanage` / `ditto`.

## Status

Done — broad parity with Finder's menu. Services / Quick Actions submenu now
auto-populated by AppKit through a custom `ServicesAnchor` responder spliced
into the window's chain at startup ([crates/feraille-shell-mac/src/services.rs](../../crates/feraille-shell-mac/src/services.rs)).

## Surfaces Covered

- File tree (sidebar): Open, Reveal in Finder, Quick Look, Copy Path,
  Open Terminal Here (folders), Pin / Remove from Favorites.
- List pane (per-row): full Finder-equivalent — Open, Open With submenu,
  Reveal in Finder, Get Info, Quick Look, Rename, Duplicate, Make Alias,
  Compress (submenu: ZIP / 7-Zip / TAR ▸ Gzip·Bzip2·XZ·Uncompressed),
  Extract (archive rows only; submenu: Extract Here / Extract To…),
  Copy Path, Open Terminal Here (folders), Share…, Tags row
  (7 colours), Move to Trash.
- List pane (background): right-click empty area shows New Folder, Reveal
  in Finder, Refresh, Show Hidden Files toggle.
- Disk-usage treemap: Open, Reveal, Copy Path, Quick Look, Zoom into,
  Move to Trash. Multi-selection-aware.

## Architecture

Right-click sites build a [`MenuPlan`] of [`MenuPlanItem::Action`] /
`Separator` / `Submenu` entries. Each `Action` carries a `CommandId` and
optional `CommandPayload` (tag colour, Open With bundle path).
[`feraille_shell_mac::show_context_menu`] turns the plan into an `NSMenu`,
runs it modally via `popUpMenuPositioningItem:`, and returns a `MenuPick`
combining the chosen `CommandId` with whatever payload was attached.

Commands fan out to host methods via a match on `CommandId.0` at each
site; multi-selection is honoured by `App::resolve_selected_paths` which
reads the `SelectionSet` once per right-click.

Slow operations run on workers and report back through
`AppEvent::FileOpComplete`:

- Duplicate (`feraille_shell_mac::duplicate_path`)
- Compress / Extract — the pure-Rust archive engine
  (`feraille_fs_native::{create_archive, extract_archive}`, backed by the
  `feraille-archive` model crate). Creates zip / 7z / tar / tar.gz / tar.bz2 /
  tar.xz; extracts all of those plus gzip/bzip2/xz single members. The GPUI
  shell no longer shells out to `/usr/bin/ditto`, so every platform shares one
  path. Extract is offered only for archive rows (lexical extension check,
  precomputed at right-click); "Extract Here" targets the current folder and
  "Extract To…" a native folder picker (run inside a spawned task so its nested
  run-loop holds no `App` borrow). Both pick a smart destination off-thread —
  extract in place when the archive has a single root folder, otherwise a
  `" 2"`-deduped wrapper named after the archive.

Synchronous-but-fast Cocoa hops:

- Quick Look: `qlmanage -p` subprocess (spawn-and-detach)
- Make Alias: `NSURL.bookmarkData(...).suitableForBookmarkFile`
- Tags read/write: `URLTagNamesKey` via `NSURL.setResourceValue:`
- Open With enumeration: `NSWorkspace.URLsForApplicationsToOpenURL:`
- Share: `NSSharingServicePicker.showRelativeToRect:ofView:`

## Multi-Selection

The list pane respects the per-tab selection set. Right-clicking an
already-selected row keeps the multi-set; right-clicking elsewhere
collapses the selection to that row. Action titles update to "Reveal N in
Finder", "Compress N Items", etc. Rename is single-target only (matching
Finder).

### Command availability over a group (GPUI)

A context command's *visibility* and its *execution* must agree on which
files they target — otherwise a command can be shown for a file it won't
touch, or hidden for one it would (the Clear-Quarantine-on-a-mixed-pair
bug). The GPUI shell guarantees this by resolving the target set **once**,
from a single place, and reusing it for both:

- `Shell::resolve_targets(context_row, …)` resolves the **dispatch** set:
  `context_row` (right-click on an unselected row) → the whole visible
  selection → the lead row. `action_entries_visible_order` consumes
  `context_row` for one-shot dispatch.
- `file_list::resolve_menu_targets(entries, selected, row_ix)` resolves the
  **menu** set, and must agree with it row for row: clicked row inside the
  selection → the whole set; outside → that row alone (because the click
  collapses the selection onto it). It returns the `MenuTargets` snapshot
  the menu gates on — `Vec<TargetCap>` capability caps + a single `anchor`
  — projected from the already-loaded `FileEntry`s, so it stays cache-only.
- It runs **when the menu builds**, from the delegate's own mirrored
  selection, not from a snapshot staged ahead of time. That is not a style
  choice: gpui-component builds the menu inside a `window.defer` callback
  queued from its mouse-down listener, and that listener is registered
  *after* the row's (so it runs first in the bubble phase). The deferred
  build therefore lands **before** the table's `RightClickedRow` event ever
  reaches the Shell. Anything the Shell stages from that event is one
  right-click late — which is exactly what made the first menu after a
  folder load silently drop every gated command (Rename, Copy Path,
  Extract, Open as Archive, Open With) until you right-clicked a second
  time. Both right-click sites (list rows and the icons grid) go through
  the same delegate builder, so both get the fix.
  `--context-menu-row N` (docs/features/SCREENSHOTS.md) captures the
  first-right-click case headlessly.

#### Three command archetypes

Every command is classified by how it behaves across a multi-selection:

1. **Batch** — one operation over the whole set (Compress → one archive,
   Extract → one op per selected archive, Move to Trash → one batch op,
   Tags → applied to all). Always shown; a large count is the point, so it
   is never guarded.
2. **FanOut** — invoked once per file (Open, Quick Look, Reveal in Finder,
   Get Info, Open in New Tab, Duplicate, Make Alias). Always shown, but the
   handler iterates over the resolved set.
3. **SingleOnly** — meaningful only for one file (Copy Path, Rename, Open
   With). Hidden once more than one row is targeted.

Plus **capability / anchor** rules that don't fit a count (Clear
Quarantine = any target quarantined; Open Terminal Here / Favorites =
anchor is a folder; Slideshow from Here = anchor is a file).

Visibility is expressed with the `Availability` type and evaluated against
the resolved `MenuTargets`:

```rust
enum Availability {
    SingleOnly,                     // hide once >1 target
    When(fn(&MenuTargets) -> bool), // capability / anchor callback
}
```

Batch and FanOut need no rule — they're added unconditionally and differ
only in how the *handler* treats the group. `SingleOnly` gates on
`MenuTargets::is_single()`; `When` is the per-command callback the menu
asked for. Clear Quarantine is `When(avail_any_quarantined)`, matching
`Shell::on_clear_quarantine`, which strips the Mark-of-the-Web from the
quarantined subset — so right-clicking the clean file in a mixed selection
still offers it.

To gate a new command, pick `SingleOnly` or write a `When` closure; to
gate on a new per-file capability, add a field to `TargetCap` (projected
from the cached `FileEntry` — no I/O) and read it through `any`/`all`.
Caps are cache-only, honouring the prime directive.

#### Fan-out confirmation

A FanOut command that spawns a *separate foreground artifact per file* — a
tab, a Get Info window, an app launch, a Finder reveal — routes through
`Shell::confirm_fanout`. Below the threshold (10) it runs immediately;
at or above it, a confirmation dialog asks first so a stray 200-row
selection can't silently open 200 windows. Power users can still proceed.
Batch commands that collapse to one operation (Compress, Trash, Tags) and
single-window FanOut (Quick Look opens one HUD for all paths) never
confirm. Guarded today: Open, Open in New Tab, Get Info, Reveal in Finder.

A FanOut that opens its own OS windows (Get Info) cascades them along a
spiral instead of stacking on the centred spot — see `crate::window_cascade`.
The slot is an Archimedean spiral (`r ∝ √slot`, `θ ∝ √slot`, equal
arc-length steps) around the display centre, clamped to stay on-screen so a
large batch is still grabbable. Each window holds a slot guard that frees
its slot on close, so closing them all re-centres the next one. The module
is window-kind-agnostic — any future multi-window surface can reuse it with
its own slot counter.

## Tags

Reads the cursor entry's tags synchronously when right-click fires (one
Cocoa hop per path). The seven canonical colour tags render with their
emoji (🔴 🟠 🟡 🟢 🔵 🟣 ⚪) so AppKit picks up native colour rendering
without us painting attributed strings. Currently-set tags get a
checkmark. Toggling applies to the whole resolved selection. Round-trips
with Finder via Launch Services.

## Prewarm Rule

The plan is built from cached app state at right-click time; no I/O on
hover. Open With's Launch Services query is the one synchronous Cocoa
call still on the right-click path — it's typically <50 ms but a future
iteration can move it to selection-change pre-warm with generation
tokens (mirrors Ferail's `menu_preload.rs`) if real users hit a stutter.

## Windows Notes That Did Not Port

- `IContextMenu`, `TrackPopupMenuEx`, PIDLs, shell extensions, and wait-
  cursor suppression are Windows implementation details. Ferail's
  `shell_pump.rs` state machine (1 ms slices interleaved with UI) is
  the right reference for any future async population work.
- The lesson that did port: the first context menu must not make the
  app look frozen, and slow third-party plug-ins must not block paint.

## Open Work

- Tags row as a single horizontal `setView:` NSMenuItem matching
  Finder's compact swatch strip, instead of seven stacked emoji
  rows. Custom `NSView` with tracking areas and click hit-testing.
- Async Open With pre-warm with debounced selection-change +
  generation tokens. Only worth it if cold-cache stutter is
  observed in practice (synchronous query is typically <50 ms).
- Per-target enable/disable rules (read-only volumes, missing
  files, permission-denied). `MenuPlanItem` already supports
  `enabled: bool` — call sites just don't compute it yet.
