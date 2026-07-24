// Global-zoom slice: the shared terminal font size, clamped to bounds and
// persisted. Action bodies are verbatim; the store closure's `persist()` helper is
// reached via `deps`.
import {
  DEFAULT_FONT_SIZE,
  clampFont,
  type SliceDeps,
  type StoreGet,
  type StoreSet,
  type WorkspaceState,
} from "./internal";

export const createZoomSlice = (
  set: StoreSet,
  get: StoreGet,
  deps: SliceDeps,
): Pick<WorkspaceState, "zoomIn" | "zoomOut" | "zoomReset"> => {
  const { persist } = deps;

  return {
    zoomIn: () => {
      set({ fontSize: clampFont(get().fontSize + 1) });
      persist();
    },
    zoomOut: () => {
      set({ fontSize: clampFont(get().fontSize - 1) });
      persist();
    },
    zoomReset: () => {
      set({ fontSize: DEFAULT_FONT_SIZE });
      persist();
    },
  };
};
