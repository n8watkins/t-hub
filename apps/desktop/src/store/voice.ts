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

/** Fields THIS store instance changed since the last successful load/persist.
 *  Persist is read-MERGE-write over this set: dirty fields carry the store's
 *  value, everything else re-adopts the FILE's current value - so an external
 *  edit (a captain script flipping `enabled` off) survives an unrelated
 *  slider drag instead of being resurrected from stale store state. */
let dirtyFields = new Set<keyof VoiceSettings>();

export const useVoice = create<VoiceState>((set, get) => {
  /** Flush to voice.json now: re-read the file, merge (dirty fields win from
   *  the store, the rest from the file), adopt the merged view into the live
   *  store, write it back. The Rust side additionally preserves unknown keys. */
  const persistNow = async () => {
    persistTimer = null;
    const dirtyNow = new Set(dirtyFields);
    let fileSettings: VoiceSettings | null = null;
    try {
      fileSettings = (await readVoiceSettings()) ?? null;
    } catch {
      // Unreadable/no backend: fall back to writing the store's view wholesale.
    }
    const s = get();
    const owned: VoiceSettings = {
      enabled: s.enabled,
      voice: s.voice,
      volume: s.volume,
      sapiRate: s.sapiRate,
      announceOnAttention: s.announceOnAttention,
    };
    const merged: VoiceSettings = fileSettings
      ? {
          enabled: dirtyNow.has("enabled") ? owned.enabled : fileSettings.enabled,
          voice: dirtyNow.has("voice") ? owned.voice : fileSettings.voice,
          volume: dirtyNow.has("volume") ? owned.volume : fileSettings.volume,
          sapiRate: dirtyNow.has("sapiRate")
            ? owned.sapiRate
            : fileSettings.sapiRate,
          announceOnAttention: dirtyNow.has("announceOnAttention")
            ? owned.announceOnAttention
            : fileSettings.announceOnAttention,
        }
      : owned;
    // Surface externally-changed (non-dirty) values in the UI immediately.
    if (fileSettings) set(merged);
    try {
      await writeVoiceSettings(merged);
      // Only the fields THIS flush covered come clean - a setter that fired
      // mid-flight re-dirtied its field and scheduled another persist.
      for (const f of dirtyNow) dirtyFields.delete(f);
    } catch {
      // Best-effort: outside Tauri (plain `pnpm dev`) there is no backend;
      // the fields stay dirty so a later flush retries them.
    }
  };

  /** Debounced flush (a slider drag becomes one write, not dozens). */
  const schedulePersist = () => {
    if (persistTimer) clearTimeout(persistTimer);
    persistTimer = setTimeout(() => void persistNow(), VOICE_PERSIST_DEBOUNCE_MS);
  };

  // A change made within the debounce window of closing the window must not
  // vanish - the file is shared with external tooling. pagehide fires on both
  // close-to-tray (webview persists) and real quit; the flush is fire-and-
  // forget, which is the best a teardown path can do.
  if (typeof window !== "undefined") {
    window.addEventListener("pagehide", () => {
      if (persistTimer) {
        clearTimeout(persistTimer);
        void persistNow();
      }
    });
  }

  return {
    ...DEFAULT_VOICE_SETTINGS,
    loaded: false,
    voices: null,
    voicesUnavailable: false,

    load: async () => {
      // Unflushed local intent (a pending debounce or an unpersisted dirty
      // field) must not be clobbered by stale file values - the imminent
      // merge-persist re-reads the file itself, so skipping here loses nothing.
      if (persistTimer !== null || dirtyFields.size > 0) return;
      try {
        const s = await readVoiceSettings();
        set({ ...s, loaded: true });
        dirtyFields.clear();
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
      dirtyFields.add("enabled");
      schedulePersist();
    },
    setVoice: (v) => {
      set({ voice: v });
      dirtyFields.add("voice");
      schedulePersist();
    },
    setVolume: (v) => {
      set({ volume: Math.max(0, Math.min(1, v)) });
      dirtyFields.add("volume");
      schedulePersist();
    },
    setAnnounceOnAttention: (v) => {
      set({ announceOnAttention: v });
      dirtyFields.add("announceOnAttention");
      schedulePersist();
    },
  };
});

/** Test-only: clear the module-level persist machinery (pending timer + dirty
 *  set) so cases can't leak debounced writes into each other. */
export function _resetVoicePersistForTest(): void {
  if (persistTimer) clearTimeout(persistTimer);
  persistTimer = null;
  dirtyFields = new Set();
}
