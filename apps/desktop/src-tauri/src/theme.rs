//! The theme contract — the backend half of T-Hub's live theming system, and
//! the surface a parallel MCP track forwards so Claude can retheme by talking.
//!
//! It is deliberately *opaque*: the rich `Theme` shape lives in the frontend
//! (`src/store/theme.ts`); here a theme is just a JSON string we persist and
//! re-emit verbatim. That keeps this module a stable transport even as the token
//! set evolves, and lets MCP push/read the exact JSON the editor produces.
//!
//! Contract (keep in lockstep with `src/ipc/theme.ts` and the MCP forwarder):
//!   - command `get_theme() -> String`  — the persisted theme JSON, or `""`
//!     when nothing has been persisted yet (fresh install).
//!   - command `set_theme(theme: String)` — validate it's JSON, persist it
//!     (managed in-memory copy + a file under the config dir), then emit
//!     `theme://changed` with the new JSON so every window applies it live.
//!   - event   `theme://changed` — payload is the theme JSON String.
//!
//! Persistence path: `~/.config/t-hub/theme.json` (honoring `XDG_CONFIG_HOME`
//! / `CLAUDE_CONFIG_DIR`'s parent is not used — this is our own dir). We reuse
//! the project's proven HOME-based resolution rather than the Tauri path plugin
//! so it behaves identically inside WSL and in tests.

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use tauri::{Emitter, Manager};

/// The event channel emitted whenever the theme changes (incl. via MCP/another
/// window). Payload: the theme JSON `String`. Mirrors `ThemeEvents.changed`.
pub const THEME_CHANGED: &str = "theme://changed";

/// Tauri-managed state: the latest theme JSON, in memory, behind a mutex. Seeded
/// from disk at startup (see [`ThemeState::load`]) so `get_theme` is a cheap,
/// allocation-light read that never blocks on the filesystem.
#[derive(Default)]
pub struct ThemeState {
    /// The current theme as a JSON string; empty until first set/loaded.
    json: Mutex<String>,
}

impl ThemeState {
    /// Build the managed state, seeding it from the persisted file if present.
    /// A missing/unreadable file is not an error — we simply start empty and the
    /// frontend seeds us from its local default on first boot.
    pub fn load() -> Self {
        let json = read_persisted().unwrap_or_default();
        Self {
            json: Mutex::new(json),
        }
    }

    /// Snapshot the current theme JSON (`""` if none).
    fn get(&self) -> String {
        self.json.lock().expect("theme mutex poisoned").clone()
    }

    /// Replace the in-memory theme JSON.
    fn set(&self, value: String) {
        *self.json.lock().expect("theme mutex poisoned") = value;
    }
}

/// `~/.config/t-hub` (honoring `XDG_CONFIG_HOME`), the dir we persist into.
/// Returns `None` only when none of `XDG_CONFIG_HOME` / `HOME` / `USERPROFILE` is
/// set. `USERPROFILE` is the Windows home — without it the app (which runs ON
/// Windows, where `HOME` is unset) could never resolve a config dir, so every theme
/// read/write failed with "could not resolve a config dir" — on a tight loop, that
/// was a steady error/log storm.
fn config_dir() -> Option<PathBuf> {
    if let Some(xdg) = std::env::var_os("XDG_CONFIG_HOME") {
        if !xdg.is_empty() {
            return Some(Path::new(&xdg).join("t-hub"));
        }
    }
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(|h| Path::new(&h).join(".config").join("t-hub"))
}

/// Full path to the persisted theme file.
fn theme_file() -> Option<PathBuf> {
    config_dir().map(|d| d.join("theme.json"))
}

/// Read the persisted theme JSON from disk, if the file exists and is readable.
fn read_persisted() -> Option<String> {
    let path = theme_file()?;
    match std::fs::read_to_string(&path) {
        Ok(s) if !s.trim().is_empty() => Some(s),
        _ => None,
    }
}

/// Persist the theme JSON to disk (best-effort, creating the dir as needed).
/// Returns an error string on failure so `set_theme` can surface it; an error
/// here does NOT prevent the in-memory update or the change event — a theme that
/// can't be written to disk still applies for the session.
fn write_persisted(json: &str) -> Result<(), String> {
    let path = theme_file()
        .ok_or_else(|| "could not resolve a config dir (no XDG_CONFIG_HOME / HOME)".to_string())?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("create {}: {e}", parent.display()))?;
    }
    std::fs::write(&path, json).map_err(|e| format!("write {}: {e}", path.display()))
}

/// Return the persisted theme as a JSON string, or `""` if none is set yet.
/// This is the read side of the MCP/editor contract.
#[tauri::command]
pub async fn get_theme(app: tauri::AppHandle) -> Result<String, String> {
    Ok(app.state::<ThemeState>().get())
}

/// Persist a theme (a JSON string) and broadcast it.
///
/// Steps: validate the payload parses as JSON (so we never store/emit garbage);
/// update the in-memory copy; emit `theme://changed` with the JSON so the
/// frontend applies it live; then best-effort write it to disk. Disk failures
/// are logged but not fatal — the live apply already happened. This is the write
/// side the MCP track forwards to.
#[tauri::command]
pub async fn set_theme(app: tauri::AppHandle, theme: String) -> Result<(), String> {
    // Validate: must be a JSON object/value. Reject anything else early.
    serde_json::from_str::<serde_json::Value>(&theme)
        .map_err(|e| format!("theme is not valid JSON: {e}"))?;

    app.state::<ThemeState>().set(theme.clone());

    // Broadcast first so every window re-renders immediately; the payload is the
    // raw JSON string the frontend parses back into a Theme.
    if let Err(e) = app.emit(THEME_CHANGED, theme.clone()) {
        eprintln!("theme: failed to emit {THEME_CHANGED}: {e}");
    }

    if let Err(e) = write_persisted(&theme) {
        // Non-fatal: the theme is live + in memory; only the durable copy failed.
        eprintln!("theme: failed to persist: {e}");
    }
    Ok(())
}

// --- Shared workspace layout (#9: persist workspaces across variants) ----------
// A single `~/.config/t-hub/workspaces.json` shared by ALL variants (prod + dev),
// a sibling of theme.json. The per-variant SQLite copy (db.rs) stays the PRIMARY
// durable store; this shared file is what carries your workspace layout across a
// dev↔prod switch — adopted on a fresh variant whose per-variant copy is empty.

/// Full path to the shared (all-variants) workspace layout file.
fn shared_layout_file() -> Option<PathBuf> {
    config_dir().map(|d| d.join("workspaces.json"))
}

/// Read the shared workspace layout JSON, or `None` if absent/empty/unreadable.
#[tauri::command]
pub async fn load_shared_layout() -> Result<Option<String>, String> {
    let Some(path) = shared_layout_file() else {
        return Ok(None);
    };
    match std::fs::read_to_string(&path) {
        Ok(s) if !s.trim().is_empty() => Ok(Some(s)),
        _ => Ok(None),
    }
}

/// Write the shared workspace layout JSON (best-effort, creating the dir as needed).
#[tauri::command]
pub async fn save_shared_layout(layout: String) -> Result<(), String> {
    let path = shared_layout_file()
        .ok_or_else(|| "could not resolve a config dir (no XDG_CONFIG_HOME / HOME)".to_string())?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("create {}: {e}", parent.display()))?;
    }
    std::fs::write(&path, layout).map_err(|e| format!("write {}: {e}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::Mutex as StdMutex;

    // `XDG_CONFIG_HOME` is process-global, so the tests can't safely run in
    // parallel even with distinct paths (one test's set_var would change the dir
    // another is mid-resolving). This lock serializes them; a per-call counter
    // also gives each its own dir so a crash never leaks state into the next.
    static ENV_LOCK: StdMutex<()> = StdMutex::new(());
    static COUNTER: AtomicU64 = AtomicU64::new(0);

    /// Run a closure with a clean, isolated config dir via XDG_CONFIG_HOME, then
    /// restore the prior env. Serialized across tests via ENV_LOCK.
    fn with_temp_config<T>(f: impl FnOnce() -> T) -> T {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let n = COUNTER.fetch_add(1, Ordering::SeqCst);
        let dir = std::env::temp_dir().join(format!("t-hub-theme-test-{}-{n}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let prev = std::env::var_os("XDG_CONFIG_HOME");
        std::env::set_var("XDG_CONFIG_HOME", &dir);
        let out = f();
        match prev {
            Some(v) => std::env::set_var("XDG_CONFIG_HOME", v),
            None => std::env::remove_var("XDG_CONFIG_HOME"),
        }
        let _ = std::fs::remove_dir_all(&dir);
        out
    }

    #[test]
    fn persists_and_reads_back() {
        with_temp_config(|| {
            assert!(read_persisted().is_none(), "starts empty");
            let json = r#"{"name":"Test","chrome":{}}"#;
            write_persisted(json).expect("write");
            assert_eq!(read_persisted().as_deref(), Some(json));
        });
    }

    #[test]
    fn load_seeds_from_disk() {
        with_temp_config(|| {
            let json = r#"{"name":"Seeded"}"#;
            write_persisted(json).expect("write");
            let state = ThemeState::load();
            assert_eq!(state.get(), json);
        });
    }

    #[test]
    fn empty_when_no_file() {
        with_temp_config(|| {
            let state = ThemeState::load();
            assert_eq!(state.get(), "");
        });
    }

    #[test]
    fn rejects_non_json_payload() {
        // Mirror set_theme's validation gate (without needing an AppHandle): a
        // non-JSON string must be rejected before we ever persist/emit.
        assert!(serde_json::from_str::<serde_json::Value>("not json {").is_err());
        assert!(serde_json::from_str::<serde_json::Value>(r#"{"ok":true}"#).is_ok());
    }
}
