// The canvas is a responsive auto-grid of terminal tiles (PRD §5.3, FR-001/FR-002):
//   - On mount: listTerminals() seeds the store; onState() keeps tile chrome live.
//   - Deterministic near-square grid sized from the tile count, gap-free.
//   - Spawn (+ button, empty-state button, Ctrl/Cmd+T) inserts after the focused
//     tile; Ctrl/Cmd+W detaches the focused tile.
//   - 0.1 is a single canvas with no hidden tabs, so every tile mounts visible.
import { useCallback, useEffect } from "react";
import { useWorkspace } from "../store/workspace";
import {
  spawnTerminal,
  listTerminals,
  closeTerminal,
  onState,
} from "../ipc/client";
import { Tile } from "./Tile";

/**
 * Split `ids` into balanced rows that completely fill the canvas — no empty
 * cells. Columns target a near-square (cols = ceil(sqrt(n))); the tiles are then
 * spread as evenly as possible across the rows, so a short last row's tiles just
 * grow wider instead of leaving a gap.
 */
function splitRows<T>(ids: T[]): T[][] {
  const n = ids.length;
  if (n === 0) return [];
  const cols = Math.ceil(Math.sqrt(n));
  const rows = Math.ceil(n / cols);
  const base = Math.floor(n / rows);
  const extra = n % rows; // the first `extra` rows get one additional tile
  const out: T[][] = [];
  let i = 0;
  for (let r = 0; r < rows; r++) {
    const count = base + (r < extra ? 1 : 0);
    out.push(ids.slice(i, i + count));
    i += count;
  }
  return out;
}

export interface CanvasProps {
  /** Toggle the 0.5 supervision sidebar (Ctrl/Cmd+B). Optional so the 0.1
   *  nucleus canvas still works standalone. */
  onToggleSidebar?: () => void;
}

export function Canvas({ onToggleSidebar }: CanvasProps = {}) {
  const order = useWorkspace((s) => s.order);
  const focusedId = useWorkspace((s) => s.focusedId);
  const setTerminals = useWorkspace((s) => s.setTerminals);
  const addAfterFocused = useWorkspace((s) => s.addAfterFocused);
  const remove = useWorkspace((s) => s.remove);
  const setFocus = useWorkspace((s) => s.setFocus);
  const updateState = useWorkspace((s) => s.updateState);
  const zoomIn = useWorkspace((s) => s.zoomIn);
  const zoomOut = useWorkspace((s) => s.zoomOut);
  const zoomReset = useWorkspace((s) => s.zoomReset);

  // Seed the live terminal set and keep lifecycle state in sync with the backend.
  useEffect(() => {
    let unlisten: (() => void) | undefined;
    let disposed = false;

    void listTerminals()
      .then(setTerminals)
      .catch((err) => console.error("listTerminals failed", err));

    void onState((e) => updateState(e.id, e.state))
      .then((fn) => {
        if (disposed) fn();
        else unlisten = fn;
      })
      .catch((err) => console.error("onState subscribe failed", err));

    return () => {
      disposed = true;
      unlisten?.();
    };
  }, [setTerminals, updateState]);

  const spawn = useCallback(async () => {
    try {
      const info = await spawnTerminal({});
      addAfterFocused(info);
    } catch (err) {
      console.error("spawnTerminal failed", err);
    }
  }, [addAfterFocused]);

  const closeFocused = useCallback(() => {
    const id = useWorkspace.getState().focusedId;
    if (!id) return;
    void closeTerminal(id).catch((err) =>
      console.error("closeTerminal failed", err),
    );
    remove(id);
  }, [remove]);

  const close = useCallback(
    (id: string) => {
      void closeTerminal(id).catch((err) =>
        console.error("closeTerminal failed", err),
      );
      remove(id);
    },
    [remove],
  );

  // Global keybindings: Ctrl/Cmd+T = new terminal, Ctrl/Cmd+W = close focused,
  // Ctrl/Cmd+B = toggle the 0.5 supervision sidebar.
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      const mod = e.ctrlKey || e.metaKey;
      if (!mod || e.altKey) return;
      const key = e.key.toLowerCase();
      if (key === "t") {
        e.preventDefault();
        void spawn();
      } else if (key === "w") {
        e.preventDefault();
        closeFocused();
      } else if (key === "=" || key === "+") {
        e.preventDefault();
        zoomIn();
      } else if (key === "-" || key === "_") {
        e.preventDefault();
        zoomOut();
      } else if (key === "0") {
        e.preventDefault();
        zoomReset();
      } else if (key === "b" && onToggleSidebar) {
        e.preventDefault();
        onToggleSidebar();
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [spawn, closeFocused, zoomIn, zoomOut, zoomReset, onToggleSidebar]);

  // Empty state: a single centered call-to-action.
  if (order.length === 0) {
    return (
      <div className="flex h-full w-full items-center justify-center bg-neutral-950">
        <button
          type="button"
          onClick={() => void spawn()}
          className="rounded-md border border-neutral-700 bg-neutral-900 px-5 py-3 text-base text-neutral-200 hover:border-emerald-600 hover:text-white"
        >
          ＋ New terminal
        </button>
      </div>
    );
  }

  const layout = splitRows(order);

  return (
    <div className="relative h-full w-full bg-neutral-950">
      <div className="flex h-full w-full flex-col gap-1 p-1">
        {layout.map((row, r) => (
          <div key={r} className="flex min-h-0 flex-1 gap-1">
            {row.map((id) => (
              <div key={id} className="min-h-0 min-w-0 flex-1">
                <Tile
                  terminalId={id}
                  focused={id === focusedId}
                  onFocus={() => setFocus(id)}
                  onClose={() => close(id)}
                />
              </div>
            ))}
          </div>
        ))}
      </div>

      {/* Persistent affordance to add more terminals. */}
      <button
        type="button"
        onClick={() => void spawn()}
        title="New terminal (Ctrl/Cmd+T)"
        aria-label="New terminal"
        className="absolute bottom-3 right-3 z-30 flex h-9 w-9 cursor-pointer items-center justify-center rounded-full border border-neutral-700 bg-neutral-900/90 text-lg leading-none text-neutral-200 shadow-lg hover:border-emerald-600 hover:text-white"
      >
        +
      </button>
    </div>
  );
}
