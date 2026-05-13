//! SQLite-backed persistent metadata. Schema lifted from the Ferail
//! predecessor with macOS-flavored adjustments — `ino`/`dev` columns
//! on the `files` table for future move-aware identity, no Windows
//! drive-letter assumptions in `tabs.path`, and a `magic_label`
//! column so the magic cache can hydrate without a second table.
//!
//! Schema bumps: increment [`DB_VERSION`] when changing column shape.
//! On open, a stored version mismatch deletes the file and recreates
//! it — same hard-reset policy as Ferail. Caches built on top are
//! all derived data, so a recreate is cheap.

use std::path::Path;

use rusqlite::{params, Connection};

/// Schema version. Bump on any structural change.
pub const DB_VERSION: u32 = 1;

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
/// items if/when those land. Picking a narrow scope is the point —
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
    /// Only `folder_usage` — the Ant Trail heat map.
    AntTrail,
    /// `files.magic_label` only. Keeps quarantine + hash data.
    Magic,
    /// `files.quarantine_*` only. Keeps magic + hash data.
    Quarantine,
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
        }
    }
}

/// One row from the `folder_usage` table — Ant Trail heat snapshot.
#[derive(Debug, Clone)]
pub struct AntTrailEntry {
    pub folder_path: String,
    pub hits: u32,
    pub last_access_unix: i64,
}

/// One row from the `files` table — derived metadata cache. All
/// fields except `path` are optional so callers can update one
/// dimension at a time (write magic, write hash, write quarantine
/// independently).
#[derive(Debug, Clone)]
pub struct FileMetaRecord {
    pub path: String,
    pub mtime_unix: i64,
    pub size: u64,
    pub magic_label: Option<String>,
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

/// SQLite-backed metadata store. Single connection — wrap in a
/// `Mutex` at the call site if cross-thread access is needed.
pub struct MetadataDb {
    conn: Connection,
}

impl MetadataDb {
    /// Open or create the database at `path`. If the on-disk schema
    /// version doesn't match [`DB_VERSION`], the file is deleted
    /// and recreated. Caller is responsible for ensuring the parent
    /// directory exists ([`crate::ensure_parent_dir`]).
    pub fn open(path: &Path) -> Result<Self> {
        if path.exists() {
            if let Ok(conn) = Connection::open(path) {
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
                if stored != Some(DB_VERSION) {
                    drop(conn);
                    let _ = std::fs::remove_file(path);
                }
            } else {
                // Couldn't even open — corrupted file. Wipe and start over.
                let _ = std::fs::remove_file(path);
            }
        }
        let conn = Connection::open(path)?;
        let db = Self { conn };
        db.init_schema()?;
        db.set_preference("db_version", &DB_VERSION.to_string())?;
        Ok(db)
    }

    /// Open an in-memory database. Used by tests and by the
    /// screenshot harness when `$HOME` is unset.
    pub fn in_memory() -> Result<Self> {
        let conn = Connection::open_in_memory()?;
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

            -- Ant Trail folder-usage. `score` is computed at read time
            -- from hits + last_access; we only persist the raw signal.
            CREATE TABLE IF NOT EXISTS folder_usage (
                folder_path TEXT PRIMARY KEY,
                hits INTEGER NOT NULL DEFAULT 0,
                last_access_unix INTEGER NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_folder_hits ON folder_usage(hits DESC);

            -- Window state — single row.
            CREATE TABLE IF NOT EXISTS window_state (
                id INTEGER PRIMARY KEY CHECK (id = 1),
                width INTEGER NOT NULL DEFAULT 1180,
                height INTEGER NOT NULL DEFAULT 760,
                maximized INTEGER NOT NULL DEFAULT 0
            );

            -- Layout state — single row.
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

            -- Pinned sidebar items (ordered).
            CREATE TABLE IF NOT EXISTS pinned_items (
                id INTEGER PRIMARY KEY,
                path TEXT NOT NULL,
                sort_order INTEGER NOT NULL
            );
            "#,
        )?;
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

    // ---- files ----

    /// Look up a cached file row by path. Caller checks
    /// `mtime_unix` against the live filesystem to decide whether
    /// the row is still valid.
    pub fn get_file(&self, path: &str) -> Result<Option<FileMetaRecord>> {
        let mut stmt = self.conn.prepare(
            "SELECT path, mtime_unix, size, magic_label, partial_hash, full_hash, \
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
                partial_hash: row.get(4)?,
                full_hash: row.get(5)?,
                mime: row.get(6)?,
                quarantined: row
                    .get::<_, Option<i64>>(7)?
                    .map(|v| v != 0),
                quarantine_agent: row.get(8)?,
                quarantine_iso: row.get(9)?,
                quarantine_where_from: row.get(10)?,
                indexed_at_unix: row.get(11)?,
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
        // If the path exists with a different mtime, clear stale
        // derived data first so the new rec doesn't ride alongside
        // outdated hashes/magic/quarantine.
        let stale_mtime = self
            .conn
            .query_row(
                "SELECT mtime_unix FROM files WHERE path = ?1",
                params![rec.path],
                |row| row.get::<_, i64>(0),
            )
            .ok();
        if let Some(prev) = stale_mtime {
            if prev != rec.mtime_unix {
                self.conn
                    .execute("DELETE FROM files WHERE path = ?1", params![rec.path])?;
            }
        }
        self.conn.execute(
            "INSERT INTO files (path, mtime_unix, size, magic_label, partial_hash, \
                                full_hash, mime, quarantined, quarantine_agent, \
                                quarantine_iso, quarantine_where_from, indexed_at_unix) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12) \
             ON CONFLICT(path) DO UPDATE SET \
               mtime_unix = excluded.mtime_unix, \
               size = excluded.size, \
               magic_label = COALESCE(excluded.magic_label, files.magic_label), \
               partial_hash = COALESCE(excluded.partial_hash, files.partial_hash), \
               full_hash = COALESCE(excluded.full_hash, files.full_hash), \
               mime = COALESCE(excluded.mime, files.mime), \
               quarantined = COALESCE(excluded.quarantined, files.quarantined), \
               quarantine_agent = COALESCE(excluded.quarantine_agent, files.quarantine_agent), \
               quarantine_iso = COALESCE(excluded.quarantine_iso, files.quarantine_iso), \
               quarantine_where_from = COALESCE(excluded.quarantine_where_from, files.quarantine_where_from), \
               indexed_at_unix = excluded.indexed_at_unix",
            params![
                rec.path,
                rec.mtime_unix,
                rec.size as i64,
                rec.magic_label,
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
                    DELETE FROM folder_usage;
                    DELETE FROM window_state;
                    DELETE FROM layout_state;
                    DELETE FROM tabs;
                    DELETE FROM pinned_items;
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
                    DELETE FROM folder_usage;
                    "#,
                )?;
            }
            ResetScope::AntTrail => {
                self.conn.execute("DELETE FROM folder_usage", [])?;
            }
            ResetScope::Magic => {
                self.conn
                    .execute("UPDATE files SET magic_label = NULL", [])?;
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
    fn file_upsert_clears_stale_derived_data() {
        let db = MetadataDb::in_memory().unwrap();
        db.upsert_file(&FileMetaRecord {
            path: "/x.txt".into(),
            mtime_unix: 100,
            size: 10,
            magic_label: Some("Plain text".into()),
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
        // Same mtime, partial update — old hash preserved.
        db.upsert_file(&FileMetaRecord {
            path: "/x.txt".into(),
            mtime_unix: 100,
            size: 10,
            magic_label: None,
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

        // mtime changed — stale derived data must be cleared.
        db.upsert_file(&FileMetaRecord {
            path: "/x.txt".into(),
            mtime_unix: 999,
            size: 20,
            magic_label: None,
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
        assert_eq!(db.get_preference("db_version").unwrap().as_deref(), Some("1"));
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
        assert_eq!(ResetScope::from_cli("ant-trail"), Some(ResetScope::AntTrail));
        assert_eq!(ResetScope::from_cli("ant_trail"), Some(ResetScope::AntTrail));
        assert_eq!(ResetScope::from_cli("magic"), Some(ResetScope::Magic));
        assert_eq!(ResetScope::from_cli("quarantine"), Some(ResetScope::Quarantine));
        assert!(ResetScope::from_cli("bogus").is_none());
    }
}
