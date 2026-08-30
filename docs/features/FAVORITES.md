# Ferail - Favorites Feature Specification

A complete behavioral spec for the Favorites section of the sidebar. Written to be implemented against directly.

---

## 1. Concept and scope

**Favorites** is a user-curated, ordered list of locations pinned to the sidebar for one-click access. A favorite is a *reference to a location*, a folder, a volume, a saved search, or a tag, not a copy of its contents.

Favorites are distinct from:
- **Locations** (Home, Desktop, Documents, etc.), the OS-standard folders. These are *not* favorites; they're a separate, fixed section. (Note: this also resolves the duplicate-Downloads bug, Locations is the standard set, Favorites is the user's set, and they're visually separated.)
- **The file tree**: the expandable hierarchy. The tree shows structure; Favorites shows shortcuts.
- **Tags**: if Ferail supports colored tags, they may appear in their own section or be favoritable; see §9.

A favorite has these intrinsic properties:

| Property | Type | Notes |
|---|---|---|
| `id` | stable UUID | Never changes, even on rename/move. Used for ordering and dedup. |
| `target` | location reference | Path for folders/volumes; query for saved searches; tag id for tags. |
| `kind` | enum | `Folder` / `Volume` / `SavedSearch` / `Tag` / `Application` |
| `display_name` | string | Defaults to the target's basename; user-overridable (§6). |
| `custom_icon` | optional | User may override the auto-resolved icon (§7). |
| `sort_index` | integer or fractional | Determines order within the section. See §4 for the ordering model. |
| `date_added` | timestamp | For "sort by date added" and for analytics. |

---

## 2. Adding a favorite

A favorite can be created from many entry points. All of them funnel into one internal operation: `add_favorite(target, kind) -> Favorite`.

### 2.1 Entry points

**Drag and drop into the sidebar.** The primary gesture. User drags a folder, from the file list, from the tree, from the breadcrumb, from another Ferail window, or from Finder/desktop, and drops it onto the Favorites section. Drop is accepted anywhere in the section; the insertion point follows the cursor (§4.3). On drop outside a valid insertion zone but still over the section, append to the end.

**Right-click context menu: "Add to Favorites."** Available on any folder row in the file list, any node in the tree, any breadcrumb segment, and on the current-folder header. If the target is *already* a favorite, the menu item reads "Remove from Favorites" instead (it's a toggle). See §5.

**Keyboard shortcut.** With a folder selected in the file list or tree, `Cmd+D` toggles it as a favorite (mirrors Finder's "Add to Sidebar" muscle memory; `Cmd+T` is taken by New Tab). Same toggle semantics as the context menu.

**Menu bar.** `File → Add to Favorites`, bound to the same `ToggleFavoriteForTarget` (Cmd+D) command, operating on the current selection or current folder. This is the discoverable entry point for users who don't know the shortcut. Static wording "Add to Favorites"; on an already-favorited target it removes (toggle).

**Drag onto the Ferail dock icon**: optional, later. Dropping a folder on the dock icon could open it; holding a modifier could favorite it. Low priority.

**"Add current folder" affordance in the section header.** A small `+` button that appears on hover of the Favorites section header, adds the currently-viewed folder. Mirrors the settings-panel pattern.

### 2.2 Add behavior

- New favorite is appended to the **end** of the list by default, unless the entry point specifies an insertion index (drag-drop does; everything else doesn't).
- `display_name` defaults to the target basename. For a volume, the volume name. For a saved search, the search's saved name. For the filesystem root, "Macintosh HD" or the volume name, never an empty string.
- `date_added` = now.
- The add is **immediately persisted** (§10), no save step.
- The add is **animated**: the new row fades/slides in over ~150ms so the user sees where it landed.
- If the target is *already* a favorite, adding again is a no-op: do **not** create a duplicate. Instead, briefly highlight the existing entry (a one-shot pulse of the row background) so the user understands it's already there. This is the dedup rule and it's important: favorites are a set keyed by `target`.

### 2.3 What can and cannot be favorited

| Target | Allowed? | Notes |
|---|---|---|
| Local folder | Yes | The common case. |
| Volume / mounted disk | Yes | Network shares, external drives, disk images. See §8 for unavailability handling. |
| The filesystem root `/` | Yes | |
| Saved search | Yes | Stored as a query, not a path. |
| Tag | Yes (if tags exist) | See §9. |
| Application bundle | Optional | `.app` is technically a folder. Decide: treat as a folder, or disallow. Recommend: allow, opens the bundle contents. |
| A non-folder file | **No** | Favorites are locations. A file is not a location. Reject the drop, show a subtle "Only folders can be added to Favorites" toast. |
| A folder already favorited | No-op | Dedup, see §2.2. |

---

## 3. Removing a favorite

### 3.1 Entry points

- **Right-click on the favorite → "Remove from Favorites."**
- **Drag the favorite out of the sidebar** and release over a non-sidebar area: the row "tears off" and vanishes with a poof-style animation. This mirrors the macOS muscle memory exactly.
- **Keyboard:** favorite selected in the sidebar, press `Delete` / `Backspace`.
- **Toggle from the original location:** right-clicking the *source folder* (in the list/tree) shows "Remove from Favorites" if it's currently favorited (§5).
- **Menu bar:** `File → Remove from Favorites` when the selection is a favorite or is favorited.

### 3.2 Remove behavior

- Removal is **immediate** and **persisted immediately**.
- Removal is **animated**: the row collapses/fades over ~150ms; rows below slide up to close the gap.
- **No confirmation dialog.** Removing a favorite is non-destructive: it does not touch the underlying folder, only the shortcut. A confirmation here would be friction with no safety benefit.
- **Undo is supported.** Removal pushes onto the undo stack; `Cmd+Z` restores the favorite at its previous `sort_index` with its previous `display_name` and `custom_icon`. A toast appears: "Removed '[name]' from Favorites · Undo". The toast's Undo button does the same thing as `Cmd+Z`. Toast auto-dismisses after ~6s.
- Removing a favorite does **not** affect any other favorite, and does **not** affect the same folder's presence in Locations or the tree.

---

## 4. Reordering

### 4.1 The ordering model

Each favorite has a `sort_index`. Use **fractional indexing** (a.k.a. order keys) rather than dense integers: when inserting between two favorites with indices `a` and `b`, assign `(a + b) / 2`. This means a reorder touches *one* row's stored index, not the whole list. On the rare occasion precision runs out, renormalize the whole list to clean integers in a background pass.

The list is always rendered sorted by `sort_index` ascending.

### 4.2 Drag to reorder

- **Grab:** mouse-down on a favorite row and move past a small threshold (~4px) starts a reorder drag. Below threshold, it's a click (§11.1).
- **During drag:**
  - The dragged row lifts, subtle shadow (`elevation-raised`), slight scale (~1.02), reduced opacity (~0.9), and follows the cursor vertically. It's constrained to the Favorites section's vertical axis; horizontal cursor movement is ignored.
  - A clear **insertion indicator** (a 2px accent-colored line) shows where the row will land, snapping between rows.
  - Other rows animate out of the way to open a gap at the insertion point (~120ms ease).
  - Auto-scroll: if the drag reaches the top or bottom edge of a scrollable sidebar, scroll in that direction, speed proportional to how far into the edge zone the cursor is.
- **Drop:**
  - Inside the section → the row takes the indicated `sort_index`, list re-sorts, change persists immediately.
  - Outside the section (over file list, over the window's non-sidebar area, off-window) → this is a **remove** gesture (§3.1), not a reorder. Show the tear-off affordance once the cursor leaves the section bounds so the user knows the semantics changed.
  - Over the **Locations** section → reject. Favorites cannot be reordered into Locations. Show the insertion indicator only within Favorites bounds.
- **Cancel:** `Esc` during a drag aborts: row animates back to its origin, no change.

### 4.3 Drag-to-add and drag-to-reorder share an insertion model

When an *external* folder is dragged into the section (§2.1), the same insertion indicator logic applies: the drop creates a new favorite at the indicated index rather than moving an existing one. The visual language is identical so the user doesn't have to learn two things.

### 4.4 Non-drag reordering (accessibility + power users)

- With a favorite selected in the sidebar: `Cmd+Option+Up` / `Cmd+Option+Down` moves it one position. This is mandatory for keyboard-only and VoiceOver users: drag-only reordering is an accessibility failure.
- Optional: a context-menu submenu "Move → To Top / Up / Down / To Bottom."

### 4.5 Sort options

The list is manually ordered by default. Offer, via the section header's context menu, one-shot sorts that *rewrite* `sort_index` for all entries:
- Sort by Name (A–Z)
- Sort by Date Added (newest/oldest)
- Sort by Kind
After a one-shot sort, the list is still manually reorderable: the sort doesn't "lock" the order, it just sets it.

---

## 5. The "underline if also in Favorites" behavior

This is the bidirectional-awareness requirement and it deserves its own section because it touches multiple views.

**Rule:** anywhere a folder is displayed in Ferail, if that folder's `target` matches an existing favorite, the folder is rendered with a **favorited indicator**.

### 5.1 Where the indicator appears

- File list rows.
- Tree nodes.
- Breadcrumb segments.
- The current-folder header.
- Open/column-view columns, if you have them.

### 5.2 What the indicator is

The brief calls it "underline." I'd recommend against a literal text underline: it collides with hyperlink semantics and with text-selection rendering. Better options, pick one and use it consistently:

- **A small filled dot or star** in the accent color, placed after the name or in the row's trailing margin. Most legible, least ambiguous.
- **A subtle accent-colored left edge** on the row (2px bar), matching how the active selection is shown but in a different color/weight.
- If you do want underline-like treatment: a **short accent underline under the icon**, not the text: distinct from any text styling.

Whatever you pick: it must be distinct from hover state, from selection state, and from focus state. A favorited + selected + focused row must be unambiguously readable as all three.

### 5.3 How it stays in sync

This is the important part. The favorited indicator is **derived state**, not stored per-row. There is one source of truth, the Favorites list, and every view *observes* it.

- Maintain an in-memory `HashSet<Target>` (or a hash map `Target → favorite_id`) derived from the Favorites list. Call it the **favorites index**.
- The index is rebuilt (or incrementally updated) whenever the Favorites list changes: add, remove, or a target-affecting change.
- Every view that renders folders queries the index when rendering a row: `favorites_index.contains(row.target)`.
- In GPUI terms: the Favorites list is an `Entity`. The favorites index lives alongside it (or is recomputed on change). Views that render folder rows observe that entity, so when favorites change, those views re-render and the indicators update **immediately and everywhere**: no manual invalidation, no stale underlines.

### 5.4 The toggle relationship

Because of the index, the context-menu item on a *source folder* is a true toggle:
- Folder not in index → menu shows "Add to Favorites."
- Folder in index → menu shows "Remove from Favorites."
And toggling it updates the index, which updates every visible indicator in the same frame.

---

## 6. Renaming a favorite

A favorite's `display_name` is independent of the underlying folder's name. Renaming the favorite is renaming the *shortcut's label*, not the folder.

- **Trigger:** context menu → "Rename…", or `Enter` with the favorite selected.
- **Behavior:** a small modal text-prompt opens pre-filled with the current name, selected for overtype. `Enter` commits, `Esc` cancels.
  - *Implementation note:* Favorites keep the compact gpui text prompt because
    this non-virtualized sidebar operation edits a shortcut label. Filesystem
    rows use Ferail's shared inline editor instead; both remain cross-platform
    and share the same keyboard semantics.
- **Empty name:** rejected: the commit is a no-op, leaving the previous name (or the folder basename if there was no custom name).
- **"Reset name":** context menu offers "Reset to Original Name," which clears `display_name` back to tracking the folder basename.
- Renaming the favorite does **not** rename the folder on disk. Make this unambiguous: if there's any doubt in testing, add a tooltip or a one-time hint.
- If the underlying folder is later renamed on disk *and* the favorite had no custom name, the favorite's displayed name should follow the folder. If the favorite *had* a custom name, it keeps the custom name regardless.

---

## 7. Icons

- **Default:** the favorite's icon is auto-resolved from its kind and target: a folder icon, a volume icon, the special icons for Home/Downloads/etc. if the target is a known location, a saved-search icon, a tag swatch for tags.
- **Custom icon:** context menu → "Change Icon" lets the user pick from a curated set (Lucide/Isocons, since gpui-component ships them) or assign a color tint. Stored as `custom_icon`.
- **Reset:** "Reset Icon" clears `custom_icon`.
- **Folder-color inheritance:** if Ferail supports colored folders/tags, a favorite may optionally inherit the folder's color. Decide and be consistent.
- Icon size matches the sidebar's other sections, 16×16 at standard density, scaling with the sidebar density setting.

---

## 8. Unavailable and broken favorites

Favorites can point at things that aren't always there: unmounted volumes, ejected drives, network shares that are offline, folders that were deleted or moved.

### 8.1 States

| State | Meaning | Rendering |
|---|---|---|
| `Available` | Target exists and is reachable. | Normal. |
| `Unmounted` | Target is a volume/share that isn't currently mounted. | Dimmed (~50% opacity), with a small "offline" affordance. Still visible. Clicking attempts to mount/connect. |
| `Missing` | Target is a local path that no longer exists (deleted/moved). | Dimmed, with a distinct "broken" affordance (e.g. a small warning glyph). |

### 8.2 Behavior

- **Never silently drop a favorite** because its target went missing. The user pinned it deliberately; a drive being unplugged is not consent to forget it. Keep it, render it as `Unmounted`/`Missing`.
- **Unmounted volume, clicked:** attempt to mount/connect. Show a spinner on the row during the attempt. On success → navigate to it, state → `Available`. On failure → toast with the reason, row stays `Unmounted`.
- **Missing path, clicked:** don't navigate into the void. Show a small dialog/popover: "[name] can't be found. It may have been moved or deleted." with actions: **Locate…** (opens a folder picker; choosing a new path repoints the favorite's `target`, keeping its `id`, `display_name`, `sort_index`), **Remove from Favorites**, **Keep** (dismiss, leave it broken).
- **Re-availability:** when a missing/unmounted target becomes reachable again (drive plugged in, folder recreated at the same path), the state flips back to `Available` automatically. The favorites index and the file-watcher feed this.
- **Detection:** hook the volume mount/unmount notifications and the file-system watcher. Don't poll on a timer if you can avoid it; react to events.

### 8.3 Repointing

"Locate…" (above) is the general repoint mechanism. A favorite's `target` can be reassigned while keeping its identity. This is also useful if a user *moves* a favorited folder and wants the favorite to follow: offer "Locate…" in the normal context menu too, not just the broken-state dialog.

---

## 9. Saved searches and tags as favorites (if applicable)

If Ferail supports saved searches and/or tags, they're favoritable, with these differences:

- **Saved search favorite:** `target` is a stored query, not a path. Clicking runs the search and shows results. It is never `Missing` (a query is always valid) but may return zero results. Icon is a search/magnifier glyph.
- **Tag favorite:** `target` is a tag id. Clicking shows all files with that tag. Icon is the tag's color swatch. If the tag is deleted, the favorite becomes `Missing` and follows §8.
- Both participate in ordering, renaming, the favorites index, and persistence exactly like folder favorites.
- The "underline if also in favorites" rule (§5) applies to saved searches and tags too, anywhere they're listed elsewhere in the UI.

---

## 10. Persistence

- Favorites persist across launches. Store them in Ferail's settings/state store (the same domain crate that owns other persisted state: keep this out of the UI layer).
- **Every mutation persists immediately**: add, remove, reorder, rename, icon change, repoint. No "save favorites" action exists. If the app is force-quit one second after a reorder, the reorder survived.
- Serialized form per favorite: `id`, `kind`, `target` (path or query or tag-id), `display_name` (nullable: null means "track basename"), `custom_icon` (nullable), `sort_index`, `date_added`.
- **Schema versioning:** include a version field on the favorites collection so the format can evolve. On load, migrate older versions forward.
- **Corruption safety:** if the favorites store fails to parse, do not crash and do not wipe it. Load an empty list, log the error, keep the corrupt file aside (`.bak`) so it can be recovered. A user losing their favorites silently is a serious trust failure.
- **Sync (future):** if Ferail ever syncs settings across machines, favorites are a candidate, but path-based favorites don't transfer meaningfully across machines (different home dirs, different volumes). If/when sync happens, treat machine-local path favorites and portable favorites (tags, saved searches) differently. Out of scope for v1; just don't design the storage in a way that blocks it.

---

## 11. Interaction details and edge cases

### 11.1 Click vs drag disambiguation
Mouse-down then release without crossing the ~4px threshold = **click** = navigate to the favorite. Mouse-down then cross the threshold = **drag** (reorder, or remove if it leaves the section). This is the standard threshold model; get it right or every reorder will feel like it eats clicks.

### 11.2 Single click navigates
A single click on a favorite navigates the active pane/tab to that location. Not double-click: favorites are shortcuts, the whole point is one action. (This differs from file rows, which use double-click to open. That's fine; the sidebar and the file list are different contexts with different conventions, and this matches Finder.)

### 11.3 Modifier-clicks
- `Cmd+click`: open the favorite in a **new tab**.
- `Cmd+Option+click` or middle-click: open in a **new window**. (Pick one; be consistent with how the rest of Ferail opens new windows.)
- These mirror whatever the file list does for open-in-new-tab/window, so the modifier vocabulary is consistent app-wide.

### 11.4 Selection and keyboard nav
- The Favorites section participates in sidebar keyboard navigation: arrow keys move focus through Locations, Favorites, and the tree as one navigable list.
- `Enter` on a focused favorite navigates to it. `Space` could trigger Quick Look of the folder, if that's a thing Ferail does.
- The currently-viewed location, if it matches a favorite, shows that favorite in a **selected/active** state (distinct from the §5 favorited indicator and from keyboard focus).

### 11.5 Drag a file *onto* a favorite
Dragging a *file or folder* from the file list and dropping it **onto a favorite row** (not between rows) is a **move/copy into that location**: same semantics as dropping onto a folder in the file list (move within volume, copy across volumes, `Option` forces copy). The insertion-line UI (§4.2) is for dropping *between* rows; dropping *on* a row is a file operation. Distinguish these clearly: between-rows shows the accent line, on-row highlights the whole row as a drop target.

### 11.6 Duplicate display names
Two favorites can have the same `display_name` (e.g. two different folders both named "src", both renamed, whatever). Allowed: they're keyed by `id`/`target`, not name. Don't dedup on name. Consider showing a disambiguating parent-path hint on hover.

### 11.7 Empty state
If the user has zero favorites, the Favorites section shows a quiet empty state: a single muted line like "Drag folders here for quick access." The section header still shows. Don't hide the section entirely: that hides the affordance for ever getting a first favorite.

The empty-state line **is itself the drop target**, not just a caption, with no rows there are no inter-row insertion gaps (§4.3), so without this a folder dragged onto an empty section would have nowhere to land. It renders as a dashed-outline drop well with a faint fill so it reads as "put something here" at rest (not only mid-drag), and `drag_over` deepens the fill; a drop routes through the same validate-off-thread-then-add path as the gaps, inserting the first favorite at a valid mid-slot (`-INF`/`+INF` bounds). Implemented in `favorites_section.rs::add_dropped_folders`, shared with the inter-row gaps.

### 11.8 Section collapse
The Favorites section header is collapsible (disclosure triangle), like Locations and the tree. Collapsed/expanded state persists. Collapsing doesn't affect the favorites themselves or the §5 indicators elsewhere.

### 11.9 Very long lists
If a user has 50 favorites, the section scrolls within the sidebar. Consider a soft cap warning at some large number, but don't hard-cap. If lists get huge, the one-shot sorts (§4.5) and possibly a future search-favorites box become relevant: out of scope for v1.

### 11.10 Reacting to filesystem changes
The file watcher already feeds the tree and list. It also feeds favorites:
- Folder renamed on disk → favorites with no custom name update their displayed name (§6).
- Folder deleted → matching favorite goes `Missing` (§8).
- Folder moved → matching favorite goes `Missing` (it can't know where it went); user can "Locate…" it. (If Ferail can track moves via inode/file-id on the same volume, follow the move automatically and update `target`. Nice-to-have, not required.)
- Volume mounted/unmounted → matching favorites flip availability state.

---

## 12. Component mapping (gpui-component)

For implementation against the library you're already using:

- **Sidebar**: the Favorites section is a section within the Sidebar component.
- **Icon**: favorite icons, state glyphs, the favorited indicator.
- **Menu**: the right-click context menus on favorites, on source folders, on the section header.
- **Tooltip**: hover hints (full path on truncated names, "this renames the shortcut, not the folder" if needed).
- **Input**: the rename text-prompt modal (Ferail renames via the shared modal, not an in-row field; see §6).
- **Notification**: the remove/undo toast, the mount-failure toast, the "only folders" rejection toast.
- **Dialog** / **Popover**: the missing-target "can't be found" prompt.
- **Kbd**: showing shortcuts in menus and tooltips.
- **Spinner**: on a row while a volume mount/connect is in progress.
- GPUI's drag-and-drop and the `Entity` observation model, for reorder, drag-to-add, and the §5.3 live-sync of the favorites index.

---

## 13. Implementation order

Build it in this sequence, each step is usable and testable before the next.

1. **Data model + persistence** (§1, §10). The `Favorite` struct, the collection, immediate-persist, load-with-corruption-safety. No UI yet. Unit-test the store.
2. **Render the section** (§11.7, §11.8). Favorites section in the sidebar, renders a static list, empty state, collapse. Hard-code a favorite or two to see it.
3. **Add and remove** (§2, §3): context menu + menu bar paths first (no drag yet). Immediate persist. Animations.
4. **The favorites index + §5 indicators.** Build the index, wire every folder-rendering view to observe it, add the indicator. This is high-value and touches a lot: do it early so everything after benefits.
5. **Click to navigate** (§11.1, §11.2, §11.3), including the click/drag threshold groundwork.
6. **Drag to reorder** (§4.2, §4.4): fractional indexing, insertion indicator, keyboard reorder.
7. **Drag to add / drag to remove** (§2.1, §3.1): reuses the insertion model and the threshold model.
8. **Rename** (§6) and **custom icons** (§7).
9. **Unavailable/missing handling** (§8): states, click behavior, Locate, re-availability detection.
10. **Drag file onto favorite** (§11.5), **saved searches/tags** (§9), **one-shot sorts** (§4.5), remaining edge cases.
11. **Undo** for remove (§3.2): can slot in earlier if your undo stack already exists.

---

## 14. Acceptance checklist

The feature is done when all of these are true:

- [ ] A folder can be favorited from: drag-in, file-list context menu, tree context menu, breadcrumb context menu, keyboard shortcut, menu bar, section-header `+`.
- [ ] Favoriting an already-favorited folder is a no-op and pulses the existing entry.
- [ ] Non-folder files cannot be favorited; the attempt is rejected with a toast.
- [ ] A favorite can be removed via context menu, drag-out (tear-off), keyboard, and the source-folder toggle.
- [ ] Removal has no confirmation but is undoable via toast and `Cmd+Z`, restoring position/name/icon.
- [ ] Favorites reorder by drag, with a clear insertion indicator and rows animating aside.
- [ ] Favorites reorder by keyboard (`Cmd+Option+Up/Down`).
- [ ] Reorder persistence survives a force-quit one second later.
- [ ] Any folder shown anywhere in the app that is also a favorite displays the favorited indicator.
- [ ] Toggling a favorite updates every visible indicator in the same frame.
- [ ] A favorite's display name can be changed without renaming the folder; it can be reset.
- [ ] A favorite with no custom name follows the folder's on-disk renames.
- [ ] Custom icons can be set and reset.
- [ ] An unmounted-volume favorite renders dimmed and attempts to mount on click.
- [ ] A missing-path favorite renders broken, is never silently dropped, and offers Locate/Remove/Keep.
- [ ] A missing target that returns flips back to available automatically.
- [ ] Single click navigates; `Cmd`-click opens a new tab; the new-window modifier works.
- [ ] Dropping a file *onto* a favorite moves/copies it there; dropping *between* favorites reorders.
- [ ] Click vs drag is correctly disambiguated by threshold: reorders don't eat clicks.
- [ ] The section shows a useful empty state at zero favorites.
- [ ] Favorites persist across launches; a corrupt store doesn't crash or silently wipe.
- [ ] Every favorites mutation persists immediately with no save action.
- [ ] All favorites interactions are reachable without a mouse (VoiceOver / keyboard).
