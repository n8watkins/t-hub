import { create } from "zustand";

// --- Open-state store -------------------------------------------------------
// A tiny store so the executor (registerPaletteOpener) and any UI can open the
// palette without threading props through the tree.
interface PaletteState {
  open: boolean;
  setOpen: (v: boolean) => void;
}

export const usePalette = create<PaletteState>((set) => ({
  open: false,
  setOpen: (v) => set({ open: v }),
}));

/** Open the palette imperatively. Used by the Settings "Keyboard" section's
 *  rebind affordance (the palette is where rebinding lives). */
export function openKeyboardPalette(): void {
  usePalette.getState().setOpen(true);
}
