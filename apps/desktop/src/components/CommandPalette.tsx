// The fuzzy command palette + prefix HUD (WS-3). Mounted once at the app root.
//
// PALETTE (opened by the `commandPalette` command, default Ctrl+K):
//   - A centered modal listing every command with its current DIRECT binding.
//   - A fuzzy search box filters by label/description/category (subsequence
//     match, ranked).
//   - ↑/↓ move the selection; Enter EXECUTES the highlighted command (and closes).
//   - A "rebind" affordance on each row starts an interactive "press new key"
//     capture that writes the next chord to the keybindings store.
//
// PREFIX HUD: a small bottom hint that appears while the tmux-style prefix is
// armed (driven by usePrefixHud), so the user sees "Ctrl/Cmd+B …" and knows the
// next key is being captured.
import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { create } from "zustand";
import { COMMANDS, type CommandId } from "../lib/commands";
import { useKeybindings } from "../store/keybindings";
import { useCaptain } from "../store/captain";
import { useWorkspace, deriveLabel } from "../store/workspace";
import { runCommand, registerPaletteOpener } from "../lib/keymapExecutor";
import { chordFromEvent, formatChord } from "../lib/chord";
import { usePrefixHud } from "../lib/prefixKeyHandler";

// --- Open-state store -------------------------------------------------------
// A tiny store so the executor (registerPaletteOpener) and any UI can open the
// palette without threading props through the tree.
interface PaletteState {
  open: boolean;
  setOpen: (v: boolean) => void;
}
const usePalette = create<PaletteState>((set) => ({
  open: false,
  setOpen: (v) => set({ open: v }),
}));

/** Open the palette imperatively. Used by the Settings "Keyboard" section's
 *  rebind affordance (the palette is where rebinding lives). */
export function openKeyboardPalette(): void {
  usePalette.getState().setOpen(true);
}

// --- Fuzzy matching ---------------------------------------------------------
/** Case-insensitive subsequence test with a light score: lower is better
 *  (tighter match / earlier start). Returns null when `q` isn't a subsequence of
 *  `text`. An empty query matches everything at score 0. */
function fuzzyScore(text: string, q: string): number | null {
  if (!q) return 0;
  const t = text.toLowerCase();
  const query = q.toLowerCase();
  let ti = 0;
  let score = 0;
  let firstIdx = -1;
  let prevIdx = -1;
  for (let qi = 0; qi < query.length; qi++) {
    const ch = query[qi];
    const found = t.indexOf(ch, ti);
    if (found === -1) return null;
    if (firstIdx === -1) firstIdx = found;
    if (prevIdx !== -1) score += found - prevIdx - 1; // gaps cost
    prevIdx = found;
    ti = found + 1;
  }
  return score + firstIdx; // prefer earlier first-hit + tighter runs
}

/**
 * One palette row. Static commands (from lib/commands) carry a `commandId` and
 * are rebindable; DYNAMIC rows (captain-list: one "Summon captain: <name>" per
 * pinned captain) carry a `captainId` and execute a store action directly -
 * they have no CommandId, no chord, and no rebind affordance.
 */
interface PaletteEntry {
  key: string;
  label: string;
  description: string;
  category: string;
  commandId?: CommandId;
  captainId?: string;
}

interface Scored {
  entry: PaletteEntry;
  score: number;
}

const STATIC_ENTRIES: PaletteEntry[] = COMMANDS.map((c) => ({
  key: c.id,
  label: c.label,
  description: c.description,
  category: c.category,
  commandId: c.id,
}));

export function CommandPalette() {
  const open = usePalette((s) => s.open);
  const setOpen = usePalette((s) => s.setOpen);
  const direct = useKeybindings((s) => s.direct);

  const [query, setQuery] = useState("");
  const [selected, setSelected] = useState(0);
  // When set, we're capturing a new chord for this command (the rebind flow).
  const [rebindFor, setRebindFor] = useState<CommandId | null>(null);
  const inputRef = useRef<HTMLInputElement | null>(null);
  const listRef = useRef<HTMLDivElement | null>(null);

  // The executor opens the palette through this opener; register on mount.
  useEffect(() => {
    registerPaletteOpener(() => setOpen(true));
    return () => registerPaletteOpener(null);
  }, [setOpen]);

  // Reset transient UI each time the palette opens, and focus the search box.
  useEffect(() => {
    if (!open) return;
    setQuery("");
    setSelected(0);
    setRebindFor(null);
    // Defer a frame so the input exists + isn't fighting the keydown that opened us.
    const id = requestAnimationFrame(() => inputRef.current?.focus());
    return () => cancelAnimationFrame(id);
  }, [open]);

  // Dynamic per-captain entries (captain-list): "Summon captain: <name>" for
  // every pinned captain, MRU order, keyboard parity with the titlebar
  // dropdown / overlay switcher. Labels derive exactly like the tiles/sidebar.
  const captainIds = useCaptain((s) => s.captainIds);
  const terminals = useWorkspace((s) => s.terminals);
  const labels = useWorkspace((s) => s.labels);
  const captainEntries = useMemo<PaletteEntry[]>(
    () =>
      captainIds.map((id) => ({
        key: `summonCaptain:${id}`,
        label: `Summon captain: ${deriveLabel({
          id,
          label: labels[id],
          title: terminals[id]?.title,
          cwd: terminals[id]?.cwd,
        })}`,
        description: "Summon this pinned captain in the overlay",
        category: "App",
        captainId: id,
      })),
    [captainIds, terminals, labels],
  );

  // Ranked, filtered command list.
  const results = useMemo<Scored[]>(() => {
    const entries = [...STATIC_ENTRIES, ...captainEntries];
    const scored: Scored[] = [];
    for (const entry of entries) {
      const hay = `${entry.label} ${entry.description} ${entry.category}`;
      const s = fuzzyScore(hay, query.trim());
      if (s !== null) scored.push({ entry, score: s });
    }
    // Stable-ish: by score, then original order (entry index).
    scored.sort(
      (a, b) =>
        a.score - b.score ||
        entries.indexOf(a.entry) - entries.indexOf(b.entry),
    );
    return scored;
  }, [query, captainEntries]);

  // Keep the selection in range as the result set shrinks/grows.
  useEffect(() => {
    setSelected((s) => (results.length === 0 ? 0 : Math.min(s, results.length - 1)));
  }, [results.length]);

  const close = useCallback(() => {
    setOpen(false);
    setRebindFor(null);
  }, [setOpen]);

  const execute = useCallback(
    (entry: PaletteEntry) => {
      close();
      // Run AFTER the palette tears down so a command that itself touches focus
      // (e.g. toggleFocusRegion, summonCaptain) doesn't fight the closing modal.
      requestAnimationFrame(() => {
        if (entry.commandId) runCommand(entry.commandId);
        else if (entry.captainId)
          useCaptain.getState().summonCaptain(entry.captainId);
      });
    },
    [close],
  );

  // While capturing a rebind, the NEXT keydown becomes the new chord. Registered
  // at the window in capture so it wins over the palette's own list navigation.
  const setBinding = useKeybindings((s) => s.setBinding);
  useEffect(() => {
    if (!open || rebindFor == null) return;
    const onCapture = (e: KeyboardEvent) => {
      // Escape cancels the capture without binding.
      if (e.key === "Escape") {
        e.preventDefault();
        e.stopPropagation();
        setRebindFor(null);
        return;
      }
      // Ignore lone modifier presses — wait for the real key.
      if (
        e.key === "Control" ||
        e.key === "Shift" ||
        e.key === "Alt" ||
        e.key === "Meta"
      ) {
        return;
      }
      const chord = chordFromEvent(e);
      if (!chord) return;
      e.preventDefault();
      e.stopPropagation();
      setBinding(rebindFor, chord);
      setRebindFor(null);
    };
    window.addEventListener("keydown", onCapture, true);
    return () => window.removeEventListener("keydown", onCapture, true);
  }, [open, rebindFor, setBinding]);

  // Palette-level key handling (navigation + execute + close). Disabled while a
  // rebind capture is active (the capture handler above owns the keyboard then).
  const onKeyDown = useCallback(
    (e: React.KeyboardEvent) => {
      if (rebindFor != null) return;
      if (e.key === "Escape") {
        e.preventDefault();
        close();
      } else if (e.key === "ArrowDown") {
        e.preventDefault();
        setSelected((s) => (results.length ? (s + 1) % results.length : 0));
      } else if (e.key === "ArrowUp") {
        e.preventDefault();
        setSelected((s) =>
          results.length ? (s - 1 + results.length) % results.length : 0,
        );
      } else if (e.key === "Enter") {
        e.preventDefault();
        const hit = results[selected];
        if (hit) execute(hit.entry);
      }
    },
    [rebindFor, close, results, selected, execute],
  );

  // Keep the highlighted row in view as the selection moves.
  useEffect(() => {
    const el = listRef.current?.querySelector<HTMLElement>(
      `[data-idx="${selected}"]`,
    );
    el?.scrollIntoView({ block: "nearest" });
  }, [selected]);

  if (!open) return null;

  return (
    <div
      className="fixed inset-0 z-[55] flex items-start justify-center p-6 pt-[12vh]"
      onMouseDown={close}
      style={{ backgroundColor: "rgba(0,0,0,0.5)", pointerEvents: "auto" }}
    >
      <div
        className="flex max-h-[64vh] w-[560px] max-w-[92vw] flex-col overflow-hidden rounded-lg border shadow-2xl"
        onMouseDown={(e) => e.stopPropagation()}
        onKeyDown={onKeyDown}
        style={{
          backgroundColor: "var(--th-sidebar-bg)",
          borderColor: "var(--th-border)",
          color: "var(--th-fg)",
          fontFamily: "var(--th-font)",
        }}
      >
        {/* Search box */}
        <div
          className="shrink-0 border-b px-3 py-2.5"
          style={{ borderColor: "var(--th-border)" }}
        >
          <input
            ref={inputRef}
            value={query}
            onChange={(e) => {
              setQuery(e.target.value);
              setSelected(0);
            }}
            placeholder={
              rebindFor
                ? "Press a key combination… (Esc to cancel)"
                : "Search commands…"
            }
            disabled={rebindFor != null}
            className="w-full bg-transparent text-sm outline-none placeholder:opacity-60"
            style={{ color: "var(--th-fg)" }}
            spellCheck={false}
            autoComplete="off"
          />
        </div>

        {/* Result list */}
        <div ref={listRef} className="th-scroll min-h-0 flex-1 overflow-y-auto py-1">
          {results.length === 0 ? (
            <div
              className="px-3 py-6 text-center text-sm"
              style={{ color: "var(--th-fg-muted)" }}
            >
              No matching commands
            </div>
          ) : (
            results.map(({ entry }, i) => {
              const isSel = i === selected;
              const isRebinding =
                entry.commandId != null && rebindFor === entry.commandId;
              const chord = entry.commandId ? direct[entry.commandId] : undefined;
              return (
                <div
                  key={entry.key}
                  data-idx={i}
                  onMouseMove={() => setSelected(i)}
                  onMouseDown={(e) => {
                    e.preventDefault();
                    if (!isRebinding) execute(entry);
                  }}
                  className="mx-1 flex cursor-pointer items-center justify-between gap-3 rounded px-2.5 py-2"
                  style={{
                    backgroundColor: isSel ? "var(--th-tile-bg)" : "transparent",
                  }}
                >
                  <div className="min-w-0">
                    <div className="truncate text-sm" style={{ color: "var(--th-fg)" }}>
                      {entry.label}
                    </div>
                    <div
                      className="truncate text-xs"
                      style={{ color: "var(--th-fg-muted)" }}
                    >
                      {entry.description}
                    </div>
                  </div>
                  {/* Dynamic captain rows have no CommandId: no chord to show,
                      nothing to rebind (Ctrl+B C cycles; the entry is mouse/
                      Enter parity for a SPECIFIC captain). */}
                  {entry.commandId != null && (
                    <div className="flex shrink-0 items-center gap-2">
                      {isRebinding ? (
                        <span
                          className="rounded border px-1.5 py-0.5 text-xs"
                          style={{
                            borderColor: "var(--th-accent)",
                            color: "var(--th-accent)",
                          }}
                        >
                          press a key…
                        </span>
                      ) : (
                        <kbd
                          className="rounded border px-1.5 py-0.5 font-mono text-xs"
                          style={{
                            borderColor: "var(--th-border)",
                            color: chord ? "var(--th-fg)" : "var(--th-fg-muted)",
                            backgroundColor: "var(--th-tile-bg)",
                          }}
                        >
                          {chord ? formatChord(chord) : "unbound"}
                        </kbd>
                      )}
                      <button
                        type="button"
                        onMouseDown={(e) => {
                          e.preventDefault();
                          e.stopPropagation();
                          setRebindFor(isRebinding ? null : entry.commandId!);
                        }}
                        className="rounded px-1.5 py-0.5 text-xs transition-colors hover:bg-neutral-700/40"
                        style={{ color: "var(--th-fg-muted)" }}
                        title="Rebind this command's direct shortcut"
                      >
                        {isRebinding ? "cancel" : "rebind"}
                      </button>
                    </div>
                  )}
                </div>
              );
            })
          )}
        </div>

        {/* Footer hint */}
        <div
          className="flex shrink-0 items-center justify-between border-t px-3 py-1.5 text-xs"
          style={{ borderColor: "var(--th-border)", color: "var(--th-fg-muted)" }}
        >
          <span>↑↓ navigate · Enter run · Esc close</span>
          <span>rebind sets the direct shortcut</span>
        </div>
      </div>
    </div>
  );
}

/**
 * The prefix HUD — a small bottom-center pill shown while the tmux-style prefix
 * is armed, so the user sees the next key is being captured. Mounted alongside
 * the palette at the app root.
 */
export function PrefixHint() {
  const armed = usePrefixHud((s) => s.armed);
  const label = usePrefixHud((s) => s.prefixLabel);
  if (!armed) return null;
  return (
    <div
      className="pointer-events-none fixed inset-x-0 bottom-6 z-[55] flex justify-center"
      aria-live="polite"
    >
      <div
        className="rounded-md border px-3 py-1.5 text-sm shadow-lg"
        style={{
          backgroundColor: "var(--th-sidebar-bg)",
          borderColor: "var(--th-accent)",
          color: "var(--th-fg)",
          fontFamily: "var(--th-font)",
        }}
      >
        <span style={{ color: "var(--th-accent)" }}>
          {label ? formatChord(label) : "Prefix"}
        </span>{" "}
        <span style={{ color: "var(--th-fg-muted)" }}>— waiting for key…</span>
      </div>
    </div>
  );
}
