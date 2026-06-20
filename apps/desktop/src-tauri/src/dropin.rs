//! Drop-in / paste helpers for terminal input (Lane C).
//!
//! Today this is just the clipboard-image path for C2: when the user pastes an
//! IMAGE (not text) into a terminal, the frontend can't hand a raw bitmap to the
//! PTY — Claude/Codex read images by FILE PATH. So we read the clipboard image
//! here (Rust side, via `tauri-plugin-clipboard-manager`), encode it to a PNG in
//! the OS temp dir, and return that file's NATIVE path. The frontend translates
//! the native (Windows) path to a WSL path and types it into the prompt.
//!
//! Path translation lives in the frontend (`src/lib/dropPaste.ts`) so there is a
//! single implementation shared by both the file-drop (C1) and image-paste (C2)
//! flows — this module only deals in native paths.

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use tauri_plugin_clipboard_manager::ClipboardExt;

/// Monotonic suffix so two pastes within the same millisecond don't collide on a
/// temp-file name (the timestamp alone isn't unique enough under a fast paste).
static PASTE_SEQ: AtomicU64 = AtomicU64::new(0);

/// If the clipboard holds an image, save it as a PNG under the OS temp dir and
/// return that file's NATIVE path; otherwise return `Ok(None)` so the caller
/// falls back to a normal text paste. Errors only on a real failure (encode /
/// write), never on "the clipboard simply has no image" — an empty/text-only
/// clipboard makes `read_image` error, which we map to `None`.
///
/// Runs on a blocking thread: the clipboard read takes a lock that can deadlock
/// if driven from the main thread (per the plugin's own warning), and PNG encode
/// + file write are blocking I/O we don't want on the async executor.
#[tauri::command]
pub async fn clipboard_image_to_temp(app: tauri::AppHandle) -> Result<Option<String>, String> {
    tauri::async_runtime::spawn_blocking(move || read_clipboard_image_to_temp(&app))
        .await
        .map_err(|e| format!("clipboard image task failed: {e}"))?
}

fn read_clipboard_image_to_temp(app: &tauri::AppHandle) -> Result<Option<String>, String> {
    // `read_image` errors when the clipboard has no image (or holds text) — that
    // is the common "not an image paste" case, so treat ANY read error as "no
    // image" and let the caller paste text instead.
    let image = match app.clipboard().read_image() {
        Ok(img) => img,
        Err(_) => return Ok(None),
    };

    let width = image.width();
    let height = image.height();
    let rgba = image.rgba().to_vec();
    if width == 0 || height == 0 || rgba.is_empty() {
        return Ok(None);
    }

    let buf = image::RgbaImage::from_raw(width, height, rgba)
        .ok_or_else(|| "clipboard image buffer size mismatch".to_string())?;

    // Opportunistically reap PNGs from earlier pastes so our paste dir doesn't
    // grow without bound (nothing else deletes them, and %TEMP% isn't auto-purged
    // on Windows). This scans only our dedicated subdir, so it's cheap regardless
    // of how cluttered the OS temp dir is.
    prune_old_pastes();

    let path = temp_png_path();
    buf.save_with_format(&path, image::ImageFormat::Png)
        .map_err(|e| format!("failed to write paste image: {e}"))?;

    Ok(Some(path.to_string_lossy().into_owned()))
}

/// Age beyond which a leftover paste PNG is reaped. Generous so an image the user
/// pasted but hasn't submitted to the agent yet is never deleted out from under
/// them, while still bounding accumulation to roughly a day's worth.
const PASTE_MAX_AGE: std::time::Duration = std::time::Duration::from_secs(24 * 60 * 60);

/// Dedicated subdirectory of the OS temp dir that holds ONLY our paste PNGs.
///
/// Isolating our files here means the reaper scans a handful of our own entries
/// instead of stat-ing every file in `%TEMP%` (which on Windows routinely holds
/// thousands), so neither the save nor the prune does an O(N-of-whole-temp) sweep.
///
/// We `create_dir_all` the subdir but FALL BACK to the temp root if that fails:
/// a paste must never fail just because the housekeeping subdir couldn't be made.
fn paste_dir() -> PathBuf {
    let dir = std::env::temp_dir().join("termhub-paste");
    match std::fs::create_dir_all(&dir) {
        Ok(()) => dir,
        Err(_) => std::env::temp_dir(),
    }
}

/// Delete `termhub-paste-*.png` files older than [`PASTE_MAX_AGE`]. Scans ONLY our
/// dedicated [`paste_dir`], not the whole temp dir, so this stays O(our files).
/// Best-effort: any error (unreadable dir, file vanished, no mtime) is ignored —
/// reaping is housekeeping, never the caller's concern.
fn prune_old_pastes() {
    let dir = paste_dir();
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return;
    };
    let now = SystemTime::now();
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
        if !(name.starts_with("termhub-paste-") && name.ends_with(".png")) {
            continue;
        }
        let stale = entry
            .metadata()
            .and_then(|m| m.modified())
            .ok()
            .and_then(|mtime| now.duration_since(mtime).ok())
            .map(|age| age > PASTE_MAX_AGE)
            .unwrap_or(false);
        if stale {
            let _ = std::fs::remove_file(entry.path());
        }
    }
}

/// A unique `termhub-paste-<millis>-<seq>.png` path in our dedicated [`paste_dir`]
/// (falling back to the temp root if the subdir couldn't be created).
fn temp_png_path() -> PathBuf {
    let millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    let seq = PASTE_SEQ.fetch_add(1, Ordering::Relaxed);
    paste_dir().join(format!("termhub-paste-{millis}-{seq}.png"))
}
