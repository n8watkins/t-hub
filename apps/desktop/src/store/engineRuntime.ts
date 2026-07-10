// Live managed-lifecycle status for the webview (Settings degraded state + the
// announce path's synthesis routing). A thin store over the supervisor snapshot
// (ipc/engine.ts): hydrated once at startup and kept current by the supervisor's
// control://event pushes (wired in lib/engineStatusMount.ts).
//
// Transient by design (never persisted): this reflects the backend's live view,
// not user settings. When the managed lifecycle is off (default), `status`
// stays managed:false and the UI keeps using the #52 direct probes.
import { create } from "zustand";
import {
  engineRuntimeStatus,
  type EngineRuntimeStatus,
} from "../ipc/engine";

interface EngineRuntimeState {
  /** Latest supervisor snapshot, or null before the first read resolves. */
  status: EngineRuntimeStatus | null;
  /** Hydrate once from the command (safe to re-run). */
  load: () => Promise<void>;
  /** Apply a pushed snapshot (from the control://event stream). */
  apply: (s: EngineRuntimeStatus) => void;
}

export const useEngineRuntime = create<EngineRuntimeState>((set) => ({
  status: null,
  load: async () => {
    try {
      set({ status: await engineRuntimeStatus() });
    } catch {
      // No backend (plain `pnpm dev`) / command missing: leave null; the UI
      // treats null the same as unmanaged and uses the #52 probes.
    }
  },
  apply: (s) => set({ status: s }),
}));
