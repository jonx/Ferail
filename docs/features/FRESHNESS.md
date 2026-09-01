# Cache Freshness & Subtree Invalidation

← [Feature index](README.md) · [Architecture](../ARCHITECTURE.md) ·
[TODO](../../TODO.md)

How Ferail keeps **derived values that summarize a whole subtree**: today
the file list's folder sizes and Get Info's "Calculate": honest when the
underlying tree changes, whether the change came from inside the app or from a
3rd-party tool.

This is the general model the codebase reaches for whenever a cached value is a
function of an entire directory subtree (folder size and recursive item counts
now; duplicate sets and clone-aware sizing later). Read it before adding another
such cache.

<!-- toc depth=2 -->

- [The problem](#the-problem)
- [The model - three mechanisms](#the-model---three-mechanisms)
- [One source of truth](#one-source-of-truth)
- [Where each piece lives](#where-each-piece-lives)
- [Known gaps / future work](#known-gaps--future-work)

<!-- /toc -->

## The problem

A recursive folder size is cached in the `folder_sizes` table
([ferail-meta/src/db.rs](../../crates/ferail-meta/src/db.rs)) keyed by path.
The obvious validity signal, *has the folder's own `mtime` changed?*, has a
hard blind spot on POSIX and Windows alike:

> A directory's `mtime` only moves when its **direct** children change. A write
> deep inside a subtree (`/Photos/2024/raw/new.cr2`) leaves `/Photos/2024`'s
> `mtime` untouched, so a cached size for `/Photos/2024` still validates as
> fresh even though it is now wrong.

So "I did work down in a subfolder and came back" and "a 3rd-party tool changed
things" both leave stale sizes if `mtime` is the only check.

We deliberately do **not** solve this with a recursive FSEvents/`ReadDirectory­
ChangesW` watch. The existing watcher
([fs_watcher.rs](../../crates/ferail-gpui/src/fs_watcher.rs)) stays
non-recursive and only drives **listing** reloads (the set of rows in the
current directory): that is the cheap, correct signal for direct-child changes.
Recursive watching of large trees trades a real event-volume / lifecycle cost
for liveness we can get more cheaply. Instead, freshness rests on three
augmentations layered onto machinery that already exists.

## The model - three mechanisms

### 1. mtime fast-path (unchanged)

The cache sweep still trusts a row while its `mtime_unix` matches the folder's
live mtime. This alone correctly catches every **shallow** change (a direct
child added/removed/resized) and is what makes revisiting an unchanged folder
fill every size in a single frame. The two mechanisms below close only the
*deep* blind spot.

### 2. Exact ancestor invalidation for in-app work

When **Ferail itself** mutates the filesystem, it knows precisely what
changed, so it invalidates precisely. Every mutation already funnels its reload
through one choke point:
`Shell::broadcast_reload_for_process`
([shell.rs](../../crates/ferail-gpui/src/shell.rs)), and that now also calls
`Shell::invalidate_folder_size_ancestors`, which deletes the cached size for the
changed path **and every ancestor up to the root** (`delete_folder_size`).

Why the whole ancestor chain: a change at `P` alters the recursive size of `P`
and of every directory that contains it, yet only `P`'s own `mtime` moves. The
ancestors are exactly the rows the mtime fast-path can't catch, so they are
exactly the rows we drop. The deletes run off the UI thread and are single-row
primary-key hits.

This makes the headline case: *enter a subfolder, do work, navigate back*:
correct **immediately**, with no walking until the parent is actually viewed
again. The same path also covers external **shallow** changes the non-recursive
watcher reports (the watched dir's content changed → its ancestors' sizes are
now stale → invalidate).

### 3. Lazy catch-up for 3rd-party deep changes

When an **external** tool writes deep into a subtree, we get no event (the watch
is non-recursive) and no mtime movement on the ancestors. Two lazy nets bound
how long that can hide a stale size:

- **TTL.** A cached row counts as a hit only while it is younger than
  `FOLDER_SIZE_TTL_SECS`
  ([folder_sizes.rs](../../crates/ferail-gpui/src/folder_sizes.rs), 10 min
  today, one tunable const). Past that, the next *visit* recomputes. A
  recompute only happens when the folder is actually loaded, off the UI thread,
  so a longer TTL trades a little staleness for fewer re-walks of big trees.
- **Explicit Refresh.** The one gesture that means "measure this again" is the
  Refresh command, and it is the only thing that bypasses the cache. It arms
  `Tab::force_folder_sizes` (alongside the existing `force_resniff`) for exactly
  its own load; `finish_directory_load_in_tab` consumes the flag and passes it
  to `folder_sizes::start(.., force)`. A Refresh superseded before it finishes
  is dropped by the generation guard, and the superseding load resets the flag.
  The same gesture also invalidates completed content previews, decoded text,
  immediate-folder sidecar scans and thumbnail tiers beneath the refreshed
  directory. These process-memory caches are strictly capped (16 paths for
  each preview/sidecar cache and 512 thumbnail tiers), so invalidation is
  independent of whether the directory contains ten rows or ten million.
  Entries belonging to other directories remain warm; in-flight work keeps its
  scheduler reservation rather than being duplicated.

- **Activation re-seed.** When the window returns from the background we re-run
  the pass, but **cache-first** (`restart_folder_size_passes(force = false)`),
  so rows whose TTL lapsed while we were away get recomputed and everything else
  answers instantly. It reuses GPUI's framework-level
  `observe_window_activation` (no new platform-shell surface) and only fires on
  a genuine background→foreground transition: the `Shell::was_window_active`
  guard skips the initial launch activation and any same-state re-fire.

> **This used to force.** Until 2026-08-04 the activation path passed
> `force = true`, re-walking every visible tree on every return to the app.
> Switching apps is something a user does dozens of times an hour, so in
> practice the Size column could never settle: a folder measured a moment ago
> was measured again the next time the window came forward, and a big tree that
> lost the race to the next navigation was never cached at all. The liveness it
> bought is already covered from both sides: watcher-driven ancestor
> invalidation catches what the app and 3rd-party tools do to a *watched*
> directory, and the TTL bounds how long a *deep* external change can hide.
> What was left was cost without a matching guarantee. Don't reintroduce it:
> if deep external liveness ever needs to be better than the TTL, the seam is a
> recursive watcher feeding `invalidate_folder_size_ancestors`, not a periodic
> re-walk.

## One source of truth

Get Info's "Calculate" button shares the same cache via
`folder_sizes::folder_size_cached`
([folder_sizes.rs](../../crates/ferail-gpui/src/folder_sizes.rs)): a folder
already sized in the Size column answers instantly, and a value computed in the
inspector feeds the column. Both honor the identical contract (mtime + TTL) and
both participate in the same invalidation, so the two surfaces can never
disagree.

## Where each piece lives

| Concern | Location |
| --- | --- |
| Cache table + `delete_folder_size` | [ferail-meta/src/db.rs](../../crates/ferail-meta/src/db.rs) |
| TTL, `force`, batch worker, single-path helper | [folder_sizes.rs](../../crates/ferail-gpui/src/folder_sizes.rs) |
| Ancestor invalidation choke point | `Shell::invalidate_folder_size_ancestors` in [shell.rs](../../crates/ferail-gpui/src/shell.rs) |
| Forced re-walk on Refresh | `Tab::force_folder_sizes` ([tab.rs](../../crates/ferail-gpui/src/shell/tab.rs)), armed in `Shell::on_refresh`, consumed in `finish_directory_load_in_tab` |
| Cache-first re-seed on return | `observe_window_activation` wiring in `Shell::new`; `restart_folder_size_passes(force = false)` |
| Hit/miss visibility | `folder-sizes: N cache hit(s), M to walk` at log level 90 |
| Get Info reuse | `calculate_size` in [entry_info.rs](../../crates/ferail-gpui/src/entry_info.rs) |

## Known gaps / future work

- **Multi-parent mutations.** A cross-directory move touches two parents, but
  `spawn_file_op` reloads (and so invalidates) a single `reload_path`. Both
  endpoints' ancestor chains should be invalidated.
- **Upgrade path to live deep updates.** If liveness ever outweighs the cost,
  the ancestor-invalidation service is the seam a recursive watcher would plug
  into: it would feed the same `invalidate_folder_size_ancestors`, and nothing
  downstream would change.
- **Generalization.** Recursive **item counts** now ride this exact model: the
  same walk that sums a folder's size also counts its files and sub-folders, both
  cached in the `folder_sizes` row (`file_count` / `dir_count`) and rendered in
  the Description column as "N files · M folders" (see
  [MAGIC_DESCRIPTION.md](MAGIC_DESCRIPTION.md)). APFS clone-aware sizing
  ([TODO.md](../../TODO.md)) is the next recursive aggregate and should reuse this
  model (cache keyed by path, mtime + TTL validity, ancestor invalidation on
  mutation) rather than inventing a parallel one.
</content>
</invoke>
