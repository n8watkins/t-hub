// Typed wrappers over the voice command surface (src-tauri/src/voice.rs):
// voice.json persistence + the loopback Piper TTS proxy. Kept as its own
// module (rather than inline invokes) so the voice store, the announce
// watcher, and the Settings section share one mockable seam in tests.
import { invoke } from "@tauri-apps/api/core";

/** The shared ~/.t-hub/voice.json schema. camelCase on disk and over IPC -
 *  external captain tooling (announce.sh) reads the same field names, so the
 *  FILE is the source of truth (no localStorage mirror). */
export interface VoiceSettings {
  enabled: boolean;
  voice: string;
  /** Playback volume 0..=1. */
  volume: number;
  /** SAPI fallback speech rate for the external announce.sh path; the app UI
   *  does not edit it but must round-trip it faithfully. */
  sapiRate: number;
  announceOnAttention: boolean;
}

export function readVoiceSettings(): Promise<VoiceSettings> {
  return invoke("voice_settings_read");
}

export function writeVoiceSettings(settings: VoiceSettings): Promise<void> {
  return invoke("voice_settings_write", { settings });
}

/** Installed voice names from the TTS server's /voices (via the backend
 *  proxy). Rejects when the server is unreachable - the Settings section
 *  renders that as the "voice server unavailable" degradation state. */
export function listVoices(): Promise<string[]> {
  return invoke("voice_list_voices");
}

/** Synthesize `text` with `voice`; resolves to base64 WAV bytes for playback
 *  via an Audio data URI (the webview must not fetch the TTS server itself -
 *  it rejects browser-Origin requests; see src-tauri/src/voice.rs). */
export function synthesizeVoice(text: string, voice: string): Promise<string> {
  return invoke("voice_tts", { text, voice });
}
