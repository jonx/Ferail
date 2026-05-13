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

**Status:** Not started
**Goal:** make the main file list feel like a commercial file manager
— scannable by icon, no redundant columns, hover/select state visible.

### Deliverables

- [ ] Build `file_type_icon(entry: &FileEntry) -> IconName` in a new
      [crates/feraille-gpui/src/icons_lucide.rs](../crates/feraille-gpui/src/icons_lucide.rs)
      (or extend `icons.rs`). Use gpui-component's Lucide icon set.
      Mapping: folder, image, video, audio, PDF, document,
      markdown/text/code, archive, disk-image, executable/app,
      symlink, unknown.
- [ ] Tint icons by category using theme tokens (no hard-coded
      colours). Categories from
      [feraille-disk-usage::FileCategory](../crates/feraille-disk-usage/src/model.rs)
      or a fresh enum in `feraille-core` if the disk-usage one is too
      specific.
- [ ] Decide folder/volume icons: keep NSWorkspace (current
      [icons.rs](../crates/feraille-gpui/src/icons.rs)) or switch to
      Lucide for visual consistency. **Recommendation:** keep
      NSWorkspace for folders (users recognise their custom folder
      icons / sync-cloud overlays); Lucide for everything else.
- [ ] Replace `Kind` + `Magic` columns in
      [file_list.rs:64-65](../crates/feraille-gpui/src/file_list.rs#L64)
      with one `Format` column. Source: `display_magic` when
      non-empty, else `display_kind`. **Domain change** (additive):
      add `FileEntry::format_label() -> (String, bool)` returning
      `(primary, has_mismatch)` in
      [feraille-core/src/lib.rs](../crates/feraille-core/src/lib.rs).
- [ ] Mismatch indicator: when extension-implied kind ≠
      magic-detected format, render a small alert dot or info icon
      after the format text. Tooltip on hover: "Extension: .X, but
      content looks like Y."
- [ ] Tooltip on truncated filename column (gpui-component `Tooltip`
      wrapping the row's name cell when text overflows).
- [ ] Hover state per row: theme-driven row background opacity bump.
      Audit current `render_tr` for missing hover.
- [ ] Selected state per row: stronger than hover, accent-tinted.
      Verify it's already loud enough vs. selected theme-tile in
      Settings (the brief flags inconsistency between the two).

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

**Status:** Not started
**Goal:** remove the duplicate-Downloads bug and pick a single
sidebar IA model.

### Open decision (resolve before implementation)

- [ ] **Pick the sidebar model:**
  - (A) **Flat Favorites only.** Drop the Home-as-tree. Sidebar is
        just the user's pinned locations (Home, Desktop, Documents,
        Downloads, Applications, mounted Volumes, custom favorites).
        Filesystem browsing happens via main pane + breadcrumb.
  - (B) **Tree only.** A single hierarchical tree rooted at `/` (or
        Home), with mounted volumes as sibling roots. Favorites
        become tree "pinned" markers, not separate entries.
  - (C) **Both, deduplicated and visually distinct.** Favorites
        section at top (flat shortcuts); Tree section below as
        "Browse" (lazy filesystem explorer). The tree never contains
        nodes that are also pinned to Favorites *unless* they appear
        with a visual badge indicating "this is also a Favorite."
- *Recommendation:* (C) with strict deduplication. Most native Finder
  / Files apps converged on this; users want both.

### Deliverables (assuming option C)

- [ ] Remove the duplicate Downloads entry in the current sidebar.
- [ ] Adopt `gpui-component::Sidebar` end-to-end. We're already using
      it for the top-level container; ensure every group uses
      `SidebarGroup` + `SidebarMenu` + `SidebarMenuItem` (or the
      header API for sub-sections).
- [ ] Move the existing tree code in
      [tree.rs](../crates/feraille-gpui/src/tree.rs) under a
      `SidebarGroup::new("Browse")` with collapsible header.
- [ ] Investigate `gpui-component::Tree` (if present in the version we
      pin). If suitable, swap our custom `TreeSection` for it; keep
      our `TreeRowSpec` + lazy children API as the data shape.
- [ ] Icons on every sidebar row (Phase 3 covers Settings sidebar
      icons; this covers main sidebar).
- [ ] Hover, active, focus, disabled states verified per row.
- [ ] Persistent active marker matches the file-list selection
      contrast level (paired with Phase 1's selected-row work).

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

**Status:** Not started
**Goal:** replace our hand-rolled
[settings.rs](../crates/feraille-gpui/src/settings.rs) with the
library's `Settings` primitive. The library already ships search,
icons, item layouts, field types, and reset support — so the work
collapses to "translate our categories into Pages + Groups + Items."

### Deliverables

- [ ] Rebuild [settings.rs](../crates/feraille-gpui/src/settings.rs)
      as a thin wrapper around `setting::Settings::new(...).pages(...)`.
- [ ] Translate existing categories → `SettingPage` with Lucide
      icon: Appearance = `palette`, Files = `folder`, Layout =
      `layout-dashboard`, Shortcuts = `keyboard` (or fallback to
      `info` if absent in our bundle), About = `info`.
- [ ] Each existing row becomes a `SettingItem` with the matching
      `SettingField`:
  - Theme picker → custom `SettingField::element` rendering the
    three preview tiles (keeps our preview-tile design).
  - Show-hidden → `SettingField::switch`.
  - Sidebar width → `SettingField::dropdown` (Narrow / Medium /
    Wide) or custom segmented control.
  - UI scale → `SettingField::dropdown` or `NumberInput`.
- [ ] Shortcuts page: custom `SettingField::render` rendering our
      existing catalogue-grouped list — `Settings`'s search/filter
      will index it automatically because we provide titles +
      descriptions per item.
- [ ] About page: custom group with the existing inner card content.
- [ ] **Strengthen selected theme tile** — accent ring (2 DIPs) +
      `check-circle` badge in the corner. Same treatment for the
      sidebar-width segmented control.
- [ ] **"Saved" feedback pill** — `Notification` (toast) via
      `window.push_notification` on every persist, copy
      "Saved · <category name>"; or anchored inline if the
      `setting::Settings` provides a header slot we can fill.
- [ ] Verify built-in search filters across all pages.

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

**Status:** Not started
**Goal:** more useful info in less vertical space; long names /
paths recoverable.

### Deliverables

- [ ] Replace stacked label/value blocks in
      [preview.rs](../crates/feraille-gpui/src/preview.rs) with
      `gpui-component::DescriptionList`. One row per metadata field
      (Format, Size, Modified, Created, Permissions, Path).
- [ ] Filename: truncate cleanly with the basename always visible.
      `Tooltip` carries the full name.
- [ ] Path: middle-truncate when long (helper `middle_truncate(s,
      max_chars) -> String` in a shared util). Full path in Tooltip.
- [ ] Compact action row at the bottom of the pane: Open / Reveal in
      Finder / Copy Path / Get Info. Uses `gpui-component::Button`
      with `compact()` and an icon. Each button has a Tooltip showing
      its keyboard shortcut via `Kbd`.
- [ ] Thumbnail/icon area: better sizing constraints (max-height,
      preserved aspect ratio, fade-in when the BG fetch returns —
      already partly there).

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
