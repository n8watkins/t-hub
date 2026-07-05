//! Client-owned layout persistence (T8): tabs + active tab as a small JSON file.
//!
//! Decision (per the T8 brief "own SQLite or JSON file - decide and document"):
//! a **JSON file**, `~/.t-hub/native-layout.json` (`THN_LAYOUT` overrides the
//! path). The layout is a handful of tab names and session-id lists - human
//! readable, trivially diffable, and written atomically (temp file + rename);
//! SQLite buys nothing at this size. The SERVER keeps owning sessions (D5);
//! this file only records how THIS client arranges them.
//!
//! What is saved: tab names, each tab's ordered tile ids, the active tab, and
//! (T10) which tabs are torn off into satellite windows plus those windows'
//! last known bounds. What is NOT saved: focus (transient), the hidden set (a
//! client restart re-lists everything live - see
//! [`crate::chrome::model::ChromeModel`]), and anything session-derived
//! (titles come from `list_terminals` live).
//!
//! Evolution is tolerant in both directions: new fields are optional with
//! serde defaults (an old layout loads into the defaults), and serde ignores
//! unknown fields (an old binary reading a new layout skips them).

use std::path::PathBuf;

use anyhow::{Context as _, Result};
use serde::{Deserialize, Serialize};

use super::model::{ChromeModel, GridRatios, RowRatio, Workspace};
use super::windows::{SatBounds, WindowRegistry};
use crate::font::FontSpec;

/// On-disk shape, versioned for forward evolution.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Layout {
    pub version: u32,
    pub tabs: Vec<LayoutTab>,
    pub active: usize,
    /// User-assigned work names by session cwd (N1). Absent in older layouts.
    #[serde(default, skip_serializing_if = "std::collections::HashMap::is_empty")]
    pub work_names: std::collections::HashMap<String, String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LayoutTab {
    /// Stable tab id (T12 - the MCP addresses tabs by id). Absent in pre-T12
    /// layouts; a fresh id is minted on load so every tab is addressable.
    #[serde(default)]
    pub id: String,
    pub name: String,
    pub tiles: Vec<String>,
    /// Optional per-workspace font override (T7). Absent in pre-T7 layouts.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub font: Option<FontConfig>,
    /// Present when this workspace is torn off into a satellite window (T10);
    /// carries the window's per-window state. Absent in pre-T10 layouts and
    /// for main-window workspaces.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub satellite: Option<SatelliteConfig>,
    /// Manual pane ratios (T26). Absent in pre-T26 layouts and for auto-grid
    /// workspaces; a malformed value is dropped on load (auto grid), never an
    /// error.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub grid: Option<GridConfig>,
}

/// The serialized form of [`GridRatios`] (mirrored locally so the gpui-free
/// `model` stays serde-free, like [`FontConfig`] does for `font`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GridConfig {
    pub rows: Vec<GridRowConfig>,
}

/// One row's height fraction and its tiles' width fractions.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GridRowConfig {
    pub h: f32,
    pub cols: Vec<f32>,
}

impl GridConfig {
    fn from_ratios(g: &GridRatios) -> GridConfig {
        GridConfig {
            rows: g
                .rows
                .iter()
                .map(|r| GridRowConfig { h: r.h, cols: r.cols.clone() })
                .collect(),
        }
    }

    /// Into the model's ratios, sanitized: a hand-edited or future file with
    /// unusable numbers loads as `None` (the auto grid) instead of erroring.
    fn into_ratios(self) -> Option<GridRatios> {
        GridRatios {
            rows: self.rows.into_iter().map(|r| RowRatio { h: r.h, cols: r.cols }).collect(),
        }
        .sanitized()
    }
}

/// Per-satellite-window persisted state (the serialized cousin of the runtime
/// [`crate::chrome::windows::SatWindow`]; mirrored locally like [`FontConfig`]
/// so `windows` stays serde-free).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SatelliteConfig {
    /// Last known window bounds, restored on boot. Absent when the window
    /// never reported bounds before the layout was written.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bounds: Option<BoundsConfig>,
}

/// Window bounds in logical pixels.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct BoundsConfig {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

impl BoundsConfig {
    fn from_sat(b: SatBounds) -> BoundsConfig {
        BoundsConfig { x: b.x, y: b.y, w: b.w, h: b.h }
    }

    fn into_sat(self) -> SatBounds {
        SatBounds { x: self.x, y: self.y, w: self.w, h: self.h }
    }
}

/// The serialized form of a [`FontSpec`] (mirrored locally so the gpui-free
/// `font` module stays serde-free).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FontConfig {
    pub family: String,
    pub size: f32,
    #[serde(default = "default_true")]
    pub ligatures: bool,
}

fn default_true() -> bool {
    true
}

impl FontConfig {
    fn from_spec(s: &FontSpec) -> FontConfig {
        FontConfig { family: s.family.clone(), size: s.size, ligatures: s.ligatures }
    }

    fn into_spec(self) -> FontSpec {
        FontSpec { family: self.family, size: self.size, ligatures: self.ligatures }
    }
}

impl Layout {
    /// Snapshot the model AND the window registry (satellite bounds live
    /// there, refreshed by each satellite's paint loop).
    pub fn from_state(m: &ChromeModel, reg: &WindowRegistry) -> Layout {
        Layout {
            version: 1,
            tabs: m
                .tabs
                .iter()
                .map(|t| LayoutTab {
                    id: t.id.clone(),
                    name: t.name.clone(),
                    tiles: t.tiles.clone(),
                    font: t.font.as_ref().map(FontConfig::from_spec),
                    satellite: t.satellite.then(|| SatelliteConfig {
                        bounds: reg.bounds_of(t.wsid).map(BoundsConfig::from_sat),
                    }),
                    grid: t.grid.as_ref().map(GridConfig::from_ratios),
                })
                .collect(),
            active: m.active,
            work_names: m.work_names.clone(),
        }
    }

    /// The persisted satellite bounds, by tab index, for seeding the window
    /// registry at boot (consume BEFORE [`Self::into_model`] takes `self`).
    pub fn satellite_bounds(&self) -> Vec<(usize, Option<SatBounds>)> {
        self.tabs
            .iter()
            .enumerate()
            .filter_map(|(i, t)| {
                t.satellite.as_ref().map(|s| (i, s.bounds.map(BoundsConfig::into_sat)))
            })
            .collect()
    }

    pub fn into_model(self) -> ChromeModel {
        let work_names = self.work_names;
        let mut m = ChromeModel::from_layout(
            self.tabs
                .into_iter()
                .map(|t| Workspace {
                    // Pre-T12 layouts carry no id: mint one so the tab is
                    // MCP-addressable; it persists on the next save.
                    id: if t.id.is_empty() { super::model::mint_tab_id() } else { t.id },
                    name: t.name,
                    tiles: t.tiles,
                    font: t.font.map(FontConfig::into_spec),
                    grid: t.grid.and_then(GridConfig::into_ratios),
                    satellite: t.satellite.is_some(),
                    wsid: 0, // reassigned by from_layout
                    // Transient (webview parity): fullscreen never persists.
                    fullscreen: None,
                })
                .collect(),
            self.active,
        );
        m.work_names = work_names;
        m
    }
}

/// The layout file path: `THN_LAYOUT` override, else `~/.t-hub/native-layout.json`
/// next to the control handshake (HOME on WSL, USERPROFILE on Windows).
pub fn layout_path() -> PathBuf {
    if let Ok(p) = std::env::var("THN_LAYOUT") {
        if !p.is_empty() {
            return PathBuf::from(p);
        }
    }
    let home = std::env::var("HOME")
        .ok()
        .or_else(|| std::env::var("USERPROFILE").ok())
        .unwrap_or_default();
    let mut p = PathBuf::from(home);
    p.push(".t-hub");
    p.push("native-layout.json");
    p
}

/// Load the layout, or `None` when the file is missing (first run). A corrupt
/// file is an error the caller downgrades to a fresh default (never fatal).
pub fn load(path: &std::path::Path) -> Result<Option<Layout>> {
    let raw = match std::fs::read_to_string(path) {
        Ok(s) => s,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e).with_context(|| format!("read layout {}", path.display())),
    };
    let layout: Layout = serde_json::from_str(&raw)
        .with_context(|| format!("parse layout {}", path.display()))?;
    Ok(Some(layout))
}

/// Single-instance guard (T-B): an exclusive OS file lock beside the layout
/// file. Two cockpits on one layout would fight over the JSON (last writer
/// wins) AND over the server's tab-registry reporter (`report_workspace_tabs`
/// is last-writer-wins by design, §0 of the parity doc) — so the second
/// instance must refuse to start instead. Advisory `flock`/`LockFileEx`
/// semantics (std `File::try_lock`): the lock dies with the process, so a
/// crash never leaves a stale guard the way a pid file would. Keep the
/// returned guard alive for the process lifetime; dropping it releases the
/// lock. Distinct `THN_LAYOUT` paths (e.g. acceptance harnesses on a scratch
/// layout) lock distinct files and coexist, exactly as intended.
#[derive(Debug)]
pub struct InstanceLock {
    file: std::fs::File,
}

impl Drop for InstanceLock {
    fn drop(&mut self) {
        // Best-effort: the OS releases the lock on close anyway.
        let _ = self.file.unlock();
    }
}

/// Path of the lock file guarding `layout_path`: `<layout>.lock`.
pub fn instance_lock_path(layout_path: &std::path::Path) -> PathBuf {
    let mut os = layout_path.as_os_str().to_owned();
    os.push(".lock");
    PathBuf::from(os)
}

/// Try to become the single instance for `layout_path`. `Err` carries a
/// user-facing explanation when another instance already holds the lock (or
/// the lock file cannot be created).
pub fn acquire_instance_lock(layout_path: &std::path::Path) -> Result<InstanceLock> {
    use anyhow::bail;
    let lock_path = instance_lock_path(layout_path);
    if let Some(dir) = lock_path.parent() {
        std::fs::create_dir_all(dir)
            .with_context(|| format!("create lock dir {}", dir.display()))?;
    }
    let file = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(&lock_path)
        .with_context(|| format!("open instance lock {}", lock_path.display()))?;
    match file.try_lock() {
        Ok(()) => Ok(InstanceLock { file }),
        Err(std::fs::TryLockError::WouldBlock) => bail!(
            "another t-hub-native instance is already running against {} \
             (two instances would fight over the layout file and the server's \
             tab registry). Use it, or point THN_LAYOUT at a different file \
             for a second, isolated cockpit.",
            layout_path.display()
        ),
        Err(std::fs::TryLockError::Error(e)) => {
            Err(e).with_context(|| format!("lock {}", lock_path.display()))
        }
    }
}

/// Save atomically: write `<path>.tmp`, then rename over the target, so a crash
/// mid-write never leaves a torn layout.
pub fn save(path: &std::path::Path, layout: &Layout) -> Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)
            .with_context(|| format!("create layout dir {}", dir.display()))?;
    }
    let tmp = path.with_extension("json.tmp");
    let body = serde_json::to_string_pretty(layout).context("serialize layout")?;
    std::fs::write(&tmp, body).with_context(|| format!("write {}", tmp.display()))?;
    std::fs::rename(&tmp, path)
        .with_context(|| format!("rename {} over {}", tmp.display(), path.display()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn instance_lock_excludes_a_second_holder_and_releases_on_drop() {
        let dir = std::env::temp_dir().join(format!("thn-lock-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let layout = dir.join("native-layout.json");

        // First acquire wins (and creates the dir + lock file).
        let first = acquire_instance_lock(&layout).expect("first instance locks");
        assert!(instance_lock_path(&layout).exists());

        // A second holder is refused with the user-facing message.
        let err = acquire_instance_lock(&layout).unwrap_err().to_string();
        assert!(err.contains("already running"), "got: {err}");

        // Dropping the guard releases the lock; the next instance may start.
        drop(first);
        let again = acquire_instance_lock(&layout).expect("lock is free after drop");
        drop(again);

        // A DIFFERENT layout path is a different lock: two isolated cockpits
        // (scratch THN_LAYOUT harnesses) coexist.
        let _a = acquire_instance_lock(&layout).unwrap();
        let other = dir.join("scratch-layout.json");
        let _b = acquire_instance_lock(&other).expect("distinct layouts coexist");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn round_trips_through_disk_and_model() {
        let dir = std::env::temp_dir().join(format!("thn-layout-test-{}", std::process::id()));
        let path = dir.join("native-layout.json");
        let _ = std::fs::remove_dir_all(&dir);

        assert!(load(&path).unwrap().is_none()); // first run: no file

        let mut m = ChromeModel::default();
        m.reconcile(&["aa".to_string(), "bb".to_string()]);
        m.add_tab();
        m.tabs[1].name = "ops".to_string();
        // Per-workspace font override (T7 FontSpec) round-trips too.
        m.tabs[1].font =
            Some(FontSpec { family: "Cascadia Code".to_string(), size: 15.0, ligatures: false });

        // Work names (N1) ride the layout, keyed by cwd.
        m.work_names.insert("/repo/app".to_string(), "auth fix".to_string());

        save(&path, &Layout::from_state(&m, &WindowRegistry::default())).unwrap();
        let restored = load(&path).unwrap().unwrap().into_model();
        assert_eq!(restored.tabs, m.tabs); // ids round-trip too (T12)
        assert_eq!(restored.active, 1);
        assert_eq!(restored.work_name_for("/repo/app"), Some("auth fix"));
        assert_eq!(restored.tabs[1].font.as_ref().unwrap().family, "Cascadia Code");
        // A pre-T7/T10/T12 layout (no font, no satellite, no id) still loads; a
        // fresh id is minted so the tab is MCP-addressable.
        std::fs::write(
            &path,
            r#"{"version":1,"tabs":[{"name":"w","tiles":["aa"]}],"active":0}"#,
        )
        .unwrap();
        let old = load(&path).unwrap().unwrap().into_model();
        assert_eq!(old.tabs[0].font, None);
        assert!(!old.tabs[0].id.is_empty());
        assert!(!old.tabs[0].satellite); // pre-T10 layout: nothing torn off

        // Corrupt file -> an error, not a panic (the caller falls back fresh).
        std::fs::write(&path, "{ not json").unwrap();
        assert!(load(&path).is_err());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn grid_ratios_round_trip_and_tolerate_garbage() {
        let dir = std::env::temp_dir().join(format!("thn-grid-test-{}", std::process::id()));
        let path = dir.join("native-layout.json");
        let _ = std::fs::remove_dir_all(&dir);

        let mut m = ChromeModel::default();
        m.reconcile(&["aa".to_string(), "bb".to_string(), "cc".to_string()]);
        m.tabs[0].grid = Some(GridRatios {
            rows: vec![
                RowRatio { h: 0.7, cols: vec![0.25, 0.75] },
                RowRatio { h: 0.3, cols: vec![1.0] },
            ],
        });
        save(&path, &Layout::from_state(&m, &WindowRegistry::default())).unwrap();
        let restored = load(&path).unwrap().unwrap().into_model();
        assert_eq!(restored.tabs[0].grid, m.tabs[0].grid);

        // A workspace with no ratios serializes WITHOUT the field (a pre-T26
        // binary reading this file sees nothing new).
        let mut plain = ChromeModel::default();
        plain.reconcile(&["aa".to_string()]);
        save(&path, &Layout::from_state(&plain, &WindowRegistry::default())).unwrap();
        assert!(!std::fs::read_to_string(&path).unwrap().contains("\"grid\""));

        // Hand-edited garbage ratios load as None (auto grid), never an error;
        // unknown fields inside grid (future versions) are skipped.
        std::fs::write(
            &path,
            r#"{"version":1,"tabs":[{"name":"w","tiles":["aa"],"grid":{"rows":[{"h":0.0,"cols":[1.0]}],"zoom":3}}],"active":0}"#,
        )
        .unwrap();
        let bad = load(&path).unwrap().unwrap().into_model();
        assert_eq!(bad.tabs[0].grid, None);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn satellites_round_trip_with_their_window_bounds() {
        let dir = std::env::temp_dir().join(format!("thn-sat-test-{}", std::process::id()));
        let path = dir.join("native-layout.json");
        let _ = std::fs::remove_dir_all(&dir);

        let mut m = ChromeModel::default();
        m.reconcile(&["aa".to_string()]);
        m.add_tab();
        m.reconcile(&["aa".to_string(), "bb".to_string()]);
        let wsid = m.tear_off(1).unwrap();

        let mut reg = WindowRegistry::default();
        reg.open(wsid, Some("bb".to_string()), None);
        reg.set_bounds(wsid, SatBounds { x: 40.0, y: 60.0, w: 900.0, h: 650.0 });

        save(&path, &Layout::from_state(&m, &reg)).unwrap();
        let layout = load(&path).unwrap().unwrap();

        // The registry seed reads tab index + bounds back out.
        let sats = layout.satellite_bounds();
        assert_eq!(sats.len(), 1);
        assert_eq!(sats[0].0, 1);
        assert_eq!(sats[0].1, Some(SatBounds { x: 40.0, y: 60.0, w: 900.0, h: 650.0 }));

        // The model round-trips the torn-off flag (wsids are reassigned).
        let restored = layout.into_model();
        assert!(restored.tabs[1].satellite);
        assert!(!restored.tabs[0].satellite);
        // Active never rests on a satellite while a main tab exists.
        assert!(!restored.tabs[restored.active].satellite);

        // A satellite that never reported bounds persists as bounds-less.
        let mut reg2 = WindowRegistry::default();
        reg2.open(wsid, None, None);
        save(&path, &Layout::from_state(&m, &reg2)).unwrap();
        let sats = load(&path).unwrap().unwrap().satellite_bounds();
        assert_eq!(sats, vec![(1, None)]);

        // Forward tolerance: an unknown field inside `satellite` (a future
        // version's addition) is skipped, not an error.
        std::fs::write(
            &path,
            r#"{"version":1,"tabs":[{"name":"w","tiles":["aa"],"satellite":{"bounds":{"x":1.0,"y":2.0,"w":3.0,"h":4.0},"zoom":2}}],"active":0}"#,
        )
        .unwrap();
        let fut = load(&path).unwrap().unwrap();
        assert_eq!(fut.satellite_bounds(), vec![(0, Some(SatBounds { x: 1.0, y: 2.0, w: 3.0, h: 4.0 }))]);

        let _ = std::fs::remove_dir_all(&dir);
    }
}
