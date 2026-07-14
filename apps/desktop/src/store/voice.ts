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
  voiceHealth,
  writeVoiceSettings,
  type VoiceEngine,
  type VoiceSettings,
} from "../ipc/voice";

/** Per-engine reachability for the Settings health display. "unknown" before
 *  the first probe resolves (rendered as "checking…"), then "up"/"down". */
export type EngineHealthStatus = "unknown" | "up" | "down";

/** Every engine the health display covers. Order = display order. */
export const HEALTH_ENGINES: readonly VoiceEngine[] = ["piper", "kokoro"];

/** Defaults when voice.json is missing/unreadable - mirrors the Rust side
 *  (voice.rs VoiceSettings::default): announcements opt-in, Piper engine, Ryan
 *  voice. */
export const DEFAULT_VOICE_SETTINGS: VoiceSettings = {
  enabled: false,
  engine: "piper",
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
  /** Reachability of BOTH engines (not just the selected one) for the Settings
   *  health display. Transient, never persisted; "unknown" until first probed. */
  health: Record<VoiceEngine, EngineHealthStatus>;

  /** Hydrate from voice.json (defaults when missing). Safe to re-run. */
  load: () => Promise<void>;
  /** Re-query the SELECTED engine's voice list; flips voicesUnavailable on
   *  error. Clears the stale list first so a slow/failed switch never shows the
   *  previous engine's voices. */
  refreshVoices: () => Promise<void>;
  /** Probe every engine's /health (bounded, in parallel) and update `health`.
   *  Called on Settings open + a slow periodic tick while the panel is open, so
   *  a silent engine death surfaces without the general noticing by ear. */
  probeHealth: () => Promise<void>;
  setEnabled: (v: boolean) => void;
  /** Switch the TTS engine and immediately re-query its voice list (the two
   *  engines have disjoint voice sets). */
  setEngine: (v: VoiceEngine) => void;
  setVoice: (v: string) => void;
  setVolume: (v: number) => void;
  setAnnounceOnAttention: (v: boolean) => void;
}

let persistTimer: ReturnType<typeof setTimeout> | null = null;
let loadGeneration = 0;

/** Fields THIS store instance changed since the last successful load/persist.
 *  Persist is read-MERGE-write over this set: dirty fields carry the store's
 *  value, everything else re-adopts the FILE's current value - so an external
 *  edit (a captain script flipping `enabled` off) survives an unrelated
 *  slider drag instead of being resurrected from stale store state. */
let dirtyFields = new Set<keyof VoiceSettings>();
const fieldGenerations: Record<keyof VoiceSettings, number> = {
  enabled: 0,
  engine: 0,
  voice: 0,
  volume: 0,
  sapiRate: 0,
  announceOnAttention: 0,
};

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
      engine: s.engine,
      voice: s.voice,
      volume: s.volume,
      sapiRate: s.sapiRate,
      announceOnAttention: s.announceOnAttention,
    };
    const merged: VoiceSettings = fileSettings
      ? {
          enabled: dirtyNow.has("enabled") ? owned.enabled : fileSettings.enabled,
          engine: dirtyNow.has("engine") ? owned.engine : fileSettings.engine,
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
    health: { piper: "unknown", kokoro: "unknown" },

    load: async () => {
      const generation = ++loadGeneration;
      const observedFieldGenerations = { ...fieldGenerations };
      const changedDuringLoad = (field: keyof VoiceSettings) =>
        dirtyFields.has(field) ||
        fieldGenerations[field] !== observedFieldGenerations[field];
      try {
        const file = await readVoiceSettings();
        if (generation !== loadGeneration) return;
        const current = get();
        set({
          enabled: changedDuringLoad("enabled") ? current.enabled : file.enabled,
          engine: changedDuringLoad("engine") ? current.engine : file.engine,
          voice: changedDuringLoad("voice") ? current.voice : file.voice,
          volume: changedDuringLoad("volume") ? current.volume : file.volume,
          sapiRate: changedDuringLoad("sapiRate") ? current.sapiRate : file.sapiRate,
          announceOnAttention: changedDuringLoad("announceOnAttention")
            ? current.announceOnAttention
            : file.announceOnAttention,
          loaded: true,
        });
      } catch {
        if (generation !== loadGeneration) return;
        const current = get();
        set({
          enabled: changedDuringLoad("enabled")
            ? current.enabled
            : DEFAULT_VOICE_SETTINGS.enabled,
          engine: changedDuringLoad("engine")
            ? current.engine
            : DEFAULT_VOICE_SETTINGS.engine,
          voice: changedDuringLoad("voice")
            ? current.voice
            : DEFAULT_VOICE_SETTINGS.voice,
          volume: changedDuringLoad("volume")
            ? current.volume
            : DEFAULT_VOICE_SETTINGS.volume,
          sapiRate: changedDuringLoad("sapiRate")
            ? current.sapiRate
            : DEFAULT_VOICE_SETTINGS.sapiRate,
          announceOnAttention: changedDuringLoad("announceOnAttention")
            ? current.announceOnAttention
            : DEFAULT_VOICE_SETTINGS.announceOnAttention,
          loaded: true,
        });
      }
    },

    refreshVoices: async () => {
      const engine = get().engine;
      try {
        const voices = await listVoices(engine);
        // Ignore a stale response if the engine changed while in flight (a fast
        // dropdown toggle): only the latest engine's list should win.
        if (get().engine !== engine) return;
        set({ voices, voicesUnavailable: false });
      } catch {
        if (get().engine !== engine) return;
        set({ voices: null, voicesUnavailable: true });
      }
    },

    probeHealth: async () => {
      // Probe every engine in parallel; each backend call is individually
      // bounded (2s) and resolves even for a down server, so this settles fast
      // and never hangs the panel. A probe that rejects outright (backend task
      // failure) is treated as "down" - a definite state beats a stuck spinner.
      const results = await Promise.all(
        HEALTH_ENGINES.map(async (engine): Promise<[VoiceEngine, EngineHealthStatus]> => {
          try {
            const h = await voiceHealth(engine);
            return [engine, h.reachable ? "up" : "down"];
          } catch {
            return [engine, "down"];
          }
        }),
      );
      set((s) => {
        const health = { ...s.health };
        for (const [engine, status] of results) health[engine] = status;
        return { health };
      });
    },

    setEnabled: (v) => {
      set({ enabled: v });
      fieldGenerations.enabled += 1;
      dirtyFields.add("enabled");
      schedulePersist();
    },
    setEngine: (v) => {
      if (get().engine === v) return;
      // Drop the old engine's voice list immediately so the UI never shows the
      // wrong set mid-switch. The NEW engine's list is loaded by the Settings
      // section's engine-keyed effect (the single refreshVoices seam), so this
      // action stays pure state + persist and never double-queries.
      set({ engine: v, voices: null, voicesUnavailable: false });
      fieldGenerations.engine += 1;
      dirtyFields.add("engine");
      schedulePersist();
    },
    setVoice: (v) => {
      set({ voice: v });
      fieldGenerations.voice += 1;
      dirtyFields.add("voice");
      schedulePersist();
    },
    setVolume: (v) => {
      set({ volume: Math.max(0, Math.min(1, v)) });
      fieldGenerations.volume += 1;
      dirtyFields.add("volume");
      schedulePersist();
    },
    setAnnounceOnAttention: (v) => {
      set({ announceOnAttention: v });
      fieldGenerations.announceOnAttention += 1;
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
