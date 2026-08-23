# Ant Trail

Ant Trail is Ferail-Win32's folder-usage heat system. It tracks where the user
actually goes and uses that signal to make navigation feel smarter.

## Status

Shipped with follow-ups.

Ferail currently has:

- `AntTrail` logic in `ferail-core`.
- Process-wide path-based visit counts in `ProcessState`.
- SQLite persistence in `MetadataDb.folder_usage`.
- Startup hydration of visits and recents from the metadata DB.
- Navigation-commit recording with async DB write-through.
- Log-scaled heat normalization.
- Heat tinting in the file list/grid for visited directories, with a master
  on/off switch and a customizable base color (Settings → Appearance → Ant
  Trail).
- Recents sidebar section backed by the same visit log, with its own master
  on/off switch (Settings → Appearance → Recents).
- Remove-from-Recents and Clear-Recents actions. Recency (`last_access_unix`)
  and heat (`hits`) are independent columns of the same `folder_usage` row, so
  these clear *only* the recency and keep the heat tint — taking folders off the
  recent list doesn't erase how often you visit them. Clear Recents is a
  catalogued command (Go menu / ⌘K palette) and confirms first, since it can't
  be undone.
- A default-on "Don't track favorites" option that skips visit recording when a
  folder is reached via its favorite (see [Customization](#customization)).
- **Sort by Ant Trail** in the toolbar's sort menu (and the ⌘K palette, as
  *Sort by Ant Trail*), ranking the current folder's subfolders by heat — see
  [Sorting](#sorting).

The remaining work is prediction: using the heat signal to prewarm likely next
folders and adding time decay/recency weighting once the data proves useful.

## Rules

- Ant Trail records committed navigation, not mere hover.
- Expanding/collapsing a tree node is not a visit.
- Navigating *via a favorite* is, by default, not a visit — a favorite is a
  deliberate shortcut, not organic browsing. Reaching the same folder any other
  way still records. This is route-based, decided at the favorite-activation
  site, and can be turned off (see [Customization](#customization)).
- Reading heat for paint must be cheap and in-memory.
- Persistence, decay, and prediction run outside paint.

## Data Flow

On startup:

1. Open `MetadataDb`.
2. Load `folder_usage` rows ordered by hits.
3. Build `ProcessState::ant_visits` and `ant_max`.
4. Build `ProcessState::recents` from the same rows ordered by last access.
5. Refresh active-tab heat tints once hydration completes.

On navigation commit:

0. If the navigation came from a favorite and the "Don't track favorites"
   setting is on (the default), stop here — no visit is recorded.
   `Shell::navigate_from_favorite` gates the recording; every other route
   falls through `Shell::navigate` and always records.
1. Resolve the committed `NodeId` to a path at an action/job boundary.
2. Increment the in-memory hit count.
3. Promote the path to the front of Recents.
4. Update the `NodeStore` heat cache.
5. Spawn a background `record_folder_visit` write to SQLite.

On render:

- File rows read precomputed heat from the table delegate.
- Tree/sidebar surfaces read in-memory recents/heat state.
- No SQLite or filesystem work happens from paint.

## Sorting

`SortColumn::AntTrail` orders the listing by heat, hottest first on the first
pick (re-picking flips it, like every other sort column). It is the sort menu's
only entry with no column header behind it — the other four duplicate a header
click, this one is the reason to open the menu.

Heat is not a `FileEntry` field, so the ordering can't ride `sort_in_place` like
the other columns: `FileListDelegate::sort_model` runs `sort_by_heat` against
the delegate's row-parallel `heats` vector — the same cached values the tint
paints from, so the sort costs no extra lookups and no I/O. Folders lead, as in
every other column, which here doubles as "the rows this ordering is about,
first"; files and never-visited folders fall below in name order, so a cold
directory still reads like a normal listing.

Two consequences worth knowing:

- When hydration finishes (or a visit lands) and `refresh_active_tab_heats`
  replaces the heats, an active Ant Trail sort re-ranks — the heat *is* the sort
  key, so new heat means a new order, not just a new tint.
- Include Subfolders keeps `heats` empty on purpose, so the menu entry is
  hidden there and a carried-over Ant Trail sort falls back to Name ascending.
  That fallback is decided by `resolve_ant_sort` from **the rows in hand**, not
  from which surface the delegate thinks it is. The distinction is not
  academic: `replace_entries` used to leave `flat_paths` set after Include
  Subfolders closed, so an ordinary listing — heat and all — took the flat sort
  path and the pick did nothing until the user changed folder. Both halves were
  fixed; key ordering off the data and a surface flag can never disagree with
  the rows again.

The `--sort ant[-desc]` screenshot flag drives the same path.

## Heat Model

Heat is normalized to `0.0..=1.0` against the most-visited folder and log-scaled:

```text
log2(visits + 1) / log2(max_visits + 1)
```

That keeps one heavily used folder from washing out every other folder.

## Customization

Both knobs live in **Settings → Appearance → Ant Trail** and are backed by
[`crate::ant_trail`] (process-wide `gpui::Global`s, the same live-update pattern
as the selection accent):

- **Show Ant Trail** (default on) — the master switch, `ant_trail_enabled` in
  `app_state`, surfaced live as `ant_trail::AntTrailEnabled`. When off, the list
  and grid skip the heat tint (`ant_trail::enabled(cx)` gates both render sites),
  but visits are still recorded — Recents and future prediction keep working.
- **Ant Trail color** — the base hue of the heat tint. Stored as a
  `#RRGGBB(AA)` hex in `app_state` (`ant_trail_color`); the picked alpha is
  ignored because per-folder heat drives the tint's translucency
  (`ant_trail::tint` *sets* alpha to `heat * 0.30`). Clearing the picker
  (`None`) falls back to the original warm orange (`ant_trail::default_base`).
  The list and grid read `ant_trail::base(cx)` during render, so editing the
  color recolors open windows immediately.
- **Don't track favorites** (default on) — `exclude_favorites_from_tracking` in
  `app_state`, surfaced live as `ant_trail::ExcludeFavoritesFromTracking`. When
  on, `Shell::navigate_from_favorite` skips `record_ant_visit`, which means the
  folder neither gains Ant Trail heat nor enters Recents. Toggling it takes
  effect on the next favorite click without a relaunch.

A separate **Settings → Appearance → Recents** group carries the Recents
feature's own switch, backed by [`crate::recents_section`] the same way:

- **Show Recents** (default on) — the master switch, `recents_enabled` in
  `app_state`, surfaced live as `recents_section::RecentsEnabled`. When off,
  `build_recents_section` returns `None` (the sidebar section disappears) and
  `record_ant_visit` skips `push_recent`, so navigation stops feeding the list.
  Visits are still written to `folder_usage`, so the Ant Trail — its own
  switch — keeps its heat. Flipping it shows/hides the section without a
  relaunch (`recents_section::recents_enabled(cx)` is read during render).
  **Clear Recents…** (Go menu / ⌘K) wipes the list for those who keep it.

## Future Prediction

Ant Trail should eventually feed idle prefetch:

- likely child folders,
- parent/sibling folders,
- magic sniffing,
- previews,
- icon cache,
- disk usage summaries.

Keep decay disabled until there is evidence it improves daily navigation. A
simple cumulative count is predictable and easy to reason about.

## Mac Notes

- Heat keys should survive common Mac path churn where possible. Prefer stable
  file IDs/bookmarks later; path is acceptable for the first persistence slice.
- Network volumes and external drives need volume identity in the key.

## Remaining Work

Tracked in [TODO.md](../../TODO.md):

- Stable identity across rename/move/mount changes instead of path-only heat
  keys.
- Hot-folder query API.
- Prediction scheduler.
- Optional time decay / recency weighting.
- Debug overlay for heat and prefetch decisions.
- Tests for "navigation commit only" semantics.
