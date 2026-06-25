// The PREFIX state machine (WS-3) — the tmux-style second tier of the hybrid
// keymap. The prefix (default ctrl+b) ARMS a transient mode; the next key
// resolves a `prefixed` binding and dispatches it through the executor.
//
// Lifecycle of one prefix interaction:
//   1. Canvas detects the prefix chord and calls `armPrefix()`. We flip a small
//      subscribable store (so a HUD hint can show "Ctrl+B …") and start a ~1.5s
//      timeout.
//   2. The NEXT keydown is routed here via `handlePrefixedKey(e)`:
//        - if it's the prefix AGAIN  -> send a LITERAL prefix keystroke to the
//          focused terminal (tmux's "press it twice for a real C-b") and disarm.
//        - if a bare key matches a `prefixed` binding -> dispatch it + disarm.
//        - otherwise (unbound key) -> disarm and let the key through.
//   3. The timeout disarms silently if no second key arrives.
//
// Only the prefix-armed window lives here; Canvas owns the document-level
// keydown listener and decides when to hand keys to this module.
import { create } from "zustand";
import { useKeybindings, prefixedCommandForKey } from "../store/keybindings";
import { runCommand } from "./keymapExecutor";
import { bareKeyFromEvent, chordFromEvent } from "./chord";
import { useWorkspace } from "../store/workspace";
import { writeTerminal } from "../ipc/client";

/** How long (ms) the prefix stays armed waiting for the second key. */
export const PREFIX_TIMEOUT_MS = 1500;

interface PrefixHud {
  /** Whether the prefix is currently armed (drives the HUD hint). */
  armed: boolean;
  /** The prefix chord that armed it (for the hint label), or null. */
  prefixLabel: string | null;
}

/** Subscribable armed-state for a small HUD hint (PrefixHint component). */
export const usePrefixHud = create<PrefixHud>(() => ({
  armed: false,
  prefixLabel: null,
}));

let timer: number | undefined;

function clearTimer(): void {
  if (timer !== undefined) {
    if (typeof window !== "undefined") window.clearTimeout(timer);
    timer = undefined;
  }
}

/** True while the prefix is armed (Canvas reads this to route the next key). */
export function isPrefixArmed(): boolean {
  return usePrefixHud.getState().armed;
}

/** Arm the prefix: show the hint + start the disarm timeout. Idempotent — a
 *  re-arm just restarts the timeout. */
export function armPrefix(prefixChord: string): void {
  clearTimer();
  usePrefixHud.setState({ armed: true, prefixLabel: prefixChord });
  if (typeof window !== "undefined") {
    timer = window.setTimeout(disarm, PREFIX_TIMEOUT_MS);
  }
}

/** Disarm: hide the hint + cancel the timeout. */
export function disarm(): void {
  clearTimer();
  if (usePrefixHud.getState().armed) {
    usePrefixHud.setState({ armed: false, prefixLabel: null });
  }
}

/**
 * Map a prefix chord to the literal control byte the terminal expects when the
 * prefix is pressed twice. `ctrl+<letter>` -> the C0 control code (Ctrl+A = 0x01
 * … Ctrl+Z = 0x1a); ctrl+b therefore sends 0x02, the real tmux prefix. Returns
 * null when the chord has no single-control-byte equivalent (we then skip the
 * literal send rather than guess).
 */
function literalForPrefix(prefixChord: string): string | null {
  const tokens = prefixChord.split("+");
  const key = tokens[tokens.length - 1];
  const hasCtrl = tokens.includes("ctrl");
  if (hasCtrl && key.length === 1 && key >= "a" && key <= "z") {
    return String.fromCharCode(key.charCodeAt(0) - 96); // 'a'->0x01
  }
  return null;
}

/**
 * Handle the key that follows an armed prefix. Returns true if the key was
 * consumed here (Canvas should preventDefault/stopPropagation and NOT fall
 * through to xterm); false means "not consumed" (let it through). Always disarms.
 */
export function handlePrefixedKey(e: KeyboardEvent): boolean {
  const prefixChord = useKeybindings.getState().prefixKey;

  // Double-tap the prefix -> send a literal prefix keystroke to the terminal.
  const asChord = chordFromEvent(e);
  if (asChord && asChord === prefixChord) {
    const literal = literalForPrefix(prefixChord);
    if (literal) {
      const id = useWorkspace.getState().focusedId;
      if (id) void writeTerminal(id, literal).catch(() => {});
    }
    disarm();
    return true;
  }

  // A bare key matching a prefixed binding -> dispatch the command.
  const bare = bareKeyFromEvent(e);
  if (bare) {
    const cmd = prefixedCommandForKey(bare);
    if (cmd) {
      disarm();
      runCommand(cmd);
      return true;
    }
  }

  // Anything else (unbound key) -> disarm and let it through.
  disarm();
  return false;
}
