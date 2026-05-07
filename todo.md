# Feraille — Open Risks & Backlog

Free-form list of things to look into later. Not a roadmap (see
[docs/ROADMAP.md](docs/ROADMAP.md)) and not a feature ledger (see
[docs/FEATURE_LEDGER.md](docs/FEATURE_LEDGER.md)) — both of those are
structured. This file is for the unstructured "remember to revisit X"
notes that would otherwise crowd CLAUDE.md or rot in code comments.

Two buckets: **Near-term** for items that would meaningfully improve
the app in the next handful of iters, and **Later** for architectural
or speculative work that needs design before code. When an item ships,
delete it; the commit + NOTES.md entry is the record.

## Near-term

### Command system follow-throughs (iter-5.8 unlocks)

- **Settings window — real one.** Iter-5.11 ships an NSAlert
  placeholder that prints current state. Replace with a proper modal
  panel (theme picker, hidden-files toggle, sidebar-width slider,
  …) backed by a config file so changes persist across runs.
- **Update `PROJECT_URL` once the repo is public.** iter-5.12 ships
  Help → "Feraille on GitHub" pointing at a deliberately invalid
  placeholder (`https://example.invalid/feraille`) — swap it out
  when the real repo lands.

### Other gaps that matter today

- **Flag unusual filenames in the UI.** Filenames are arbitrary byte
  sequences (within fs encoding rules), and some characters are
  visually invisible or ambiguous. The list and the rename editor
  should call them out so the user notices what they're dealing with
  — pre-rename and on the row itself. Cases to surface:
  - **Trailing or leading whitespace** (` foo` / `foo `) — easy to
    type by accident, invisible in monospace columns.
  - **Hidden / zero-width unicode** — ZWSP (U+200B), ZWNJ, BOM (U+FEFF),
    soft hyphen, RTL/LTR marks, U+202E override, etc.
  - **Control characters** — anything `< U+0020` plus DEL.
  - **Look-alike Cyrillic/Greek/etc.** — `а` (U+0430) vs `a` (U+0061).
    Probably out of scope for v1; ZWSP / trailing space first.
  - **Combining-character soup** — long combiner sequences that aren't
    obvious from the rendered glyph.

  Surface ideas: a small pill or icon on the row when the name has
  any of these; in the rename editor, render whitespace as a visible
  glyph (·) and combiners as their codepoint. Prereq: pre-rename we
  should also *show* the name as it actually is so the user knows
  what they're starting from. Inline rename already preserves
  whitespace verbatim post iter-5.7.5; this is the visibility half.

- **Drop targets** (drag-into-folder, drag-into-tree, drag-into-tab).
  Drag-out is done; the inverse pasteboard handling is the missing
  half of file-manager drag-drop.
- **TextInput IME / composition.** No full input-method-editor support
  yet. Will matter for non-Latin keyboards and Asian-language naming.
- **Render the volume capacity bar.** Iter-5.14 plumbs `VolumeInfo`
  with `total_bytes` / `available_bytes` / `is_local` / `is_removable`
  through `list_volumes()`. Next step: thread those values through
  the FileTree's section model so each Volumes row can paint a small
  horizontal "space used" bar (and skip it for `is_local == false`).

- **Chevron as expandability affordance.** Today every folder row in
  Locations / Volumes paints a `▶` chevron regardless of whether the
  folder actually has subfolders — so users discover empty folders by
  clicking and getting nothing. Make the chevron telegraph the answer
  before the click. Three flavors worth picking between when this
  comes up:
  1. **Binary** — filled `▶` if the folder has subfolders, hollow /
     no chevron if proven leaf. Finder-style; lowest noise.
  2. **Density** — chevron weight scales with subfolder count (1 vs 5
     vs 50 read differently), so depth telegraphs itself.
  3. **Heat-blended** — chevron picks up ant-trail color when the user
     frequently expands it, surfacing "places you go" to the eye.

  All three need the same infrastructure: an async "peek subfolder
  count" that runs off-thread (CLAUDE.md prime directive — no I/O on
  the UI thread), caches per-`NodeId`, batches like `IconChunkTick`
  to stay under frame budget, and invalidates on rename/move/create.
  Probably worth doing alongside or after streaming enumeration so the
  peek can reuse the same worker plumbing.

### Task manager follow-ups (post iter-5.15)

- **Live GUI smoke test.** The iter-5.15 verification used the
  `--simulate-task-panel` screenshot fixture, not a real run. Walk
  through the golden path interactively: navigate into a slow folder,
  click the status bar to open the popover, click `[×]` to cancel
  enumeration / icon prefetch, hit Escape and click outside to dismiss,
  confirm two overlapping tasks render two rows.
- **ETAs and byte counts.** The registry stores
  `TaskProgress::{Indeterminate, Determinate(f32)}` only — no rate, no
  ETA, no byte totals. Add a `details: Option<String>` (or richer enum)
  on `ActiveTask` and surface "X of Y · ETA Z" in the popover row when
  the task can supply it. Wait until copy/move land so we have a real
  consumer.
- **Wire copy/move/trash/search workers into the registry.** As each of
  those features lands, its scheduler should call `App::begin_task` /
  `end_task` instead of (or in addition to) driving the strip
  directly. That is the entire point of having a single registry —
  don't let new long-running paths regress to private state fields.
- **Determinate strip while determinate tasks are active.** Today the
  strip stays indeterminate as long as the registry is non-empty. Once
  copy/move ship determinate progress, switch the strip to a determinate
  fill (averaged over determinate tasks, or the primary one) so the
  status comet matches reality.

## Later

### Architectural

- **Streaming filesystem enumeration with cancellation.** Eager batch
  enumerate is the biggest performance gap for large or slow folders.
  Spec drafted in [docs/features/STREAMING_ENUMERATION.md](docs/features/STREAMING_ENUMERATION.md);
  implementation is multi-iter and should not begin without a fresh
  review of the spec first.
- **NodeStore identity model port.** Stable-id mapping that survives
  rename / move / mount changes — the durable substrate for ant trail,
  selection, and external watcher events. Deferred until streaming
  enumeration is in place; the two designs are coupled.
- **Status progress / task aggregation — extend.** Iter-5.15 ships a
  central `TaskRegistry` + bottom-right popover surfacing every active
  task with cancellation for enumeration / icon prefetch. Still
  pending: byte counts, ETAs, and integration with future copy / move /
  trash / search workers as they land.

### Feature surfaces (need design before code)

- **Real previews.** Text / image / PDF / Quick Look. Each is an
  async cancellable provider that returns a renderable bitmap or text
  buffer. Quick Look in particular is its own AppKit subsystem.
- **Persistent Ant Trail + metadata DB.** SQLite for ant-trail
  history, magic cache, recent folders, thumbnails. Per-user
  database; needs schema and migration plan.
- **Duplicate finder.** Size → partial-hash → full-hash funnel, all
  off-thread.
- **Command palette UI.** Fuzzy search over `all_commands()`. Needs a
  modal control + scoring algorithm + keybinding system to invoke it.
- **User-overridable key bindings.** Config file format (JSON / TOML /
  whatever the rest of the app picks), override layer on top of
  `default_shortcut`, per-OS sections.
- **Plugin / scripting surface.** Letting external scripts register or
  invoke commands by id. Needs a sandbox / IPC story; far future.

## Branding / packaging

- **Rework the app icon to macOS conventions.** The current icon
  ([crates/feraille-app/resources/feraille.png](crates/feraille-app/resources/feraille.png))
  is the Windows-shaped folder reused from Ferail. Macs expect the
  standard squircle silhouette — a square with rounded borders, full-bleed
  background, content centred — at 1024×1024 with the canonical macOS
  rounding radius. Generate `.iconset` folder and `iconutil` it into
  `feraille.icns` for eventual `.app` bundling.
- **Ship as a real .app bundle.** Without `Info.plist` + `Contents/
  Resources/AppIcon.icns`, the dock/About icon has to be set at runtime
  (currently via `feraille_shell_mac::set_app_icon_from_png_bytes`) and
  the binary identifies as a generic exec to launch services. cargo-bundle
  or a hand-rolled bundle script is the fix. Triggers that flip this
  from "nice-to-have" to "needed":
  - **TCC identity matters.** When the Documents/Desktop/Downloads
    permission grants need to stick across runs (today every cargo build
    re-prompts because the binary changes), a stable bundle ID is the fix.
  - **File associations.** "Open With Feraille" on a file from Finder
    requires the bundle to advertise UTIs in `Info.plist`.
  - **Code signing / Gatekeeper.** Anything we want to share with another
    Mac needs a signed bundle, otherwise it's quarantined.
  - **Drag-and-drop from Finder with full UTIs.** Bundle declares which
    document types it accepts.

  Until then, keep `cargo run` as the dev path. When we bundle, expect:
  dev cycle becomes `cargo build && open .../Feraille.app` (or a tools/
  script); stderr disappears unless launched via `open -a` from a terminal;
  TCC grants tied to the bundle ID instead of the binary path.

## macOS provenance xattrs — v2

V1 surfaces `com.apple.quarantine` and `kMDItemWhereFroms` in the Get-Info
modal plus a corner dot on the file icon. Open follow-ups:

- Surface Gatekeeper assessment via SecAssessment APIs ("blocked by
  Gatekeeper", "developer ID OK") for executables and `.app` bundles.
- Surface code-signature identity (`SecCodeCopySigningInformation`) on
  signed binaries — team identifier, signing authority.
- "Clear quarantine" context-menu action (writes via `xattr -d`) — tied
  to the future Mac context-menu surface.
- Dedicated design token for the quarantine dot if `status.warning` ends
  up colliding with other row-state usage.
- Consider whether the dot belongs in the tree pane for downloaded `.app`
  bundles surfaced in Locations.
- The current dot uses `bg.layer1` as the halo; on selected rows the row
  background is `accent.subtle`, so the halo can look mismatched. If it
  becomes annoying, draw the halo in the actual row background color
  (requires plumbing the selection state into the badge call).

## Notes from the porting effort

- The "Source" column in [docs/FEATURE_LEDGER.md](docs/FEATURE_LEDGER.md)
  references docs in the original Windows project at
  `/Users/jkn/Source/Ferail/`. When something here looks regressed
  compared to Ferail, read the analogous code/doc there before
  redesigning. (See CLAUDE.md → Cross-Reference: Ferail.)
