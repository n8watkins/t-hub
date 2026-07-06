// The voice-announcements store (Settings > Voice).
//
// UNLIKE store/settings.ts this deliberately does NOT persist to localStorage:
// the source of truth is ~/.t-hub/voice.json (via the voice_settings_* Tauri
// commands), because EXTERNAL captain tooling (announce.sh) reads the same
// file. The store is a live view of that file: load() hydrates it at startup,
// setters update state immediately and flush to the file on a short debounce
// (a volume-slider drag becomes one write, not dozens).
//
// Transient (never written): the /voices list from the TTS server and its
// unavailable flag, which drive the Settings section's degradation state.
import { create } from "zustand";
import {
  listVoices,
  readVoiceSettings,
  writeVoiceSettings,
  type VoiceSettings,
} from "../ipc/voice";

/** Defaults when voice.json is missing/unreadable - mirrors the Rust side
 *  (voice.rs VoiceSettings::default): announcements opt-in, Ryan voice. */
export const DEFAULT_VOICE_SETTINGS: VoiceSettings = {
  enabled: false,
  voice: "en_US-ryan-high.onnx",
  volume: 0.8,
  sapiRate: 0,
  announceOnAttention: false,
};

/** Debounce for file writes: long enough to coalesce a slider drag, short
 *  enough that an external reader sees the change near-immediately. */
export const VOICE_PERSIST_DEBOUNCE_MS = 300;

interface VoiceState extends VoiceSettings {
  /** True once load() has resolved (from the file or to defaults). */
  loaded: boolean;
  /** Installed voices from the TTS server, or null before/without a fetch. */
  voices: string[] | null;
  /** True when the /voices proxy failed - the server is down/unreachable. */
  voicesUnavailable: boolean;

  /** Hydrate from voice.json (defaults when missing). Safe to re-run. */
  load: () => Promise<void>;
  /** Re-query the TTS server's voice list; flips voicesUnavailable on error. */
  refreshVoices: () => Promise<void>;
  setEnabled: (v: boolean) => void;
  setVoice: (v: string) => void;
  setVolume: (v: number) => void;
  setAnnounceOnAttention: (v: boolean) => void;
}

let persistTimer: ReturnType<typeof setTimeout> | null = null;

export const useVoice = create<VoiceState>((set, get) => {
  /** Write the five persisted fields to voice.json now. Read-modify-write on
   *  the Rust side, so unknown keys survive. */
  const persistNow = () => {
    persistTimer = null;
    const s = get();
    void writeVoiceSettings({
      enabled: s.enabled,
      voice: s.voice,
      volume: s.volume,
      sapiRate: s.sapiRate,
      announceOnAttention: s.announceOnAttention,
    }).catch(() => {
      // Best-effort: outside Tauri (plain `pnpm dev`) there is no backend;
      // the in-memory state still drives the session.
    });
  };

  /** Debounced flush (a slider drag becomes one write, not dozens). */
  const schedulePersist = () => {
    if (persistTimer) clearTimeout(persistTimer);
    persistTimer = setTimeout(persistNow, VOICE_PERSIST_DEBOUNCE_MS);
  };

  // A change made within the debounce window of closing the window must not
  // vanish - the file is shared with external tooling. pagehide fires on both
  // close-to-tray (webview persists) and real quit; the flush is fire-and-
  // forget, which is the best a teardown path can do.
  if (typeof window !== "undefined") {
    window.addEventListener("pagehide", () => {
      if (persistTimer) {
        clearTimeout(persistTimer);
        persistNow();
      }
    });
  }

  return {
    ...DEFAULT_VOICE_SETTINGS,
    loaded: false,
    voices: null,
    voicesUnavailable: false,

    load: async () => {
      try {
        const s = await readVoiceSettings();
        set({ ...s, loaded: true });
      } catch {
        set({ ...DEFAULT_VOICE_SETTINGS, loaded: true });
      }
    },

    refreshVoices: async () => {
      try {
        const voices = await listVoices();
        set({ voices, voicesUnavailable: false });
      } catch {
        set({ voices: null, voicesUnavailable: true });
      }
    },

    setEnabled: (v) => {
      set({ enabled: v });
      schedulePersist();
    },
    setVoice: (v) => {
      set({ voice: v });
      schedulePersist();
    },
    setVolume: (v) => {
      set({ volume: Math.max(0, Math.min(1, v)) });
      schedulePersist();
    },
    setAnnounceOnAttention: (v) => {
      set({ announceOnAttention: v });
      schedulePersist();
    },
  };
});
