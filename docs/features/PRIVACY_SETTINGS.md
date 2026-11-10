# Privacy settings

← [Feature notes](README.md) · [Status](../STATUS.md) ·
[Architecture](../ARCHITECTURE.md) · [Privacy policy](../../PRIVACY.md) ·
[Open work](../../TODO.md)

Settings → Privacy decides what Ferail writes down about the folders and
files the user browses, and erases it on request. It is unrelated to
[Private Mode](PRIVATE_MODE.md), which only changes what is painted.

## What is stored

| Trace | Where | Controlled by |
| --- | --- | --- |
| Visited folders (Recents, Ant Trail heat) | `metadata.db`, `folder_usage` | Record visited folders, retention |
| File details (type, checksums, download origin) | `metadata.db`, `files` | retention |
| Folder sizes | `metadata.db`, `folder_sizes` | retention |
| Where trashed items came from | `metadata.db`, `put_back` | retention |
| Last folder and open tabs | `gpui-state.txt`, `last_dir`, `tab`, `tab_active` | Reopen the last session |
| Crash, freeze and issue reports | `reports/` | Keep reports for |
| Thumbnails | the operating system's own cache | Use the system thumbnail service |

Favorites, preferences and layout are settings, not traces. No privacy
switch stops them being saved.

## The settings

| Setting | State key | Default | Effect |
| --- | --- | --- | --- |
| Remember nothing | `remember_nothing` | off | Master switch, see below |
| Reopen the last session | `restore_session` | on | Off starts at home and saves neither `last_dir` nor the tab list |
| Record visited folders | `record_visits` | on | Off records no visit, in memory or on disk |
| Keep history and caches for | `retention_days` | 90 | Rows not refreshed within the period are deleted at launch |
| Never record these folders | `privacy_exclude` (one line each) | none | See [Excluded locations](#excluded-locations) |
| Never record other volumes | `privacy_exclude_volumes` | off | Adds the platform's volume mount roots to the exclusions |
| Use the system thumbnail service | `system_thumbnails` | on | Off skips the platform thumbnail call |
| Keep reports for | `report_retention_days` | forever | Older `ferail-*` files in `reports/` are deleted at launch |

`crate::privacy::Policy` resolves these with their defaults.
`crate::privacy::apply` brings the running process in line after any change,
so no setting needs a relaunch.

## Remember nothing

While on:

- `MetadataDb::set_ephemeral(true)` creates in-memory twins of the four
  browsing tables in SQLite's `temp` schema. An unqualified table name
  resolves to `temp` first, so every existing query reads and writes memory.
  The on-disk tables are neither read nor changed, and are visible again
  when the switch goes off. `temp_store = MEMORY` keeps the temp schema out
  of the system temp folder.
- The saved session is emptied and no longer written.
- The system thumbnail service is bypassed, whatever its own switch says.

Put Back works for items trashed during the session and stops working for
them when Ferail quits.

## Session restore

The session is the folder tabs of the window used last, in order, plus the
index of the front tab. Tool tabs (search, duplicates, an archive, Disk
Usage) are results, not places, and are left out. Split View is not part of
the session.

Only the launch window restores. It boots on `last_dir` as before; the other
tabs are created on either side of it and each validates its folder on the
background executor with the same bounded probe. A folder that no longer
exists does not come back as a tab. At most 32 tabs are restored. Opening
Ferail on a folder or an archive, and a screenshot run, restore nothing, and
a screenshot run does not overwrite the saved session.

## Excluded locations

`ferail_core::privacy::Exclusions` is a lexical, component-wise prefix test:
`/a/b` covers `/a/b/c` and never `/a/bc`. It costs no I/O.

Under an excluded location:

- the store's write methods (`record_folder_visit`, `upsert_file`,
  `upsert_files`, `upsert_folder_size`, `record_put_back`) are no-ops;
- a visit is not counted in memory either, so Recents and the heat tint
  never show the place;
- a tab is left out of the saved session.

Adding an exclusion also deletes the rows already stored under it
(`MetadataDb::purge_excluded`), and the same purge runs at every launch.

"Other volumes" means, per platform:

| Platform | Roots | Covers |
| --- | --- | --- |
| macOS | `/Volumes` | external and network disks, disk images, additional internal disks |
| Linux | `/media`, `/run/media`, `/mnt` | desktop automounts and manual mounts |
| Windows | `\\` | network shares only; a removable drive is added as a folder |

## Thumbnails

With the system service off, `video_poster::fetch_content_inner` and the
viewer's full-resolution fallback skip the platform call (Quick Look, the
Explorer thumbnail provider, the freedesktop thumbnail folder). The
remaining tiers answer: the bundled decoders for PNG, JPEG, GIF, WebP, BMP
and TIFF, embedded audio cover art, and mpv posters. Every other type shows
its type icon. Thumbnails the system cached earlier stay in its cache;
Ferail cannot clear it.

Quick Look opened with Space is an explicit request to the system and is not
affected.

## Clearing

Each button erases one kind of trace, in memory at once and on disk on the
background executor:

| Button | Erases |
| --- | --- |
| Visited folders | `folder_usage`, the in-memory visit counts and Recents |
| File and folder caches | `files`, `folder_sizes` |
| Put-back records | `put_back` |
| Saved session | `last_dir`, the tab list |
| Reports | every `ferail-*` file in `reports/` |

`MetadataDb::reset` always targets the on-disk tables, also while Remember
nothing is on, and empties the session tables with them. The store runs with
`secure_delete = ON`, so deleted rows are zeroed in place. After a clear,
`MetadataDb::compact` truncates the write-ahead log and vacuums the file.

## Retention

`MetadataDb::prune_stale(now, days)` runs once per launch. It deletes
`files`, `folder_sizes` and `put_back` rows older than the period and visits
not repeated within it. A row removed from Recents carries no date and keeps
its heat until Visited folders is cleared.

## Tests

- `ferail-core`: `privacy::tests` (prefix matching).
- `ferail-meta`: `db::tests` (`ephemeral_*`, `excluded_*`, `purge_excluded_*`,
  `prune_honours_*`, `reset_reaches_the_disk_while_ephemeral`).
- `ferail-gpui`: `privacy::tests` (policy defaults, session filtering, report
  pruning), `app_state::tests::privacy_state_round_trips`.
