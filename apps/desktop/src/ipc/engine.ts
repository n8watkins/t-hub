// Managed TTS-engine lifecycle, webview side (backend: src-tauri/src/
// engine_supervisor.rs). The supervisor owns the Kokoro child, health-watches
// it, and AUTO-FALLS-BACK to Piper on failure; this module is how Settings and
// the announce path learn the live ACTIVE engine (vs the user's SELECTED one)
// and the green/amber/red degraded level, so the general is always told.
//
// Delivery: the supervisor emits over the control://event stream, forwarded to
// the webview and demuxed by `onControlEvent` (same path as onSupervision /
// onSessionStatus). When the managed lifecycle is OFF (default), the command
// returns `managed:false` and callers fall back to the #52 direct probes.
import { invoke } from "@tauri-apps/api/core";
import { onControlEvent } from "./controlClient";
import type { VoiceEngine } from "./voice";

/** The green/amber/red ladder (mirrors the Rust RuntimeLevel). */
export type RuntimeLevel = "green" | "amber" | "red" | "unknown";

/** Per-engine reachability as the supervisor's watcher sees it. */
export type EngineRuntimeHealth = "unknown" | "up" | "down";

/** The supervisor snapshot (mirrors Rust SupervisorSnapshot, camelCase). */
export interface EngineRuntimeStatus {
  /** False when the managed lifecycle isn't running (flag off) - callers then
   *  use the #52 dual-engine probes instead of this snapshot. */
  managed: boolean;
  /** The user's chosen engine (the primary the supervisor manages). */
  selectedEngine: VoiceEngine;
  /** Where synthesis is currently routed (differs from selected on fallback). */
  activeEngine: VoiceEngine;
  /** True while active != selected due to a primary failure. */
  degraded: boolean;
  level: RuntimeLevel;
  kokoro: EngineRuntimeHealth;
  piper: EngineRuntimeHealth;
}

/** A fallback/recovery toast pushed by the supervisor (kind maps to notify()). */
export interface EngineToast {
  kind: "error" | "done";
  title: string;
  body: string;
}

/** Backend channel names (the raw control:// channels the supervisor emits). */
export const EngineEvents = {
  runtimeStatus: "engine://runtime_status",
  toast: "engine://toast",
} as const;

/** Current managed-lifecycle status (one-shot). Cheap: reads shared backend
 *  state under a short lock, never a live probe. */
export function engineRuntimeStatus(): Promise<EngineRuntimeStatus> {
  return invoke("engine_runtime_status");
}

/** Subscribe to live supervisor status pushes. Returns an unsubscribe fn. */
export function onEngineRuntimeStatus(
  cb: (s: EngineRuntimeStatus) => void,
): () => void {
  return onControlEvent(EngineEvents.runtimeStatus, (p) =>
    cb(p as EngineRuntimeStatus),
  );
}

/** Subscribe to fallback/recovery toasts. Returns an unsubscribe fn. */
export function onEngineToast(cb: (t: EngineToast) => void): () => void {
  return onControlEvent(EngineEvents.toast, (p) => cb(p as EngineToast));
}

/** Default synthesis voice per engine when we don't have a user-chosen one for
 *  it - used to remap on fallback (the selected Kokoro voice is not a Piper
 *  voice). Mirrors the Rust/store defaults. */
export const DEFAULT_ENGINE_VOICE: Record<VoiceEngine, string> = {
  piper: "en_US-ryan-high.onnx",
  kokoro: "af_heart",
};

/**
 * The effective synthesis {engine, voice} given the managed runtime status and
 * the user's selected voice. THE wave-1 correctness catch: when the supervisor
 * has fallen back, synthesis must target the ACTIVE engine with a VALID voice
 * for it - the selected Kokoro voice (e.g. `af_heart`) would 400 on Piper.
 *
 * Unmanaged (or not degraded): pass through the selected engine + voice. On
 * fallback: switch to the active engine and its default voice.
 */
export function effectiveTarget(
  status: EngineRuntimeStatus | null,
  selectedEngine: VoiceEngine,
  selectedVoice: string,
): { engine: VoiceEngine; voice: string } {
  if (!status || !status.managed || status.activeEngine === selectedEngine) {
    return { engine: selectedEngine, voice: selectedVoice };
  }
  return {
    engine: status.activeEngine,
    voice: DEFAULT_ENGINE_VOICE[status.activeEngine],
  };
}
