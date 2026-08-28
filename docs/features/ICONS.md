# Icons

The complete reference for every icon the app draws: where it comes from,
who it's attributed to, and which command or surface uses it. **When you add,
move, or repurpose an icon, update this file in the same change** (see
[Adding a new icon](#adding-a-new-icon)). This is the file that lets us catch
glyphs that are too weak — or too reused — for what they're meant to mean.

← Back to [feature notes](README.md) · [Architecture](../ARCHITECTURE.md)

Audited against the Rust sources on 2026-06-20.

## Sources at a glance

The app pulls icons from three places, resolved through one composite
`AssetSource` ([assets.rs](../../crates/ferail-gpui/src/assets.rs)):

| Source | What it covers | Format | Resolves via |
| --- | --- | --- | --- |
| **macOS NSWorkspace** | Folder + volume artwork, custom Finder folder icons | Raster (RGBA→BGRA) | [`IconCache`](../../crates/ferail-gpui/src/icons.rs) → [`fetch_icon_rgba`](../../crates/ferail-fs-native/src/icons.rs) |
| **Local SVG bundle** | File-type glyphs, sidebar Locations, most toolbar chrome | SVG (Lucide-derived, stroke 1.75) | [`LocalAssets`](../../crates/ferail-gpui/src/assets.rs) — `resources/icons/**` |
| **Upstream `gpui-component-assets`** | Generic UI chrome (close, search, chevrons, etc.) | SVG (Lucide, stroke 2) | `gpui_component_assets::Assets` fallback |

`FeraAssets::load` tries the local bundle first, then falls back to upstream, so
`svg().path("icons/X.svg")` resolves transparently from whichever bundle has the
file. **Consequence worth remembering:** a path like `icons/folder.svg` (no
`nav/` or `file/` prefix) has no local file, so it silently resolves to the
*upstream* Lucide copy — see [Known gaps](#known-gaps--weak-icons).

## Attribution & licensing

- **[Lucide](https://lucide.dev)** — ISC License. The origin set for both our
  local SVGs and the upstream bundle. Our local copies are Lucide glyphs
  re-saved at `stroke-width="1.75"` (Lucide ships them at `2`); a few are
  in-house glyphs drawn in the same style (listed below).
- **`gpui-component-assets`** (longbridge/gpui-component, pinned rev
  `c112e7b`) — Apache-2.0. Its `crates/assets/assets/icons/*.svg` are
  Lucide-derived (`class="lucide lucide-…"`, stroke 2). 99 icons; we reference
  ~28 of them.
- **Apple system icons** via `NSWorkspace iconForFile:` — Apple artwork,
  fetched at runtime, never redistributed. Folders/volumes only.
- **[Bootstrap Icons](https://icons.getbootstrap.com)** — MIT License. Two
  glyphs, a matched pair for the iCloud badge's two states: `nav/cloud-fill.svg`
  (solid `cloud-fill`) = downloaded, `nav/cloud.svg` (outline `cloud`) =
  not-downloaded placeholder. The pair is intentionally one family so fill vs.
  outline reads as the state axis. Native `0 0 16 16` viewBox +
  `fill="currentColor"` kept so they scale and inherit the icon colour.

If we ever ship a `LICENSE`/attribution bundle, Lucide's ISC notice covers both
SVG bundles; Apache-2.0 covers the upstream crate.

## Spare upstream icons (check here before vendoring)

The upstream `gpui-component-assets` bundle ships **99 Lucide glyphs**; we use
~34. The other ~65 are **already compiled into the binary** — reference any with
`icons/<name>.svg` at zero cost (no new file, no normalization), they just render
at Lucide's heavier stroke `2`. **When you need a new icon, look here first**: a
spare-pool glyph is free; a local copy is only worth it for primary chrome that
needs the 1.75 weight, or for a glyph the pool lacks.

Snapshot at rev `c112e7b` — re-run the listing below if you bump gpui-component:

- **Arrows / chevrons** — `arrow-down` `arrow-left` `arrow-right` `chevron-down` `chevron-up`
- **Stars / reactions** — `star` `star-fill` `star-off` `heart` `heart-off` `thumbs-up` `thumbs-down`
- **Files / storage** — `file` `folder-closed` `hard-drive` `gallery-vertical-end`
- **Windows / panels** — `panel-left` `panel-bottom` `panel-bottom-open` `window-close` `window-maximize` `window-minimize` `window-restore` `frame` `resize-corner`
- **Theme / view** — `sun` `moon` `eye` `eye-off` `layout-dashboard` `chart-pie`
- **Text / search controls** — `a-large-small` `case-sensitive` `asterisk` `dash` `check` `replace`
- **System / hardware** — `memory-stick` `square-terminal` `bot` `inspector` `loader` `loader-circle` `battery` (+ `-charging` `-full` `-medium` `-low` `-warning`) (`cpu` now used by the Settings → Performance page; `network` now used for network-mount volume rows)
- **People / misc** — `user` `circle-user` `delete` `bell` `calendar` `book-open` `building-2` `globe` `github` `map` `menu` `ellipsis-vertical` `undo` `redo-2` (`undo-2` now used by the toolbar's Reset-icon-size button)

```sh
# Regenerate after a gpui-component bump:
ls "$(find ~/.cargo/git/checkouts -type d -path '*gpui-component*/crates/assets/assets/icons' | tail -1)"/*.svg
```

There is **no `keyboard` glyph in the pool** — that one has to be vendored
locally (see gaps below).

## House style (all local SVGs)

Match this exactly for any new local glyph:

- `viewBox="0 0 24 24"`, no `width`/`height` attributes.
- `fill="none" stroke="currentColor"` — single-color so `text_color(...)` tints
  it from the active theme. (Filled glyphs use `fill="currentColor"
  stroke="none"` — only `star.svg` does this today.)
- `stroke-width="1.75"`, `stroke-linecap="round"`, `stroke-linejoin="round"`.
- One minified line. Strip Lucide's `class="lucide …"` and `width/height`.
- No hard-coded colors — every tint rides a theme token (see `tint_color`).

> Stroke-weight caveat: icons that resolve from **upstream** render at Lucide's
> stroke `2`, slightly heavier than our local `1.75`. Some toolbars still mix
> both (e.g. Sort/±/ellipsis are upstream-2 beside local-1.75 nav glyphs) — full
> uniformity would mean vendoring every upstream chrome glyph locally, which we
> haven't done. When a local glyph sits inside an **all-upstream cluster** —
> e.g. the settings
> rail, where every other icon is upstream-2 — match the cluster at stroke 2
> rather than the 1.75 default, so the odd-one-out is weight-consistent with its
> neighbors. `keyboard.svg` does this.

## Platform neutrality

Ferail ships on **macOS, Windows, and Linux from one icon set**. A glyph should
read the same on all three — **avoid OS-specific metaphors** for generic
commands:

- No `command`/⌘, no Windows logo, no Apple/Finder-specific chrome as a generic
  glyph. Pick a universal one (`keyboard`, *not* ⌘, for "Keyboard Shortcuts" —
  the page exists on every platform).
- A platform-flavored glyph is only OK when the control itself is already
  `#[cfg]`-gated to that OS (e.g. the macOS-only Show Desktop button).
- Lucide's `folder` / `hard-drive` / `file-*` metaphors are neutral by design —
  prefer them in painted UI over OS-native artwork. The NSWorkspace raster path
  is the one deliberate macOS exception, and it covers folders/volumes only.

## File-type row icons (file list + grid)

Classified by [`file_type_icon`](../../crates/ferail-gpui/src/icons.rs) — pure,
no I/O. Extension and magic string pick a `FileTypeTint`; the tint picks a
default glyph (some extensions override the glyph but keep the tint). Color comes
from `tint_color` against the theme's chart palette.

| Asset | Origin | Tint → theme token | Triggered by |
| --- | --- | --- | --- |
| `file/generic.svg` | Lucide `file` | Unknown → `muted_foreground` | Fallback / unknown |
| `file/text.svg` | Lucide `file-text` | Document → `chart_4` | txt, md, doc(x), rtf, odt, epub, … |
| `file/code.svg` | Lucide `file-code` | Code → `chart_5` | rs, py, js, ts, json, toml, sh, … |
| `file/image.svg` | Lucide `file-image` | Image → `chart_1` | png, jpg, gif, webp, heic, svg, … |
| `file/video.svg` | Lucide `file-video` | Video → `chart_2` | mp4, mov, mkv, webm, … |
| `file/audio.svg` | Lucide `file-audio` | Audio → `chart_3` | mp3, wav, flac, m4a, … |
| `file/archive.svg` | Lucide `file-archive` | Archive → `muted_foreground` | zip, tar, gz, 7z, rar, zst, … |
| `file/disk.svg` | Lucide `disc` | Disk → `info` | dmg, iso, img, vmdk, … |
| `file/app.svg` | In-house (app tile) | Executable → `danger` | app, exe, dll, so, dylib, pkg, … |
| `file/symlink.svg` | Lucide `file-symlink` | Symlink → `info` | symlink kind |
| `file/pdf.svg` | In-house (`file` + "PDF" text) | Document → `chart_4` | `.pdf` (glyph override) |
| `file/html.svg` | In-house (`file` + globe) | Code → `chart_5` | `.html` / `.htm` (glyph override) |
| `file/spreadsheet.svg` | Lucide `file-spreadsheet` | Document → `chart_4` | csv, tsv, xls(x), ods, numbers (override) |
| *(folder)* `icons/folder.svg` | **Upstream** Lucide `folder` | Folder → `primary` | directory kind; also directory rows in the archive workbench (`archive.rs`) |

> Directories render the **upstream** `folder.svg` here, *not* the local
> `nav/folder.svg` the New Folder button uses — two near-identical folders at
> different stroke weights. The archive workbench's directory rows reuse the
> same directory-kind `folder.svg` (a type indicator, not a command glyph).

## Sidebar Locations (well-known folders)

Mapped in [`well_known_locations`](../../crates/ferail-fs-native/src/paths.rs).
All local, all Lucide-derived.

| Location | Asset | Lucide origin |
| --- | --- | --- |
| Home | `nav/home.svg` | `house` |
| Applications | `nav/apps.svg` | Lucide `layout-dashboard` |
| User Applications | `nav/apps.svg` | Lucide `layout-dashboard` |
| Desktop | `nav/desktop.svg` | `monitor` |
| Documents | `nav/documents.svg` | `file-text` |
| Downloads | `nav/downloads.svg` | `download` |
| Trash | `nav/trash.svg` | `trash-2` |
| Movies / Videos | `nav/movies.svg` | `film` |
| Music | `nav/music.svg` | `music` |
| Pictures | `nav/pictures.svg` | `image` |

> The two Applications rows (`/Applications` and, when it holds anything,
> `~/Applications`) share `nav/apps.svg` on purpose: same *kind* of folder, told
> apart by the label. This is a location glyph, not a command glyph, so the
> ~1:1 command→icon rule doesn't apply.

Mounted **volumes** in the tree use `nav/drive.svg` (Lucide `hard-drive`) —
[tree.rs](../../crates/ferail-gpui/src/tree.rs). **Network mounts**
(`is_local == false`) instead draw `network.svg` (Lucide `network`, from the
spare upstream pool) so a remote share reads differently from a local disk
(`TreeRowIcon::Network`). Removable/external volume rows (`is_removable`, i.e.
the boot disk never) also draw a **trailing** `nav/eject.svg` button (Lucide
`eject`, local 1.75): clicking it unmounts the drive (`Shell::eject_path`)
without opening the context menu, matching Finder.

The **iCloud Drive** sidebar Location (surfaced only when the ubiquity
container exists) uses the same `nav/cloud.svg` outline glyph as the leading
icon — the iCloud metaphor, distinct from the trailing cloud *badge* below,
which is a per-location download-state indicator, not a command.

iCloud Locations draw a **trailing** cloud badge at `icon_px(14)` in
`sidebar_foreground` (matching the black Locations leading icons, not muted
grey) — e.g. Desktop / Documents under "Desktop & Documents Folders"
(render.rs `build_locations_menu`). The glyph encodes Finder's
downloaded-vs-evicted distinction: **solid `nav/cloud-fill.svg`** = downloaded
locally, **outline `nav/cloud.svg`** = a not-downloaded placeholder ("set up for
cloud but not enabled"). State is computed off-thread by
`ferail_fs_native::cloud_state` — `path_is_cloud_synced`
(`~/Library/Mobile Documents/` prefix **or** `NSURLIsUbiquitousItemKey`) gates
membership, then the `SF_DATALESS` stat flag picks downloaded vs. placeholder
(both read via `lstat`, never materializing the file) — and cached in
`ProcessState::cloud_locations` (`PathBuf → CloudState`), so render never
touches the filesystem. When an entry is both synced and a Favorite, the cloud
sits left of the trailing star.

## Toolbar / chrome / commands

Every command-bound icon, the asset it draws, and where it lives. `↑upstream`
marks paths that resolve from `gpui-component-assets`; everything else is local.

| Command / element | Icon path | Origin | Where |
| --- | --- | --- | --- |
| Sidebar toggle | *(SidebarToggleButton)* ↑ `panel-left-open/close` | Lucide | render.rs:1442 |
| Back | `icons/nav/chevron-left.svg` | Lucide `chevron-left` (local 1.75) | render.rs:1464 |
| Forward | `icons/nav/chevron-right.svg` | Lucide `chevron-right` (local 1.75) | render.rs:1476 |
| Scroll tabs left / right | `icons/nav/chevrons-left.svg` / `chevrons-right.svg` | In-house, Lucide `chevrons-left` / `-right` (local 1.75) | render.rs `tabstrip` |
| New tab | `icons/nav/plus.svg` | In-house Lucide `plus` (local 1.75; matches the sidebar family — distinct from the upstream stroke-2 `icons/plus.svg` used by the zoom controls) | render.rs `tabstrip` |
| Add favorite (Favorites section +) | `icons/nav/plus.svg` ↑ | In-house Lucide `plus` (local 1.75) | favorites_section.rs header |
| Sort (asc/desc) | `icons/sort-ascending.svg` / `sort-descending.svg` ↑ | Lucide | render.rs:1407 |
| Show Desktop | `icons/nav/show-desktop.svg` | In-house (corner-arrows) | render.rs:1562 |
| New Folder | `icons/nav/folder.svg` | Lucide `folder` (1.75) | render.rs:1575 |
| Refresh | `icons/nav/refresh.svg` | Lucide `refresh-cw` | render.rs:1588 |
| Dock window to a screen edge (toolbar button) | `icons/dock.svg` | **In-house** (house style, stroke 1.75) — a screen rect with a grip bar on each side (edge-neutral "dockable to a side"). Drawn distinct from the sidebar toggle's `panel-left-*` so the whole-window dock isn't confused with the sidebar (docs/features/DOCK.md). | render.rs `title_bar` |
| → Dock Left / Dock Right (menu items) | `icons/dock-left.svg` / `icons/dock-right.svg` | **In-house** (house style) — the screen rect with a docked drawer column + grip on the respective side. Same family as `dock.svg`. | dock dropdown |
| → Undock (menu item) | `icons/undock.svg` | **In-house** (house style) — a plain window (rect + titlebar line), i.e. "back to a free-floating window". | dock dropdown |
| List view | `icons/view-list.svg` | Lucide `list` | render.rs:1604 |
| Icon view | `icons/view-grid.svg` | Lucide `layout-grid` | render.rs:1618 |
| Flat view / include subfolders | `icons/folder-tree.svg` | In-house (folder feeding a two-branch tree, stroke 1.75) | render.rs `title_bar` |
| Edit breadcrumb path | `icons/path-edit.svg` | **In-house** text cursor inside an input outline (house style, stroke 1.75), distinct from rename and text-size commands | breadcrumb row tail |
| Private Mode | `icons/privacy.svg` | **In-house** shield-and-eye (house style, stroke 1.75), distinct from the lock-holder and show/hide glyphs; the existing title-bar button highlights in place while active | render.rs title bar (sole visible Private Mode affordance) |
| Verify checksum manifest | `icons/check.svg` ↑ | Lucide `check` | `VerifyView` report header |
| Smaller / larger icons | `icons/minus.svg` / `plus.svg` ↑ | Lucide | render.rs `title_bar` |
| Reset icon size | `icons/undo-2.svg` | Lucide `undo-2` (from the spare upstream pool, stroke 2 — it sits in the all-upstream ±/ellipsis cluster, so the heavier weight is the consistent one here). A revert arrow, not a refresh circle: `nav/refresh.svg` is the Refresh *command* two buttons away and the two must not read alike. Grid view only, beside the −/＋ stepper it undoes. | render.rs `title_bar` |
| Overflow menu | `icons/ellipsis.svg` ↑ | Lucide | render.rs:1668 |
| Column sort header | `IconName::SortAscending` / `SortDescending` / `ChevronsUpDown` ↑ | Lucide | multi_table/state.rs:1509 |
| Empty table state | `IconName::Inbox` ↑ / `icons/inbox.svg` ↑ | Lucide | multi_table/delegate.rs:137, file_list.rs:1262 |
| Format disguise (danger) | `icons/triangle-alert.svg` ↑ | Lucide | file_list.rs `render_td` "format" |
| Format benign-mismatch cue | `icons/circle-help.svg` | **In-house** Lucide `circle-help` (house style; spare pool has `circle-check`/`circle-x`/`circle-user` but no neutral `?`-in-circle). Muted, non-danger. | file_list.rs `render_td` "format" |
| Filter-syntax help — the (?) beside the filter field | `icons/circle-help.svg` | Same in-house `circle-help` as the format cue above. Deliberate share: both mean "explain this", and neither is a command glyph — the 1:1 command→icon rule is about commands. Revisit if the filter grows a chips UI. | render.rs `title_bar`, filter_help.rs |
| Tool-result pop-out | `icons/maximize.svg` ↑ | Lucide | render.rs:2458 |
| Tool-result close | `icons/close.svg` ↑ | Lucide | render.rs:2468 |
| Task-panel dismiss | `icons/close.svg` ↑ | Lucide | task_panel.rs:101 |
| Tab close | `icons/close.svg` ↑ | Lucide — shared "close" chrome glyph (replaced a literal `"x"` text char) | render.rs `tabstrip` |
| Filter clear (✕ inside the filter field) | `IconName::Close` ↑ | Lucide — drawn by gpui-component's own `clear_button`, which `Input::cleanable(true)` turns on; we don't pass a glyph. Same "close" chrome family as the row above. | render.rs `title_bar` |

### Preview-pane actions ([render.rs](../../crates/ferail-gpui/src/shell/render.rs))

| Command | Icon path | Origin |
| --- | --- | --- |
| Open | `icons/external-link.svg` ↑ | Lucide |
| Reveal in Finder | `icons/folder-open.svg` ↑ | Lucide |
| Copy Path | `icons/copy.svg` ↑ | Lucide |

The preview pane also draws a plain-div **resize grip** (rounded pill) under
the thumbnail — pure chrome, not an icon asset.

### Archive workbench ([archive.rs](../../crates/ferail-gpui/src/archive.rs) + the Name cell in [file_list.rs](../../crates/ferail-gpui/src/file_list.rs))

The workbench renders through the normal file table, so it inherits that
table's icon set. The one addition is the tree disclosure caret on folder rows.

| Surface | Icon path | Origin |
| --- | --- | --- |
| Tree caret (collapsed / expanded) | `icons/chevron-right.svg` / `icons/chevron-down.svg` ↑ | **Upstream** Lucide |
| Directory rows | `icons/folder.svg` ↑ | Upstream Lucide (the directory-*kind* glyph, shared with the file list) |

> The caret deliberately uses the **upstream** chevrons, not the local
> `nav/chevron-*.svg` pair: those are the Back / Forward *command* glyphs, and
> the command→icon mapping stays ~1:1. A disclosure caret is a state
> affordance, not a command.

### Viewer window ([viewer/window.rs](../../crates/ferail-gpui/src/viewer/window.rs))

| Command | Icon path | Origin |
| --- | --- | --- |
| Prev / Next | `icons/chevron-left.svg` / `chevron-right.svg` ↑ | Lucide (stays upstream-2 to match this all-upstream toolbar) |
| Play / Pause (slideshow + video) | `icons/play.svg` / `pause.svg` ↑ | Lucide |
| Zoom out / in | `icons/minus.svg` / `plus.svg` ↑ | Lucide |
| Rotate | `icons/redo.svg` ↑ | Lucide |
| Color adjust | `icons/palette.svg` ↑ | Lucide |
| Auto-enhance ("magic") | `icons/wand-sparkles.svg` | **In-house** Lucide `wand-sparkles` (house style, stroke 1.75). Spare pool has no wand/sparkles glyph. Adjustments-panel header, beside Reset. |
| Move to Trash | `icons/trash.svg` | **In-house** Lucide `trash` (house style, stroke 1.75). Neither pool has a trash glyph. Deliberately the plain can — the sidebar Trash *place* uses `nav/trash.svg` (`trash-2`, with inner lines), so the command and the location stay distinguishable. |
| Video mute toggle | `icons/volume-x.svg` (muted) / `icons/volume-2.svg` (audible) | **In-house** Lucide `volume-x` / `volume-2` (house style, stroke 1.75). Spare pool has no speaker/volume glyph. One button, state-swapped icon — video audio is muted by default, opt-in per window. |
| Overflow menu (narrow window) | `icons/ellipsis.svg` ↑ | Lucide — same "More" affordance as the shell toolbar's overflow menu (render.rs:1668), deliberately shared: one glyph for the one concept. Appears only when the width-tiered toolbar has folded clusters away. |
| Fullscreen | `icons/maximize.svg` ↑ | Lucide |

### Disk Usage window ([disk_usage.rs](../../crates/ferail-gpui/src/disk_usage.rs))

| Command | Icon path | Origin |
| --- | --- | --- |
| Cancel scan | `icons/close.svg` ↑ | Lucide |
| Refresh / restart scan | `icons/nav/refresh.svg` | Lucide `refresh-cw` |
| Re-run checksum verification | `icons/nav/refresh.svg` | Same refresh command language as scans; icon-only with tooltip. |
| Filter checksum report to problems | `icons/eye-off.svg` / `icons/eye.svg` | Hide successful rows / restore every row. |
| Select problematic checksum target | `icons/folder-open.svg` | Opens the containing folder in Ferail and selects the existing target. |
| Dock in tab | `icons/minimize.svg` ↑ | Lucide `minimize` (inverse of pop-out's `maximize`) |
| Zoom out (up a level) | `icons/arrow-up.svg` ↑ | Lucide |
| Show/hide largest-files panel | `icons/panel-right-open.svg` / `panel-right-close.svg` ↑ | Lucide |
| Scan packages toggle | `icons/nav/package.svg` | Lucide `package` |

### Settings pages ([settings.rs](../../crates/ferail-gpui/src/settings.rs))

| Page | Icon path | Origin |
| --- | --- | --- |
| Search & Duplicates | `icons/search.svg` ↑ | Lucide |
| Appearance | `icons/palette.svg` ↑ | Lucide |
| Files | `icons/folder.svg` ↑ | Lucide |
| Performance | `icons/cpu.svg` ↑ | Lucide (from the spare upstream pool) |
| Layout | `icons/settings-2.svg` ↑ | Lucide |
| Plugins | `icons/settings.svg` ↑ | Lucide |
| Keyboard Shortcuts | `icons/keyboard.svg` | **In-house** Lucide `keyboard` (stroke 2, matches rail) |
| Diagnostics | `icons/activity.svg` | **In-house** Lucide `activity` (heartbeat line; stroke 2, matches rail). Spare pool lacked a health/diagnostic glyph (only `circle-check`/`heart`, both taken). |
| About | `icons/info.svg` ↑ | Lucide |
| (in-page checkmark) | `icons/circle-check.svg` ↑ | Lucide |

## The favorite star

`nav/star.svg` (in-house filled star) is the app's "favorited" marker, drawn
identically as a **trailing** indicator across surfaces for visual consistency:

- File-list favorited directory — file_list.rs:854
- Tree favorited row — tree.rs:586
- Breadcrumb favorited segment — render.rs:2380
- Locations favorited entry — render.rs:390
- Grid-slot favorited badge — render.rs:928
- Favorites section: the Available-state trailing indicator — favorites_section.rs:485

The **leading** icon of a Favorites row reflects the *target type* (fixed — see
gaps #6): saved-search → `nav/search.svg`, tag → `nav/tag.svg`, plain path → its
NSWorkspace folder bitmap, custom pick → the chosen glyph. The star is no longer
the catch-all leading glyph.

## Favorite icon picker

Right-click a Favorites row → **Change Icon…** opens the icon-picker window
([favorite_icon_picker.rs](../../crates/ferail-gpui/src/favorite_icon_picker.rs)):
a scrollable grid of the **flat `icons/<name>.svg` library** (the upstream Lucide
set plus our top-level adds — ~102 glyphs), enumerated live from the asset
bundle. Picking one stores `FavoriteIcon::Lucide(name)` on the favorite (resolved
as `icons/<name>.svg`); **Reset Icon** clears back to the kind+target default.

This replaced an emoji-prefixed submenu of six curated picks — the emoji clashed
with the line-icon language and the picks didn't actually apply reliably. The
`FavoriteIcon::TintedFolder` variant (a folder-accent-color icon never wired to
any UI) was removed at the same time; legacy `tint:` DB rows degrade to the
default icon. Render `--icon-picker` via the screenshot CLI to capture the grid.

## Known gaps & weak icons

All nine items surfaced by the original audit have been **resolved** (verified by
screenshot where visual). Kept here as a record of what changed and why; file new
issues in [TODO.md](../../TODO.md).

1. ✅ **`keyboard.svg` missing** (blank Settings "Keyboard Shortcuts" icon).
   Vendored a local `keyboard.svg` (Lucide `keyboard`, stroke 2 to match the
   all-upstream settings rail). settings.rs already pointed at `icons/keyboard.svg`,
   so the file alone resolved it — no code change. Chose `keyboard` over
   `command`/⌘ for [platform neutrality](#platform-neutrality).
2. ✅ **Orphaned `nav/chevron-down.svg` / `nav/chevron-right.svg`.** Deleted
   `chevron-down` (no consumer); added a local `nav/chevron-left.svg` and set
   `nav/chevron-right.svg` to stroke 1.75; repointed shell Back/Forward
   (render.rs:1464/1476) at the local copies. *(Viewer Prev/Next deliberately
   stay on the upstream chevrons — that toolbar is all upstream-2; see #9.)*
3. ✅ **Disk vs. drive collision.** `file/disk.svg` is now Lucide `disc`
   (optical-disc), visibly distinct from the `hard-drive` glyph volumes use.
4. ✅ **`view-list` reused as "Dock in tab".** Disk Usage dock button now uses
   `icons/minimize.svg` — the inverse of the pop-out button's `maximize.svg`.
5. ✅ **`apps` vs `view-grid` look-alike.** `nav/apps.svg` is now Lucide
   `layout-dashboard` (asymmetric tiles), clearly not the uniform view-grid.
6. ✅ **Star overload.** Favorites leading icons now resolve by target type
   (search / tag / folder); see [the favorite star](#the-favorite-star). The
   star remains only the plain-path and trailing "favorited" marker.
7. ✅ **`triangle-alert` reused for two states.** *Missing* keeps the warning
   triangle; *Unmounted* now uses `icons/circle-x.svg` ("disconnected"). A
   dedicated `nav/eject.svg` now exists, but for the *action* of unmounting a
   live removable volume (trailing button on Volumes rows) — distinct from
   circle-x, which still marks a Favorite whose volume is gone.
8. ✅ **PDF `<text>` render check.** Verified by screenshot that GPUI's SVG stack
   (usvg/resvg) **does** render the `<text>` "PDF" label crisply at row scale —
   no change needed. The other in-house glyphs (`show-desktop`, `apps`, `app`,
   `html`) still warrant a manual eye when the icon language evolves.
9. ✅ **Mixed toolbar stroke weights.** Shell Back/Forward unified to local 1.75
   (folded into #2). Residual, by design: Sort / ± / ellipsis and the
   gpui-component sidebar toggle remain upstream-2 (full uniformity would require
   vendoring every upstream chrome glyph locally — deferred, not a defect).

## Adding a new icon

Follow this so we don't end up with two glyphs for one idea, or one glyph
overloaded across unrelated commands:

1. **Check this file first.** Is the concept already drawn? If a suitable glyph
   exists, reuse it *only* if the meaning is genuinely the same. **Never reuse
   an existing command's glyph for a different command** — the command→icon
   tables above are meant to stay close to 1:1. If two commands would share a
   glyph, draw a distinct one.
2. **Check the [spare pool](#spare-upstream-icons-check-here-before-vendoring)
   next.** If a suitable Lucide glyph already ships upstream, just reference
   `icons/<name>.svg` — no new file. It renders at stroke 2; fine for incidental
   chrome. Skip to step 5.
3. **Only vendor a local copy** when the pool lacks the glyph, or it's primary
   chrome where the heavier stroke would clash with its 1.75 neighbors. Pull the
   closest glyph from [lucide.dev](https://lucide.dev), **normalize to
   [house style](#house-style-all-local-svgs)**, and save under the right folder:
   `resources/icons/file/` (file-type), `resources/icons/nav/` (sidebar/toolbar),
   or `resources/icons/` root (view modes). Reference it as
   `icons/<folder>/<name>.svg`.
5. **Update this file:** add the row (asset, origin/attribution, command, tint),
   and if it's a new command add the command→icon mapping. If the glyph is
   in-house, say so. If you introduced a new *source*, update
   [Attribution](#attribution--licensing).
6. **Render and eyeball it.** Screenshot the surface (`--screenshot …` to
   `screenshots/`) and check weight + legibility against its neighbors, then note
   anything weak under [Known gaps](#known-gaps--weak-icons).
