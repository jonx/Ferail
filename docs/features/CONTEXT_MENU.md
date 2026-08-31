# Context Menu

Ferail-Win32 used Win32 shell context menus and prewarming to avoid the first
right-click feeling stuck. Ferail rebuilds the same outcome with Mac-native
building blocks. macOS does not expose Finder's actual menu through any public
API, so we compose a faithful equivalent from `NSWorkspace`,
`NSSharingServicePicker`, `URLTagNamesKey`, and `qlmanage` / `ditto`.

## Status

Done: broad parity with Finder's menu. Services / Quick Actions submenu now
auto-populated by AppKit through a custom `ServicesAnchor` responder spliced
into the window's chain at startup ([crates/ferail-shell-mac/src/services.rs](../../crates/ferail-shell-mac/src/services.rs)).

## Surfaces Covered

- File tree (sidebar): Open in New Tab, Get Info (volume rows open with
  `InfoTarget::Volume` so the header names the volume), Reveal in Finder,
  Copy Path, Open Terminal Here, Add / Remove from Favorites, New Folder
  Here, Eject and What's Blocking Eject? (removable volume rows only; the
  latter additionally gated on `lock_diagnostics_available()`: Windows-only
  today, see `shell/lock_info.rs`).
- List pane (per-row): full Finder-equivalent: Open, Open in New Tab
  (folders/files), Edit in TextEdit/Notepad/Text Editor (one file), Open With submenu,
  Reveal in Finder, Get Info, Quick Look, Rename, Duplicate, Make Alias,
  Compress (submenu: ZIP / 7-Zip / TAR ▸ Gzip·Bzip2·XZ·Uncompressed),
  Extract (archive rows only; submenu: Extract Here / Extract To…),
  Copy Path, Open Terminal Here (folders), What's Locking This? (platforms
  with `lock_diagnostics_available()`: Windows-only today; a Restart-Manager
  dialog naming the processes holding the selection open, with force-close
  buttons: `shell/lock_info.rs`), Share…, Tags row
  (7 colours), Move to Trash.
- List pane (background): right-click on empty space: below the last row,
  or anywhere in an empty folder: targets the *folder being browsed*,
  never the selection: New Folder, Paste, Select All, Get Info, Reveal in
  Finder, Copy Path, Open Terminal Here, Add Folder to Favorites, Refresh.
  Region is decided in the mouse capture phase (`TableState`'s
  `right_clicked_background`): every right-click starts as background, a
  row's bubble handler claims it, the header un-flags it. The folder verbs
  ride the `context_target` actions; `TableEvent::RightClickedBackground`
  (emitted at menu-build time) is what stages the current directory there.
  An earlier attempt hung a `.context_menu` on the file-pane wrapper and
  ate the row menus' clicks: this one lives inside the same
  `PlatformContextMenu` wrapper the rows use, so the two can't fight.
- Disk-usage treemap: Open, Reveal, Copy Path, Quick Look, Zoom into,
  Move to Trash. Multi-selection-aware.

## Architecture

The **file row** (which also serves the icon grid) and the **file background**
menus are built as data first: `file_list.rs` assembles a
[`MenuPlan`](../../crates/ferail-gpui/src/menu_plan.rs) of `PlanItem::Action` /
`Submenu` / `Separator` entries, each carrying a stable `CommandId`, and
`plan.render(menu)` turns it into a `gpui_component::menu::PopupMenu` at the
end. Which entries go into the plan is still decided inline by `Availability`
rules over the resolved target set (see
[Command availability over a group](#command-availability-over-a-group-gpui),
computed once per open by `resolve_menu_targets_with_mode`). Choosing an entry
dispatches a `gpui::Action` through the window's normal dispatch path, anchored
to the Shell's focus handle so keyboard-shortcut hints resolve on the first
frame.

Two things live in the render step rather than at the call sites:

- **Separator hygiene.** Groups empty out as `Availability` hides their
  entries, leaving the separators that framed them behind. `tidy` drops
  leading and trailing separators and collapses runs to one, in a single pass
  over the finished list. As bookkeeping at each `if` this would be unreadable
  and untestable; as a list rule it is four lines and has its own tests.
- **Duplicate-id detection**, as a debug assertion. A repeated id is not
  cosmetic: it would make one preference govern two different entries.

Submenus (Compress, Extract, Open With, Tags) stay where they are built,
because gpui-component builds a nested `PopupMenu` through its own `build`,
which needs a `Window`. The plan decides only whether the submenu appears and
under which id.

### Entry ids

[`menu_plan::ids`](../../crates/ferail-gpui/src/menu_plan/ids.rs) is the table.
An entry that is a catalogue command reuses the catalogue's id, so
`file.rename` means the same thing in the menu, the palette and the shortcuts
page. An entry that exists only as a menu item mints one in the same namespace
and says why, because giving it a `CommandSpec` would put it in the Cmd+K
palette where it would sit disabled: most of them need a right-clicked target
to mean anything. A test pins that split, so an id that is neither a catalogue
command nor a declared menu-only entry (a typo, most likely) fails the build's
tests instead of silently becoming a third kind of id.

The remaining eight surfaces (breadcrumb, the three sidebar sections, browse
tree, disk-usage treemap, table header, duplicates panel) are still imperative
chains. They convert the same way, one at a time, whenever their customization
is wanted.

> **Historical note.** This section once described a `MenuPlan` of
> `MenuPlanItem::Action` / `Separator` / `Submenu` entries carrying a
> `CommandId`, rendered by `ferail_shell_mac::show_context_menu` into an
> `NSMenu`. That was the pre-GPUI Cocoa app, and the port to gpui-component's
> builder chains dropped the shape entirely; the description outlived the code
> by a long way. The plan type above deliberately **restores** it, minus the
> `NSMenu` renderer and the `CommandPayload` (payload-carrying entries, tag
> colours and Open With slots, live inside submenus built at their own sites).

## Multi-Selection

The list pane respects the per-tab selection set. Right-clicking an
already-selected row keeps the multi-set; right-clicking elsewhere
collapses the selection to that row. Action titles update to "Reveal N in
Finder", "Compress N Items", etc. Rename is single-target only (matching
Finder).

### Command availability over a group (GPUI)

A context command's *visibility* and its *execution* must agree on which
files they target, otherwise a command can be shown for a file it won't
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
  the menu gates on: `Vec<TargetCap>` capability caps + a single `anchor`
 : projected from the already-loaded `FileEntry`s, so it stays cache-only.
- It runs **when the menu builds**, from the delegate's own mirrored
  selection, not from a snapshot staged ahead of time. That is not a style
  choice: gpui-component builds the menu inside a `window.defer` callback
  queued from its mouse-down listener, and that listener is registered
  *after* the row's (so it runs first in the bubble phase). The deferred
  build therefore lands **before** the table's `RightClickedRow` event ever
  reaches the Shell. Anything the Shell stages from that event is one
  right-click late, which is exactly what made the first menu after a
  folder load silently drop every gated command (Rename, Copy Path,
  Extract, Open as Archive, Open With) until you right-clicked a second
  time. Both right-click sites (list rows and the icons grid) go through
  the same delegate builder, so both get the fix.
  `--context-menu-row N` (docs/features/SCREENSHOTS.md) captures the
  first-right-click case headlessly.

#### Content that can't be fetched on the UI thread

Some menu content, "Open With" candidates, most obviously, comes from a
blocking shell query that the [Prime Directive](../ARCHITECTURE.md#prime-directive)
forbids on the UI thread, and the menu builder is synchronous. The rule is:

- The builder reads **only caches** (`open_with_warm`, the per-row caps). On a
  miss it creates an Open-With submenu with a disabled loading row and kicks
  the off-thread fetch.
- When that fetch reports back, it updates the delegate cache and calls
  `PopupMenu::rebuild` on that retained submenu entity. The root menu, its
  focus and its highlighted item are untouched (see
  [GPUI-UPSTREAM.md §4b](../GPUI-UPSTREAM.md)).

So warming is a latency optimisation and nothing more: a cold right-click
shows the same menu a warm one does, a beat later. Never gate a command's
*existence* on a cache being populated: gate it on the caps, which are
projected from rows already in memory.

#### Three command archetypes

Every command is classified by how it behaves across a multi-selection:

1. **Batch**: one operation over the whole set (Compress → one archive,
   Extract → one op per selected archive, Move to Trash → one batch op,
   Tags → applied to all). Always shown; a large count is the point, so it
   is never guarded.
2. **FanOut**: invoked once per file (Open, Quick Look, Reveal in Finder,
   Get Info, Duplicate, Make Alias). Always shown, but the handler iterates
   over the resolved set. Open in New Tab fans out too, but is *also*
   anchor-gated (below): a tab is a folder view, so it only exists for a
   folder anchor and its handler drops file targets from the set.
3. **SingleOnly**: meaningful only for one file (Copy Path, Rename, Open
   With). Hidden once more than one row is targeted.

Plus **capability / anchor** rules that don't fit a count (Clear
Quarantine = any target quarantined; Open Terminal Here / Favorites /
Open in New Tab = anchor is a folder; Slideshow from Here = anchor is a
file).

Visibility is expressed with the `Availability` type and evaluated against
the resolved `MenuTargets`:

```rust
enum Availability {
    SingleOnly,                     // hide once >1 target
    When(fn(&MenuTargets) -> bool), // capability / anchor callback
}
```

Batch and FanOut need no rule: they're added unconditionally and differ
only in how the *handler* treats the group. `SingleOnly` gates on
`MenuTargets::is_single()`; `When` is the per-command callback the menu
asked for. Clear Quarantine is `When(avail_any_quarantined)`, matching
`Shell::on_clear_quarantine`, which strips the Mark-of-the-Web from the
quarantined subset and recursively beneath a directory anchor, so
right-clicking the clean file in a mixed selection
still offers it.

To gate a new command, pick `SingleOnly` or write a `When` closure; to
gate on a new per-file capability, add a field to `TargetCap` (projected
from the cached `FileEntry`: no I/O) and read it through `any`/`all`.
Caps are cache-only, honouring the prime directive.

#### Fan-out confirmation

A FanOut command that spawns a *separate foreground artifact per file*: a
tab, a Get Info window, an app launch, a Finder reveal: routes through
`Shell::confirm_fanout`. Below the threshold (10) it runs immediately;
at or above it, a confirmation dialog asks first so a stray 200-row
selection can't silently open 200 windows. Power users can still proceed.
Batch commands that collapse to one operation (Compress, Trash, Tags) and
single-window FanOut (Quick Look opens one HUD for all paths) never
confirm. Guarded today: Open, Open in New Tab, Get Info, Reveal in Finder.

A FanOut that opens its own OS windows (Get Info) cascades them along a
spiral instead of stacking on the centred spot: see `crate::window_cascade`.
The slot is an Archimedean spiral (`r ∝ √slot`, `θ ∝ √slot`, equal
arc-length steps) around the display centre, clamped to stay on-screen so a
large batch is still grabbable. Each window holds a slot guard that frees
its slot on close, so closing them all re-centres the next one. The module
is window-kind-agnostic, any future multi-window surface can reuse it with
its own slot counter.

## Tags

Reads the cursor entry's tags synchronously when right-click fires (one
Cocoa hop per path). The seven canonical colour tags render with their
emoji (🔴 🟠 🟡 🟢 🔵 🟣 ⚪) so AppKit picks up native colour rendering
without us painting attributed strings. Currently-set tags get a
checkmark. Toggling applies to the whole resolved selection. Round-trips
with Finder via Launch Services.

## Open Terminal Here

Folder-anchor command on both surfaces, configurable in **Settings →
Files → Terminal** and resolved at click time by
`feature_settings::TerminalConfig` → `ferail_core::terminal::TerminalSpec`
→ `platform_shell::open_terminal_with` (settings read + process spawn on
a worker):

- **Terminal application**: blank uses the platform default
  (Terminal.app / `wt.exe`, falling back to `cmd.exe` / the Linux
  `$TERMINAL`-then-probe chain). Accepts an app name or `.app` bundle
  (macOS, launched via `open -a`), a program path, or a `PATH` command.
- **Arguments**: one params string, split shell-style (double quotes
  group); `{dir}` expands per-token to the target folder. Without
  `{dir}` the child instead inherits the folder as its working
  directory.
- **Launch mode**: Standard, or Administrator: `ShellExecuteExW`
  verb `runas` (UAC) on Windows; on macOS and Linux the terminal opens
  into a `sudo -s` root shell (macOS routes app bundles through
  Terminal.app AppleScript `do script`, which needs the one-time
  Automation consent; CLI terminals get their exec flag, `-e`, `--`,
  `wezterm start --`, `-x`: from `ferail_core::terminal::exec_prefix_for`).

## Prewarm Rule

The plan is built from cached app state at right-click time; no I/O on
hover. Open With's Launch Services query, once the last synchronous
Cocoa call on this path, now pre-warms off the UI thread on
selection-lead change (`spawn_open_with_warm`), and a cache miss shows a
disabled "Open With (loading…)" item that the arriving fetch replaces in
place by rebuilding only the retained submenu. Measured, the query costs a one-time ~5–10 ms
Launch Services bootstrap per process and then ~0.03 ms per call, so the
warm cache is insurance against cold bundles on slow volumes rather than
a hot-path saving (see [OPEN_WITH.md](OPEN_WITH.md) §3).

## Customizing Which Entries Appear (planned)

Goal: let the user hide the context-menu entries they never use, per
menu, the way the table header already lets them hide columns. Not built
This section is the design the work should follow.

### The precedent is already in the codebase

`FileListDelegate::header_context_menu`
([file_list.rs:2414](../../crates/ferail-gpui/src/file_list.rs#L2414))
is this feature, for columns: a ✓/blank toggle per column built from
closure-backed `PopupMenuItem`s, persisted through the existing
subscription, with a **Reset Columns** escape hatch and a primary column
that can never be hidden. `split_persisted_columns`
([file_list.rs:2947](../../crates/ferail-gpui/src/file_list.rs#L2947))
supplies the storage rules to copy verbatim:

- unknown keys are ignored, so a spec written by a newer build does not
  break an older one;
- entries the spec never mentions (new in this build) **default to
  visible**, so a new command is never invisible to an upgrading user;
- the visible set is never allowed to go empty.

### Prerequisite: a stable id per entry  ✅ done for the two file-pane menus

The row menu used to be an imperative chain,
`menu.menu(tr!("Rename…"), Box::new(RenameSelected))`, so an entry's identity
was its Rust action type and there was nothing to key a preference on.

Both file-pane menus now build a [`MenuPlan`](#architecture) of id-carrying
entries instead, which is the enabling change: hiding an entry becomes one
predicate in `render`. The same refactor is a prerequisite for user-defined
tools ([OPEN_WITH.md](OPEN_WITH.md) §5.6), which cannot hard-code an action
type per user-created entry.

Still open from this step: the eight other surfaces are unconverted, and the
label duplication between the catalogue's `msgid!` and the menu site's `tr!`
survives. Sourcing an entry's label from its catalogue spec is possible now
that the ids line up, but it is not automatic: a menu label and a palette
title differ on purpose in places, so each one has to be looked at.

### The editor picks a surface

There is more than one menu, so the editor is a two-pane thing: choose
the surface, then toggle its entries. The surfaces are:

| Surface | Built in |
|---|---|
| File list / icon grid **row** (one definition serves both) | [file_list.rs:2465](../../crates/ferail-gpui/src/file_list.rs#L2465) |
| File list **background** (targets the browsed folder) | [file_list.rs:2812](../../crates/ferail-gpui/src/file_list.rs#L2812) |
| Table **header** (already customizable: columns) | [file_list.rs:2414](../../crates/ferail-gpui/src/file_list.rs#L2414) |
| **Breadcrumb** segment | [shell/render.rs:3111](../../crates/ferail-gpui/src/shell/render.rs#L3111) |
| Sidebar **Favorites** section header / favorite row | [favorites_section.rs:258](../../crates/ferail-gpui/src/favorites_section.rs#L258), [:741](../../crates/ferail-gpui/src/favorites_section.rs#L741) |
| Sidebar **Locations / Volumes** row | [locations_section.rs:325](../../crates/ferail-gpui/src/locations_section.rs#L325) |
| Sidebar **Recents** header / row | [recents_section.rs:145](../../crates/ferail-gpui/src/recents_section.rs#L145), [:258](../../crates/ferail-gpui/src/recents_section.rs#L258) |
| **Browse tree** row | [tree.rs:802](../../crates/ferail-gpui/src/tree.rs#L802) |
| **Disk-usage treemap** | [disk_usage.rs:1223](../../crates/ferail-gpui/src/disk_usage.rs#L1223) |

Each gets a stable `MenuSurface` id, and visibility is stored per
`(surface, command)`: the same command can be wanted in one menu and
not another.

### Opening a menu must not get slower

Dynamic entries must not add per-open cost. The rules:

- Parse the persisted spec **once**: at startup and on settings change:
  into an in-memory structure keyed by surface, exactly like
  `app_state`'s memoized `load()`. **Never** read the file, and never
  call a parsing `load()`, at menu-open time (Prime Directive: menu
  building is read-only and I/O-free).
- Resolve per entry with an **array index or a small set probe** against
  a dense id, not a string parse. For scale: every entry already pays a
  `tr_raw` (an `arc_swap` load plus, in a non-English catalog, a HashMap
  lookup and an `Arc` clone) just to get its label: a visibility check
  is strictly cheaper than what the loop already does, so a correct
  implementation is unmeasurable next to today's build.
- The common case is "nothing hidden". Keep a fast path: an empty
  override set short-circuits to exactly the current behaviour.

### Separators  ✅ done

Hiding entries leaves doubled and leading separators. `menu_plan::tidy` is that
pass, already running for both file-pane menus: drop separators at the head,
collapse any run of adjacent separators to one, drop separators at the tail.
Deliberately a post-processing step on the item list rather than per-`if`
bookkeeping at each call site: the group structure stays readable, and the rule
is testable in isolation, which it is.

### Safety rules

- **Preference AND availability, never OR.** A user marking an entry
  "shown" must not override `Availability`: otherwise the
  shown-for-a-file-it-will-not-touch class of bug that the availability
  machinery exists to prevent comes straight back.
- **Never hide everything.** Same rule as columns: the visible set can
  not go empty, and each surface has a **Reset** action.
- Consider a small always-on floor (Open / Get Info) so a user cannot
  configure themselves out of the app's primary verb, mirroring how the
  `name` column can never be hidden.

## Windows Native Menu

Windows keeps Ferail's menu as the ordinary, instant right-click path. The
final **More options from Windows…** entry, Shift+right-click, and Shift+F10
explicitly request the native Shell menu. Nothing is prefetched on selection,
hover, navigation, or while constructing Ferail's own menu.

The native implementation is deliberately out of process. A disposable
`ferail-gpui.exe --windows-context-menu-broker` role binds same-parent child
PIDLs, obtains `IContextMenu`, forwards owner-draw and dynamic-submenu messages
through `IContextMenu2/3`, displays `TrackPopupMenuEx`, invokes the selected
offset, and exits. Third-party DLLs therefore never enter the GPUI process. A
private readiness handshake bounds a stuck pre-display provider; once the
native popup is ready it is user-modal and has no timeout. Invocation uses a
Unicode `CMINVOKECOMMANDINFOEX` with `CMIC_MASK_NOASYNC`. The canonical
`properties` verb is the exception: its handler can return before its property
sheet is established, so the broker routes one item through
`SHObjectProperties` and a multi-selection through `SHMultiFileProperties`.
Its tool-window owner sits at the click point rather than off-screen so owned
dialogs cannot inherit an off-screen placement. Shift+right-click is filtered
inside Ferail's live-menu listener as well as routed to the Shell; GPUI window
listeners do not honor element `stop_propagation()` by themselves.

### Shell Namespace Rows (This PC, Recycle Bin)

Rows that have no filesystem identity of their own get the same treatment
through the tab's namespace provider, with two rules worth stating because both
were once wrong.

**Every namespace row offers the native menu.** The capability used to be
granted only to items *without* a filesystem path, which quietly emptied the
Recycle Bin's menu: a recycled item does report a path, so it lost the
capability and right-click produced a popup with nothing in it. The Shell's own
menu is the right menu for any namespace row.

**Restore is Ferail's own entry, above the Shell's.** It is the one thing
people open the Recycle Bin to do, so it does not live two clicks deep under
*More…*. It carries its own capability (`RESTORE`), granted by a provider whose
root is the Recycle Bin; it is never implied by `TRASH_RECOVERABLE`, which is
the opposite direction. The work itself is the Shell's `undelete` verb, invoked
by the same disposable broker with no menu on screen: only the Shell knows
where a recycled item came from, and only it can put it back with the original
name, timestamps and permissions. The verb is resolved against the selection's
real menu offsets rather than passed to `InvokeCommand` as a string, so a
selection that cannot be restored reports a failure instead of doing nothing.
The broker accepts verbs from a fixed list, so the flag cannot turn it into a
general Shell command runner.

macOS has no equivalent yet: Put Back is a Finder record, not a filesystem
operation, so the Trash folder there is an ordinary directory and offers no
Restore.

Ferail-Win32's old selection-change `menu_preload.rs` and interleaved
`shell_pump.rs` were behavioral references only. They are intentionally not
ported: the normal menu never pays the Shell-extension cost.

## Open Work

- Tags row as a single horizontal `setView:` NSMenuItem matching
  Finder's compact swatch strip, instead of seven stacked emoji
  rows. Custom `NSView` with tracking areas and click hit-testing.
- Open With follow-ups: an "Other…" app chooser, user-defined custom
  tools, "Always Open With", and allowing the submenu on a
  multi-selection (the dispatch already handles many files).
  [OPEN_WITH.md](OPEN_WITH.md).
- User-customizable menu entries: see
  [Customizing Which Entries Appear](#customizing-which-entries-appear-planned)
  above. Blocked on giving every entry a stable `CommandId` and building
  the menus from a table.
- Per-target enable/disable rules (read-only volumes, missing files,
  permission-denied): today an unavailable entry is hidden, never shown
  greyed out. Needs an `enabled` flag on each entry, which the table-driven
  refactor above is the natural place to introduce.
