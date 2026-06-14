// useShiftHeld — a tiny shared hook reporting whether the Shift key is currently
// held. Used to morph each tile's close (×) control into a delete control while
// Shift is down (hold Shift → every "close" becomes "delete session", the
// macOS-style "reveal the alternate action" pattern).
//
// Backed by a single module-level listener that fans out to all subscribers, so
// a wall of tiles doesn't install one window listener each. Resets on window
// blur so a Shift held while the window loses focus can't get stuck "on".
import { useEffect, useState } from "react";

let shiftHeld = false;
const subscribers = new Set<(v: boolean) => void>();
let installed = false;

function setShift(v: boolean): void {
  if (v === shiftHeld) return;
  shiftHeld = v;
  for (const fn of subscribers) fn(v);
}

function ensureInstalled(): void {
  if (installed || typeof window === "undefined") return;
  installed = true;
  window.addEventListener("keydown", (e) => {
    if (e.key === "Shift") setShift(true);
  });
  window.addEventListener("keyup", (e) => {
    if (e.key === "Shift") setShift(false);
  });
  // If the window loses focus while Shift is down, the keyup never arrives —
  // clear it so the delete affordance can't latch on.
  window.addEventListener("blur", () => setShift(false));
}

/** Reactively report whether Shift is currently held (one shared listener). */
export function useShiftHeld(): boolean {
  const [v, setV] = useState(shiftHeld);
  useEffect(() => {
    ensureInstalled();
    subscribers.add(setV);
    setV(shiftHeld); // sync in case it changed before subscribe
    return () => {
      subscribers.delete(setV);
    };
  }, []);
  return v;
}
