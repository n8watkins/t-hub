// Voice announcements (Settings > Voice): persistence for ~/.t-hub/voice.json
// plus a loopback proxy to a local TTS server, selectable between two ENGINES -
// Piper (port 7477) and Kokoro (port 7478).
//
// Why a backend proxy at all: the TTS servers REJECT requests that carry a
// browser Origin header, so the webview must never fetch them directly. ureq
// sends no Origin, so routing /voices and /tts through these commands sidesteps
// that wholesale (and keeps the webview free of mixed-content/CORS concerns).
// Both engines expose the SAME API (GET /health, GET /voices, POST /tts) so a
// single proxy serves either one - only the base URL (port) differs by engine,
// chosen live per call (the frontend passes the selected engine) so switching
// the Settings dropdown re-targets immediately, before the debounced save.
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
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

/// Cap on the /tts `text` from the webview: announcements are one short
/// phrase ("<label> needs your attention" / the test phrase), so anything
/// beyond a few KB is a bug or abuse, not a request to honor.
const MAX_TTS_TEXT_BYTES: usize = 4096;

/// Cap on the /tts response body. A short-phrase WAV runs ~100-400 KB (Piper)
/// or a bit more at Kokoro's 24 kHz; a response past this is a misbehaving
/// server, and truncating silently would hand the webview a corrupt WAV - so
/// error instead.
const MAX_TTS_RESPONSE_BYTES: u64 = 16 * 1024 * 1024;

/// The selectable TTS backend. Serialized lowercase ("piper" / "kokoro") in
/// voice.json and over IPC so external tooling reads the same token. Piper is
/// the default (the pre-existing engine; Kokoro is additive).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum VoiceEngine {
    #[default]
    Piper,
    Kokoro,
}

impl VoiceEngine {
    /// Parse the lenient wire token; anything unrecognized falls back to Piper
    /// (the safe default engine) rather than erroring a whole settings read.
    fn from_token(s: &str) -> Self {
        match s {
            "kokoro" => VoiceEngine::Kokoro,
            _ => VoiceEngine::Piper,
        }
    }

    /// The lowercase token written to voice.json / sent over IPC.
    fn token(self) -> &'static str {
        match self {
            VoiceEngine::Piper => "piper",
            VoiceEngine::Kokoro => "kokoro",
        }
    }

    /// STRICT token parse: only the two known engines, `None` for anything else.
    /// Unlike `from_token` (which defaults unknown -> Piper for lenient settings
    /// reads), identity checks must NOT coerce a foreign token to a real engine -
    /// the engine supervisor's "is the port occupant provably ours?" decision
    /// (D2) depends on an unrecognized `/health` yielding None (a stranger).
    pub(crate) fn from_token_strict(s: &str) -> Option<Self> {
        match s {
            "kokoro" => Some(VoiceEngine::Kokoro),
            "piper" => Some(VoiceEngine::Piper),
            _ => None,
        }
    }

    /// The loopback port each engine's local server listens on. Piper owns
    /// 7477 (pre-existing); Kokoro owns 7478. `pub(crate)` so the engine
    /// supervisor knows which port a spawned engine should come up on.
    pub(crate) fn default_port(self) -> u16 {
        match self {
            VoiceEngine::Piper => 7477,
            VoiceEngine::Kokoro => 7478,
        }
    }

    /// The per-engine env override key, so E2E / tests can point an engine at a
    /// stub server without a real Piper/Kokoro running.
    fn url_env_key(self) -> &'static str {
        match self {
            VoiceEngine::Piper => "T_HUB_PIPER_URL",
            VoiceEngine::Kokoro => "T_HUB_KOKORO_URL",
        }
    }
}

/// A loopback-only agent: no redirect following (a local TTS server has no
/// business redirecting, and following one could leak the request elsewhere).
/// `pub(crate)` so the engine supervisor's health-watch reuses the same
/// no-Origin, redirect-free client the proxy uses.
pub(crate) fn agent() -> ureq::Agent {
    ureq::AgentBuilder::new().redirects(0).build()
}

/// The TTS server base URL for `engine`: the per-engine env override when set,
/// else `http://127.0.0.1:<default_port>`. The port is the ONLY thing that
/// differs between engines (identical API), so routing is pure port selection.
/// `pub(crate)` so the engine supervisor probes the same base URL the proxy uses.
pub(crate) fn base_url_for_engine(engine: VoiceEngine) -> String {
    if let Ok(u) = std::env::var(engine.url_env_key()) {
        if !u.trim().is_empty() {
            return u;
        }
    }
    format!("http://127.0.0.1:{}", engine.default_port())
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
    /// The selected TTS backend (default Piper). The voices list + /tts target
    /// follow this.
    pub engine: VoiceEngine,
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
            engine: VoiceEngine::Piper,
            voice: "en_US-ryan-high.onnx".to_string(),
            volume: 0.8,
            sapi_rate: 0,
            announce_on_attention: false,
        }
    }
}

/// Extract settings from an already-parsed JSON object, field-by-field, each
/// falling back to the default independently - so a partial file written by
/// external tooling (or a pre-Kokoro file with no `engine` key) never zeroes
/// the rest. Split out from `read_settings` so the lenient contract is unit
/// testable without touching the filesystem.
fn parse_settings(v: &serde_json::Value) -> VoiceSettings {
    let d = VoiceSettings::default();
    VoiceSettings {
        enabled: v
            .get("enabled")
            .and_then(|x| x.as_bool())
            .unwrap_or(d.enabled),
        engine: v
            .get("engine")
            .and_then(|x| x.as_str())
            .map(VoiceEngine::from_token)
            .unwrap_or(d.engine),
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
        sapi_rate: v
            .get("sapiRate")
            .and_then(|x| x.as_i64())
            .unwrap_or(d.sapi_rate),
        announce_on_attention: v
            .get("announceOnAttention")
            .and_then(|x| x.as_bool())
            .unwrap_or(d.announce_on_attention),
    }
}

/// The currently-selected engine from voice.json (the managed lifecycle's
/// PRIMARY). `pub(crate)` so the engine supervisor picks the same engine the
/// user chose in Settings. Lenient: defaults to Piper if the file is absent.
pub(crate) fn current_engine() -> VoiceEngine {
    read_settings().engine
}

/// Read voice.json leniently: missing file or unparseable content yields the
/// defaults.
fn read_settings() -> VoiceSettings {
    let raw = match std::fs::read_to_string(voice_settings_path()) {
        Ok(s) if !s.trim().is_empty() => s,
        _ => return VoiceSettings::default(),
    };
    match serde_json::from_str::<serde_json::Value>(&raw) {
        Ok(v) => parse_settings(&v),
        Err(_) => VoiceSettings::default(),
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
    obj.insert("engine".into(), serde_json::json!(settings.engine.token()));
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
    let body =
        serde_json::to_vec_pretty(&root).map_err(|e| format!("serialize voice.json: {e}"))?;
    // Atomic replace (temp + rename): external tooling polls this file, and a
    // plain in-place write could hand it a truncated read mid-write. std's
    // rename replaces the destination on Windows too (MOVEFILE_REPLACE_EXISTING).
    // The temp name carries pid + a process-wide counter so two concurrent
    // writers (or two app instances) can never interleave on the same temp
    // file - each renames its own complete body; last rename wins whole.
    static TMP_COUNTER: AtomicU64 = AtomicU64::new(0);
    let tmp = path.with_extension(format!(
        "json.{}.{}.tmp",
        std::process::id(),
        TMP_COUNTER.fetch_add(1, Ordering::Relaxed),
    ));
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

/// GET /voices on the selected engine's local server. Liberal in what it
/// accepts: a JSON array (of strings or objects) or an object wrapping one
/// under "voices" (Piper returns the latter; the Kokoro server matches it). An
/// unreachable server surfaces as Err - the UI renders that as the "voice
/// server unavailable" degradation state.
fn fetch_voices(engine: VoiceEngine) -> Result<Vec<String>, String> {
    let url = format!("{}/voices", base_url_for_engine(engine));
    let body = agent()
        .get(&url)
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
pub async fn voice_list_voices(engine: VoiceEngine) -> Result<Vec<String>, String> {
    tauri::async_runtime::spawn_blocking(move || fetch_voices(engine))
        .await
        .map_err(|e| format!("voice_list_voices task failed: {e}"))?
}

/// POST /tts {text, voice} to the selected engine and return the WAV bytes
/// base64-encoded (the webview plays them via an Audio data URI - it must not
/// fetch the server itself, see the module header).
fn synthesize(text: &str, voice: &str, engine: VoiceEngine) -> Result<String, String> {
    if text.len() > MAX_TTS_TEXT_BYTES {
        return Err(format!(
            "tts text too long: {} bytes (max {MAX_TTS_TEXT_BYTES})",
            text.len(),
        ));
    }
    let url = format!("{}/tts", base_url_for_engine(engine));
    let body = serde_json::json!({ "text": text, "voice": voice }).to_string();
    let resp = agent()
        .post(&url)
        .timeout(Duration::from_secs(10))
        .set("Content-Type", "application/json")
        .send_string(&body)
        .map_err(|e| format!("tts request failed: {e}"))?;
    let mut wav = Vec::new();
    // Read one byte past the cap so an oversized response is DETECTED and
    // rejected rather than silently truncated into a corrupt WAV.
    resp.into_reader()
        .take(MAX_TTS_RESPONSE_BYTES + 1)
        .read_to_end(&mut wav)
        .map_err(|e| format!("tts response read failed: {e}"))?;
    if wav.len() as u64 > MAX_TTS_RESPONSE_BYTES {
        return Err(format!(
            "tts response exceeds {MAX_TTS_RESPONSE_BYTES} bytes",
        ));
    }
    Ok(STANDARD.encode(wav))
}

#[tauri::command]
pub async fn voice_tts(text: String, voice: String, engine: VoiceEngine) -> Result<String, String> {
    tauri::async_runtime::spawn_blocking(move || synthesize(&text, &voice, engine))
        .await
        .map_err(|e| format!("voice_tts task failed: {e}"))?
}

/// One engine's reachability snapshot for the Settings health display. Serialized
/// camelCase over IPC. `reachable` is the ONLY thing the UI keys its error state
/// on; `detail` carries the probe error (server down / timeout) for a tooltip,
/// never a reason to hide the down state.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EngineHealth {
    pub engine: VoiceEngine,
    pub reachable: bool,
    pub detail: Option<String>,
}

/// GET /health on the engine's local server with a SHORT bounded timeout. A
/// down/slow/unreachable server is a NORMAL outcome here (reachable=false with
/// the error in `detail`), not a command error - the Settings panel renders it
/// as the engine's error state, so the probe itself must always resolve Ok.
///
/// Bounded on purpose (this ship spent #45/#48/#50 killing unbounded calls): the
/// 2s connect/read timeout caps a single probe, and the caller (Settings, on
/// open + a slow periodic tick) fans out at most one probe per engine at a time.
fn probe_health(engine: VoiceEngine) -> EngineHealth {
    let base = base_url_for_engine(engine);
    probe_health_at(engine, &base)
}

/// The URL-taking core of `probe_health`, split out so a test can point it at an
/// arbitrary (e.g. deliberately-dead ephemeral) base URL WITHOUT mutating the
/// process-global engine-URL env - hermetic under a parallel test harness.
/// `pub(crate)` so the engine supervisor's health-watch reuses the exact same
/// bounded probe the Settings health display uses.
pub(crate) fn probe_health_at(engine: VoiceEngine, base_url: &str) -> EngineHealth {
    let url = format!("{base_url}/health");
    match agent().get(&url).timeout(Duration::from_secs(2)).call() {
        // A 2xx: the server is up and healthy.
        Ok(_) => EngineHealth {
            engine,
            reachable: true,
            detail: None,
        },
        // ureq surfaces a 4xx/5xx as Error::Status - the server ANSWERED, so it
        // is reachable (a live-but-sick server), just not a clean 200. We carry
        // the code in `detail` but keep reachable=true: the incident we guard
        // against is a fully-dead server (connection refused), which is a
        // Transport error below, not a status.
        Err(ureq::Error::Status(code, _)) => EngineHealth {
            engine,
            reachable: true,
            detail: Some(format!("health returned HTTP {code}")),
        },
        // Transport error: connection refused / timeout / DNS - the server is
        // not there. THIS is the silent-death case the Settings error surfaces.
        Err(e) => EngineHealth {
            engine,
            reachable: false,
            detail: Some(e.to_string()),
        },
    }
}

/// The occupant's SELF-IDENTIFIED engine from `GET /health`, or None when the
/// server is unreachable, answers non-2xx, isn't JSON, or its `engine` field is
/// absent/unrecognized. Bounded (2s) via the same no-Origin agent.
///
/// This is the identity the engine supervisor's startup squatter policy (D2)
/// needs: a foreign HTTP server squatting the port answers with no recognized
/// `engine`, so it yields None -> "stranger" -> never reclaimed/adopted. Kokoro
/// self-identifies (`{"status":"ok","engine":"kokoro",...}`); Piper's /health
/// has no `engine` field, so it too reads as None here (fine - Piper is never
/// the managed primary in wave 1, and the standby-adopt path uses reachability,
/// not this identity).
pub(crate) fn probe_identity_at(base_url: &str) -> Option<VoiceEngine> {
    let url = format!("{base_url}/health");
    let body = agent()
        .get(&url)
        .timeout(Duration::from_secs(2))
        .call()
        .ok()?
        .into_string()
        .ok()?;
    let v: serde_json::Value = serde_json::from_str(&body).ok()?;
    let token = v.get("engine").and_then(|x| x.as_str())?;
    VoiceEngine::from_token_strict(token)
}

/// Probe one engine's /health for the Settings dual-engine status display.
/// Always Ok (a down server is `reachable: false`); errors only if the blocking
/// task itself panics.
#[tauri::command]
pub async fn voice_health(engine: VoiceEngine) -> Result<EngineHealth, String> {
    tauri::async_runtime::spawn_blocking(move || probe_health(engine))
        .await
        .map_err(|e| format!("voice_health task failed: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Engine-selection routing: each engine's default base URL targets its own
    /// loopback port (Piper 7477, Kokoro 7478), so /voices and /tts reach the
    /// right server. Guarded against ambient env overrides so the mapping under
    /// test is the pure default.
    #[test]
    fn base_url_routes_by_engine_default_ports() {
        // Only meaningful when the per-engine override envs are unset (they are
        // in a normal test run; skip the assertion if some outer env set one).
        if std::env::var(VoiceEngine::Piper.url_env_key()).is_err() {
            assert_eq!(
                base_url_for_engine(VoiceEngine::Piper),
                "http://127.0.0.1:7477"
            );
        }
        if std::env::var(VoiceEngine::Kokoro.url_env_key()).is_err() {
            assert_eq!(
                base_url_for_engine(VoiceEngine::Kokoro),
                "http://127.0.0.1:7478"
            );
        }
        // The port mapping itself is pure - always assert that.
        assert_eq!(VoiceEngine::Piper.default_port(), 7477);
        assert_eq!(VoiceEngine::Kokoro.default_port(), 7478);
    }

    #[test]
    fn engine_token_round_trips() {
        assert_eq!(VoiceEngine::from_token("piper"), VoiceEngine::Piper);
        assert_eq!(VoiceEngine::from_token("kokoro"), VoiceEngine::Kokoro);
        // Unknown / legacy tokens fall back to the safe default engine.
        assert_eq!(VoiceEngine::from_token("festival"), VoiceEngine::Piper);
        assert_eq!(VoiceEngine::from_token(""), VoiceEngine::Piper);
        assert_eq!(VoiceEngine::Piper.token(), "piper");
        assert_eq!(VoiceEngine::Kokoro.token(), "kokoro");
    }

    /// voice.json engine round-trip: a settings blob carrying engine "kokoro"
    /// reads back as Kokoro (and serializes as the lowercase token).
    #[test]
    fn engine_persists_in_voice_json_shape() {
        let json = serde_json::json!({
            "enabled": true,
            "engine": "kokoro",
            "voice": "af_heart",
            "volume": 0.5,
            "sapiRate": 0,
            "announceOnAttention": false,
        });
        let parsed: VoiceSettings = serde_json::from_value(json).unwrap();
        assert_eq!(parsed.engine, VoiceEngine::Kokoro);
        // Serializes back to the lowercase wire token external tooling reads.
        let out = serde_json::to_value(&parsed).unwrap();
        assert_eq!(out.get("engine").and_then(|v| v.as_str()), Some("kokoro"));
    }

    /// A voice.json with NO engine key (a pre-Kokoro file) parses to Piper via
    /// the lenient reader, so the upgrade never breaks an existing install -
    /// and the other fields still come through.
    #[test]
    fn missing_engine_key_defaults_to_piper() {
        let parsed = parse_settings(&serde_json::json!({
            "enabled": true,
            "voice": "en_US-ryan-high.onnx",
            "volume": 0.6,
            "sapiRate": 0,
            "announceOnAttention": false,
        }));
        assert_eq!(parsed.engine, VoiceEngine::Piper);
        assert!(parsed.enabled);
        assert_eq!(parsed.voice, "en_US-ryan-high.onnx");
        assert_eq!(parsed.volume, 0.6);
    }

    /// STRICT identity parse (engine supervisor D2): only the two known tokens
    /// map to an engine; ANYTHING else is None, so a foreign `/health` never
    /// coerces to a real engine and never triggers a reclaim/adopt.
    #[test]
    fn from_token_strict_only_matches_known_engines() {
        assert_eq!(
            VoiceEngine::from_token_strict("kokoro"),
            Some(VoiceEngine::Kokoro)
        );
        assert_eq!(
            VoiceEngine::from_token_strict("piper"),
            Some(VoiceEngine::Piper)
        );
        assert_eq!(VoiceEngine::from_token_strict("festival"), None);
        assert_eq!(VoiceEngine::from_token_strict(""), None);
        assert_eq!(VoiceEngine::from_token_strict("Kokoro"), None); // case-sensitive
    }

    /// The lenient reader picks up an explicit Kokoro engine too.
    #[test]
    fn parse_settings_reads_kokoro_engine() {
        let parsed = parse_settings(&serde_json::json!({ "engine": "kokoro" }));
        assert_eq!(parsed.engine, VoiceEngine::Kokoro);
    }

    /// A dead server (nothing listening) probes as reachable=false with the
    /// transport error in `detail` - the exact silent-death case the Settings
    /// error state exists to surface. Hermetic: binds an ephemeral loopback port,
    /// reads it, then DROPS the listener so the port is free-but-unlistened, and
    /// points the probe straight at that URL - no process-global env mutation
    /// (safe under the parallel test harness / Rust 2024). Connection-refused
    /// returns fast, well inside the 2s bound.
    #[test]
    fn probe_health_reports_dead_server_unreachable() {
        use std::net::TcpListener;
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind ephemeral port");
        let port = listener.local_addr().unwrap().port();
        drop(listener); // close it: the port is now reliably refusing connections
        let base = format!("http://127.0.0.1:{port}");

        let h = probe_health_at(VoiceEngine::Kokoro, &base);
        assert_eq!(h.engine, VoiceEngine::Kokoro);
        assert!(!h.reachable, "a dead server must probe as unreachable");
        assert!(
            h.detail.is_some(),
            "the transport error is carried for the UI"
        );
    }

    /// EngineHealth serializes camelCase so the webview reads `reachable`
    /// directly (and the engine token stays the lowercase wire form).
    #[test]
    fn engine_health_serializes_camel_case() {
        let out = serde_json::to_value(EngineHealth {
            engine: VoiceEngine::Piper,
            reachable: true,
            detail: None,
        })
        .unwrap();
        assert_eq!(out.get("reachable").and_then(|v| v.as_bool()), Some(true));
        assert_eq!(out.get("engine").and_then(|v| v.as_str()), Some("piper"));
    }
}
