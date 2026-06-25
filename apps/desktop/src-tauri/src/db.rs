//! Durable SQLite persistence for the workspace layout (#sqlite phase 1).
//!
//! Today the workspace snapshot (tabs/order/sizes/focus/poppedOutTabs/fontSize)
//! lives only in the webview's `localStorage` (`src/store/workspace.ts`, key
//! `t-hub.workspace.v2`). That is fine for round-tripping a single window but
//! is fragile: a cleared web cache, a corrupt profile, or a crash mid-write can
//! lose the arrangement, and there is no out-of-process copy to recover from.
//!
//! This module adds a small, durable SQLite copy as the foundation for recovery.
//! Phase 1 is deliberately minimal — reliable save/load of one JSON snapshot —
//! and the recovery-review UI lands in a later phase.
//!
//! Design:
//!   - A single SQLite database under Tauri's `app_data_dir` (`t-hub.db`).
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
/// Resolved ONCE at startup from `$T_HUB_DB_NAME`, defaulting to
/// `"t-hub.db"`. The env hook exists so a side-by-side **DEV** instance can
/// keep its workspace state in a SEPARATE SQLite file (e.g.
/// `T_HUB_DB_NAME=t-hub-dev.db`) within the same app data dir, instead of
/// reading/writing production's `t-hub.db`. With NO env var set the filename
/// is exactly `"t-hub.db"`, so default behavior is byte-for-byte unchanged.
static DB_FILE: LazyLock<String> =
    LazyLock::new(|| std::env::var("T_HUB_DB_NAME").unwrap_or_else(|_| "t-hub.db".into()));

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
        // Native session-restore (WS-6): a durable per-tile map of the last Claude
        // session we saw running in each T-Hub terminal. Keyed by `terminal_id`
        // (one row per tile — a tile only ever hosts one live Claude session at a
        // time, so the latest statusline upserts in place). `tmux_session`
        // (`th_<id>`) is the ROBUST tile↔session key we cross-reference against the
        // surviving tmux sessions on boot; `session_id` is the `claude --resume`
        // handle, `cwd` the directory to resume it in. After the app/backend/host
        // restarts, a row whose `tmux_session` is GONE but whose `session_id`
        // transcript still EXISTS is a resumable orphan (see list_orphaned_sessions).
        // No `agent_kind`: `claude --resume` is agent-agnostic.
        conn.execute(
            "CREATE TABLE IF NOT EXISTS tile_sessions (
                 terminal_id  TEXT PRIMARY KEY,
                 session_id   TEXT NOT NULL,
                 cwd          TEXT NOT NULL,
                 tmux_session TEXT NOT NULL,
                 created_at   INTEGER NOT NULL
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

    // --- Native session-restore (WS-6) -------------------------------------

    /// Upsert the Claude session a tile is hosting (WS-6). Keyed by
    /// `terminal_id`: each statusline that arrives for a tile overwrites that
    /// tile's row in place (a tile hosts one live session at a time), so the
    /// table is a current map of tile → last-seen Claude session, never a log.
    /// `created_at` stamps when THIS binding was first recorded (the upsert keeps
    /// it stable across re-stamps so it reads as "last seen at"). Best-effort: a
    /// `None`-backed DB is a no-op (no restore catalog, but live state is fine).
    pub fn record_tile_session(
        &self,
        terminal_id: &str,
        session_id: &str,
        cwd: &str,
        tmux_session: &str,
    ) -> rusqlite::Result<()> {
        let guard = self.conn.lock().expect("db mutex poisoned");
        let Some(conn) = guard.as_ref() else {
            return Ok(()); // no DB → no restore catalog
        };
        // On conflict (re-stamp of the same tile) refresh session/cwd/tmux but
        // KEEP the original created_at, so the column always means "first seen".
        conn.execute(
            "INSERT INTO tile_sessions(terminal_id, session_id, cwd, tmux_session, created_at)
             VALUES(?1, ?2, ?3, ?4, ?5)
             ON CONFLICT(terminal_id) DO UPDATE SET
                 session_id   = ?2,
                 cwd          = ?3,
                 tmux_session = ?4",
            rusqlite::params![terminal_id, session_id, cwd, tmux_session, now_secs()],
        )?;
        Ok(())
    }

    /// All recorded tile→session bindings (WS-6). The boot-time orphan scan reads
    /// this and cross-references each row's `tmux_session` against the surviving
    /// tmux sessions + the on-disk transcript catalog. Empty when none recorded or
    /// no DB.
    pub fn all_tile_sessions(&self) -> rusqlite::Result<Vec<TileSession>> {
        let guard = self.conn.lock().expect("db mutex poisoned");
        let Some(conn) = guard.as_ref() else {
            return Ok(Vec::new());
        };
        let mut stmt = conn.prepare(
            "SELECT terminal_id, session_id, cwd, tmux_session, created_at
               FROM tile_sessions ORDER BY created_at DESC",
        )?;
        let rows = stmt.query_map([], |row| {
            Ok(TileSession {
                terminal_id: row.get(0)?,
                session_id: row.get(1)?,
                cwd: row.get(2)?,
                tmux_session: row.get(3)?,
                created_at: row.get(4)?,
            })
        })?;
        rows.collect()
    }

    /// Prune the given `terminal_id`s from `tile_sessions` (WS-6, #9). Called by
    /// the orphan scan with the rows it proved DEAD (tmux session gone AND no
    /// transcript), so they never accumulate — they can never be restored, so the
    /// boot scan needn't keep re-reading them. Best-effort: a `None`-backed DB or
    /// an empty list is a no-op. Done in one transaction so the delete is atomic.
    pub fn delete_tile_sessions(&self, terminal_ids: &[String]) -> rusqlite::Result<()> {
        if terminal_ids.is_empty() {
            return Ok(());
        }
        let mut guard = self.conn.lock().expect("db mutex poisoned");
        let Some(conn) = guard.as_mut() else {
            return Ok(()); // no DB → nothing to prune
        };
        let tx = conn.transaction()?;
        {
            let mut stmt = tx.prepare("DELETE FROM tile_sessions WHERE terminal_id = ?1")?;
            for id in terminal_ids {
                stmt.execute([id])?;
            }
        }
        tx.commit()?;
        Ok(())
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

/// One recorded tile→session binding (WS-6), as stored in `tile_sessions`. Read
/// by the boot-time orphan scan; not serialized to the frontend directly (the
/// scan maps it onto [`OrphanedSession`] after cross-referencing live tmux +
/// transcripts).
#[derive(Debug, Clone)]
pub struct TileSession {
    /// The T-Hub terminal id (the tmux session's `th_<id>` suffix); primary key.
    pub terminal_id: String,
    /// Claude's session id — the `claude --resume <id>` handle.
    pub session_id: String,
    /// The directory the session ran in (where `--resume` must land).
    pub cwd: String,
    /// The owning tmux session name (`th_<terminal_id>`); the robust liveness key.
    pub tmux_session: String,
    /// Unix epoch seconds the binding was first recorded (≈ last-seen).
    pub created_at: i64,
}

/// A resumable orphaned Claude session (WS-6): a tile we recorded whose tmux
/// session is GONE (the app/backend/host restarted) but whose transcript still
/// EXISTS on disk, so `claude --resume <sessionId>` can pick it back up. Returned
/// by [`list_orphaned_sessions`]; mirrored by `src/ipc/sessions.ts` (camelCase).
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OrphanedSession {
    /// Claude's session id — the `--resume <id>` handle the Restore button passes.
    pub session_id: String,
    /// The directory to resume the session in (spawned as the new tile's cwd).
    pub cwd: String,
    /// A friendly label (the transcript summary/first-prompt when known, else the
    /// cwd basename) so the row is recognizable.
    pub label: String,
    /// Unix epoch seconds we last recorded this tile binding (sorts newest-first).
    pub last_seen: i64,
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
    let db = app.state::<std::sync::Arc<Db>>();
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
    app.state::<std::sync::Arc<Db>>()
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
    app.state::<std::sync::Arc<Db>>()
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
    app.state::<std::sync::Arc<Db>>()
        .get(WORKSPACE_KEY)
        .map_err(|e| format!("load_workspace_snapshot: {e}"))
}

// --- Native session-restore (WS-6) -----------------------------------------

/// List the resumable ORPHANED Claude sessions (WS-6) — the boot-time restore
/// catalog. Cross-references the recorded `tile_sessions` map against:
///   1. the SURVIVING `th_*` tmux sessions (same source `list_terminals` uses) —
///      a binding whose tmux session is STILL LIVE is not orphaned (its tile is
///      either placed or auto-adopted), so we skip it; and
///   2. the on-disk transcript catalog (recent.rs) — a binding whose transcript
///      is GONE can't be `--resume`d, so we skip it.
/// What remains is exactly the sessions a crash/restart left behind that can be
/// brought back. Newest-first. Best-effort: any failure degrades to an empty list
/// (nothing offered) rather than erroring the UI.
#[tauri::command]
pub async fn list_orphaned_sessions(
    app: tauri::AppHandle,
) -> Result<Vec<OrphanedSession>, String> {
    let rows = app
        .state::<std::sync::Arc<Db>>()
        .all_tile_sessions()
        .map_err(|e| format!("list_orphaned_sessions: {e}"))?;
    if rows.is_empty() {
        return Ok(Vec::new());
    }

    // Move the cross-referencing work (a tmux listing + a transcript-catalog
    // walk, both blocking I/O) off the async executor.
    let scan = tauri::async_runtime::spawn_blocking(move || cross_reference_orphans(rows))
        .await
        .unwrap_or_default();
    // #9: prune the rows we proved DEAD (tmux gone AND no transcript) — they can
    // never be restored, so they'd only grow the table + future boot scans. Best-
    // effort: a prune failure is logged, never failing the restore list.
    if let Err(e) = app
        .state::<std::sync::Arc<Db>>()
        .delete_tile_sessions(&scan.dead_terminal_ids)
    {
        eprintln!("db: prune of dead tile_sessions failed: {e}");
    }
    Ok(scan.orphans)
}

/// The outcome of one orphan cross-reference (WS-6): the resumable orphans to
/// offer for restore, plus the `terminal_id`s proven DEAD (tmux session gone AND
/// no transcript) that the caller prunes from `tile_sessions` (#9).
#[derive(Debug, Default)]
struct OrphanScan {
    orphans: Vec<OrphanedSession>,
    dead_terminal_ids: Vec<String>,
}

/// Filter recorded tile→session bindings down to resumable orphans (WS-6): drop
/// any whose tmux session is still LIVE, and any whose transcript is GONE; for
/// the rest, attach a friendly label from the transcript catalog. Also collects
/// the provably-dead rows (tmux gone AND no transcript) for the caller to prune
/// (#9). Pulled out as a plain fn so the orphan logic is testable without
/// Tauri/DB. Newest-first.
fn cross_reference_orphans(rows: Vec<TileSession>) -> OrphanScan {
    // Live tmux sessions (the `t-hub` socket) — a still-running binding isn't
    // orphaned. FAIL CLOSED (#3): if the listing ITSELF fails, liveness is
    // unknown, so we must NOT offer any session for restore (a still-live session
    // would otherwise look orphaned and get double-`--resume`d into a new tile).
    // We also skip pruning, since we can't prove anything dead without the live
    // set.
    let live: std::collections::HashSet<String> = match crate::tmux::list_sessions() {
        Ok(names) => names.into_iter().collect(),
        Err(e) => {
            eprintln!(
                "db: tmux list_sessions failed during orphan scan: {e} \
                 (failing closed — offering no sessions for restore)"
            );
            return OrphanScan::default();
        }
    };
    // The not-live rows — computed ONCE (#21) and driving BOTH the transcript
    // lookup and the output, so the `!live.contains` predicate is evaluated a
    // single time per row. (`classify_orphans` re-applies the same `!live`
    // filter; here we pre-filter only to scope the transcript lookup to the
    // sessions that can actually be offered.)
    let candidates: std::collections::HashSet<String> = rows
        .iter()
        .filter(|r| !live.contains(&r.tmux_session))
        .map(|r| r.session_id.clone())
        .collect();
    // The resumable transcript catalog for just those ids: id → (label, cwd).
    // A present transcript IS the resumability signal (`--resume` reads it).
    let catalog = crate::recent::resumable_entries(&candidates);

    classify_orphans(rows, &live, &catalog)
}

/// The PURE combining core of the orphan scan (WS-6): given ALL recorded rows, the
/// set of LIVE tmux session names, and the resumable transcript catalog, split the
/// rows into resumable orphans (tmux session GONE *and* transcript EXISTS) and the
/// provably-dead `terminal_id`s to prune (tmux gone *and* transcript GONE),
/// attaching a friendly label + the resume cwd to each orphan. A row whose tmux
/// session is still LIVE is skipped entirely (never offered, never pruned).
/// Extracted from [`cross_reference_orphans`] so these subtle combining rules are
/// testable WITHOUT tmux/transcripts/DB.
///
/// FAIL-CLOSED note: the empty-live-set case is handled by the caller (a
/// `list_sessions()` Err returns early before reaching here); this fn trusts the
/// `live` set it is handed. The `!live.contains` predicate is the SINGLE liveness
/// gate — evaluated once per row. Newest-first.
fn classify_orphans(
    rows: Vec<TileSession>,
    live: &std::collections::HashSet<String>,
    catalog: &std::collections::HashMap<String, crate::recent::ResumableEntry>,
) -> OrphanScan {
    // The not-live rows — a still-running binding isn't orphaned, so it's never
    // offered and never pruned. Computed ONCE (#21), driving the output below.
    let not_live: Vec<TileSession> = rows
        .into_iter()
        .filter(|r| !live.contains(&r.tmux_session))
        .collect();

    let mut scan = OrphanScan::default();
    for r in not_live {
        // A not-live row whose transcript is GONE can never be resumed — record
        // its terminal_id for pruning (#9) and skip it.
        let Some(entry) = catalog.get(&r.session_id) else {
            scan.dead_terminal_ids.push(r.terminal_id);
            continue;
        };
        scan.orphans.push(OrphanedSession {
            // Prefer the recorded cwd; fall back to the catalog's if blank.
            cwd: if r.cwd.trim().is_empty() {
                entry.cwd.clone()
            } else {
                r.cwd
            },
            label: entry.label.clone(),
            last_seen: r.created_at,
            session_id: r.session_id,
        });
    }
    scan.orphans.sort_by(|a, b| b.last_seen.cmp(&a.last_seen));
    scan
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A fresh DB in a unique temp dir, isolated per test (no env globals).
    fn temp_db() -> (Db, PathBuf) {
        let dir = std::env::temp_dir().join(format!(
            "t-hub-db-test-{}-{:?}",
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

    // --- Native session-restore (WS-6) --------------------------------------

    #[test]
    fn record_tile_session_upserts_by_terminal_id() {
        let (db, dir) = temp_db();
        // First binding for a tile.
        db.record_tile_session("t1", "sess-a", "/home/u/p", "th_t1").unwrap();
        // A later statusline for the SAME tile (new session) overwrites in place.
        db.record_tile_session("t1", "sess-b", "/home/u/p2", "th_t1").unwrap();
        // A different tile is a separate row.
        db.record_tile_session("t2", "sess-c", "/home/u/q", "th_t2").unwrap();
        let rows = db.all_tile_sessions().unwrap();
        assert_eq!(rows.len(), 2, "one row per terminal_id (upsert, not insert)");
        let t1 = rows.iter().find(|r| r.terminal_id == "t1").unwrap();
        assert_eq!(t1.session_id, "sess-b", "latest session wins for the tile");
        assert_eq!(t1.cwd, "/home/u/p2");
        assert_eq!(t1.tmux_session, "th_t1");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn tile_sessions_survive_reopen() {
        let (db, dir) = temp_db();
        db.record_tile_session("t1", "sess-a", "/work", "th_t1").unwrap();
        drop(db);
        let db2 = Db::open_in(dir.clone());
        let rows = db2.all_tile_sessions().unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].session_id, "sess-a");
        drop(db2);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn none_backed_tile_sessions_is_noop() {
        let db = Db::default();
        assert!(db.record_tile_session("t1", "s", "/c", "th_t1").is_ok());
        assert!(db.all_tile_sessions().unwrap().is_empty());
        // #9: pruning a None-backed DB (and the empty case) is a silent no-op.
        assert!(db.delete_tile_sessions(&[]).is_ok());
        assert!(db.delete_tile_sessions(&["t1".into()]).is_ok());
    }

    #[test]
    fn delete_tile_sessions_prunes_only_named_rows() {
        let (db, dir) = temp_db();
        db.record_tile_session("t1", "sess-a", "/a", "th_t1").unwrap();
        db.record_tile_session("t2", "sess-b", "/b", "th_t2").unwrap();
        db.record_tile_session("t3", "sess-c", "/c", "th_t3").unwrap();
        // Prune two; the third survives.
        db.delete_tile_sessions(&["t1".into(), "t3".into()]).unwrap();
        let rows = db.all_tile_sessions().unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].terminal_id, "t2");
        // An empty list deletes nothing.
        db.delete_tile_sessions(&[]).unwrap();
        assert_eq!(db.all_tile_sessions().unwrap().len(), 1);
        let _ = std::fs::remove_dir_all(dir);
    }

    // --- Orphan classifier (WS-6 cross-reference core) ----------------------
    // These exercise the PURE `classify_orphans` combining logic directly, with a
    // fixture row set × a fake live-tmux set × a fake transcript catalog — no
    // tmux, transcripts, or DB needed.

    /// One recorded tile→session binding. `terminal_id`/`tmux_session` follow the
    /// real `th_<id>` convention so the liveness key lines up with the live set.
    fn row(terminal_id: &str, session_id: &str, cwd: &str, created_at: i64) -> TileSession {
        TileSession {
            terminal_id: terminal_id.to_string(),
            session_id: session_id.to_string(),
            cwd: cwd.to_string(),
            tmux_session: format!("th_{terminal_id}"),
            created_at,
        }
    }

    /// A fake transcript catalog: each entry is a session id that "still has a
    /// transcript on disk" with the given label + resume cwd.
    fn catalog(
        entries: &[(&str, &str, &str)],
    ) -> std::collections::HashMap<String, crate::recent::ResumableEntry> {
        entries
            .iter()
            .map(|(id, label, cwd)| {
                (
                    id.to_string(),
                    crate::recent::ResumableEntry {
                        label: label.to_string(),
                        cwd: cwd.to_string(),
                    },
                )
            })
            .collect()
    }

    /// A fake live-tmux set from terminal ids (mirroring `th_<id>`).
    fn live_set(terminal_ids: &[&str]) -> std::collections::HashSet<String> {
        terminal_ids.iter().map(|id| format!("th_{id}")).collect()
    }

    #[test]
    fn classify_gone_tmux_with_transcript_is_resumable_orphan() {
        // (a) tmux session GONE (not in live set) AND transcript EXISTS ⇒ offered
        // as a resumable orphan, carrying the recorded cwd + the catalog label.
        let rows = vec![row("t1", "sess-a", "/home/u/proj", 100)];
        let cat = catalog(&[("sess-a", "Fix the parser", "/catalog/cwd")]);
        let scan = classify_orphans(rows, &live_set(&[]), &cat);

        assert_eq!(scan.orphans.len(), 1);
        let o = &scan.orphans[0];
        assert_eq!(o.session_id, "sess-a");
        assert_eq!(o.cwd, "/home/u/proj", "recorded cwd is preferred over catalog");
        assert_eq!(o.label, "Fix the parser");
        assert_eq!(o.last_seen, 100);
        assert!(scan.dead_terminal_ids.is_empty(), "a resumable row is not pruned");
    }

    #[test]
    fn classify_live_tmux_is_skipped_never_offered() {
        // (b) tmux session still LIVE ⇒ skipped entirely: not orphaned (its tile is
        // placed/adopted), so never offered AND never pruned — even though its
        // transcript exists in the catalog.
        let rows = vec![row("t1", "sess-a", "/home/u/proj", 100)];
        let cat = catalog(&[("sess-a", "Still running", "/catalog/cwd")]);
        let scan = classify_orphans(rows, &live_set(&["t1"]), &cat);

        assert!(scan.orphans.is_empty(), "a live session is never offered for restore");
        assert!(
            scan.dead_terminal_ids.is_empty(),
            "a live session is never pruned (we can't prove it dead)",
        );
    }

    #[test]
    fn classify_gone_transcript_is_skipped_and_pruned() {
        // (c) tmux session GONE but transcript GONE (absent from the catalog) ⇒ can
        // never be `--resume`d: skipped from the offer AND collected for prune (#9).
        let rows = vec![row("t1", "sess-a", "/home/u/proj", 100)];
        let cat = catalog(&[]); // no transcripts on disk
        let scan = classify_orphans(rows, &live_set(&[]), &cat);

        assert!(scan.orphans.is_empty(), "no transcript ⇒ not resumable");
        assert_eq!(
            scan.dead_terminal_ids,
            vec!["t1".to_string()],
            "a provably-dead row is collected for prune",
        );
    }

    #[test]
    fn classify_sorts_orphans_newest_first() {
        // (d) Multiple resumable orphans come back sorted by last_seen DESC
        // (newest first), independent of input order.
        let rows = vec![
            row("t1", "sess-old", "/a", 100),
            row("t2", "sess-new", "/b", 300),
            row("t3", "sess-mid", "/c", 200),
        ];
        let cat = catalog(&[
            ("sess-old", "old", "/a"),
            ("sess-new", "new", "/b"),
            ("sess-mid", "mid", "/c"),
        ]);
        let scan = classify_orphans(rows, &live_set(&[]), &cat);

        let order: Vec<&str> = scan.orphans.iter().map(|o| o.session_id.as_str()).collect();
        assert_eq!(order, vec!["sess-new", "sess-mid", "sess-old"]);
    }

    #[test]
    fn classify_blank_cwd_falls_back_to_catalog_cwd() {
        // A whitespace-only recorded cwd falls back to the catalog's cwd, so the
        // session resumes somewhere valid; a non-blank recorded cwd wins.
        let rows = vec![
            row("t1", "sess-blank", "   ", 100),
            row("t2", "sess-set", "/recorded", 100),
        ];
        let cat = catalog(&[
            ("sess-blank", "blank", "/from-catalog"),
            ("sess-set", "set", "/from-catalog"),
        ]);
        let scan = classify_orphans(rows, &live_set(&[]), &cat);

        let blank = scan.orphans.iter().find(|o| o.session_id == "sess-blank").unwrap();
        let set = scan.orphans.iter().find(|o| o.session_id == "sess-set").unwrap();
        assert_eq!(blank.cwd, "/from-catalog", "blank recorded cwd falls back to catalog");
        assert_eq!(set.cwd, "/recorded", "non-blank recorded cwd is preferred");
    }

    #[test]
    fn classify_mixed_partitions_offers_and_prunes() {
        // A realistic mix: one live (skip), one gone-with-transcript (offer), one
        // gone-without-transcript (prune). Confirms the three buckets are disjoint
        // and complete in a single pass.
        let rows = vec![
            row("live1", "sess-live", "/x", 100),  // still live ⇒ skip
            row("orph1", "sess-orph", "/y", 200),  // gone + transcript ⇒ offer
            row("dead1", "sess-dead", "/z", 300),  // gone + no transcript ⇒ prune
        ];
        let cat = catalog(&[
            ("sess-live", "live", "/x"),  // present but its row is live ⇒ unused
            ("sess-orph", "orphan", "/y"),
        ]);
        let scan = classify_orphans(rows, &live_set(&["live1"]), &cat);

        assert_eq!(scan.orphans.len(), 1);
        assert_eq!(scan.orphans[0].session_id, "sess-orph");
        assert_eq!(scan.dead_terminal_ids, vec!["dead1".to_string()]);
    }
}
