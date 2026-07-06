// Voice announcements (Settings > Voice): persistence for ~/.t-hub/voice.json
// plus a loopback proxy to the local Piper TTS server.
//
// Why a backend proxy at all: the TTS server (default http://127.0.0.1:7477)
// REJECTS requests that carry a browser Origin header, so the webview must
// never fetch it directly. ureq sends no Origin, so routing /voices and /tts
// through these commands sidesteps that wholesale (and keeps the webview free
// of mixed-content/CORS concerns).
//
// voice.json sits beside control.json in the app home (same HOME/USERPROFILE
// resolution as control::handshake_path) because EXTERNAL captain tooling
// (announce.sh) reads the same file - it is the source of truth, not a mirror
// of some webview store. Writes therefore PRESERVE unknown keys: a foreign
// field added by a script survives a settings save from the UI.
use base64::{engine::general_purpose::STANDARD, Engine as _};
use serde::{Deserialize, Serialize};
use std::io::Read;
use std::path::PathBuf;
use std::time::Duration;

/// The Piper TTS server base URL; `T_HUB_TTS_URL` overrides for tests/E2E.
fn tts_base_url() -> String {
    std::env::var("T_HUB_TTS_URL")
        .unwrap_or_else(|_| "http://127.0.0.1:7477".to_string())
}

/// `~/.t-hub/voice.json`, beside control.json. `T_HUB_VOICE_FILE` overrides
/// (mirroring the `T_HUB_CONTROL_FILE` / `T_HUB_SERVER_KEY_FILE` pattern).
fn voice_settings_path() -> PathBuf {
    if let Ok(p) = std::env::var("T_HUB_VOICE_FILE") {
        return PathBuf::from(p);
    }
    let home = std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."));
    home.join(".t-hub").join("voice.json")
}

/// The shared voice.json schema (camelCase on disk and over IPC - external
/// scripts read the same field names).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VoiceSettings {
    pub enabled: bool,
    pub voice: String,
    pub volume: f64,
    /// SAPI fallback speech rate for the external announce.sh path; the app
    /// UI does not edit it but must round-trip it faithfully.
    pub sapi_rate: i64,
    pub announce_on_attention: bool,
}

impl Default for VoiceSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            voice: "en_US-ryan-high.onnx".to_string(),
            volume: 0.8,
            sapi_rate: 0,
            announce_on_attention: false,
        }
    }
}

/// Read voice.json leniently: missing file or unparseable content yields the
/// defaults; each field falls back independently so a partial file written by
/// external tooling never zeroes the rest.
fn read_settings() -> VoiceSettings {
    let d = VoiceSettings::default();
    let raw = match std::fs::read_to_string(voice_settings_path()) {
        Ok(s) if !s.trim().is_empty() => s,
        _ => return d,
    };
    let v: serde_json::Value = match serde_json::from_str(&raw) {
        Ok(v) => v,
        Err(_) => return d,
    };
    VoiceSettings {
        enabled: v.get("enabled").and_then(|x| x.as_bool()).unwrap_or(d.enabled),
        voice: v
            .get("voice")
            .and_then(|x| x.as_str())
            .map(str::to_string)
            .unwrap_or(d.voice),
        volume: v
            .get("volume")
            .and_then(|x| x.as_f64())
            .map(|x| x.clamp(0.0, 1.0))
            .unwrap_or(d.volume),
        sapi_rate: v.get("sapiRate").and_then(|x| x.as_i64()).unwrap_or(d.sapi_rate),
        announce_on_attention: v
            .get("announceOnAttention")
            .and_then(|x| x.as_bool())
            .unwrap_or(d.announce_on_attention),
    }
}

/// Write the five owned fields into voice.json, PRESERVING any unknown keys an
/// external script may have added (read-modify-write on the JSON object).
fn write_settings(settings: &VoiceSettings) -> Result<(), String> {
    let path = voice_settings_path();
    let mut root = std::fs::read_to_string(&path)
        .ok()
        .and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok())
        .filter(|v| v.is_object())
        .unwrap_or_else(|| serde_json::json!({}));
    let obj = root.as_object_mut().expect("filtered to object above");
    obj.insert("enabled".into(), serde_json::json!(settings.enabled));
    obj.insert("voice".into(), serde_json::json!(settings.voice));
    obj.insert(
        "volume".into(),
        serde_json::json!(settings.volume.clamp(0.0, 1.0)),
    );
    obj.insert("sapiRate".into(), serde_json::json!(settings.sapi_rate));
    obj.insert(
        "announceOnAttention".into(),
        serde_json::json!(settings.announce_on_attention),
    );
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("create {}: {e}", parent.display()))?;
    }
    let body = serde_json::to_vec_pretty(&root).map_err(|e| format!("serialize voice.json: {e}"))?;
    // Atomic replace (temp + rename): external tooling polls this file, and a
    // plain in-place write could hand it a truncated read mid-write. std's
    // rename replaces the destination on Windows too (MOVEFILE_REPLACE_EXISTING).
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, body).map_err(|e| format!("write {}: {e}", tmp.display()))?;
    std::fs::rename(&tmp, &path).map_err(|e| format!("rename {}: {e}", path.display()))
}

#[tauri::command]
pub async fn voice_settings_read() -> Result<VoiceSettings, String> {
    tauri::async_runtime::spawn_blocking(read_settings)
        .await
        .map_err(|e| format!("voice_settings_read task failed: {e}"))
}

#[tauri::command]
pub async fn voice_settings_write(settings: VoiceSettings) -> Result<(), String> {
    tauri::async_runtime::spawn_blocking(move || write_settings(&settings))
        .await
        .map_err(|e| format!("voice_settings_write task failed: {e}"))?
}

/// Pull one voice name out of a /voices entry: a bare string, or the first
/// likely string field of an object ({name}/{id}/{file}/{voice}).
fn voice_entry_name(v: &serde_json::Value) -> Option<String> {
    if let Some(s) = v.as_str() {
        return Some(s.to_string());
    }
    for key in ["name", "id", "file", "voice"] {
        if let Some(s) = v.get(key).and_then(|x| x.as_str()) {
            return Some(s.to_string());
        }
    }
    None
}

/// GET /voices on the local TTS server. Liberal in what it accepts: a JSON
/// array (of strings or objects) or an object wrapping one under "voices".
/// An unreachable server surfaces as Err - the UI renders that as the
/// "voice server unavailable" degradation state.
fn fetch_voices() -> Result<Vec<String>, String> {
    let url = format!("{}/voices", tts_base_url());
    let body = ureq::get(&url)
        .timeout(Duration::from_secs(3))
        .call()
        .map_err(|e| format!("voices request failed: {e}"))?
        .into_string()
        .map_err(|e| format!("voices response read failed: {e}"))?;
    let v: serde_json::Value =
        serde_json::from_str(&body).map_err(|e| format!("voices response not JSON: {e}"))?;
    let arr = v
        .as_array()
        .or_else(|| v.get("voices").and_then(|x| x.as_array()))
        .ok_or_else(|| "voices response has no voice list".to_string())?;
    Ok(arr.iter().filter_map(voice_entry_name).collect())
}

#[tauri::command]
pub async fn voice_list_voices() -> Result<Vec<String>, String> {
    tauri::async_runtime::spawn_blocking(fetch_voices)
        .await
        .map_err(|e| format!("voice_list_voices task failed: {e}"))?
}

/// POST /tts {text, voice} and return the WAV bytes base64-encoded (the
/// webview plays them via an Audio data URI - it must not fetch the server
/// itself, see the module header).
fn synthesize(text: &str, voice: &str) -> Result<String, String> {
    let url = format!("{}/tts", tts_base_url());
    let body = serde_json::json!({ "text": text, "voice": voice }).to_string();
    let resp = ureq::post(&url)
        .timeout(Duration::from_secs(10))
        .set("Content-Type", "application/json")
        .send_string(&body)
        .map_err(|e| format!("tts request failed: {e}"))?;
    let mut wav = Vec::new();
    resp.into_reader()
        // Piper WAVs for a short phrase are ~100-400 KB; 16 MB is a generous
        // ceiling that still stops a runaway response from ballooning memory.
        .take(16 * 1024 * 1024)
        .read_to_end(&mut wav)
        .map_err(|e| format!("tts response read failed: {e}"))?;
    Ok(STANDARD.encode(wav))
}

#[tauri::command]
pub async fn voice_tts(text: String, voice: String) -> Result<String, String> {
    tauri::async_runtime::spawn_blocking(move || synthesize(&text, &voice))
        .await
        .map_err(|e| format!("voice_tts task failed: {e}"))?
}
