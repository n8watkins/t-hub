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

    let path = temp_png_path();
    buf.save_with_format(&path, image::ImageFormat::Png)
        .map_err(|e| format!("failed to write paste image: {e}"))?;

    Ok(Some(path.to_string_lossy().into_owned()))
}

/// A unique `termhub-paste-<millis>-<seq>.png` path in the OS temp dir.
fn temp_png_path() -> PathBuf {
    let millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    let seq = PASTE_SEQ.fetch_add(1, Ordering::Relaxed);
    std::env::temp_dir().join(format!("termhub-paste-{millis}-{seq}.png"))
}
