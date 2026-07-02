//! Client-owned layout persistence (T8): tabs + active tab as a small JSON file.
//!
//! Decision (per the T8 brief "own SQLite or JSON file - decide and document"):
//! a **JSON file**, `~/.t-hub/native-layout.json` (`THN_LAYOUT` overrides the
//! path). The layout is a handful of tab names and session-id lists - human
//! readable, trivially diffable, and written atomically (temp file + rename);
//! SQLite buys nothing at this size. The SERVER keeps owning sessions (D5);
//! this file only records how THIS client arranges them.
//!
//! What is saved: tab names, each tab's ordered tile ids, and the active tab.
//! What is NOT saved: focus (transient), the hidden set (a client restart
//! re-lists everything live - see [`crate::chrome::model::ChromeModel`]), and
//! anything session-derived (titles come from `list_terminals` live).

use std::path::PathBuf;

use anyhow::{Context as _, Result};
use serde::{Deserialize, Serialize};

use super::model::{ChromeModel, Workspace};
use crate::font::FontSpec;

/// On-disk shape, versioned for forward evolution.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Layout {
    pub version: u32,
    pub tabs: Vec<LayoutTab>,
    pub active: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LayoutTab {
    pub name: String,
    pub tiles: Vec<String>,
    /// Optional per-workspace font override (T7). Absent in pre-T7 layouts.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub font: Option<FontConfig>,
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
    pub fn from_model(m: &ChromeModel) -> Layout {
        Layout {
            version: 1,
            tabs: m
                .tabs
                .iter()
                .map(|t| LayoutTab {
                    name: t.name.clone(),
                    tiles: t.tiles.clone(),
                    font: t.font.as_ref().map(FontConfig::from_spec),
                })
                .collect(),
            active: m.active,
        }
    }

    pub fn into_model(self) -> ChromeModel {
        ChromeModel::from_layout(
            self.tabs
                .into_iter()
                .map(|t| Workspace {
                    name: t.name,
                    tiles: t.tiles,
                    font: t.font.map(FontConfig::into_spec),
                })
                .collect(),
            self.active,
        )
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

        save(&path, &Layout::from_model(&m)).unwrap();
        let restored = load(&path).unwrap().unwrap().into_model();
        assert_eq!(restored.tabs, m.tabs);
        assert_eq!(restored.active, 1);
        assert_eq!(restored.tabs[1].font.as_ref().unwrap().family, "Cascadia Code");
        // A pre-T7 layout (no font field) still loads.
        std::fs::write(
            &path,
            r#"{"version":1,"tabs":[{"name":"w","tiles":["aa"]}],"active":0}"#,
        )
        .unwrap();
        let old = load(&path).unwrap().unwrap().into_model();
        assert_eq!(old.tabs[0].font, None);

        // Corrupt file -> an error, not a panic (the caller falls back fresh).
        std::fs::write(&path, "{ not json").unwrap();
        assert!(load(&path).is_err());

        let _ = std::fs::remove_dir_all(&dir);
    }
}
