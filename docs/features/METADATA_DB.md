# Metadata DB

← [Feature notes](README.md) · [Status](../STATUS.md) ·
[Architecture](../ARCHITECTURE.md) · [Open work](../../TODO.md)

Persistent SQLite-backed substrate for everything Ferail has been
keeping in-memory: Ant Trail heat, magic cache, quarantine cache,
window/layout/tab geometry, pinned items, file-hash funnel for the
duplicate finder. Ported from the Ferail-Win32 predecessor's
`crates/ferail-core/src/metadata/{db,cache}.rs`;
schema reused with macOS-flavored adjustments.

<!-- toc depth=2 -->

- [What is built](#what-is-built)
- [Crate](#crate)
- [Schema](#schema)
- [Lifecycle](#lifecycle)
- [How to add a new persistent field](#how-to-add-a-new-persistent-field)
- [CLI: `--reset-db <scope>`](#cli---reset-db-)
- [Differences from Ferail-Win32](#differences-from-ferail-win32)
- [Open items](#open-items)

<!-- /toc -->

## What is built

Done (iter-8.0 → 8.5). All five sub-iters shipped:

- **8.0**: foundation crate + schema + version-bump-or-recreate.
- **8.1**: Ant Trail visit counts persist across launches; navigate
  writes through, open hydrates.
- **8.2**: magic cache rows write through on `MagicBatch`; the
  prefetch-start path hydrates the in-memory `magic_cache` from the
  DB before kicking the worker.
- **8.3**: quarantine flag + agent + ISO + where-from URLs persist;
  hydrate during `start_quarantine_prefetch`.
- **8.4**: window inner-size, sidebar / preview splitter widths,
  preview-pane visibility, and the open-tab list (path / active /
  scroll / selection) save on `CloseRequested`, restore on app
  resume / DB open.
- **8.5**: DU window geometry + Top-N panel width migrated from the
  flat `du_window.txt` file into the DB's `layout_state.du_*`
  columns. Reads prefer the DB; the txt file is a transition-window
  fallback.

## Crate

[`crates/ferail-meta`](../../crates/ferail-meta/): single
`MetadataDb` connection, schema-versioned (delete-and-recreate on
mismatch: Ferail-Win32's policy; caches built on top are derived data, so
a recreate is cheap), plus a thin `MetadataCache` (FIFO-ish bounded
HashMap) for hot-data read-amplification.

Dependency: `rusqlite = { version = "0.31", features = ["bundled"] }`
so we don't need the system SQLite to be a particular version.

DB lives at `~/Library/Application Support/Ferail/metadata.db`
(macOS), or in-memory in tests / when `$HOME` is unset (the
screenshot harness).

## Schema

```sql
-- Generic key/value preferences. db_version lives here.
preferences (key TEXT PRIMARY KEY, value TEXT NOT NULL)

-- File metadata cache. Derived data keyed by absolute path.
-- mtime_unix invalidates downstream columns on edit.
files (
    path TEXT PRIMARY KEY,
    mtime_unix INTEGER NOT NULL,
    size INTEGER NOT NULL,
    magic_label TEXT,                   -- iter-8.2
    partial_hash TEXT,                  -- duplicate finder, future
    full_hash TEXT,                     -- duplicate finder, future
    mime TEXT,                          -- future, content-sniffed
    quarantined INTEGER,                -- iter-8.3, 0/1, NULL=unknown
    quarantine_agent TEXT,              -- iter-8.3
    quarantine_iso TEXT,                -- iter-8.3
    quarantine_where_from TEXT,         -- iter-8.3, \n-joined URLs
    indexed_at_unix INTEGER NOT NULL
)
+ idx_files_full_hash, idx_files_partial_hash, idx_files_size

-- Ant Trail folder-usage. `score` is computed at read time from
-- hits + last_access; we only persist the raw signal.
folder_usage (
    folder_path TEXT PRIMARY KEY,
    hits INTEGER NOT NULL DEFAULT 0,
    last_access_unix INTEGER NOT NULL
)
+ idx_folder_hits

-- Window state: single row.
window_state (id INTEGER PRIMARY KEY CHECK (id = 1),
              width, height, maximized)

-- Layout state: single row, includes DU-window geometry so the
-- separate `du_window.txt` file can retire.
layout_state (id INTEGER PRIMARY KEY CHECK (id = 1),
              sidebar_width, preview_width, preview_visible,
              du_width, du_height, du_topn_width)

-- Open tabs at last quit. Replaced in full on save.
tabs (id, path, is_active, sort_order, scroll_offset,
      selected_index, sort_column, sort_ascending)

-- Pinned sidebar items (ordered).
pinned_items (id, path, sort_order)
```

Indexes prepare the way for the duplicate-finder funnel
(size → partial_hash → full_hash) and the Ant Trail "hottest folders"
queries.

## Lifecycle

1. **Open** at app start (`App::open_metadata_db`): best-effort.
   Failure logs and leaves `metadata_db = None`; the rest of the app
   degrades gracefully (in-memory caches still work, just don't
   survive restart).
2. **Hydrate**: Ant Trail visit counts re-bind to fresh `NodeId`s
   via `NativeFs::id_for_path` since IDs are session-scoped. Magic
   cache hydrates lazily during `start_magic_prefetch` for the
   active folder's entries.
3. **Write through**: every `record(NodeId)` on Ant Trail mirrors
   to `record_folder_visit(path, now)`; every `MagicBatch` arm
   upserts a `FileMetaRecord` for each result.
4. **Schema bumps**: increment `DB_VERSION` in
   [`db.rs`](../../crates/ferail-meta/src/db.rs) when changing
   column shape. The on-open version check deletes mismatched files
   and recreates fresh.

## How to add a new persistent field

Future-me, this is the pattern. Pick the bucket your field belongs
in, then walk down the checklist.

### A. Pure UI / user setting (window size, splitter, toggle)

1. Add a column to the right table in
   [`ferail-meta/src/db.rs`](../../crates/ferail-meta/src/db.rs)
   inside `init_schema()`: usually `window_state` or
   `layout_state` for single-row state, or a new dedicated table
   for list-like state.
2. Add the field to the matching Rust struct (`WindowState`,
   `LayoutState`, `TabState`, …) and to the corresponding
   `save_*` / `load_*` SQL.
3. Bump `DB_VERSION` (top of `db.rs`). On next open the old file
   gets deleted and recreated: fine for derived UI state.
4. In `ferail-gpui`, extend the persistence site that owns that
   state (e.g. the `persist_*` writers in `settings.rs` /
   `favorites.rs`, or the shell's layout persistence) to write the
   new field, and load it back on open.

### B. Derived cache (hash, magic, quarantine, future thumbnails)

1. Add a column to the `files` table in `init_schema()` and to
   `FileMetaRecord`.
2. Update `get_file` / `upsert_file` SQL to read/write it. Keep
   the `COALESCE(excluded.X, files.X)` pattern in the UPSERT so
   partial writes preserve other columns.
3. Bump `DB_VERSION`.
4. In the App's worker-result handler (the `AppEvent` arm that
   produces the new data), upsert a `FileMetaRecord` with just
   the new field set. Mirror the pattern in
   `AppEvent::MagicBatch` / `AppEvent::QuarantineBatch`.
5. In the App's *prefetch-start* path (the place that today
   checks the in-memory cache before kicking the worker), look
   up the row via `db.get_file(path)`, verify
   `rec.mtime_unix == entry.mtime_unix`, and populate the
   in-memory cache from the DB row.

### C. Ant Trail-shaped signal (counter + last-access)

Probably wants its own table: `folder_usage` is dedicated
because it accumulates ad-hoc. New tables:

1. Add the `CREATE TABLE` + indexes to `init_schema()`.
2. Add a `record_*` / `load_*` / `save_*` trio on `MetadataDb`
   with the same shape as `record_folder_visit` /
   `load_ant_trail` / `save_ant_trail`. Use the
   `ON CONFLICT … DO UPDATE SET hits = hits + 1` pattern for
   counters.
3. Bump `DB_VERSION`.
4. Hook write-through at the event that increments the signal
   (e.g. file open ↔ recent files, search submit ↔ recent
   queries). Hydrate at the equivalent of
   `hydrate_ant_trail_from_db`.

### D. Add a corresponding `ResetScope`

If the new data is something a support engineer might want to
clear independently (and isn't covered by `Ui` / `Caches`), add a
variant:

1. Extend the `ResetScope` enum in `db.rs`.
2. Add the `from_cli` / `help_label` arms.
3. Add the match arm to `MetadataDb::reset` with the DELETE / UPDATE.
4. Extend `print_reset_db_usage` in `main.rs` with the new name.
5. Write a one-line test in `db.rs` that calls `reset(YourScope)`
   and asserts the right tables emptied + others intact.

### Things to NOT do

- Don't store `Vec<T>` or `HashMap` as a serialised blob if you
  can express it as a separate table. The DB is cheap; ad-hoc
  blobs aren't queryable and rot fast.
- Don't write to the DB from paint. All persistence calls happen
  at event-handler time, navigate-commit time, or `CloseRequested`.
- Don't forget mtime invalidation. The `files` table assumes
  `mtime_unix` matches the live filesystem; stale rows are the
  caller's problem. Drop derived columns when `mtime_unix`
  changes in your write path.
- Don't keep a path-keyed row alive after a rename. The future
  NodeStore (stable `(dev, ino)` or NSURL bookmark) will replace
  the path key; until then renames orphan rows.

## CLI: `--reset-db <scope>`

A pre-event-loop flag for nuking parts of the DB without making
the user `rm` the file by hand. Runs the reset, prints a
one-line confirmation to stderr, and exits 0.

```sh
Ferail --reset-db all          # delete the DB file outright
Ferail --reset-db ui           # window size, splitters, tabs, pinned
Ferail --reset-db caches       # files table + folder_usage (all derived)
Ferail --reset-db ant-trail    # just the folder_usage table
Ferail --reset-db magic        # NULL out files.magic_label
Ferail --reset-db quarantine   # NULL out files.quarantine_*
```

`all` literally deletes the file (cheaper than DELETEing every
table, and dodges any stale index / FK state). Every other
scope preserves `preferences.db_version` so the next open
doesn't trigger the version-mismatch recreate path.

## Differences from Ferail-Win32

| Aspect | Ferail-Win32 | Ferail |
|---|---|---|
| Path format | Windows drive letters | macOS POSIX |
| Quarantine | n/a | New columns: `quarantined` flag + agent + iso + where-from |
| DU window geometry | n/a | New `layout_state` columns: `du_width`/`du_height`/`du_topn_width` |
| Schema version | 5 | restarted at 1 (we're not migrating Ferail-Win32 data) |
| `nav_history` | yes (per-tab back/forward) | omitted for now; tabs only carry the active path |
| `score` column on `folder_usage` | persisted | computed at read time from hits/recency: fewer dimensions to keep in sync |

## Open items

- **Cross-thread access**: the DB is single-connection. Once the
  duplicate-finder worker writes hashes, it'll need either
  `Arc<Mutex<MetadataDb>>` or a writer-thread pattern. Keep it
  simple for now; revisit when there's a concrete consumer.
- **Move-aware identity**: today the cache is keyed by absolute
  path, so renaming a file orphans its row. The future `NodeStore`
  work (stable id surviving move/rename via `(dev, ino)` or NSURL
  bookmark) will replace the path key.
- **`du_window.txt` cleanup**: still written for backward
  compatibility. After a release or two, the legacy file can be
  deleted on first DB-row write.
- **Window position**: only inner-size persists; macOS owns
  position via NSWindow's frame autosave when the app is bundled.
  Re-evaluate when we ship as a real `.app`.
- **Tab sort column**: the `tabs` schema has columns for
  `sort_column` / `sort_ascending`, but the App write-through
  always writes 0 / true today. Wire when the per-tab sort lands
  (it currently belongs to the global list state).
