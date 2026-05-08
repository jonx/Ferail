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
  Pin / Remove from Favorites.
- List pane (per-row): full Finder-equivalent — Open, Open With submenu,
  Reveal in Finder, Get Info, Quick Look, Rename, Duplicate, Make Alias,
  Compress, Copy Path, Share…, Tags row (7 colours), Move to Trash.
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
- Compress (`/usr/bin/ditto -c -k --sequesterRsrc --keepParent`)

Synchronous-but-fast Cocoa hops:

- Quick Look: `qlmanage -p` subprocess (spawn-and-detach)
- Make Alias: `NSURL.bookmarkData(...).suitableForBookmarkFile`
- Tags read/write: `URLTagNamesKey` via `NSURL.setResourceValue:`
- Open With enumeration: `NSWorkspace.URLsForApplicationsToOpenURL:`
- Share: `NSSharingServicePicker.showRelativeToRect:ofView:`

## Multi-Selection

The list pane respects `SelectionSet`. Right-clicking an already-selected
row keeps the multi-set; right-clicking elsewhere collapses the selection
to that row. Action titles update to "Reveal N in Finder", "Compress N
Items", etc. Rename is single-target only (matching Finder).

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
