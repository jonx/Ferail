# Feraille — Next Level Plan

Polish + density-of-decisions plan for taking the GPUI shell from
"competent" to "commercial-app." Replaces the *porting* discipline
([PORTING.md](PORTING.md)) for the forward-looking phase: the harvest
is mostly done; this document tracks the *upgrade*.

**Reference UI:** Longbridge Pro screenshot (provided 2026-05-14) is
the density target. Pixel-for-pixel matching is not the goal —
*decision density per square centimetre* is. They make ~50 small
decisions per screen; today we make ~10. Closing that gap is the work.

**Source brief:** condensed from the user-provided "Next Level Brief"
(same date). Verbatim phasing preserved; concrete file targets,
gpui-component primitive picks, and acceptance criteria added per
phase.

---

## Locked decisions (do not relitigate per phase)

1. **No blocking UI.** All filesystem, magic detection, metadata,
   thumbnails, indexing, and disk-usage work runs off the UI thread.
   The UI stays interactive at all times.
2. **No spinner-first loading.** Default feedback channels: the
   status-bar progress strip + the task-panel popover. Spinners
   appear only when a `gpui-component` primitive forces one
   internally. Skeleton rows / empty states only where there is
   *genuinely* nothing useful to show.
3. **Magic replaces Kind.** The file list renders one `Format` column
   that prefers magic-detected type and falls back to
   extension-derived type. A small mismatch indicator appears when the
   two disagree. No more "PNG / PNG image" duplication.
4. **`gpui-component` first.** Before hand-rolling anything, check the
   library. Use `Sidebar`, `Tree`, `Table`, `Input`, `Tooltip`,
   `Badge`, `Menu`, `Popover`, `DescriptionList`, `Resizable`,
   `Notification`, `Dialog`, `TitleBar`, `Kbd`, `Switch`. Hand-rolling
   is allowed only with a written justification in the commit body.
5. **Filesystem stays abstracted.** Domain types
   ([feraille-core](../crates/feraille-core/src/lib.rs)) own
   `FileEntry`, `NodeId`, `EntryKind`, the catalogue. UI talks to
   files through `FsBackend` and node ids — never raw paths where a
   NodeId is available. Any new format/category semantics land in
   `feraille-core` (additive) and ride through to the UI as
   pre-computed display strings or enums. The UI does not interpret
   raw bytes, parse extensions, or call `Path::extension()`.

---

## How to use this document

- Update at every commit boundary that touches a phase item.
- Tick `- [x]` as items complete. Don't delete; the checked list is
  the receipt.
- Sub-phase rollups → mark the phase **Done** only when *every* item
  including the acceptance criteria is checked.
- Items that get cut: strike through (`~~item~~`) with a short reason.

**Status legend per phase:** `Not started` / `In progress` / `Done` /
`Deferred`.

**Update discipline alongside [PORTING.md](PORTING.md):** if a phase
item closes out an outstanding harvest entry, mark it Ported there
too.

---

# Phase 1 — File List Becomes Scannable

**Status:** Done (pending user review)
**Goal:** make the main file list feel like a commercial file manager
— scannable by icon, no redundant columns, hover/select state visible.

### Deliverables

- [x] Built `file_type_icon(entry: &FileEntry) -> FileTypeIcon` in
      [icons.rs](../crates/feraille-gpui/src/icons.rs) returning an
      asset-source path + `FileTypeTint` enum. Mapping covers folder,
      image, video, audio, document/PDF/text/markdown, code,
      archive, disk-image, executable/app, symlink, unknown.
- [x] Tints from theme tokens only (`chart_1..chart_5`, `primary`,
      `info`, `danger`, `muted_foreground`) — no hard-coded HSLA. See
      `tint_color()`.
- [x] Decision: NSWorkspace icons stay for folders (user-customised
      folder icons + sync overlays still render). Files + symlinks
      switch to Lucide-tinted SVGs.
- [x] Replaced `Kind` + `Magic` columns with one `Format` column. New
      `FileEntry::format_label() -> (String, bool)` lives in
      [feraille-core/src/lib.rs](../crates/feraille-core/src/lib.rs)
      (additive, no UI deps; 6 unit tests cover the heuristic).
- [x] Mismatch indicator: red `triangle-alert` SVG next to the format
      text when ext-implied kind ≠ magic-detected format. Tooltip
      reads: *"Extension says X but content looks like Y."*
- [x] Alias normalization in `formats_compatible`: `jpg ≡ jpeg`,
      `tif ≡ tiff`, `htm ≡ html`, `mpg ≡ mpeg`, `yml ≡ yaml`,
      `md ≡ markdown`, plus qualifier stripping so `PDF` and
      `PDF document` compare equal. 6 extra regression tests cover
      the cases the Phase 1 review caught.
- [x] Tooltip on the Name cell with the full filename (Tooltip from
      `gpui_component::tooltip`).
- [ ] Hover + selected row states audit — left as-is for now (the
      Table primitive already paints them via theme tokens; will
      revisit in Phase 10 audit alongside the file-row / settings-
      tile consistency check the brief flagged).
- [x] New asset source [crates/feraille-gpui/src/assets.rs](../crates/feraille-gpui/src/assets.rs)
      stacks our local SVG bundle in front of the upstream gpui-
      component icon pack; both fit one `icons/X.svg` namespace.
- [x] Extension-specific SVG overrides in `file_type_icon`: PDFs use
      `icons/file/pdf.svg`, HTML uses `html.svg`,
      CSV/TSV/XLS/XLSX/ODS/Numbers use `spreadsheet.svg`. The Document
      tint no longer collapses every paper-shaped file to the same
      `text.svg` glyph.
- [x] Filter input now searches the visible Format value (the
      unified magic-or-kind label) on top of the filename. Was
      searching only `display_kind`; typing `pdf document` or
      `jpeg image` would have missed every row before the fix.
      Both filter sites updated: [file_list.rs](../crates/feraille-gpui/src/file_list.rs)
      load + [shell.rs](../crates/feraille-gpui/src/shell.rs)
      `run_directory_load`.

### Files touched

- [crates/feraille-core/src/lib.rs](../crates/feraille-core/src/lib.rs)
  — `FileEntry::format_label()` + heuristic + 6 tests.
- [crates/feraille-gpui/src/icons.rs](../crates/feraille-gpui/src/icons.rs)
  — `FileTypeTint`, `file_type_icon()`, `tint_color()`.
- [crates/feraille-gpui/src/file_list.rs](../crates/feraille-gpui/src/file_list.rs)
  — column rework, render_td rewrite, SortColumn::Format, tooltip.
- [crates/feraille-gpui/src/assets.rs](../crates/feraille-gpui/src/assets.rs)
  — new composite AssetSource.
- [crates/feraille-gpui/src/main.rs](../crates/feraille-gpui/src/main.rs)
  + [screenshot.rs](../crates/feraille-gpui/src/screenshot.rs) — wire
  `FeraAssets` in `with_assets`.
- [crates/feraille-gpui/resources/icons/file/](../crates/feraille-gpui/resources/icons/file/)
  — 13 new SVGs (text, code, image, video, audio, archive, disk,
  app, symlink, generic, spreadsheet, pdf, html).
- [crates/feraille-gpui/Cargo.toml](../crates/feraille-gpui/Cargo.toml)
  — added `rust-embed` for our local asset bundle.

### Notes for the future

- Inline SVGs are hand-simplified Lucide silhouettes. They share the
  folded-corner document shape; at 18 DIPs they're scannable but a
  larger sweep through Lucide's full set would help differentiation
  further. Park for a polish iter — not on the critical path.
- The mismatch heuristic is conservative (covers text-family +
  ZIP-family compatibility groups). False positives reported by users
  expand the compatibility table in
  [feraille-core::formats_compatible](../crates/feraille-core/src/lib.rs).
- `FileCategory` from `feraille-disk-usage` was *not* reused — the
  DU enum is shaped around treemap colouring (Other / Document /
  Executable). The file-list tint enum has finer granularity
  (Image vs Video vs Audio etc.). Keeping them separate for now to
  avoid leaking DU concerns up the stack; if they converge later,
  promote into `feraille-core`.

### gpui-component primitives

`Icon` (Lucide), `Tooltip`, `Badge` (for any per-row count badges),
existing `Table`.

### Domain-boundary notes

- New methods on `FileEntry` are pure functions over already-stored
  fields. No I/O. No raw bytes.
- Magic detection pipeline (`feraille-fs-native::detect_magic`)
  already returns a string; we feed it through `display_magic`. No
  change needed there.

### Acceptance

- A directory mixing PNG / JSON / DOCX / MD / DMG / JPG / PDF / HTML /
  ZIP is visually scannable without reading the format column.
- No duplicate "PNG / PNG image"-style data.
- Extension/magic disagreements surface a visible cue at the row.
- Hover and selected states are obvious without squinting.

---

# Phase 2 — Sidebar IA Cleanup

**Status:** Done (pending user review)
**Goal:** remove the duplicate-Downloads bug and pick a single
sidebar IA model.

**Decision (user-locked 2026-05-14):** Option **C** — flat Favorites
above an expandable Browse tree, with mounted Volumes as a third
tree-style section. Favorites do not expand, so the same path can no
longer appear both as a favorite and as a tree-descendant of the
parent home folder.

### Deliverables

- [x] Removed the duplicate Downloads — Favorites is now flat
      (no caret, no expand), and **Browse filters out depth-1 Home
      children that match a Favorite path** so Desktop / Documents /
      Downloads / Applications / Movies / Music / Pictures no longer
      reappear under the expanded Home tree. Browse now reads as
      "the parts of Home that aren't already in Favorites" — Library,
      Public, custom subfolders, etc. The filter is depth-scoped:
      deeper descendants (`Library/Application Support`, etc.) are
      untouched. Implemented via the new
      `append_tree_descendants_filtered(skip_paths: Option<&HashSet>)`
      helper.
- [x] Adopted `gpui_component::sidebar::{Sidebar, SidebarGroup,
      SidebarMenu, SidebarMenuItem}` for the Favorites section.
      Each shortcut is a `SidebarMenuItem` with a Lucide-style
      `.icon(Icon::empty().path("icons/nav/X.svg"))` prefix and an
      `.active(...)` state that matches the current tab's path.
- [x] Custom `TreeSection` retained for **Browse** (single Home
      root, expandable) and **Volumes** (each volume expandable),
      since gpui-component's `Tree` primitive doesn't yet offer the
      same lazy-children + active-path + capacity-bar story we
      already paint.
- [x] New `ShellSidebarItem` enum in
      [tree.rs](../crates/feraille-gpui/src/tree.rs) wraps
      `SidebarGroup<SidebarMenu>` and `TreeSection` so
      `Sidebar<ShellSidebarItem>` can hold a mixed sequence —
      gpui-component otherwise pins one `E` for all of a sidebar's
      children.
- [x] Bundled 10 new Lucide-style SVGs at
      [resources/icons/nav/](../crates/feraille-gpui/resources/icons/nav/):
      home, apps, desktop, documents, downloads, movies, music,
      pictures, folder, drive, chevron-right, chevron-down.
- [x] Browse section default-collapsed; clicking the Home caret
      lazy-enumerates and shows descendants (existing behaviour
      preserved).
- [ ] Hover / focus state audit on the new SidebarMenuItem rows is
      handed off to the Phase 10 polish sweep — gpui-component
      already paints hover via its theme tokens, but the *contrast*
      against our active state may want tuning.

### Files touched

- [shell.rs](../crates/feraille-gpui/src/shell.rs) — Favorite struct,
  FAVORITES const, build_favorites_menu, build_browse_rows
  (replaces build_locations_rows), render() rewiring.
- [tree.rs](../crates/feraille-gpui/src/tree.rs) — `ShellSidebarItem`
  enum + impls.
- [resources/icons/nav/](../crates/feraille-gpui/resources/icons/nav/)
  — 10 new SVGs.

### Notes for the future

- Custom favorites (drag-to-add, persist across sessions) is a Phase
  6+ concern — Favorites today is a fixed list of XDG-style home
  shortcuts. Decision: persistence key will be canonical path
  string (resolved via `feraille-fs-native::canonicalize` at save
  time), per the locked-decisions answer #2.
- `gpui_component::tree::Tree` is worth a revisit when we add a
  "Recents" section — it ships keyboard nav (arrow keys + Enter)
  that our TreeSection doesn't yet implement.
- The right-click "Reveal in Browse" affordance (Phase 6 plan)
  would expand the Browse tree to the matching ancestor; existing
  `Shell::reveal_path_in_tree` already does this for the old
  Locations API and just needs rewiring to the new single-rooted
  Browse model.

### gpui-component primitives

`Sidebar`, `SidebarGroup`, `SidebarMenu`, `SidebarMenuItem`, `Tree`
(if available), `Icon`, `Tooltip`.

### Domain-boundary notes

- Favorites persistence: store as a list of `NodeId` (or canonical
  path string for now if NodeId isn't durable across sessions —
  decision tracked below).
  - [ ] Decide: are NodeIds stable across restarts? If not, what is
        the persistence key? Likely canonical path. Document in
        `feraille-core`.
- Volumes enumeration already lives in
  `feraille-fs-native::list_volumes`. Use it; do not reach for
  `mount(8)` or similar.

### Acceptance

- "Downloads" appears at most once in the sidebar.
- Active location is unmistakable.
- Tree and Favorites are visually distinct sections, not competing
  navigation systems.
- Sidebar collapses sensibly at narrow window widths (paired with
  Phase 5).

---

# Phase 3 — Settings Polish (adopt `gpui_component::setting::Settings`)

**Status:** Done (pending user review)
**Goal:** replace our hand-rolled
[settings.rs](../crates/feraille-gpui/src/settings.rs) with the
library's `Settings` primitive. The library already ships search,
icons, item layouts, field types, and reset support — so the work
collapses to "translate our categories into Pages + Groups + Items."

### Deliverables

- [x] Rebuilt [settings.rs](../crates/feraille-gpui/src/settings.rs)
      as a thin wrapper around `setting::Settings::new(...).pages(...)`.
      External API (`SettingsView`, `SettingsCategory`,
      `category_from_arg`, `open_settings_window`, `ThemePref`)
      preserved so main / shell / screenshot didn't need changes.
- [x] Translated categories → `SettingPage` with Lucide icons:
      Appearance = `palette`, Files = `folder`, Layout =
      `settings-2`, Shortcuts = `keyboard`, About = `info`. Icons
      pulled from the upstream gpui-component bundle (already shipped
      via `FeraAssets` from Phase 1).
- [x] **Built-in search** at the top of the sidebar comes for free
      with the primitive. Filters across pages by `SettingItem`
      title + description.
- [x] Each Files / Layout row is a `SettingItem` with the matching
      `SettingField`:
  - Show-hidden → `SettingField::switch` reading/writing
    `app_state::show_hidden`.
  - UI scale → `SettingField::dropdown` with four discrete steps
    (85% / 100% / 115% / 130%).
- [x] Theme picker → `SettingField::render` painting the three
      preview tiles. Persists via `app_state::theme_pref` and applies
      live through `Theme::change`.
- [x] **Strengthened selected theme tile**: filled circle-check badge
      next to the active label (visible in the screenshot — System
      tile has the blue badge).
- [x] **Theme click repaints all open windows.** Click handler now
      passes `Some(window)` to `Theme::change` *and* calls
      `cx.refresh_windows()` so the Settings window AND the
      background Shell window both pick up the new palette
      immediately. (Phase 3 review fix — previous `None` argument
      left the UI stale until another refresh happened.)
- [x] **Appearance card layout fixed.** Theme item now uses
      `SettingItem::layout(Axis::Vertical)` so the three fixed-width
      preview tiles wrap under the title instead of competing with
      it for horizontal space — the System tile no longer clips on
      the right. (Phase 3 review fix.)
- [x] **Cached `count_home_hidden_items` on `SettingsView`** so the
      sync `$HOME` scan happens once at view construction, not on
      every render. Search-input keystrokes and page-nav clicks no
      longer pile sync I/O onto the UI thread. (Phase 3 review fix.)
- [x] **Files-page copy updated** to imply "Takes effect on next
      launch" — Show Hidden writes app_state but doesn't push into
      already-open Shell windows yet. Live propagation tracked for
      Phase 10 audit alongside the system theme observer.
- [x] Shortcuts page is one `SettingItem` per command, grouped by
      `SettingGroup` per Category. Description: "`<Category>` ·
      `<chord>`". Right-aligned chord pill via the existing
      `keyboard_help::format_shortcut`. Built-in search indexes every
      entry — typing "open", "trash", "tab" finds the right rows.
- [x] About page = single `SettingItem::render` with the existing
      card content (app name, version, tagline).
- [ ] **"Saved" feedback pill** parked. The library's
      `SettingField::set_value` callbacks receive `&mut App` only,
      and `Window::push_notification` requires `&mut Window`. The
      visible field state change (toggle flips, dropdown shows new
      value) already provides feedback; the toast wants either a
      library-side hook to expose Window in setter callbacks or a
      shared notify-from-app wrapper. Tracked in the Deferred list.

### gpui-component primitives

`setting::{Settings, SettingPage, SettingGroup, SettingItem, SettingField}`,
`Icon` (with our local SVG bundle), `Switch`, `Dropdown` /
`NumberInput`, `Notification`, `Badge` (check-circle on selected
tile).

### Acceptance

- Built-in search filters across pages and items.
- All previous categories present with icons.
- Selected theme tile reads as "active" without ambiguity.
- Every mutation produces a quiet "Saved" toast.

---

# Phase 4 — Preview Pane Densification

**Status:** Done (pending user review)
**Goal:** more useful info in less vertical space; long names /
paths recoverable.

### Deliverables

- [x] Replaced the stacked `preview_field` blocks with
      `gpui_component::description_list::DescriptionList::vertical()
      .small().columns(1)` in
      [shell.rs::preview](../crates/feraille-gpui/src/shell.rs).
      Rows: Format / Size / Modified / Where / Quarantine (last only
      when applicable). Dropped the unused `preview_field` helper.
- [x] Filename: `truncate()` with a `Tooltip` carrying the full name.
- [x] Path: new `middle_truncate_path(s, max)` helper (3 unit tests)
      keeps the basename visible — produces `/Users/jkn/…/icon.png`
      style at narrow widths. The value cell itself wraps in a div
      that adds a `Tooltip` with the full path.
- [x] Compact action row at the bottom: 4 **icon-only** `Button`s
      (Open / Reveal in Finder / Copy Path / Get Info) using
      `.xsmall().ghost()`. Each button's `tooltip_with_action(...)`
      pulls the keyboard chord from the active keymap automatically
      — hover reads "Open ⌘O", "Reveal in Finder ⌘⌥R", etc. No
      truncation risk at the default ~280-DIP preview width.
- [x] Thumbnail area: tightened to `h(200)` with
      `max_w(248)`/`max_h(184)` so the image keeps aspect ratio
      inside a fixed slot, leaving more vertical space for metadata.

### Files touched

- [shell.rs::preview](../crates/feraille-gpui/src/shell.rs) — full
  body rewrite (DescriptionList, action row, tooltips).
- [shell.rs::middle_truncate_path](../crates/feraille-gpui/src/shell.rs)
  — new helper + 3 unit tests.

### Notes

- Format value is the same `FileEntry::format_label()` we land in
  Phase 1, so the preview's Format row and the file list's Format
  column read identically — single source of truth.
- Quarantine row only appears when `entry.is_quarantined`. The
  red badge below the list expands to "Quarantined · Mark of the
  Web" copy for context.
- Screenshot pipeline still misses the thumbnail (`qlmanage -t`
  subprocess doesn't finish inside the one-shot render). The live
  app shows it correctly on the next paint. Documented limitation.

### gpui-component primitives

`DescriptionList`, `Tooltip`, `Button`, `Kbd`, existing thumbnail
pipeline.

### Domain-boundary notes

- All metadata fields already live as `display_*` strings on
  `FileEntry` or as side-channel maps (quarantine, ant-trail). No
  domain change needed — UI just reshapes the display.

### Acceptance

- Preview pane shows at least 6 metadata rows + thumbnail + action
  row without scrolling at default window size.
- Long filenames no longer wrap awkwardly.
- Long paths show ellipsis in the middle, full text on hover.

---

# Phase 5 — Resizable Layout

**Status:** In progress (sidebar splitter shipped in 5.5.d; needs
persistence + collapse rules + preview wiring polish)
**Goal:** users shape the window to their workflow; widths persist.

### Deliverables

- [ ] Persist sidebar width to
      [app_state.rs](../crates/feraille-gpui/src/app_state.rs).
      Restore on launch.
- [ ] Persist preview-pane width similarly.
- [ ] Confirm preview pane is wrapped in `resizable_panel`. Today
      it's a sibling; verify the splitter has 3 children when
      preview is visible.
- [ ] Min/max widths sensible:
  - sidebar: 160-400 DIPs (current)
  - file list: no upper bound, min 320 DIPs
  - preview: 220-520 DIPs (current)
- [ ] At narrow window widths: preview collapses first, then the
      sidebar collapses to icons. Wire a window-resize observer in
      Shell.
- [ ] At wide widths: panes have max widths so the file list grows
      rather than the sidebar sprawling.

### gpui-component primitives

`resizable::h_resizable`, `resizable_panel`, `ResizableState`
(already used).

### Acceptance

- Drag any divider; widths update fluidly.
- Restart the app; widths restore exactly.
- Shrink the window to ~800 DIPs wide; preview hides automatically.
- Shrink to ~500 DIPs wide; sidebar collapses to icons (or hides).

---

# Phase 6 — Context Menus

**Status:** In progress (file-row PopupMenu shipped via gpui-component
in 5.5.c; sidebar / breadcrumb / empty-space menus pending)
**Goal:** right-click works everywhere users expect.

### Deliverables

- [ ] **File row menu** — confirm content matches the brief:
  Open / Open With / Get Info / Quick Look / Reveal in Finder /
  Copy Path / Rename / Duplicate / Make Alias / Compress / Move to
  Trash / Tags submenu.
  - [ ] Open With submenu: list candidate apps from
        `feraille_shell_mac::open_with_candidates(path)`. Cache the
        list per-file-kind so it's not re-fetched per right-click.
- [ ] **Sidebar menu:** Open / Reveal in Finder / Remove from
      Favorites (only on Favorites entries) / Rename (favorite only).
- [ ] **Breadcrumb segment menu:** Copy Path / Open in New Tab /
      Reveal in Finder / New Folder Here.
- [ ] **Empty-space menu** in file list (right-click in the gap
      below the last row): New Folder / Paste / Refresh / Show
      Hidden toggle / Sort by submenu / View as submenu.
- [ ] **Tags submenu:** seven colour buttons (Red, Orange, Yellow,
      Green, Blue, Purple, Gray). Wire via
      `feraille_shell_mac::toggle_tag`. Carries the
      not-yet-implemented "7 typed actions" from the prior todo.
- [ ] Every menu item with a keyboard shortcut shows it inline using
      `Kbd`.
- [ ] Destructive actions: confirm via `Dialog` when count > N (e.g.
      Move-to-Trash for > 10 items), or always-confirm for permanent
      delete.

### gpui-component primitives

`Menu`, `Popover`, `Kbd`, `Tooltip`, `Dialog` (confirms).

### Domain-boundary notes

- Catalogue (`feraille-core::commands`) already drives titles +
  shortcuts. Each menu builder reads from `all_commands()` filtered
  by `Category::Context` (or a new tag) — do not hard-code titles.

### Acceptance

- Right-click anywhere reasonable; a menu appears.
- Every shortcut-bearing menu item shows the shortcut.
- Destructive bulk operations confirm with a count.

---

# Phase 7 — Toolbar and TitleBar Upgrade

**Status:** Not started
**Goal:** the top of the window stops feeling empty.

### Deliverables

- [ ] Adopt `gpui-component::TitleBar` for the window chrome. Layout:
  - Left: app icon + breadcrumb (move breadcrumb up).
  - Center: global search input ("search this folder" default,
    scope-up via dropdown).
  - Right: action buttons (View mode toggle, Sort dropdown, Group
    dropdown), overflow `…`.
- [ ] Search input wired to filter pipeline (today `filter_text`).
      Keep Cmd+F focusing this input; remove the separate filter
      input from the toolbar (or hide it when TitleBar search is
      present).
- [ ] Toolbar densification (under the TitleBar):
  - [ ] Back / Forward (existing).
  - [ ] Refresh button.
  - [ ] New Folder button.
  - [ ] View mode segmented control (List / Grid / Columns) — Grid
        and Columns may be Phase 13+ for the actual view
        implementations; today just wire the buttons that go to
        List.
  - [ ] Sort dropdown (Name / Date / Size / Kind / Format, asc/desc).
  - [ ] Group dropdown (None / Kind / Date / Size band).
  - [ ] Show Hidden toggle (already present).
  - [ ] Overflow `…` for less-used items.
- [ ] Every icon-only button has a `Tooltip` with `Kbd` shortcut.

### gpui-component primitives

`TitleBar`, `Input`, `Button`, `ButtonGroup`, `Popover` (dropdown
content), `Kbd`, `Tooltip`.

### Acceptance

- Top of window reads as a real app chrome (compare to Longbridge
  Pro reference).
- Common operations are one click away.
- Every icon-only button explains itself on hover with its shortcut.

---

# Phase 8 — Status Bar and Task Feedback

**Status:** In progress (status bar + task panel + DU task
registration shipped this session)
**Goal:** unobtrusive truth; never a frozen UI.

### Deliverables

- [ ] Status bar shows:
  - [ ] item count (existing).
  - [ ] selected count: "N of M selected".
  - [ ] selected size: "· 4.2 MB".
  - [ ] total visible size for the folder (sum of currently filtered
        rows).
  - [ ] free disk space for the active tab's volume.
  - [ ] active background tasks count (existing — refines to a
        Badge when count > 0).
  - [ ] progress strip (existing).
- [ ] Task panel polish:
  - [ ] Cancel button per task when `cancellable: true` (`Button`
        with `xs()` + `Icon::X`).
  - [ ] Persist a short history of recent finished tasks (last 5,
        with completion time + status).
  - [ ] Click-outside dismisses the popover (event capture overlay).
- [ ] Register tasks for:
  - [ ] Magic detection prefetch (already wired? confirm).
  - [ ] Thumbnail generation.
  - [ ] Copy / move / delete operations (when implemented).
  - [ ] Folder enumeration on large folders (>10k entries).
- [ ] Status-bar cut-off text bug: investigate the brief's report of
      a layout issue under "18 items"; fix.

### gpui-component primitives

Existing status bar + task_panel modules; `Badge`, `Button`, `Icon`,
`Tooltip`.

### Domain-boundary notes

- `TaskRegistry` lives in
  [tasks.rs](../crates/feraille-gpui/src/tasks.rs) (UI crate). Task
  *kinds* enum could move to `feraille-core` if any domain crate
  needs to refer to them; today none do, so leave them in UI.

### Acceptance

- User always knows work is in flight; UI never blocks.
- Selected count / size visible at a glance.
- No spinner-first surfaces (per the locked decision).

---

# Phase 9 — Notifications and Undo

**Status:** Not started
**Goal:** every mutation acknowledged; reversible operations are
undoable.

### Deliverables

- [ ] Toast notifications via `gpui-component::Notification` for:
  - [ ] Moved to Trash (with Undo button).
  - [ ] Renamed (with Undo button).
  - [ ] Created folder (no Undo — trivial to delete).
  - [ ] Copy / Move complete (with Undo).
  - [ ] Permission denied (error toast, no Undo).
  - [ ] Long operation completed when the user has switched windows
        (system-level — defer if too invasive).
- [ ] Undo stack: per-window ring of last 20 reversible actions.
      Cmd+Z fires it. Each action records the inverse op.
- [ ] Dialogs (`gpui-component::Dialog` / `AlertDialog`) for:
  - [ ] Permanent delete (Cmd+Opt+Backspace) — always confirms.
  - [ ] Bulk move-to-trash above the threshold (Phase 6).
  - [ ] Replace-on-copy collision (Keep both / Replace / Skip).
- [ ] No dialog for plain Cmd+Backspace (Trash is itself reversible
      via Finder).

### gpui-component primitives

`Notification`, `Dialog`, `AlertDialog`, `Button`.

### Domain-boundary notes

- Reversible-action recording lives in the UI crate; the *operations*
  themselves (rename, move, trash) call into
  `feraille-shell-mac` / `feraille-fs-native` as today. The undo
  layer is a façade that records → calls → can-replay-inverse.

### Acceptance

- Every state-mutating action surfaces feedback.
- Cmd+Z undoes the last trash / rename / move within the session.
- Permanent destructive actions confirm; reversible ones don't.

---

# Phase 10 — Full Interaction Audit

**Status:** Not started
**Goal:** the commercial polish pass — no mystery interactivity, no
half-themed surfaces, no cut-off text.

### Audit checklist

- [ ] Every clickable element has a visible hover state.
- [ ] Every selected state uses the same visual language across
      surfaces (file rows, sidebar entries, theme tiles, segmented
      controls).
- [ ] Every truncated string has a `Tooltip` with the full text.
- [ ] Every icon-only button has a `Tooltip` with the shortcut.
- [ ] Every menu item with a shortcut shows it via `Kbd`.
- [ ] Empty folders show a proper empty state (illustration optional,
      copy "This folder is empty" + helpful action).
- [ ] Permission errors show readable error states ("Can't read this
      folder — permission denied" + Open System Settings link).
- [ ] Theme switching updates every component instantly; no FOUC.
- [ ] Keyboard navigation works:
  - [ ] Tab order through sidebar / file list / toolbar / settings.
  - [ ] Arrow keys in lists.
  - [ ] Esc closes menus, dialogs, popovers.
  - [ ] Cmd+, opens Settings from anywhere.
- [ ] Focus rings visible on every focusable element.
- [ ] No accidental-looking spacing (audit every pixel).
- [ ] Animation budget: every animation has a justification; none
      exceed 200 ms; none are decorative.

### Acceptance

- Open the app cold, click through every visible surface, hover
  everything. Nothing surprises.
- A new developer (or designer) can open any screen and identify
  active / hover / disabled state at a glance.

---

# Component inventory (use / use soon / avoid)

Mirrored from the brief; track adoption per phase.

| Component | Status | Used in |
|---|---|---|
| `Sidebar` | Partial | Shell, Settings — Phase 2 finishes |
| `Tree` | Custom today | Phase 2 swap (if API fits) |
| `Table` | Adopted | file_list.rs |
| `Icon` (Lucide) | Not yet | Phase 1, 2, 3 |
| `Input` | Adopted | filter, breadcrumb edit, shortcuts overlay |
| `Kbd` | Adopted | shortcuts overlay, Settings — Phase 6, 7 expand |
| `Tooltip` | Adopted | sidebar tooltips — Phase 1, 4, 6, 7 expand |
| `Badge` | Not yet | Phase 8 |
| `TitleBar` | Not yet | Phase 7 |
| `Menu` / `Popover` | Adopted (file row) | Phase 6 expands |
| `Resizable` | Adopted | shell.rs — Phase 5 persists widths |
| `Sheet` | Not yet | Phase 4 (Get Info) candidate |
| `Notification` | Available via Root | Phase 9 |
| `Dialog` / `AlertDialog` | Adopted (new folder / rename) | Phase 6, 9 expand |
| `Tabs` | Custom today | Phase 7 review |
| `DescriptionList` | Not yet | Phase 4 |
| `HoverCard` | Not yet | optional Phase 10 polish |
| `Spinner` | Deferred | per locked decision #2 |
| `Skeleton` | Not yet | use sparingly per locked decision #2 |
| `Progress` | Custom strip today | re-use in task panel rows |
| `Tag` | Not yet | Phase 6 tags submenu |
| `Switch` | Adopted | Settings |
| `ContextMenu` (via Menu) | Adopted | Phase 6 |
| `Scrollable` | Implicit via overflow_y_scroll | OK |
| `Editor` | Out of scope | future quick-edit work |
| `DataTable` | Out of scope | future reporting views |
| `Chart` / `Plot` | Out of scope | future Disk Usage stats donut |
| `Calendar` / `DatePicker` | Out of scope | future date-range filter |
| `ColorPicker` | Out of scope | Phase 3 accent picker (later) |
| `Form` | Avoid | Settings is a control panel, not a form |
| `Pagination` | Avoid | virtualisation handles file lists |
| `Accordion` | Avoid | `Collapsible` suffices |

---

# Library findings (2026-05-14 docs + source review)

Done a systematic pass through both the cloned repo
(`/Users/jkn/Source/gpui-component`) and the public docs at
<https://longbridge.github.io/gpui-component/docs/components/>.
Key discoveries that *change the plan*:

1. **`gpui_component::setting::Settings`** is a full settings
   primitive: hierarchical `SettingPage → SettingGroup → SettingItem
   → SettingField`, **built-in search/filter** (`filtered_pages` +
   `SettingItem::is_match`), Lucide icons per page, optional reset,
   horizontal/vertical item layouts, field types (Switch / Checkbox
   / Input / Dropdown / NumberInput / custom element). **Phase 3 is
   no longer "add a search box to our hand-rolled settings" — it's
   "replace our hand-rolled settings with `Settings`."**
2. **`description_list::DescriptionList`** ships `DescriptionItem`,
   horizontal/vertical layouts, `columns(n)`, `label_width`,
   `bordered`, `divider()`, `span(n)`. Phase 4 uses this directly.
3. **`menu::ContextMenu` + `ContextMenuExt`** ships a context-menu
   primitive callable as `.context_menu(|menu, cx| menu.item(...))`
   on any element. Phase 6 wires this everywhere right-click is
   expected.
4. **`table::DataTable` + `TableDelegate`** is the file-list table
   we're already using (verify), with virtual scrolling and custom
   cell rendering. Per-cell content for the Phase 1 Format column +
   icon happens via `render_td`.
5. **`Sidebar::collapsible(SidebarCollapsible::Icon | Offcanvas |
   None)`** + animated `SidebarToggleButton` gives Phase 5's
   narrow-width collapse for free.
6. **`sheet::Sheet`** slide-in panels (left/right/top/bottom,
   resizable, with title/footer). Good candidate for Phase 4 Get
   Info → either inline DescriptionList or slide-in Sheet.
7. **`hover_card::HoverCard`** confirmed — Phase 10 polish option
   for richer file-row hover info.
8. **Icons**: 99 SVGs ship in `crates/assets/assets/icons/`. **Only 4
   are file-related** (`file`, `folder`, `folder-closed`,
   `folder-open`). No PDF / image / video / audio / archive / code
   glyphs ship in the bundle. Phase 1 must **add SVGs to our own
   asset bundle** (`crates/feraille-gpui/resources/icons/` or
   similar), then thread them through `Icon::path("icons/X.svg")`.
   Lucide SVGs are freely licensed; pull a curated set from
   <https://lucide.dev/icons/>.
9. **`AppMenuBar`** exists in the menu module — could replace our
   gpui `cx.set_menus` plumbing in a follow-on, but not on the
   critical path.
10. **`table::Table` (a.k.a. DataTable in docs)** — already adopted
    in [file_list.rs](../crates/feraille-gpui/src/file_list.rs).
    The `Table` / `TableDelegate` / `TableState` trio in our pinned
    version is the same primitive the docs call "DataTable."
    Phase 1's column rework operates on `render_td`.
11. **`VirtualList`** (and `h_virtual_list` / `v_virtual_list`) ships
    in the lib root. Useful for cases where Table is overkill —
    candidate for a future Grid view, the Recent / Favorites list in
    the sidebar, or any auxiliary popovers that show many rows.
    Park as a Phase 7+ option; not on the critical path today.
12. **Other modules worth mining as we touch their phase**:
    `slider`, `stepper`, `select`, `checkbox`, `radio`, `breadcrumb`,
    `link`, `label`, `group_box`, `accordion`, `collapsible`,
    `history` (undo/redo), `editor` (future Quick-Edit), `chart`
    (future Disk Usage donut), `dock` (future multi-panel layouts).
    Convention: before hand-rolling anything in a phase, check the
    `crates/ui/src/` module list first.

# Decisions made (resolved by user 2026-05-14)

1. **Sidebar IA (Phase 2):** option **C** — Favorites + Tree, both
   visually distinct, dedup'd.
2. **NodeId stability:** treat NodeIds as not-durable across
   restarts. Persistence key for Favorites / tabs = canonical path
   string. Future work can revisit if we add a stable id store.
3. **Cmd+K command palette:** deferred to a follow-on plan. Tracked
   in the "Deferred" section below.
4. **Grid / Columns view (Phase 7):** deferred. Toolbar omits the
   View-mode segmented control for now; list view is the only mode.
5. **System theme observer:** deferred to Phase 10 audit (or later
   follow-on). Today the theme is sampled at startup; users can
   restart to pick up a system Appearance change.

# Deferred / parking lot

- Cmd+K command palette.
- Grid view + Columns view + their toolbar mode-toggle.
- System theme observer (live follow on macOS Appearance change).
- Tag chips inline on file rows (carried forward from earlier todo).
- Reporting / analytics views (Disk Usage donut, indexing activity).
- Editor primitive (inline text edit in preview).
- ColorPicker-based accent color override (Phase 3 placeholder
  noted; ships only when the primitive is available in our pinned
  gpui-component version).

---

# Recommended team order

Per the brief:

1. Phase 1 — File icons + Magic/Kind cleanup
2. Phase 2 — Sidebar IA + icons
3. Phase 3 — Settings search + icons
4. Phase 4 — Preview pane densification
5. Phase 5 — Resizable persistence + collapse rules
6. Phase 6 — Context menus everywhere
7. Phase 7 — TitleBar + toolbar densification
8. Phase 8 — Status bar + task feedback polish
9. Phase 9 — Notifications + Undo
10. Phase 10 — Full interaction audit

Each phase is independently shippable. After Phase 1 the file list
feels different; after Phase 5 the window feels like a tool; after
Phase 10 the app feels finished.
