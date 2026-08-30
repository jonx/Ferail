//! SQLite-backed persistent metadata. Schema lifted from the Ferail
//! predecessor with macOS-flavored adjustments: `ino`/`dev` columns
//! on the `files` table for future move-aware identity, no Windows
//! drive-letter assumptions in `tabs.path`, and a `magic_label`
//! column so the magic cache can hydrate without a second table.
//!
//! Schema bumps: increment [`DB_VERSION`] when changing column shape.
//! On open, a stored version mismatch deletes the file and recreates
//! it, same hard-reset policy as Ferail. Caches built on top are
//! all derived data, so a recreate is cheap.

use std::path::Path;

use ferail_core::favorites::{Favorite, FavoriteIcon, FavoriteId, FavoriteKind, FavoriteTarget};
use rusqlite::{params, Connection};

/// Schema version. Bump on any structural change.
///
/// `2` adds the `favorites` table (separate from the older
/// `pinned_items` placeholder, which is now unused). Migration from
/// `1 → 2` is additive: `init_schema` is idempotent (`CREATE TABLE
/// IF NOT EXISTS`), so existing caches survive the upgrade.
///
/// `3` adds the `files.description` column for the structured
/// magic-derived Description column. Forward migration is also
/// additive: `init_schema` issues an `ALTER TABLE ... ADD COLUMN`
/// and tolerates "duplicate column" on already-migrated DBs.
///
/// `4` adds the `folder_sizes` table (recursive folder-size cache
/// for the file list's Size column). Additive: `CREATE TABLE IF
/// NOT EXISTS` covers the `3 → 4` migration.
///
/// `5` adds `folder_sizes.file_count` / `dir_count` (recursive item
/// counts for the folder Description column). Additive `ALTER TABLE
/// ... ADD COLUMN`; the same walk that computes the size fills them.
/// The migration also clears any pre-existing rows once, since a v4
/// row has no counts and would otherwise render "0 files" until its
/// mtime/TTL forced a recompute: pure cache data, safe to drop.
pub const DB_VERSION: u32 = 5;

#[derive(Debug)]
pub enum MetadataError {
    Sqlite(rusqlite::Error),
    Io(std::io::Error),
}

impl std::fmt::Display for MetadataError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            MetadataError::Sqlite(e) => write!(f, "sqlite: {e}"),
            MetadataError::Io(e) => write!(f, "io: {e}"),
        }
    }
}
impl std::error::Error for MetadataError {}
impl From<rusqlite::Error> for MetadataError {
    fn from(e: rusqlite::Error) -> Self {
        MetadataError::Sqlite(e)
    }
}
impl From<std::io::Error> for MetadataError {
    fn from(e: std::io::Error) -> Self {
        MetadataError::Io(e)
    }
}

pub type Result<T> = std::result::Result<T, MetadataError>;

/// Scope of a [`MetadataDb::reset`] call. Maps to the user-facing
/// `--reset-db <scope>` CLI flag and to the in-app "Reset…" menu
/// items if/when those land. Picking a narrow scope is the point,
/// most users wanting "fresh layout" don't also want to throw away
/// cached magic / quarantine / Ant Trail signal.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum ResetScope {
    /// Wipe everything. The DB file is recreated; the only row that
    /// survives is the new `preferences.db_version`.
    All,
    /// Window size + sidebar/preview splitter + tab list + pinned
    /// items. Leaves all derived caches intact (so reopening at home
    /// is fast).
    Ui,
    /// All derived caches: files + folder_usage. UI state survives.
    Caches,
    /// Only `folder_usage`: the Ant Trail heat map.
    AntTrail,
    /// `files.magic_label` only. Keeps quarantine + hash data.
    Magic,
    /// `files.quarantine_*` only. Keeps magic + hash data.
    Quarantine,
    /// User-curated favorites. Not bundled into `Ui` because
    /// favorites are deliberately user-pinned and shouldn't be lost
    /// when someone resets window/layout state.
    Favorites,
}

impl ResetScope {
    /// Parse the lower-cased CLI argument. Returns `None` for
    /// unrecognised inputs so the caller can print a usage hint.
    pub fn from_cli(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "all" => Some(Self::All),
            "ui" => Some(Self::Ui),
            "caches" | "cache" => Some(Self::Caches),
            "ant-trail" | "anttrail" | "ant_trail" => Some(Self::AntTrail),
            "magic" => Some(Self::Magic),
            "quarantine" => Some(Self::Quarantine),
            "favorites" | "favourites" => Some(Self::Favorites),
            _ => None,
        }
    }

    /// One-line description for the `--help` output.
    pub fn help_label(self) -> &'static str {
        match self {
            Self::All => "everything (deletes the DB file)",
            Self::Ui => "window size, splitters, tabs, pinned items",
            Self::Caches => "every derived cache (files + Ant Trail)",
            Self::AntTrail => "Ant Trail heat (folder_usage)",
            Self::Magic => "files.magic_label only",
            Self::Quarantine => "files.quarantine_* only",
            Self::Favorites => "user-curated Favorites only",
        }
    }
}

/// One row from the `folder_usage` table: Ant Trail heat snapshot.
#[derive(Debug, Clone)]
pub struct AntTrailEntry {
    pub folder_path: String,
    pub hits: u32,
    pub last_access_unix: i64,
}

/// One row from the `files` table: derived metadata cache. All
/// fields except `path` are optional so callers can update one
/// dimension at a time (write magic, write hash, write quarantine
/// independently).
#[derive(Debug, Clone)]
pub struct FileMetaRecord {
    pub path: String,
    pub mtime_unix: i64,
    pub size: u64,
    pub magic_label: Option<String>,
    /// Rich ` · `-joined fact string for the Description column,
    /// derived from the structured magic-byte parse. NULL until the
    /// magic prefetch worker fills it.
    pub description: Option<String>,
    pub partial_hash: Option<String>,
    pub full_hash: Option<String>,
    pub mime: Option<String>,
    /// `Some(true)` = the file carries `com.apple.quarantine`;
    /// `Some(false)` = we checked and it doesn't; `None` = haven't
    /// looked yet. Lets a fresh open distinguish "clean" from
    /// "unknown" without a separate column.
    pub quarantined: Option<bool>,
    pub quarantine_agent: Option<String>,
    pub quarantine_iso: Option<String>,
    /// `\n`-joined where-from URLs. URLs are URL-encoded so they
    /// can't contain literal newlines. `None` and `Some("")` both
    /// mean "no source URLs".
    pub quarantine_where_from: Option<String>,
    pub indexed_at_unix: i64,
}

/// One row from the `folder_sizes` table: cached recursive folder
/// size for the file list's Size column. `mtime_unix` is the folder's
/// own mtime at compute time; the caller compares it against the live
/// filesystem to decide whether the row is still valid. Note the
/// caveat: a directory's mtime only changes when its *direct*
/// children change, so deep edits don't invalidate this row.
#[derive(Debug, Clone)]
pub struct FolderSizeRecord {
    pub path: String,
    pub mtime_unix: i64,
    /// Logical bytes: sum of `metadata.len()` over every regular
    /// file underneath, symlinks excluded. Finder "Size" semantics.
    pub size: u64,
    pub computed_at_unix: i64,
    /// Recursive item counts from the same walk that produced `size`,
    /// for the folder Description column ("N files in M folders"). Both
    /// exclude the folder itself and describe exactly the entries `size`
    /// summed. A v4 row migrated forward carries `0`/`0` only until the
    /// folder is re-walked, but the 4 → 5 migration clears such rows so
    /// this never surfaces (see [`DB_VERSION`]).
    pub file_count: u64,
    pub dir_count: u64,
}

#[derive(Debug, Clone, Default)]
pub struct WindowState {
    pub width: i32,
    pub height: i32,
    pub maximized: bool,
}

#[derive(Debug, Clone, Default)]
pub struct LayoutState {
    pub sidebar_width: i32,
    pub preview_width: i32,
    pub preview_visible: bool,
    pub du_width: i32,
    pub du_height: i32,
    pub du_topn_width: i32,
}

#[derive(Debug, Clone)]
pub struct TabState {
    pub path: String,
    pub is_active: bool,
    pub scroll_offset: f32,
    pub selected_index: i32,
    pub sort_column: i32,
    pub sort_ascending: bool,
}

/// Append `suffix` to the *full* file name (`metadata.db` +
/// `"-journal"` → `metadata.db-journal`), matching SQLite's sibling
/// naming: `Path::with_extension` would replace `.db` instead.
fn sibling_path(path: &Path, suffix: &str) -> std::path::PathBuf {
    let mut s = path.as_os_str().to_os_string();
    s.push(suffix);
    std::path::PathBuf::from(s)
}

/// SQLite-backed metadata store. Single connection: wrap in a
/// `Mutex` at the call site if cross-thread access is needed.
pub struct MetadataDb {
    conn: Connection,
}

impl MetadataDb {
    /// Open or create the database at `path`. If the on-disk schema
    /// version doesn't match [`DB_VERSION`], the file is set aside as
    /// `<name>.bak` and recreated, never silently destroyed, because
    /// user-curated rows (favorites) live here alongside cache data.
    /// Caller is responsible for ensuring the parent directory exists
    /// ([`crate::ensure_parent_dir`]).
    pub fn open(path: &Path) -> Result<Self> {
        let mut wipe = false;
        if path.exists() {
            match Connection::open(path) {
                Ok(conn) => {
                    let stored: Option<u32> = conn
                        .query_row(
                            "SELECT value FROM preferences WHERE key = 'db_version'",
                            [],
                            |row| {
                                let v: String = row.get(0)?;
                                Ok(v.parse().ok())
                            },
                        )
                        .ok()
                        .flatten();
                    match stored {
                        Some(v) if v == DB_VERSION => {}
                        // Forward-only additive migrations: init_schema is
                        // idempotent, so v1 → v2 just adds the new table.
                        Some(v) if v < DB_VERSION => {}
                        // Future version we don't understand, or no row at
                        // all → set aside and start over.
                        _ => wipe = true,
                    }
                    drop(conn);
                }
                Err(_) => wipe = true,
            }
            if wipe {
                // Rename to .bak (one-deep) instead of deleting: a
                // downgrade to an older build or a corrupt header must
                // not cost the user their favorites. The journal/WAL
                // siblings go with the main file: a stale `-journal`
                // next to a freshly created same-name DB is a
                // documented SQLite corruption vector.
                let bak = sibling_path(path, ".bak");
                let _ = std::fs::remove_file(&bak);
                if std::fs::rename(path, &bak).is_err() {
                    let _ = std::fs::remove_file(path);
                }
                for suffix in ["-journal", "-wal", "-shm"] {
                    let _ = std::fs::remove_file(sibling_path(path, suffix));
                }
            }
        }
        let conn = Connection::open(path)?;
        // A second Ferail process (or `--reset-db` racing a live app)
        // holds the write lock briefly; without a timeout every busy
        // conflict surfaces as an instant SQLITE_BUSY error, and a
        // failed favorites load is what the wipe-on-save hazard feeds
        // on. 250ms rides out lock handoffs without wedging workers.
        conn.busy_timeout(std::time::Duration::from_millis(250))?;
        // WAL + NORMAL: the hot write paths (prefetch upserts, dupe
        // hash cache) are many small autocommit statements, under the
        // default DELETE journal each costs ~2 fsyncs. WAL brings that
        // to one WAL append, and readers stop blocking on writers.
        // journal_mode returns the resulting mode as a row, so it
        // can't go through pragma_update.
        let _mode: String = conn
            .query_row("PRAGMA journal_mode=WAL", [], |row| row.get(0))
            .unwrap_or_else(|_| "delete".into());
        let _ = conn.pragma_update(None, "synchronous", "NORMAL");
        let db = Self { conn };
        db.init_schema()?;
        db.set_preference("db_version", &DB_VERSION.to_string())?;
        Ok(db)
    }

    /// Open an in-memory database. Used by tests and by the
    /// screenshot harness when `$HOME` is unset.
    pub fn in_memory() -> Result<Self> {
        let conn = Connection::open_in_memory()?;
        conn.busy_timeout(std::time::Duration::from_millis(250))?;
        let db = Self { conn };
        db.init_schema()?;
        Ok(db)
    }

    fn init_schema(&self) -> Result<()> {
        self.conn.execute_batch(
            r#"
            -- Generic key/value preferences. db_version lives here.
            CREATE TABLE IF NOT EXISTS preferences (
                key TEXT PRIMARY KEY,
                value TEXT NOT NULL
            );

            -- File metadata cache: derived data keyed by path.
            -- mtime_unix in the WHERE clause invalidates on edit;
            -- a row with stale mtime is overwritten on next probe.
            CREATE TABLE IF NOT EXISTS files (
                path TEXT PRIMARY KEY,
                mtime_unix INTEGER NOT NULL,
                size INTEGER NOT NULL,
                magic_label TEXT,
                description TEXT,
                partial_hash TEXT,
                full_hash TEXT,
                mime TEXT,
                quarantined INTEGER,
                quarantine_agent TEXT,
                quarantine_iso TEXT,
                quarantine_where_from TEXT,
                indexed_at_unix INTEGER NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_files_full_hash ON files(full_hash);
            CREATE INDEX IF NOT EXISTS idx_files_partial_hash ON files(partial_hash);
            CREATE INDEX IF NOT EXISTS idx_files_size ON files(size);

            -- Recursive folder-size cache for the Size column.
            -- mtime_unix is the folder's own mtime at compute time;
            -- callers compare against the live mtime to validate.
            CREATE TABLE IF NOT EXISTS folder_sizes (
                path TEXT PRIMARY KEY,
                mtime_unix INTEGER NOT NULL,
                size INTEGER NOT NULL,
                computed_at_unix INTEGER NOT NULL,
                file_count INTEGER NOT NULL DEFAULT 0,
                dir_count INTEGER NOT NULL DEFAULT 0
            );

            -- Ant Trail folder-usage. `score` is computed at read time
            -- from hits + last_access; we only persist the raw signal.
            CREATE TABLE IF NOT EXISTS folder_usage (
                folder_path TEXT PRIMARY KEY,
                hits INTEGER NOT NULL DEFAULT 0,
                last_access_unix INTEGER NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_folder_hits ON folder_usage(hits DESC);

            -- Window state: single row.
            CREATE TABLE IF NOT EXISTS window_state (
                id INTEGER PRIMARY KEY CHECK (id = 1),
                width INTEGER NOT NULL DEFAULT 1180,
                height INTEGER NOT NULL DEFAULT 760,
                maximized INTEGER NOT NULL DEFAULT 0
            );

            -- Layout state: single row.
            CREATE TABLE IF NOT EXISTS layout_state (
                id INTEGER PRIMARY KEY CHECK (id = 1),
                sidebar_width INTEGER NOT NULL DEFAULT 220,
                preview_width INTEGER NOT NULL DEFAULT 320,
                preview_visible INTEGER NOT NULL DEFAULT 0,
                du_width INTEGER NOT NULL DEFAULT 1100,
                du_height INTEGER NOT NULL DEFAULT 720,
                du_topn_width INTEGER NOT NULL DEFAULT 280
            );

            -- Open tabs at last quit. Replaced in full on save.
            CREATE TABLE IF NOT EXISTS tabs (
                id INTEGER PRIMARY KEY,
                path TEXT NOT NULL,
                is_active INTEGER NOT NULL DEFAULT 0,
                sort_order INTEGER NOT NULL,
                scroll_offset REAL NOT NULL DEFAULT 0.0,
                selected_index INTEGER NOT NULL DEFAULT -1,
                sort_column INTEGER NOT NULL DEFAULT 0,
                sort_ascending INTEGER NOT NULL DEFAULT 1
            );

            -- Pinned sidebar items (ordered). Legacy placeholder kept
            -- around for one schema cycle; superseded by `favorites`.
            CREATE TABLE IF NOT EXISTS pinned_items (
                id INTEGER PRIMARY KEY,
                path TEXT NOT NULL,
                sort_order INTEGER NOT NULL
            );

            -- User-curated Favorites (docs/features/FAVORITES.md).
            -- `id` is a UUID string; `kind` is FavoriteKind::as_db_code;
            -- (target_kind, target_value) is FavoriteTarget split into
            -- discriminant + payload; `display_name` NULL means "follow
            -- target basename"; `custom_icon` NULL means "default for kind".
            -- `sort_index` is a fractional order key: reorders touch one
            -- row, not the whole list.
            CREATE TABLE IF NOT EXISTS favorites (
                id TEXT PRIMARY KEY,
                kind INTEGER NOT NULL,
                target_kind TEXT NOT NULL,
                target_value TEXT NOT NULL,
                display_name TEXT,
                custom_icon TEXT,
                sort_index REAL NOT NULL,
                date_added INTEGER NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_favorites_sort ON favorites(sort_index);
            "#,
        )?;
        // v2 → v3 forward migration. SQLite has no IF NOT EXISTS for
        // ADD COLUMN; "duplicate column" errors mean we already
        // migrated and can ignore the failure.
        let _ = self
            .conn
            .execute("ALTER TABLE files ADD COLUMN description TEXT", []);
        // v4 → v5 forward migration: folder_sizes gains recursive item
        // counts. Probe for the columns rather than relying on the
        // ADD-COLUMN error, so a crash between the two ALTERs still
        // heals. When they're absent (an old v4 DB), add them and clear
        // the count-less rows once: a fresh v5 DB already has the
        // columns via the CREATE above, so this whole block is skipped
        // and the cache survives every subsequent open.
        let has_counts = self
            .conn
            .prepare("SELECT file_count, dir_count FROM folder_sizes LIMIT 0")
            .is_ok();
        if !has_counts {
            let _ = self.conn.execute(
                "ALTER TABLE folder_sizes ADD COLUMN file_count INTEGER NOT NULL DEFAULT 0",
                [],
            );
            let _ = self.conn.execute(
                "ALTER TABLE folder_sizes ADD COLUMN dir_count INTEGER NOT NULL DEFAULT 0",
                [],
            );
            let _ = self.conn.execute("DELETE FROM folder_sizes", []);
        }
        Ok(())
    }

    // ---- preferences ----

    pub fn set_preference(&self, key: &str, value: &str) -> Result<()> {
        self.conn.execute(
            "INSERT OR REPLACE INTO preferences (key, value) VALUES (?1, ?2)",
            params![key, value],
        )?;
        Ok(())
    }
    pub fn get_preference(&self, key: &str) -> Result<Option<String>> {
        let mut stmt = self
            .conn
            .prepare("SELECT value FROM preferences WHERE key = ?1")?;
        match stmt.query_row(params![key], |row| row.get::<_, String>(0)) {
            Ok(v) => Ok(Some(v)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e.into()),
        }
    }
    pub fn get_preference_or(&self, key: &str, default: &str) -> String {
        self.get_preference(key)
            .ok()
            .flatten()
            .unwrap_or_else(|| default.to_string())
    }

    // ---- ant trail ----

    /// Replace the entire folder_usage table with `entries`.
    pub fn save_ant_trail(&self, entries: &[AntTrailEntry]) -> Result<()> {
        self.conn.execute_batch("BEGIN IMMEDIATE")?;
        let result = (|| -> Result<()> {
            self.conn.execute("DELETE FROM folder_usage", [])?;
            let mut stmt = self.conn.prepare(
                "INSERT INTO folder_usage (folder_path, hits, last_access_unix) \
                 VALUES (?1, ?2, ?3)",
            )?;
            for e in entries {
                stmt.execute(params![e.folder_path, e.hits, e.last_access_unix])?;
            }
            Ok(())
        })();
        if result.is_err() {
            let _ = self.conn.execute_batch("ROLLBACK");
            return result;
        }
        self.conn.execute_batch("COMMIT")?;
        Ok(())
    }

    /// Load the folder_usage table, ordered by hits descending.
    pub fn load_ant_trail(&self) -> Result<Vec<AntTrailEntry>> {
        let mut stmt = self.conn.prepare(
            "SELECT folder_path, hits, last_access_unix FROM folder_usage \
             ORDER BY hits DESC",
        )?;
        let rows = stmt
            .query_map([], |row| {
                Ok(AntTrailEntry {
                    folder_path: row.get(0)?,
                    hits: row.get::<_, i64>(1)? as u32,
                    last_access_unix: row.get(2)?,
                })
            })?
            .filter_map(|r| r.ok())
            .collect();
        Ok(rows)
    }

    /// Record one folder visit. Atomically inserts-or-bumps the row.
    pub fn record_folder_visit(&self, path: &str, now_unix: i64) -> Result<()> {
        self.conn.execute(
            "INSERT INTO folder_usage (folder_path, hits, last_access_unix) \
             VALUES (?1, 1, ?2) \
             ON CONFLICT(folder_path) DO UPDATE SET \
               hits = hits + 1, \
               last_access_unix = excluded.last_access_unix",
            params![path, now_unix],
        )?;
        Ok(())
    }

    /// Drop a single folder from Recents *without* forgetting its Ant
    /// Trail heat. Recents (recency) and heat (frequency) are two
    /// columns of the same `folder_usage` row, so this zeroes only the
    /// recency signal (`last_access_unix`) and leaves `hits`: and thus
    /// the heat tint: untouched. A `last_access_unix` of 0 is the
    /// "cleared" sentinel `load_recent_folders` filters out (real visits
    /// always stamp a positive epoch). Backs "Remove from Recents".
    pub fn forget_recent_access(&self, path: &str) -> Result<()> {
        self.conn.execute(
            "UPDATE folder_usage SET last_access_unix = 0 WHERE folder_path = ?1",
            params![path],
        )?;
        Ok(())
    }

    /// Clear the whole Recents list without touching Ant Trail heat:
    /// zero every row's `last_access_unix` (the recency signal) while
    /// keeping `hits` (the frequency signal). Backs "Clear Recents".
    /// See [`Self::forget_recent_access`] for the column split.
    pub fn clear_recent_access(&self) -> Result<()> {
        self.conn
            .execute("UPDATE folder_usage SET last_access_unix = 0", [])?;
        Ok(())
    }

    /// Folder paths ordered most-recently-visited first, capped at
    /// `limit`. Drives the Recents sidebar section's startup hydration.
    /// Rows whose recency was cleared (`last_access_unix == 0`, the
    /// sentinel set by [`Self::clear_recent_access`] /
    /// [`Self::forget_recent_access`]) are excluded: they may still
    /// carry heat via `hits`, but they're no longer "recent".
    pub fn load_recent_folders(&self, limit: usize) -> Result<Vec<String>> {
        let mut stmt = self.conn.prepare(
            "SELECT folder_path FROM folder_usage \
             WHERE last_access_unix > 0 \
             ORDER BY last_access_unix DESC LIMIT ?1",
        )?;
        let rows = stmt.query_map(params![limit as i64], |row| row.get::<_, String>(0))?;
        Ok(rows.filter_map(|r| r.ok()).collect())
    }

    // ---- files ----

    /// Look up a cached file row by path. Caller checks
    /// `mtime_unix` against the live filesystem to decide whether
    /// the row is still valid.
    pub fn get_file(&self, path: &str) -> Result<Option<FileMetaRecord>> {
        let mut stmt = self.conn.prepare(
            "SELECT path, mtime_unix, size, magic_label, description, partial_hash, full_hash, \
                    mime, quarantined, quarantine_agent, quarantine_iso, \
                    quarantine_where_from, indexed_at_unix \
             FROM files WHERE path = ?1",
        )?;
        match stmt.query_row(params![path], |row| {
            Ok(FileMetaRecord {
                path: row.get(0)?,
                mtime_unix: row.get(1)?,
                size: row.get::<_, i64>(2)? as u64,
                magic_label: row.get(3)?,
                description: row.get(4)?,
                partial_hash: row.get(5)?,
                full_hash: row.get(6)?,
                mime: row.get(7)?,
                quarantined: row.get::<_, Option<i64>>(8)?.map(|v| v != 0),
                quarantine_agent: row.get(9)?,
                quarantine_iso: row.get(10)?,
                quarantine_where_from: row.get(11)?,
                indexed_at_unix: row.get(12)?,
            })
        }) {
            Ok(r) => Ok(Some(r)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    /// Upsert a file row. Existing rows whose `mtime_unix` differs
    /// have their derived columns reset (the caller passes whatever
    /// fields it has; the rest stay NULL until next probe).
    pub fn upsert_file(&self, rec: &FileMetaRecord) -> Result<()> {
        // ONE statement, atomic: when the stored mtime differs the row
        // is effectively replaced (stale derived data must not ride
        // alongside a changed file), otherwise incoming NULLs preserve
        // existing derived fields. The previous SELECT + DELETE +
        // INSERT shape cost three autocommit statements per row
        // (~6 fsyncs pre-WAL) and wasn't atomic (a crash between
        // DELETE and INSERT dropped the row).
        self.conn.execute(
            "INSERT INTO files (path, mtime_unix, size, magic_label, description, partial_hash, \
                                full_hash, mime, quarantined, quarantine_agent, \
                                quarantine_iso, quarantine_where_from, indexed_at_unix) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13) \
             ON CONFLICT(path) DO UPDATE SET \
               size = excluded.size, \
               magic_label = CASE WHEN files.mtime_unix = excluded.mtime_unix \
                 THEN COALESCE(excluded.magic_label, files.magic_label) ELSE excluded.magic_label END, \
               description = CASE WHEN files.mtime_unix = excluded.mtime_unix \
                 THEN COALESCE(excluded.description, files.description) ELSE excluded.description END, \
               partial_hash = CASE WHEN files.mtime_unix = excluded.mtime_unix \
                 THEN COALESCE(excluded.partial_hash, files.partial_hash) ELSE excluded.partial_hash END, \
               full_hash = CASE WHEN files.mtime_unix = excluded.mtime_unix \
                 THEN COALESCE(excluded.full_hash, files.full_hash) ELSE excluded.full_hash END, \
               mime = CASE WHEN files.mtime_unix = excluded.mtime_unix \
                 THEN COALESCE(excluded.mime, files.mime) ELSE excluded.mime END, \
               quarantined = CASE WHEN files.mtime_unix = excluded.mtime_unix \
                 THEN COALESCE(excluded.quarantined, files.quarantined) ELSE excluded.quarantined END, \
               quarantine_agent = CASE WHEN files.mtime_unix = excluded.mtime_unix \
                 THEN COALESCE(excluded.quarantine_agent, files.quarantine_agent) ELSE excluded.quarantine_agent END, \
               quarantine_iso = CASE WHEN files.mtime_unix = excluded.mtime_unix \
                 THEN COALESCE(excluded.quarantine_iso, files.quarantine_iso) ELSE excluded.quarantine_iso END, \
               quarantine_where_from = CASE WHEN files.mtime_unix = excluded.mtime_unix \
                 THEN COALESCE(excluded.quarantine_where_from, files.quarantine_where_from) ELSE excluded.quarantine_where_from END, \
               mtime_unix = excluded.mtime_unix, \
               indexed_at_unix = excluded.indexed_at_unix",
            params![
                rec.path,
                rec.mtime_unix,
                rec.size as i64,
                rec.magic_label,
                rec.description,
                rec.partial_hash,
                rec.full_hash,
                rec.mime,
                rec.quarantined.map(|v| v as i64),
                rec.quarantine_agent,
                rec.quarantine_iso,
                rec.quarantine_where_from,
                rec.indexed_at_unix,
            ],
        )?;
        Ok(())
    }

    /// Upsert a batch of file records in ONE transaction. The hot
    /// writers (magic/quarantine prefetch over a whole directory, the
    /// dupe hash cache) call this instead of per-row [`Self::upsert_file`]
    /// autocommits: a 5,000-entry folder was ~10k autocommit
    /// statements serialized behind the connection mutex.
    pub fn upsert_files(&self, recs: &[FileMetaRecord]) -> Result<()> {
        if recs.is_empty() {
            return Ok(());
        }
        self.conn.execute_batch("BEGIN IMMEDIATE")?;
        for rec in recs {
            if let Err(e) = self.upsert_file(rec) {
                let _ = self.conn.execute_batch("ROLLBACK");
                return Err(e);
            }
        }
        self.conn.execute_batch("COMMIT")?;
        Ok(())
    }

    /// Age out cache rows so the store stops growing without bound:
    /// `files` and `folder_sizes` entries not refreshed in 90 days are
    /// dead weight (their mtime checks would re-derive anyway), and
    /// `folder_usage`: loaded wholesale into memory at startup: is
    /// capped to its most recent 4,096 rows. User-curated tables
    /// (favorites, pinned items) are never touched. Run once per
    /// launch on the background executor.
    pub fn prune_stale(&self, now_unix: i64) -> Result<()> {
        const MAX_AGE_SECS: i64 = 90 * 86_400;
        const FOLDER_USAGE_CAP: i64 = 4096;
        let cutoff = now_unix.saturating_sub(MAX_AGE_SECS);
        self.conn.execute(
            "DELETE FROM files WHERE indexed_at_unix < ?1",
            params![cutoff],
        )?;
        self.conn.execute(
            "DELETE FROM folder_sizes WHERE computed_at_unix < ?1",
            params![cutoff],
        )?;
        self.conn.execute(
            "DELETE FROM folder_usage WHERE folder_path NOT IN ( \
               SELECT folder_path FROM folder_usage \
               ORDER BY last_access_unix DESC LIMIT ?1)",
            params![FOLDER_USAGE_CAP],
        )?;
        Ok(())
    }

    // ---- folder sizes ----

    /// Look up a cached folder size by path. Caller checks
    /// `mtime_unix` against the live filesystem to decide whether
    /// the row is still valid.
    pub fn get_folder_size(&self, path: &str) -> Result<Option<FolderSizeRecord>> {
        let mut stmt = self.conn.prepare(
            "SELECT path, mtime_unix, size, computed_at_unix, file_count, dir_count \
             FROM folder_sizes WHERE path = ?1",
        )?;
        match stmt.query_row(params![path], |row| {
            Ok(FolderSizeRecord {
                path: row.get(0)?,
                mtime_unix: row.get(1)?,
                size: row.get::<_, i64>(2)? as u64,
                computed_at_unix: row.get(3)?,
                file_count: row.get::<_, i64>(4)? as u64,
                dir_count: row.get::<_, i64>(5)? as u64,
            })
        }) {
            Ok(r) => Ok(Some(r)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    /// Insert or replace a folder-size row. The table holds a single
    /// derived value, so a whole-row replace is always correct.
    pub fn upsert_folder_size(&self, rec: &FolderSizeRecord) -> Result<()> {
        self.conn.execute(
            "INSERT OR REPLACE INTO folder_sizes \
               (path, mtime_unix, size, computed_at_unix, file_count, dir_count) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                rec.path,
                rec.mtime_unix,
                rec.size as i64,
                rec.computed_at_unix,
                rec.file_count as i64,
                rec.dir_count as i64,
            ],
        )?;
        Ok(())
    }

    /// Drop the cached size for exactly `path`. Used to invalidate
    /// after an in-app mutation: a change deep inside a subtree
    /// leaves the folder's own `mtime` untouched, so the mtime
    /// fast-path can't tell the cached size is now wrong. The caller
    /// invalidates the mutated path *and its ancestors*; the next
    /// size pass recomputes them. Deleting an absent row is a no-op.
    pub fn delete_folder_size(&self, path: &str) -> Result<()> {
        self.conn
            .execute("DELETE FROM folder_sizes WHERE path = ?1", params![path])?;
        Ok(())
    }

    // ---- window / layout / tabs ----

    pub fn save_window_state(&self, s: &WindowState) -> Result<()> {
        self.conn.execute(
            "INSERT OR REPLACE INTO window_state (id, width, height, maximized) \
             VALUES (1, ?1, ?2, ?3)",
            params![s.width, s.height, s.maximized as i32],
        )?;
        Ok(())
    }
    pub fn load_window_state(&self) -> Result<Option<WindowState>> {
        let mut stmt = self
            .conn
            .prepare("SELECT width, height, maximized FROM window_state WHERE id = 1")?;
        match stmt.query_row([], |row| {
            Ok(WindowState {
                width: row.get(0)?,
                height: row.get(1)?,
                maximized: row.get::<_, i32>(2)? != 0,
            })
        }) {
            Ok(s) => Ok(Some(s)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    pub fn save_layout_state(&self, s: &LayoutState) -> Result<()> {
        self.conn.execute(
            "INSERT OR REPLACE INTO layout_state \
               (id, sidebar_width, preview_width, preview_visible, du_width, du_height, du_topn_width) \
             VALUES (1, ?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                s.sidebar_width,
                s.preview_width,
                s.preview_visible as i32,
                s.du_width,
                s.du_height,
                s.du_topn_width,
            ],
        )?;
        Ok(())
    }
    pub fn load_layout_state(&self) -> Result<Option<LayoutState>> {
        let mut stmt = self.conn.prepare(
            "SELECT sidebar_width, preview_width, preview_visible, \
                    du_width, du_height, du_topn_width \
             FROM layout_state WHERE id = 1",
        )?;
        match stmt.query_row([], |row| {
            Ok(LayoutState {
                sidebar_width: row.get(0)?,
                preview_width: row.get(1)?,
                preview_visible: row.get::<_, i32>(2)? != 0,
                du_width: row.get(3)?,
                du_height: row.get(4)?,
                du_topn_width: row.get(5)?,
            })
        }) {
            Ok(s) => Ok(Some(s)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    pub fn save_tabs(&self, tabs: &[TabState]) -> Result<()> {
        self.conn.execute_batch("BEGIN IMMEDIATE")?;
        let result = (|| -> Result<()> {
            self.conn.execute("DELETE FROM tabs", [])?;
            let mut stmt = self.conn.prepare(
                "INSERT INTO tabs (path, is_active, sort_order, scroll_offset, \
                                  selected_index, sort_column, sort_ascending) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            )?;
            for (i, t) in tabs.iter().enumerate() {
                stmt.execute(params![
                    t.path,
                    t.is_active as i32,
                    i as i64,
                    t.scroll_offset as f64,
                    t.selected_index,
                    t.sort_column,
                    t.sort_ascending as i32,
                ])?;
            }
            Ok(())
        })();
        if result.is_err() {
            let _ = self.conn.execute_batch("ROLLBACK");
            return result;
        }
        self.conn.execute_batch("COMMIT")?;
        Ok(())
    }
    pub fn load_tabs(&self) -> Result<Vec<TabState>> {
        let mut stmt = self.conn.prepare(
            "SELECT path, is_active, scroll_offset, selected_index, sort_column, sort_ascending \
             FROM tabs ORDER BY sort_order",
        )?;
        let rows = stmt
            .query_map([], |row| {
                Ok(TabState {
                    path: row.get(0)?,
                    is_active: row.get::<_, i32>(1)? != 0,
                    scroll_offset: row.get::<_, f64>(2)? as f32,
                    selected_index: row.get(3)?,
                    sort_column: row.get(4)?,
                    sort_ascending: row.get::<_, i32>(5)? != 0,
                })
            })?
            .filter_map(|r| r.ok())
            .collect();
        Ok(rows)
    }

    // ---- pinned items ----

    pub fn save_pinned_items(&self, paths: &[String]) -> Result<()> {
        self.conn.execute_batch("BEGIN IMMEDIATE")?;
        let result = (|| -> Result<()> {
            self.conn.execute("DELETE FROM pinned_items", [])?;
            let mut stmt = self
                .conn
                .prepare("INSERT INTO pinned_items (path, sort_order) VALUES (?1, ?2)")?;
            for (i, p) in paths.iter().enumerate() {
                stmt.execute(params![p, i as i64])?;
            }
            Ok(())
        })();
        if result.is_err() {
            let _ = self.conn.execute_batch("ROLLBACK");
            return result;
        }
        self.conn.execute_batch("COMMIT")?;
        Ok(())
    }
    /// Wipe a slice of the DB. See [`ResetScope`] for the
    /// granularity options. `ResetScope::All` is the only variant
    /// that touches `preferences.db_version`; every other scope
    /// leaves the version row intact so subsequent opens don't
    /// trigger the version-mismatch recreate path.
    pub fn reset(&self, scope: ResetScope) -> Result<()> {
        match scope {
            ResetScope::All => {
                self.conn.execute_batch(
                    r#"
                    DELETE FROM files;
                    DELETE FROM folder_sizes;
                    DELETE FROM folder_usage;
                    DELETE FROM window_state;
                    DELETE FROM layout_state;
                    DELETE FROM tabs;
                    DELETE FROM pinned_items;
                    DELETE FROM favorites;
                    DELETE FROM preferences WHERE key != 'db_version';
                    "#,
                )?;
            }
            ResetScope::Ui => {
                self.conn.execute_batch(
                    r#"
                    DELETE FROM window_state;
                    DELETE FROM layout_state;
                    DELETE FROM tabs;
                    DELETE FROM pinned_items;
                    "#,
                )?;
            }
            ResetScope::Caches => {
                self.conn.execute_batch(
                    r#"
                    DELETE FROM files;
                    DELETE FROM folder_sizes;
                    DELETE FROM folder_usage;
                    "#,
                )?;
            }
            ResetScope::AntTrail => {
                self.conn.execute("DELETE FROM folder_usage", [])?;
            }
            ResetScope::Magic => {
                self.conn.execute(
                    "UPDATE files SET magic_label = NULL, description = NULL",
                    [],
                )?;
            }
            ResetScope::Quarantine => {
                self.conn.execute(
                    "UPDATE files \
                     SET quarantined = NULL, \
                         quarantine_agent = NULL, \
                         quarantine_iso = NULL, \
                         quarantine_where_from = NULL",
                    [],
                )?;
            }
            ResetScope::Favorites => {
                self.conn.execute("DELETE FROM favorites", [])?;
                self.conn.execute(
                    "DELETE FROM preferences WHERE key = 'favorites_section_collapsed'",
                    [],
                )?;
            }
        }
        Ok(())
    }

    pub fn load_pinned_items(&self) -> Result<Vec<String>> {
        let mut stmt = self
            .conn
            .prepare("SELECT path FROM pinned_items ORDER BY sort_order")?;
        let rows = stmt
            .query_map([], |row| row.get::<_, String>(0))?
            .filter_map(|r| r.ok())
            .collect();
        Ok(rows)
    }

    // ---- favorites ----

    /// Load all favorites, ordered by `sort_index` ascending. Per-row
    /// parse failures (bad UUID, unknown kind/target/icon code) are
    /// skipped and logged to stderr; the table itself is never wiped
    /// or corrupted by a single bad row. If the SQL query itself fails
    /// (e.g. the table is structurally damaged), the error propagates
    /// to the caller: `ferail-gpui` is responsible for the
    /// load-empty-and-keep-`.bak` recovery policy at that layer.
    pub fn load_favorites(&self) -> Result<Vec<Favorite>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, kind, target_kind, target_value, display_name, custom_icon, \
                    sort_index, date_added \
             FROM favorites ORDER BY sort_index ASC",
        )?;
        let rows = stmt.query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, i64>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, Option<String>>(4)?,
                row.get::<_, Option<String>>(5)?,
                row.get::<_, f64>(6)?,
                row.get::<_, i64>(7)?,
            ))
        })?;
        let mut out = Vec::new();
        for row in rows {
            let (id_str, kind_code, tk, tv, name, icon, sort_index, date_added) = match row {
                Ok(r) => r,
                Err(e) => {
                    eprintln!("favorites: skipping unreadable row: {e}");
                    continue;
                }
            };
            let Ok(id) = id_str.parse::<FavoriteId>() else {
                eprintln!("favorites: skipping row with bad UUID: {id_str}");
                continue;
            };
            let Some(kind) = FavoriteKind::from_db_code(kind_code) else {
                eprintln!("favorites: skipping row {id_str} with unknown kind {kind_code}");
                continue;
            };
            let Some(target) = FavoriteTarget::from_db(&tk, &tv) else {
                eprintln!("favorites: skipping row {id_str} with bad target ({tk}, {tv})");
                continue;
            };
            let custom_icon = icon.as_deref().and_then(FavoriteIcon::from_db);
            out.push(Favorite {
                id,
                kind,
                target,
                display_name: name,
                custom_icon,
                sort_index,
                date_added,
            });
        }
        Ok(out)
    }

    /// Insert or update a single favorite. Used by every mutation in
    /// the spec's "every mutation persists immediately" contract.
    pub fn save_favorite(&self, fav: &Favorite) -> Result<()> {
        let (tk, tv) = fav.target.to_db();
        let icon_str: Option<String> = fav.custom_icon.as_ref().map(|i| i.to_db());
        self.conn.execute(
            "INSERT INTO favorites \
               (id, kind, target_kind, target_value, display_name, custom_icon, \
                sort_index, date_added) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8) \
             ON CONFLICT(id) DO UPDATE SET \
               kind = excluded.kind, \
               target_kind = excluded.target_kind, \
               target_value = excluded.target_value, \
               display_name = excluded.display_name, \
               custom_icon = excluded.custom_icon, \
               sort_index = excluded.sort_index, \
               date_added = excluded.date_added",
            params![
                fav.id.to_string(),
                fav.kind.as_db_code(),
                tk,
                tv,
                fav.display_name,
                icon_str,
                fav.sort_index,
                fav.date_added,
            ],
        )?;
        Ok(())
    }

    pub fn delete_favorite(&self, id: FavoriteId) -> Result<()> {
        self.conn.execute(
            "DELETE FROM favorites WHERE id = ?1",
            params![id.to_string()],
        )?;
        Ok(())
    }

    /// Replace the whole favorites table atomically. Used by one-shot
    /// sorts (§4.5) and by background renormalize passes.
    pub fn replace_favorites(&self, favs: &[Favorite]) -> Result<()> {
        self.conn.execute_batch("BEGIN IMMEDIATE")?;
        let result = (|| -> Result<()> {
            self.conn.execute("DELETE FROM favorites", [])?;
            for f in favs {
                self.save_favorite(f)?;
            }
            Ok(())
        })();
        if result.is_err() {
            let _ = self.conn.execute_batch("ROLLBACK");
            return result;
        }
        self.conn.execute_batch("COMMIT")?;
        Ok(())
    }

    /// Section-collapsed bit for the Favorites sidebar group.
    pub fn set_favorites_section_collapsed(&self, collapsed: bool) -> Result<()> {
        self.set_preference(
            "favorites_section_collapsed",
            if collapsed { "1" } else { "0" },
        )
    }
    pub fn favorites_section_collapsed(&self) -> bool {
        self.get_preference("favorites_section_collapsed")
            .ok()
            .flatten()
            .map(|v| v == "1")
            .unwrap_or(false)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn open_in_memory_and_set_db_version() {
        let db = MetadataDb::in_memory().unwrap();
        // db_version isn't seeded in the in-memory ctor, but
        // preferences should be writable.
        db.set_preference("k", "v").unwrap();
        assert_eq!(db.get_preference("k").unwrap().as_deref(), Some("v"));
        assert!(db.get_preference("missing").unwrap().is_none());
        assert_eq!(db.get_preference_or("missing", "fallback"), "fallback");
    }

    #[test]
    fn ant_trail_round_trip() {
        let db = MetadataDb::in_memory().unwrap();
        db.record_folder_visit("/a", 100).unwrap();
        db.record_folder_visit("/a", 200).unwrap();
        db.record_folder_visit("/b", 150).unwrap();
        let rows = db.load_ant_trail().unwrap();
        // /a has 2 hits, /b has 1; sorted by hits desc.
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].folder_path, "/a");
        assert_eq!(rows[0].hits, 2);
        assert_eq!(rows[0].last_access_unix, 200);
        assert_eq!(rows[1].folder_path, "/b");
        assert_eq!(rows[1].hits, 1);
    }

    #[test]
    fn clear_recent_access_keeps_heat() {
        // Clearing Recents must zero recency but preserve hits (heat),
        // since the two are independent columns of one row.
        let db = MetadataDb::in_memory().unwrap();
        db.record_folder_visit("/a", 100).unwrap();
        db.record_folder_visit("/a", 200).unwrap();
        db.record_folder_visit("/b", 150).unwrap();
        assert_eq!(db.load_recent_folders(10).unwrap().len(), 2);

        db.clear_recent_access().unwrap();

        // Recents is empty...
        assert!(db.load_recent_folders(10).unwrap().is_empty());
        // ...but the heat (hits) survives for every folder.
        let rows = db.load_ant_trail().unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].folder_path, "/a");
        assert_eq!(rows[0].hits, 2);
        assert_eq!(rows[0].last_access_unix, 0);

        // A fresh visit re-enters Recents and bumps heat from where it was.
        db.record_folder_visit("/a", 300).unwrap();
        assert_eq!(db.load_recent_folders(10).unwrap(), vec!["/a".to_string()]);
        assert_eq!(db.load_ant_trail().unwrap()[0].hits, 3);
    }

    #[test]
    fn forget_recent_access_keeps_heat_for_one_folder() {
        let db = MetadataDb::in_memory().unwrap();
        db.record_folder_visit("/a", 100).unwrap();
        db.record_folder_visit("/b", 150).unwrap();

        db.forget_recent_access("/a").unwrap();

        // /a drops off Recents; /b stays.
        assert_eq!(db.load_recent_folders(10).unwrap(), vec!["/b".to_string()]);
        // Both keep their heat row.
        assert_eq!(db.load_ant_trail().unwrap().len(), 2);
    }

    #[test]
    fn ant_trail_save_replaces_table() {
        let db = MetadataDb::in_memory().unwrap();
        db.record_folder_visit("/a", 100).unwrap();
        db.save_ant_trail(&[AntTrailEntry {
            folder_path: "/x".into(),
            hits: 5,
            last_access_unix: 500,
        }])
        .unwrap();
        let rows = db.load_ant_trail().unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].folder_path, "/x");
        assert_eq!(rows[0].hits, 5);
    }

    #[test]
    fn folder_size_round_trip_and_replace() {
        let db = MetadataDb::in_memory().unwrap();
        assert!(db.get_folder_size("/dir").unwrap().is_none());
        db.upsert_folder_size(&FolderSizeRecord {
            path: "/dir".into(),
            mtime_unix: 100,
            size: 12_345,
            computed_at_unix: 100,
            file_count: 7,
            dir_count: 2,
        })
        .unwrap();
        let r = db.get_folder_size("/dir").unwrap().unwrap();
        assert_eq!(r.mtime_unix, 100);
        assert_eq!(r.size, 12_345);
        assert_eq!(r.file_count, 7);
        assert_eq!(r.dir_count, 2);

        // Recompute after the folder changed: whole-row replace.
        db.upsert_folder_size(&FolderSizeRecord {
            path: "/dir".into(),
            mtime_unix: 200,
            size: 99,
            computed_at_unix: 200,
            file_count: 1,
            dir_count: 0,
        })
        .unwrap();
        let r = db.get_folder_size("/dir").unwrap().unwrap();
        assert_eq!(r.mtime_unix, 200);
        assert_eq!(r.size, 99);
        assert_eq!(r.file_count, 1);
        assert_eq!(r.dir_count, 0);
    }

    #[test]
    fn folder_size_delete_invalidates() {
        let db = MetadataDb::in_memory().unwrap();
        db.upsert_folder_size(&FolderSizeRecord {
            path: "/dir".into(),
            mtime_unix: 100,
            size: 42,
            computed_at_unix: 100,
            file_count: 3,
            dir_count: 1,
        })
        .unwrap();
        assert!(db.get_folder_size("/dir").unwrap().is_some());
        db.delete_folder_size("/dir").unwrap();
        assert!(db.get_folder_size("/dir").unwrap().is_none());
        // Deleting an absent row is a no-op, not an error.
        db.delete_folder_size("/dir").unwrap();
    }

    #[test]
    fn reset_caches_wipes_folder_sizes() {
        let db = MetadataDb::in_memory().unwrap();
        db.upsert_folder_size(&FolderSizeRecord {
            path: "/dir".into(),
            mtime_unix: 100,
            size: 1,
            computed_at_unix: 100,
            file_count: 0,
            dir_count: 0,
        })
        .unwrap();
        db.reset(ResetScope::Caches).unwrap();
        assert!(db.get_folder_size("/dir").unwrap().is_none());
    }

    #[test]
    fn file_upsert_clears_stale_derived_data() {
        let db = MetadataDb::in_memory().unwrap();
        db.upsert_file(&FileMetaRecord {
            path: "/x.txt".into(),
            mtime_unix: 100,
            size: 10,
            magic_label: Some("Plain text".into()),
            description: None,
            partial_hash: Some("abc".into()),
            full_hash: Some("def".into()),
            mime: None,
            quarantined: None,
            quarantine_agent: None,
            quarantine_iso: None,
            quarantine_where_from: None,
            indexed_at_unix: 100,
        })
        .unwrap();
        // Same mtime, partial update: old hash preserved.
        db.upsert_file(&FileMetaRecord {
            path: "/x.txt".into(),
            mtime_unix: 100,
            size: 10,
            magic_label: None,
            description: None,
            partial_hash: None,
            full_hash: None,
            mime: Some("text/plain".into()),
            quarantined: None,
            quarantine_agent: None,
            quarantine_iso: None,
            quarantine_where_from: None,
            indexed_at_unix: 200,
        })
        .unwrap();
        let r = db.get_file("/x.txt").unwrap().unwrap();
        assert_eq!(r.partial_hash.as_deref(), Some("abc"));
        assert_eq!(r.full_hash.as_deref(), Some("def"));
        assert_eq!(r.mime.as_deref(), Some("text/plain"));

        // mtime changed: stale derived data must be cleared.
        db.upsert_file(&FileMetaRecord {
            path: "/x.txt".into(),
            mtime_unix: 999,
            size: 20,
            magic_label: None,
            description: None,
            partial_hash: None,
            full_hash: None,
            mime: None,
            quarantined: None,
            quarantine_agent: None,
            quarantine_iso: None,
            quarantine_where_from: None,
            indexed_at_unix: 999,
        })
        .unwrap();
        let r = db.get_file("/x.txt").unwrap().unwrap();
        assert_eq!(r.mtime_unix, 999);
        assert_eq!(r.size, 20);
        assert!(r.partial_hash.is_none());
        assert!(r.full_hash.is_none());
        assert!(r.magic_label.is_none());
        assert!(r.mime.is_none());
    }

    #[test]
    fn window_layout_tabs_round_trip() {
        let db = MetadataDb::in_memory().unwrap();
        assert!(db.load_window_state().unwrap().is_none());

        db.save_window_state(&WindowState {
            width: 1400,
            height: 900,
            maximized: false,
        })
        .unwrap();
        let w = db.load_window_state().unwrap().unwrap();
        assert_eq!(w.width, 1400);
        assert_eq!(w.height, 900);

        db.save_layout_state(&LayoutState {
            sidebar_width: 240,
            preview_width: 360,
            preview_visible: true,
            du_width: 1500,
            du_height: 950,
            du_topn_width: 320,
        })
        .unwrap();
        let l = db.load_layout_state().unwrap().unwrap();
        assert_eq!(l.sidebar_width, 240);
        assert!(l.preview_visible);
        assert_eq!(l.du_width, 1500);
        assert_eq!(l.du_topn_width, 320);

        db.save_tabs(&[
            TabState {
                path: "/a".into(),
                is_active: true,
                scroll_offset: 100.0,
                selected_index: 5,
                sort_column: 0,
                sort_ascending: true,
            },
            TabState {
                path: "/b".into(),
                is_active: false,
                scroll_offset: 0.0,
                selected_index: -1,
                sort_column: 1,
                sort_ascending: false,
            },
        ])
        .unwrap();
        let tabs = db.load_tabs().unwrap();
        assert_eq!(tabs.len(), 2);
        assert_eq!(tabs[0].path, "/a");
        assert!(tabs[0].is_active);
        assert!((tabs[0].scroll_offset - 100.0).abs() < 0.01);
        assert_eq!(tabs[1].selected_index, -1);
    }

    #[test]
    fn pinned_items_round_trip() {
        let db = MetadataDb::in_memory().unwrap();
        db.save_pinned_items(&["/a".into(), "/b".into(), "/c".into()])
            .unwrap();
        let p = db.load_pinned_items().unwrap();
        assert_eq!(p, vec!["/a".to_string(), "/b".into(), "/c".into()]);
        // Replace.
        db.save_pinned_items(&["/x".into()]).unwrap();
        assert_eq!(db.load_pinned_items().unwrap(), vec!["/x".to_string()]);
    }

    fn rec(path: &str) -> FileMetaRecord {
        FileMetaRecord {
            path: path.into(),
            mtime_unix: 100,
            size: 10,
            magic_label: Some("Plain text".into()),
            description: None,
            partial_hash: Some("abc".into()),
            full_hash: Some("def".into()),
            mime: None,
            quarantined: Some(true),
            quarantine_agent: Some("Safari".into()),
            quarantine_iso: Some("2026-05-01".into()),
            quarantine_where_from: Some("https://example.com".into()),
            indexed_at_unix: 100,
        }
    }

    fn seed_state(db: &MetadataDb) {
        db.record_folder_visit("/a", 100).unwrap();
        db.upsert_file(&rec("/f")).unwrap();
        db.save_window_state(&WindowState {
            width: 1000,
            height: 700,
            maximized: false,
        })
        .unwrap();
        db.save_tabs(&[TabState {
            path: "/a".into(),
            is_active: true,
            scroll_offset: 0.0,
            selected_index: -1,
            sort_column: 0,
            sort_ascending: true,
        }])
        .unwrap();
        db.save_pinned_items(&["/p".into()]).unwrap();
        db.set_preference("custom", "value").unwrap();
    }

    #[test]
    fn reset_all_wipes_everything_but_db_version() {
        let db = MetadataDb::in_memory().unwrap();
        db.set_preference("db_version", "1").unwrap();
        seed_state(&db);
        db.reset(ResetScope::All).unwrap();
        assert!(db.load_ant_trail().unwrap().is_empty());
        assert!(db.get_file("/f").unwrap().is_none());
        assert!(db.load_window_state().unwrap().is_none());
        assert!(db.load_tabs().unwrap().is_empty());
        assert!(db.load_pinned_items().unwrap().is_empty());
        assert!(db.get_preference("custom").unwrap().is_none());
        // db_version row preserved so subsequent opens don't recreate.
        assert_eq!(
            db.get_preference("db_version").unwrap().as_deref(),
            Some("1")
        );
    }

    #[test]
    fn reset_ui_leaves_derived_caches_intact() {
        let db = MetadataDb::in_memory().unwrap();
        seed_state(&db);
        db.reset(ResetScope::Ui).unwrap();
        // UI gone:
        assert!(db.load_window_state().unwrap().is_none());
        assert!(db.load_tabs().unwrap().is_empty());
        assert!(db.load_pinned_items().unwrap().is_empty());
        // Caches survive:
        assert_eq!(db.load_ant_trail().unwrap().len(), 1);
        assert!(db.get_file("/f").unwrap().is_some());
    }

    #[test]
    fn reset_caches_leaves_ui_intact() {
        let db = MetadataDb::in_memory().unwrap();
        seed_state(&db);
        db.reset(ResetScope::Caches).unwrap();
        assert!(db.load_ant_trail().unwrap().is_empty());
        assert!(db.get_file("/f").unwrap().is_none());
        assert!(db.load_window_state().unwrap().is_some());
        assert!(!db.load_tabs().unwrap().is_empty());
    }

    #[test]
    fn reset_ant_trail_only() {
        let db = MetadataDb::in_memory().unwrap();
        seed_state(&db);
        db.reset(ResetScope::AntTrail).unwrap();
        assert!(db.load_ant_trail().unwrap().is_empty());
        assert!(db.get_file("/f").unwrap().is_some());
    }

    #[test]
    fn reset_magic_keeps_quarantine_and_hashes() {
        let db = MetadataDb::in_memory().unwrap();
        seed_state(&db);
        db.reset(ResetScope::Magic).unwrap();
        let f = db.get_file("/f").unwrap().unwrap();
        assert!(f.magic_label.is_none());
        assert_eq!(f.quarantine_agent.as_deref(), Some("Safari"));
        assert_eq!(f.full_hash.as_deref(), Some("def"));
    }

    #[test]
    fn reset_quarantine_keeps_magic_and_hashes() {
        let db = MetadataDb::in_memory().unwrap();
        seed_state(&db);
        db.reset(ResetScope::Quarantine).unwrap();
        let f = db.get_file("/f").unwrap().unwrap();
        assert!(f.quarantined.is_none());
        assert!(f.quarantine_agent.is_none());
        assert!(f.quarantine_iso.is_none());
        assert!(f.quarantine_where_from.is_none());
        assert_eq!(f.magic_label.as_deref(), Some("Plain text"));
        assert_eq!(f.full_hash.as_deref(), Some("def"));
    }

    #[test]
    fn reset_scope_from_cli_parses_known() {
        assert_eq!(ResetScope::from_cli("ALL"), Some(ResetScope::All));
        assert_eq!(ResetScope::from_cli("ui"), Some(ResetScope::Ui));
        assert_eq!(ResetScope::from_cli("cache"), Some(ResetScope::Caches));
        assert_eq!(
            ResetScope::from_cli("ant-trail"),
            Some(ResetScope::AntTrail)
        );
        assert_eq!(
            ResetScope::from_cli("ant_trail"),
            Some(ResetScope::AntTrail)
        );
        assert_eq!(ResetScope::from_cli("magic"), Some(ResetScope::Magic));
        assert_eq!(
            ResetScope::from_cli("quarantine"),
            Some(ResetScope::Quarantine)
        );
        assert_eq!(
            ResetScope::from_cli("favorites"),
            Some(ResetScope::Favorites)
        );
        assert_eq!(
            ResetScope::from_cli("favourites"),
            Some(ResetScope::Favorites)
        );
        assert!(ResetScope::from_cli("bogus").is_none());
    }

    // ---- favorites ----

    fn fav(label: &str, path: &str, sort_index: f64) -> Favorite {
        Favorite {
            id: FavoriteId::new(),
            kind: FavoriteKind::Folder,
            target: FavoriteTarget::Path(std::path::PathBuf::from(path)),
            display_name: Some(label.to_string()),
            custom_icon: None,
            sort_index,
            date_added: 1_700_000_000,
        }
    }

    #[test]
    fn favorites_save_load_round_trip_preserves_order_and_fields() {
        let db = MetadataDb::in_memory().unwrap();
        let a = fav("Alpha", "/a", 1024.0);
        let b = fav("Beta", "/b", 2048.0);
        let c = fav("Gamma", "/c", 3072.0);
        // Save out of order: load_favorites must sort by sort_index.
        db.save_favorite(&c).unwrap();
        db.save_favorite(&a).unwrap();
        db.save_favorite(&b).unwrap();
        let loaded = db.load_favorites().unwrap();
        let labels: Vec<&str> = loaded
            .iter()
            .map(|f| f.display_name.as_deref().unwrap_or(""))
            .collect();
        assert_eq!(labels, ["Alpha", "Beta", "Gamma"]);
        assert_eq!(loaded[0].id, a.id);
        assert_eq!(loaded[0].target, a.target);
    }

    #[test]
    fn favorites_save_is_upsert() {
        let db = MetadataDb::in_memory().unwrap();
        let mut f = fav("Old", "/x", 1.0);
        db.save_favorite(&f).unwrap();
        f.display_name = Some("New".into());
        f.sort_index = 2.0;
        db.save_favorite(&f).unwrap();
        let loaded = db.load_favorites().unwrap();
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].display_name.as_deref(), Some("New"));
        assert_eq!(loaded[0].sort_index, 2.0);
    }

    #[test]
    fn favorites_delete_removes_one_row() {
        let db = MetadataDb::in_memory().unwrap();
        let a = fav("A", "/a", 1.0);
        let b = fav("B", "/b", 2.0);
        db.save_favorite(&a).unwrap();
        db.save_favorite(&b).unwrap();
        db.delete_favorite(a.id).unwrap();
        let loaded = db.load_favorites().unwrap();
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].id, b.id);
    }

    #[test]
    fn favorites_replace_is_atomic() {
        let db = MetadataDb::in_memory().unwrap();
        db.save_favorite(&fav("Old", "/old", 1.0)).unwrap();
        let fresh = vec![fav("X", "/x", 1.0), fav("Y", "/y", 2.0)];
        db.replace_favorites(&fresh).unwrap();
        let loaded = db.load_favorites().unwrap();
        let labels: Vec<&str> = loaded
            .iter()
            .map(|f| f.display_name.as_deref().unwrap_or(""))
            .collect();
        assert_eq!(labels, ["X", "Y"]);
    }

    #[test]
    fn favorites_load_skips_unparseable_rows() {
        let db = MetadataDb::in_memory().unwrap();
        // Insert one good row and three broken rows directly.
        let good = fav("Good", "/good", 1.0);
        db.save_favorite(&good).unwrap();
        db.conn
            .execute(
                "INSERT INTO favorites (id, kind, target_kind, target_value, display_name, \
                  custom_icon, sort_index, date_added) \
                 VALUES ('not-a-uuid', 1, 'path', '/bad-uuid', NULL, NULL, 2.0, 0)",
                [],
            )
            .unwrap();
        db.conn
            .execute(
                "INSERT INTO favorites (id, kind, target_kind, target_value, display_name, \
                  custom_icon, sort_index, date_added) \
                 VALUES ('11111111-1111-1111-1111-111111111111', 99, 'path', '/bad-kind', \
                  NULL, NULL, 3.0, 0)",
                [],
            )
            .unwrap();
        db.conn
            .execute(
                "INSERT INTO favorites (id, kind, target_kind, target_value, display_name, \
                  custom_icon, sort_index, date_added) \
                 VALUES ('22222222-2222-2222-2222-222222222222', 1, 'galaxy', 'andromeda', \
                  NULL, NULL, 4.0, 0)",
                [],
            )
            .unwrap();

        let loaded = db.load_favorites().unwrap();
        assert_eq!(loaded.len(), 1, "only the good row should survive");
        assert_eq!(loaded[0].id, good.id);
    }

    #[test]
    fn favorites_custom_icon_round_trip() {
        let db = MetadataDb::in_memory().unwrap();
        let mut f = fav("Iconic", "/i", 1.0);
        f.custom_icon = Some(FavoriteIcon::Lucide("star".into()));
        db.save_favorite(&f).unwrap();
        let loaded = db.load_favorites().unwrap();
        assert_eq!(
            loaded[0].custom_icon,
            Some(FavoriteIcon::Lucide("star".into()))
        );
    }

    #[test]
    fn favorites_section_collapsed_round_trip() {
        let db = MetadataDb::in_memory().unwrap();
        assert!(!db.favorites_section_collapsed());
        db.set_favorites_section_collapsed(true).unwrap();
        assert!(db.favorites_section_collapsed());
        db.set_favorites_section_collapsed(false).unwrap();
        assert!(!db.favorites_section_collapsed());
    }

    #[test]
    fn reset_favorites_scope_only_touches_favorites() {
        let db = MetadataDb::in_memory().unwrap();
        seed_state(&db);
        db.save_favorite(&fav("F", "/f", 1.0)).unwrap();
        db.set_favorites_section_collapsed(true).unwrap();

        db.reset(ResetScope::Favorites).unwrap();

        assert!(db.load_favorites().unwrap().is_empty());
        assert!(!db.favorites_section_collapsed());
        // Other state survives.
        assert!(db.get_file("/f").unwrap().is_some());
        assert!(db.load_window_state().unwrap().is_some());
        assert!(!db.load_tabs().unwrap().is_empty());
    }

    #[test]
    fn reset_ui_leaves_favorites_intact() {
        let db = MetadataDb::in_memory().unwrap();
        seed_state(&db);
        db.save_favorite(&fav("F", "/f", 1.0)).unwrap();
        db.reset(ResetScope::Ui).unwrap();
        assert_eq!(
            db.load_favorites().unwrap().len(),
            1,
            "Ui must not wipe Favorites"
        );
    }

    #[test]
    fn reset_all_wipes_favorites_too() {
        let db = MetadataDb::in_memory().unwrap();
        seed_state(&db);
        db.save_favorite(&fav("F", "/f", 1.0)).unwrap();
        db.reset(ResetScope::All).unwrap();
        assert!(db.load_favorites().unwrap().is_empty());
    }
}
