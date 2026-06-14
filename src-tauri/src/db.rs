//! Durable SQLite persistence for the workspace layout (#sqlite phase 1).
//!
//! Today the workspace snapshot (tabs/order/sizes/focus/poppedOutTabs/fontSize)
//! lives only in the webview's `localStorage` (`src/store/workspace.ts`, key
//! `termhub.workspace.v2`). That is fine for round-tripping a single window but
//! is fragile: a cleared web cache, a corrupt profile, or a crash mid-write can
//! lose the arrangement, and there is no out-of-process copy to recover from.
//!
//! This module adds a small, durable SQLite copy as the foundation for recovery.
//! Phase 1 is deliberately minimal — reliable save/load of one JSON snapshot —
//! and the recovery-review UI lands in a later phase.
//!
//! Design:
//!   - A single SQLite database under Tauri's `app_data_dir` (`termhub.db`).
//!   - **WAL** journal mode + `synchronous=NORMAL`: durable across app crashes
//!     (a power loss can lose only the last in-flight commit), with far less
//!     fsync cost than the default `FULL` — the right trade for best-effort UI
//!     state we also mirror to localStorage.
//!   - One generic key/value table so future durable snapshots (recovery review,
//!     per-window state, ...) reuse it without a migration:
//!       `kv(key TEXT PRIMARY KEY, value TEXT, updated_at INTEGER)`.
//!   - `rusqlite` with the `bundled` feature so we compile SQLite in — no system
//!     sqlite needed inside WSL or on a clean CI box.
//!
//! Tauri surface (registered in `lib.rs`, mirrored by `src/ipc/persistence.ts`):
//!   - `save_workspace_snapshot(json: String)` — upsert key `workspace.v2`.
//!   - `load_workspace_snapshot() -> Option<String>` — select that key.
//!
//! The connection is wrapped in a `Mutex` and held in Tauri-managed state so the
//! two commands share one open handle (cheap upserts, no per-call open cost).

use std::path::PathBuf;
use std::sync::Mutex;

use rusqlite::Connection;
use tauri::Manager;

/// The kv key under which the workspace layout snapshot is stored. Mirrors the
/// localStorage key in `src/store/workspace.ts` so the two copies stay aligned.
pub const WORKSPACE_KEY: &str = "workspace.v2";

/// Database filename inside the app data dir.
const DB_FILE: &str = "termhub.db";

/// Tauri-managed state: the open SQLite connection behind a mutex. A `None`
/// connection means the DB could not be opened/initialized at startup; the two
/// commands degrade to no-ops in that case (the frontend keeps its localStorage
/// mirror, so persistence still works — it just loses the durable copy).
#[derive(Default)]
pub struct Db {
    conn: Mutex<Option<Connection>>,
}

impl Db {
    /// Open + initialize the database in `dir` (created if missing), returning a
    /// ready-to-manage `Db`. On any failure the returned `Db` holds `None` and
    /// logs the cause — startup is never aborted by a persistence-layer problem.
    pub fn open_in(dir: PathBuf) -> Self {
        match Self::try_open(&dir) {
            Ok(conn) => Db {
                conn: Mutex::new(Some(conn)),
            },
            Err(e) => {
                eprintln!(
                    "db: failed to open SQLite at {}: {e} (workspace falls back \
                     to localStorage only)",
                    dir.join(DB_FILE).display()
                );
                Db {
                    conn: Mutex::new(None),
                }
            }
        }
    }

    /// Open the connection, set WAL + NORMAL sync, and ensure the kv table.
    fn try_open(dir: &PathBuf) -> rusqlite::Result<Connection> {
        std::fs::create_dir_all(dir).map_err(|e| {
            rusqlite::Error::SqliteFailure(
                rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_CANTOPEN),
                Some(format!("create {}: {e}", dir.display())),
            )
        })?;
        let conn = Connection::open(dir.join(DB_FILE))?;
        // WAL: readers don't block the writer and a crash leaves a recoverable
        // log; NORMAL: fsync only at checkpoint, durable across app crashes.
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "synchronous", "NORMAL")?;
        conn.execute(
            "CREATE TABLE IF NOT EXISTS kv (
                 key        TEXT PRIMARY KEY,
                 value      TEXT NOT NULL,
                 updated_at INTEGER NOT NULL
             )",
            [],
        )?;
        Ok(conn)
    }

    /// Upsert `value` under `key`, stamping `updated_at` with the current epoch
    /// seconds. Returns the row error so the command can surface it.
    fn put(&self, key: &str, value: &str) -> rusqlite::Result<()> {
        let guard = self.conn.lock().expect("db mutex poisoned");
        let Some(conn) = guard.as_ref() else {
            return Ok(()); // no DB → no-op (localStorage mirror still holds)
        };
        conn.execute(
            "INSERT INTO kv(key, value, updated_at) VALUES(?1, ?2, ?3)
             ON CONFLICT(key) DO UPDATE SET value = ?2, updated_at = ?3",
            rusqlite::params![key, value, now_secs()],
        )?;
        Ok(())
    }

    /// Read the value stored under `key`, or `None` if absent (or no DB).
    fn get(&self, key: &str) -> rusqlite::Result<Option<String>> {
        let guard = self.conn.lock().expect("db mutex poisoned");
        let Some(conn) = guard.as_ref() else {
            return Ok(None);
        };
        match conn.query_row("SELECT value FROM kv WHERE key = ?1", [key], |row| {
            row.get::<_, String>(0)
        }) {
            Ok(v) => Ok(Some(v)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e),
        }
    }
}

/// Current Unix time in whole seconds (0 if the clock is before the epoch).
fn now_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// Resolve the app data dir and open the workspace DB there. Called once from
/// `setup()`; the resulting `Db` is `.manage()`d. A failure to resolve the dir
/// yields a `None`-backed `Db` (commands become no-ops) rather than aborting.
pub fn init(app: &tauri::AppHandle) -> Db {
    match app.path().app_data_dir() {
        Ok(dir) => Db::open_in(dir),
        Err(e) => {
            eprintln!("db: could not resolve app_data_dir: {e} (workspace falls back to localStorage only)");
            Db::default()
        }
    }
}

/// Persist the workspace layout snapshot (the JSON the frontend serializes).
/// Best-effort from the caller's view: a DB error is returned but the frontend
/// keeps its localStorage mirror, so a failure here never loses the live state.
#[tauri::command]
pub async fn save_workspace_snapshot(
    app: tauri::AppHandle,
    json: String,
) -> Result<(), String> {
    app.state::<Db>()
        .put(WORKSPACE_KEY, &json)
        .map_err(|e| format!("save_workspace_snapshot: {e}"))
}

/// Load the durable workspace layout snapshot, or `None` if nothing is stored
/// yet (fresh install, or the DB couldn't be opened). The frontend prefers this
/// over localStorage when present, and seeds it from localStorage once if empty.
#[tauri::command]
pub async fn load_workspace_snapshot(
    app: tauri::AppHandle,
) -> Result<Option<String>, String> {
    app.state::<Db>()
        .get(WORKSPACE_KEY)
        .map_err(|e| format!("load_workspace_snapshot: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A fresh DB in a unique temp dir, isolated per test (no env globals).
    fn temp_db() -> (Db, PathBuf) {
        let dir = std::env::temp_dir().join(format!(
            "termhub-db-test-{}-{:?}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        (Db::open_in(dir.clone()), dir)
    }

    #[test]
    fn missing_key_reads_none() {
        let (db, dir) = temp_db();
        assert_eq!(db.get(WORKSPACE_KEY).unwrap(), None);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn put_then_get_roundtrips() {
        let (db, dir) = temp_db();
        let json = r#"{"tabs":[{"id":"t1","name":"W1","order":["a","b"]}]}"#;
        db.put(WORKSPACE_KEY, json).unwrap();
        assert_eq!(db.get(WORKSPACE_KEY).unwrap().as_deref(), Some(json));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn put_upserts_in_place() {
        let (db, dir) = temp_db();
        db.put(WORKSPACE_KEY, "first").unwrap();
        db.put(WORKSPACE_KEY, "second").unwrap();
        assert_eq!(db.get(WORKSPACE_KEY).unwrap().as_deref(), Some("second"));
        // Exactly one row for the key (upsert, not insert).
        let guard = db.conn.lock().unwrap();
        let conn = guard.as_ref().unwrap();
        let n: i64 = conn
            .query_row("SELECT COUNT(*) FROM kv WHERE key = ?1", [WORKSPACE_KEY], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(n, 1);
        drop(guard);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn survives_reopen() {
        let (db, dir) = temp_db();
        db.put(WORKSPACE_KEY, "durable").unwrap();
        drop(db);
        // Reopen the same dir: the value must still be there (durable on disk).
        let db2 = Db::open_in(dir.clone());
        assert_eq!(db2.get(WORKSPACE_KEY).unwrap().as_deref(), Some("durable"));
        drop(db2);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn none_backed_db_is_noop() {
        // A Db with no connection (open failure path) accepts writes silently and
        // reads back None — the frontend's localStorage mirror covers it.
        let db = Db::default();
        assert!(db.put(WORKSPACE_KEY, "x").is_ok());
        assert_eq!(db.get(WORKSPACE_KEY).unwrap(), None);
    }
}
