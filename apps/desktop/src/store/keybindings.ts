// The keybindings store — T-Hub's HYBRID keyboard model (WS-3).
//
// Two tiers, both fully rebindable + persisted:
//   - DIRECT bindings: a single chord (e.g. `ctrl+t`) fires a command outright.
//     These are the high-frequency actions that used to be hardcoded in Canvas.
//   - PREFIXED bindings: a tmux-style two-step. Pressing the PREFIX (default
//     `ctrl+b`) arms a transient mode; the NEXT key resolves a `prefixed`
//     binding. This is the expanding command tail — low-frequency actions live
//     here without burning a top-level chord.
//
// A chord is a normalized lowercase string: `"<mod>+...+<key>"`, e.g. `"ctrl+t"`,
// `"ctrl+shift+tab"`, `"ctrl+1"`, `"ctrl+="`. Cmd (meta) is folded into `ctrl` so
// one binding string works on both macOS and Windows/Linux (mirrors the old
// `ctrlKey || metaKey` check). See lib/chord.ts for parse/format/normalize.
//
// Persistence mirrors store/settings.ts: a versioned localStorage key with a
// best-effort load that VALIDATES — bindings whose commandId is unknown (e.g. a
// command removed in a later build) are dropped on load so a stale persisted map
// can never resurrect a dead command or shadow a key for nothing.
import { create } from "zustand";
import { COMMAND_IDS, type CommandId } from "../lib/commands";
import { normalizeChord } from "../lib/chord";
import {
  loadPersisted as loadFromStorage,
  savePersisted as saveToStorage,
} from "../lib/persist";

const PERSIST_KEY = "t-hub.keybindings.v1";

/** Default prefix that arms the tmux-style command tail. Free for the app:
 *  tmux's own `C-b` prefix is disabled server-side (tmux.rs). */
export const DEFAULT_PREFIX = "ctrl+b";

/**
 * DIRECT bindings — reproduce today's hardcoded Canvas hotkeys 1:1, PLUS the new
 * fuzzy command palette (`ctrl+k`). NOTE the relocation: Ctrl+B used to toggle
 * focus terminal<->sidebar, but Ctrl+B is now the PREFIX, so focus-toggle moves
 * to a free direct chord (`ctrl+j`). Everything else keeps its old chord, so no
 * other behavior regresses.
 */
// Partial: not every command has a DIRECT chord — some (e.g. the WS-9c
// new-workspace actions) are prefix-only by default. Mirrors DEFAULT_PREFIXED.
const DEFAULT_DIRECT: Partial<Record<CommandId, string>> = {
  spawnTerminal: "ctrl+t",
  closeTerminal: "ctrl+w",
  cycleTileNext: "ctrl+tab",
  cycleTilePrev: "ctrl+shift+tab",
  focusTab1: "ctrl+1",
  focusTab2: "ctrl+2",
  focusTab3: "ctrl+3",
  focusTab4: "ctrl+4",
  focusTab5: "ctrl+5",
  focusTab6: "ctrl+6",
  focusTab7: "ctrl+7",
  focusTab8: "ctrl+8",
  focusTab9: "ctrl+9",
  zoomIn: "ctrl+=",
  zoomOut: "ctrl+-",
  zoomReset: "ctrl+0",
  toggleFocusRegion: "ctrl+j", // RELOCATED from ctrl+b (now the prefix)
  commandPalette: "ctrl+k", // NEW
};

/**
 * PREFIXED bindings — the expanding command tail. After the prefix is pressed,
 * the next BARE key (no modifiers) resolves one of these. Seeded with a useful
 * starter set so the prefix tier is discoverable out of the box; users can add
 * more via the command palette's rebind flow. Keys here are single bare keys.
 */
const DEFAULT_PREFIXED: Partial<Record<CommandId, string>> = {
  // WS-9c: the two "new" actions are the canonical owners of `c`/`w` per the
  // worktree-workflow design (docs/WORKTREE-WORKFLOW.md). `c` previously seeded
  // spawnTerminal — RELOCATED to `t` (mirrors its direct Ctrl+T; spawnTerminal is
  // also the Ctrl+T direct chord, so the prefix tier is just an alias).
  newPlainWorkspace: "c", // new plain tab (workspace)
  newWorktreeWorkspace: "w", // new tab that is a git worktree
  spawnTerminal: "t", // RELOCATED off `c` (now newPlainWorkspace)
  closeTerminal: "x", // tmux: prefix-x = kill pane
  commandPalette: "p",
  toggleFocusRegion: "o", // tmux: prefix-o = cycle panes
  cycleTileNext: "n",
  cycleTilePrev: "p", // (overridden below — kept distinct from palette)
  // WS-9e: list/re-open worktrees. `l` = "list" — free (the seeded set above uses
  // c/w/t/x/p/o/n/b). Prefix-only by default (no direct chord); rebindable.
  openWorktreesList: "l",
};
// Keep the seeded prefixed map free of duplicate keys (the last writer in the
// literal above would otherwise silently win). `p` is the palette; give cycle-
// prev its own key so both are reachable.
DEFAULT_PREFIXED.cycleTilePrev = "b";

/** The full persisted shape. */
export interface KeybindingsState {
  /** The chord that arms the prefix tier (default ctrl+b). Normalized. */
  prefixKey: string;
  /** commandId -> single chord that fires it directly. */
  direct: Record<string, string>;
  /** commandId -> single bare key that fires it after the prefix. */
  prefixed: Record<string, string>;

  /** Rebind (or clear, with an empty/blank chord) a DIRECT binding. */
  setBinding: (commandId: CommandId, chord: string) => void;
  /** Rebind (or clear) a PREFIXED binding (a single bare key). */
  setPrefixedBinding: (commandId: CommandId, key: string) => void;
  /** Change the prefix chord (normalized; blank falls back to the default). */
  setPrefix: (chord: string) => void;
  /** Restore every binding + the prefix to the shipped defaults. */
  resetDefaults: () => void;
}

interface Persisted {
  prefixKey: string;
  direct: Record<string, string>;
  prefixed: Record<string, string>;
}

const KNOWN = new Set<string>(COMMAND_IDS);

/** Coerce a persisted command->chord map: keep only KNOWN command ids whose
 *  value is a non-empty string, normalizing each chord. Unknown ids are dropped
 *  (load-time validation), so a stale persisted map can't resurrect a removed
 *  command or hold a key hostage for nothing. */
function sanitizeMap(
  input: unknown,
  normalize: (s: string) => string,
): Record<string, string> {
  const out: Record<string, string> = {};
  if (!input || typeof input !== "object") return out;
  for (const [id, v] of Object.entries(input as Record<string, unknown>)) {
    if (!KNOWN.has(id)) continue; // drop unknown commandId
    if (typeof v !== "string") continue;
    const c = normalize(v);
    if (c) out[id] = c;
  }
  return out;
}

function defaults(): Persisted {
  return {
    prefixKey: DEFAULT_PREFIX,
    direct: { ...DEFAULT_DIRECT },
    prefixed: { ...(DEFAULT_PREFIXED as Record<string, string>) },
  };
}

/** Validate one persisted blob: a normalized prefix (default on a bad/empty
 *  chord) plus the two sanitized command->key maps. Owns this store's coerce
 *  logic; the SSR guard + corrupt-fallback plumbing lives in lib/persist. */
function coercePersisted(raw: unknown): Persisted {
  const p = (raw ?? {}) as Partial<Persisted>;
  const prefixKey =
    typeof p.prefixKey === "string" && normalizeChord(p.prefixKey)
      ? normalizeChord(p.prefixKey)
      : DEFAULT_PREFIX;
  return {
    prefixKey,
    direct: sanitizeMap(p.direct, normalizeChord),
    // Prefixed values are bare single keys — lowercase + trim, no modifiers.
    prefixed: sanitizeMap(p.prefixed, (s) => s.trim().toLowerCase()),
  };
}

function loadPersisted(): Persisted {
  return loadFromStorage(PERSIST_KEY, defaults(), coercePersisted);
}

function savePersisted(s: Persisted): void {
  saveToStorage(PERSIST_KEY, s);
}

const initial = loadPersisted();

export const useKeybindings = create<KeybindingsState>((set, get) => {
  const persistAll = () => {
    const s = get();
    savePersisted({
      prefixKey: s.prefixKey,
      direct: s.direct,
      prefixed: s.prefixed,
    });
  };

  return {
    prefixKey: initial.prefixKey,
    direct: initial.direct,
    prefixed: initial.prefixed,

    setBinding: (commandId, chord) => {
      const c = normalizeChord(chord);
      set((s) => {
        const direct = { ...s.direct };
        if (c) direct[commandId] = c;
        else delete direct[commandId];
        return { direct };
      });
      persistAll();
    },

    setPrefixedBinding: (commandId, key) => {
      const k = key.trim().toLowerCase();
      set((s) => {
        const prefixed = { ...s.prefixed };
        if (k) prefixed[commandId] = k;
        else delete prefixed[commandId];
        return { prefixed };
      });
      persistAll();
    },

    setPrefix: (chord) => {
      const c = normalizeChord(chord) || DEFAULT_PREFIX;
      set({ prefixKey: c });
      persistAll();
    },

    resetDefaults: () => {
      const d = defaults();
      set({ prefixKey: d.prefixKey, direct: d.direct, prefixed: d.prefixed });
      persistAll();
    },
  };
});

/** Reverse lookup: the commandId a DIRECT chord is bound to, or null. Reads the
 *  live store (no React subscription) — for use inside the Canvas keydown
 *  handler. The chord is normalized before comparison. */
export function directCommandForChord(chord: string): CommandId | null {
  const norm = normalizeChord(chord);
  if (!norm) return null;
  const { direct } = useKeybindings.getState();
  for (const [id, c] of Object.entries(direct)) {
    if (c === norm) return id as CommandId;
  }
  return null;
}

/** Reverse lookup: the commandId a PREFIXED bare key is bound to, or null. */
export function prefixedCommandForKey(key: string): CommandId | null {
  const k = key.trim().toLowerCase();
  if (!k) return null;
  const { prefixed } = useKeybindings.getState();
  for (const [id, bare] of Object.entries(prefixed)) {
    if (bare === k) return id as CommandId;
  }
  return null;
}
