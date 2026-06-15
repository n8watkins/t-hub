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
//! ## Snapshot history (Recovery review, #recovery)
//!
//! The `kv` table above only ever holds the LATEST layout — once a save lands it
//! overwrites whatever was there. That is correct for boot-time hydration but
//! useless for *recovery*: if a crash, a botched drag, or a bad redeploy leaves
//! the layout wrong, the previous good arrangement is already gone.
//!
//! So alongside the kv upsert we also APPEND each save into a ring of recent
//! snapshots in a separate `snapshots` table:
//!   `snapshots(id INTEGER PRIMARY KEY, ts INTEGER, json TEXT)`.
//! After every append we cap the table to the last [`SNAPSHOT_HISTORY_CAP`] rows
//! (deleting the oldest), so the history stays bounded. The Recovery review UI
//! reads it back via two read-only commands:
//!   - `list_snapshots() -> Vec<SnapshotMeta>` — id + ts + a cheap summary
//!     ("N tabs · M terminals"), newest first, WITHOUT shipping every full JSON.
//!   - `get_snapshot(id) -> Option<String>` — the one full layout JSON to preview
//!     or restore.
//! All of this is best-effort and backward compatible: a history failure never
//! fails the kv save (the live save/load path is unchanged), and an absent DB
//! degrades to empty lists / `None`.
//!
//! The connection is wrapped in a `Mutex` and held in Tauri-managed state so the
//! commands share one open handle (cheap upserts, no per-call open cost).

use std::path::PathBuf;
use std::sync::{LazyLock, Mutex};

use rusqlite::Connection;
use serde::Serialize;
use tauri::Manager;

/// How many recent workspace snapshots to retain in the `snapshots` history ring.
/// Each save appends one row and then trims the table back to this many newest
/// rows. 20 is enough to roll back across a bad session without unbounded growth
/// (a layout JSON is a few KB at most).
pub const SNAPSHOT_HISTORY_CAP: i64 = 20;

/// The kv key under which the workspace layout snapshot is stored. Mirrors the
/// localStorage key in `src/store/workspace.ts` so the two copies stay aligned.
pub const WORKSPACE_KEY: &str = "workspace.v2";

/// Database filename inside the app data dir.
///
/// Resolved ONCE at startup from `$TERMHUB_DB_NAME`, defaulting to
/// `"termhub.db"`. The env hook exists so a side-by-side **DEV** instance can
/// keep its workspace state in a SEPARATE SQLite file (e.g.
/// `TERMHUB_DB_NAME=termhub-dev.db`) within the same app data dir, instead of
/// reading/writing production's `termhub.db`. With NO env var set the filename
/// is exactly `"termhub.db"`, so default behavior is byte-for-byte unchanged.
static DB_FILE: LazyLock<String> =
    LazyLock::new(|| std::env::var("TERMHUB_DB_NAME").unwrap_or_else(|_| "termhub.db".into()));

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
                    dir.join(&*DB_FILE).display()
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
        let conn = Connection::open(dir.join(&*DB_FILE))?;
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
        // Recovery-review history (#recovery): an append-only ring of recent
        // layout snapshots. `id` is an autoincrement-style rowid (monotonic, the
        // stable handle the UI passes back to get_snapshot); `ts` is epoch secs.
        conn.execute(
            "CREATE TABLE IF NOT EXISTS snapshots (
                 id   INTEGER PRIMARY KEY AUTOINCREMENT,
                 ts   INTEGER NOT NULL,
                 json TEXT NOT NULL
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

    /// Append one layout `json` to the snapshots history ring, then trim the
    /// table back to the newest [`SNAPSHOT_HISTORY_CAP`] rows. Best-effort: a
    /// `None`-backed DB is a no-op, and the caller treats any error as advisory
    /// (the kv save still succeeded). Done in a single transaction so an append +
    /// trim is atomic and concurrent readers never see the ring mid-trim.
    fn append_snapshot(&self, json: &str) -> rusqlite::Result<()> {
        let mut guard = self.conn.lock().expect("db mutex poisoned");
        let Some(conn) = guard.as_mut() else {
            return Ok(()); // no DB → no history (live kv path still holds)
        };
        let tx = conn.transaction()?;
        tx.execute(
            "INSERT INTO snapshots(ts, json) VALUES(?1, ?2)",
            rusqlite::params![now_secs(), json],
        )?;
        // Keep only the newest CAP rows: delete everything whose id falls below
        // the CAP-th largest id. (A subselect, not a row count, so it's correct
        // even if ids are sparse after earlier trims.)
        tx.execute(
            "DELETE FROM snapshots
               WHERE id NOT IN (
                 SELECT id FROM snapshots ORDER BY id DESC LIMIT ?1
               )",
            rusqlite::params![SNAPSHOT_HISTORY_CAP],
        )?;
        tx.commit()?;
        Ok(())
    }

    /// List the snapshot history newest-first as lightweight metadata (id, ts, and
    /// a derived "N tabs · M terminals" summary) — WITHOUT shipping every full
    /// JSON blob to the frontend. Empty when there is no history or no DB.
    fn list_snapshots(&self) -> rusqlite::Result<Vec<SnapshotMeta>> {
        let guard = self.conn.lock().expect("db mutex poisoned");
        let Some(conn) = guard.as_ref() else {
            return Ok(Vec::new());
        };
        let mut stmt =
            conn.prepare("SELECT id, ts, json FROM snapshots ORDER BY id DESC")?;
        let rows = stmt.query_map([], |row| {
            let id: i64 = row.get(0)?;
            let ts: i64 = row.get(1)?;
            let json: String = row.get(2)?;
            Ok(SnapshotMeta {
                id,
                ts,
                summary: summarize_layout(&json),
            })
        })?;
        rows.collect()
    }

    /// Fetch one snapshot's full layout JSON by id, or `None` if it's no longer in
    /// the ring (trimmed away) or there is no DB.
    fn get_snapshot(&self, id: i64) -> rusqlite::Result<Option<String>> {
        let guard = self.conn.lock().expect("db mutex poisoned");
        let Some(conn) = guard.as_ref() else {
            return Ok(None);
        };
        match conn.query_row("SELECT json FROM snapshots WHERE id = ?1", [id], |row| {
            row.get::<_, String>(0)
        }) {
            Ok(v) => Ok(Some(v)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e),
        }
    }
}

/// Lightweight metadata for one history snapshot, returned by `list_snapshots`.
/// Serialized camelCase to match the frontend's IPC convention. The full layout
/// JSON is fetched separately (`get_snapshot`) only when the user previews or
/// restores, so the list stays cheap even with 20 entries.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SnapshotMeta {
    /// Stable row id; the handle passed back to `get_snapshot`.
    pub id: i64,
    /// Unix epoch seconds the snapshot was captured.
    pub ts: i64,
    /// A human summary like `"5 tabs · 12 terminals"`, derived from the layout.
    pub summary: String,
}

/// Derive a cheap "N tabs · M terminals" summary from a layout snapshot JSON
/// WITHOUT depending on the frontend's exact schema: we parse loosely, count the
/// entries of a top-level `tabs` array (plus any `poppedOutTabs`, since those are
/// real tabs living in other windows), and sum the lengths of each tab's `order`
/// array (the tile/terminal ids). Any parse failure degrades to "snapshot" so the
/// row still renders. Kept in Rust so the list command is self-describing.
fn summarize_layout(json: &str) -> String {
    let parsed: serde_json::Value = match serde_json::from_str(json) {
        Ok(v) => v,
        Err(_) => return "snapshot".to_string(),
    };
    let count_tabs = |key: &str| -> (usize, usize) {
        match parsed.get(key).and_then(|v| v.as_array()) {
            Some(arr) => {
                let tabs = arr.len();
                let terms = arr
                    .iter()
                    .filter_map(|t| t.get("order").and_then(|o| o.as_array()))
                    .map(|o| o.len())
                    .sum();
                (tabs, terms)
            }
            None => (0, 0),
        }
    };
    let (vt, vterm) = count_tabs("tabs");
    let (pt, pterm) = count_tabs("poppedOutTabs");
    let tabs = vt + pt;
    let terms = vterm + pterm;
    let tab_word = if tabs == 1 { "tab" } else { "tabs" };
    let term_word = if terms == 1 { "terminal" } else { "terminals" };
    format!("{tabs} {tab_word} · {terms} {term_word}")
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
    let db = app.state::<Db>();
    // The live save: upsert the latest layout (boot hydration reads this). This
    // result is authoritative — a failure here is the command's failure.
    db.put(WORKSPACE_KEY, &json)
        .map_err(|e| format!("save_workspace_snapshot: {e}"))?;
    // Best-effort: also append to the recovery history ring. A failure here is
    // logged but NOT surfaced — it must never fail the live save above.
    if let Err(e) = db.append_snapshot(&json) {
        eprintln!("db: append_snapshot (recovery history) failed: {e}");
    }
    Ok(())
}

/// List the recent workspace-layout snapshots (Recovery review, #recovery),
/// newest first, as lightweight metadata (id, ts, and a derived summary). The
/// full JSON is fetched separately via [`get_snapshot`] only when needed. Empty
/// when there is no history yet or the DB couldn't be opened.
#[tauri::command]
pub async fn list_snapshots(app: tauri::AppHandle) -> Result<Vec<SnapshotMeta>, String> {
    app.state::<Db>()
        .list_snapshots()
        .map_err(|e| format!("list_snapshots: {e}"))
}

/// Fetch one history snapshot's full layout JSON by id (Recovery review), or
/// `None` if it has aged out of the ring or the DB is unavailable. The frontend
/// parses + applies this through the same load/apply path as boot hydration.
#[tauri::command]
pub async fn get_snapshot(
    app: tauri::AppHandle,
    id: i64,
) -> Result<Option<String>, String> {
    app.state::<Db>()
        .get_snapshot(id)
        .map_err(|e| format!("get_snapshot: {e}"))
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

    // --- Recovery snapshot history (#recovery) ------------------------------

    #[test]
    fn append_then_list_newest_first() {
        let (db, dir) = temp_db();
        db.append_snapshot(r#"{"tabs":[{"order":["a"]}]}"#).unwrap();
        db.append_snapshot(r#"{"tabs":[{"order":["a","b"]}]}"#).unwrap();
        let metas = db.list_snapshots().unwrap();
        assert_eq!(metas.len(), 2);
        // Newest first: the second append has the larger id and leads the list.
        assert!(metas[0].id > metas[1].id);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn summary_counts_tabs_and_terminals() {
        // Two visible tabs (2 + 1 terminals) plus one popped-out tab (1 terminal):
        // 3 tabs · 4 terminals.
        let json = r#"{
            "tabs":[{"order":["a","b"]},{"order":["c"]}],
            "poppedOutTabs":[{"order":["d"]}]
        }"#;
        assert_eq!(summarize_layout(json), "3 tabs · 4 terminals");
        // Singulars are pluralized correctly.
        assert_eq!(
            summarize_layout(r#"{"tabs":[{"order":["x"]}]}"#),
            "1 tab · 1 terminal",
        );
        // Garbage degrades gracefully rather than panicking.
        assert_eq!(summarize_layout("not json"), "snapshot");
    }

    #[test]
    fn history_is_capped_to_the_newest_n() {
        let (db, dir) = temp_db();
        // Append more than the cap; only the newest CAP survive.
        let extra = 5;
        for i in 0..(SNAPSHOT_HISTORY_CAP + extra) {
            db.append_snapshot(&format!(r#"{{"n":{i}}}"#)).unwrap();
        }
        let metas = db.list_snapshots().unwrap();
        assert_eq!(metas.len() as i64, SNAPSHOT_HISTORY_CAP);
        // The surviving rows are the newest ones: the very last append is present.
        let newest = metas[0].id;
        assert_eq!(
            db.get_snapshot(newest).unwrap().as_deref(),
            Some(format!(r#"{{"n":{}}}"#, SNAPSHOT_HISTORY_CAP + extra - 1).as_str()),
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn get_snapshot_roundtrips_and_misses() {
        let (db, dir) = temp_db();
        db.append_snapshot(r#"{"tabs":[]}"#).unwrap();
        let id = db.list_snapshots().unwrap()[0].id;
        assert_eq!(db.get_snapshot(id).unwrap().as_deref(), Some(r#"{"tabs":[]}"#));
        // An unknown id is a clean None, not an error.
        assert_eq!(db.get_snapshot(id + 999).unwrap(), None);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn save_command_path_also_appends_history() {
        // The kv save and the history append are independent: putting + appending
        // the same JSON leaves the latest kv value AND a history row.
        let (db, dir) = temp_db();
        let json = r#"{"tabs":[{"order":["a"]}]}"#;
        db.put(WORKSPACE_KEY, json).unwrap();
        db.append_snapshot(json).unwrap();
        assert_eq!(db.get(WORKSPACE_KEY).unwrap().as_deref(), Some(json));
        assert_eq!(db.list_snapshots().unwrap().len(), 1);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn none_backed_history_is_noop() {
        let db = Db::default();
        assert!(db.append_snapshot("x").is_ok());
        assert!(db.list_snapshots().unwrap().is_empty());
        assert_eq!(db.get_snapshot(1).unwrap(), None);
    }
}
