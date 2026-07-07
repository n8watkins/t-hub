// The CAPTAINS DECK (orchestrator UI, deck-primary layout): a full-screen view,
// top to bottom -
//   (1) the DECK: every PINNED captain tiled as a large card (stable identity,
//       status dot, real crew summary from the phase 2 registry);
//   (2) the ORCHESTRATOR OUTPUT STRIP (stage 3): the latest line of the
//       designated orchestrator's terminal;
//   (3) the persistent BOTTOM INPUT (stage 2): types a line into the designated
//       orchestrator terminal (writeTerminal), with a disabled Scribe mic
//       placeholder + a send button.
//
// It renders as an OPAQUE overlay covering the workspace canvas (a sibling of
// Canvas, higher z-index) so the terminal pool stays mounted underneath and the
// orchestrator terminal remains attached + writable. Esc or the close button
// dismisses it.
import { useEffect } from "react";
import { X } from "lucide-react";
import { useCaptain } from "../store/captain";
import {
  CaptainStatusDot,
  useCaptainDisplayLabel,
  useWorkspaceNameForTerminal,
} from "./CaptainOverlay";
import { useCrewSummary } from "../hooks/useCrewSummary";

/** The deck host: mounted by App only while `deckOpen`. Full-screen opaque. */
export function CaptainsDeck() {
  const setDeckOpen = useCaptain((s) => s.setDeckOpen);

  // Esc closes the deck (a single window-level listener; the input's own Esc
  // handler stops propagation only when it has text to clear - see stage 2).
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") setDeckOpen(false);
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [setDeckOpen]);

  return (
    <div
      className="absolute inset-0 z-40 flex flex-col"
      style={{ backgroundColor: "var(--th-app-bg)" }}
      data-captains-deck
    >
      {/* Header: title + close. */}
      <div
        className="flex shrink-0 items-center justify-between border-b px-4 py-2"
        style={{ borderColor: "var(--th-border)" }}
      >
        <span
          className="text-sm font-semibold uppercase tracking-wide"
          style={{ color: "var(--th-fg)" }}
        >
          Captains Deck
        </span>
        <button
          type="button"
          onClick={() => setDeckOpen(false)}
          aria-label="Close captains deck"
          title="Close (Esc)"
          className="flex h-7 w-7 items-center justify-center rounded opacity-70 hover:opacity-100"
          style={{ color: "var(--th-fg-muted)" }}
        >
          <X size={16} aria-hidden />
        </button>
      </div>

      {/* (1) The DECK: all pinned captains as large tiles. */}
      <DeckTiles />

      {/* (2)+(3) output strip + bottom input arrive in stages 3 and 2. */}
    </div>
  );
}

/** The tile grid: one large card per PINNED captain, in the store's MRU order. */
function DeckTiles() {
  const captainIds = useCaptain((s) => s.captainIds);

  if (captainIds.length === 0) {
    return (
      <div
        className="flex flex-1 items-center justify-center px-6 text-center text-sm"
        style={{ color: "var(--th-fg-muted)" }}
      >
        No captains pinned yet. Pin a session as a captain (right-click a tile){" "}
        to see it on the deck.
      </div>
    );
  }

  return (
    <div className="th-scroll min-h-0 flex-1 overflow-y-auto p-3">
      <div
        className="grid gap-3"
        style={{ gridTemplateColumns: "repeat(auto-fill, minmax(240px, 1fr))" }}
      >
        {captainIds.map((id) => (
          <DeckTile key={id} terminalId={id} />
        ))}
      </div>
    </div>
  );
}

/** One captain card: stable identity, status dot, workspace, crew summary, and
 *  an orchestrator badge when designated. Clicking summons that captain (and
 *  closes the deck to land on it). */
function DeckTile({ terminalId }: { terminalId: string }) {
  const identity = useCaptainDisplayLabel(terminalId);
  const workspaceName = useWorkspaceNameForTerminal(terminalId);
  const crew = useCrewSummary(terminalId);
  const isOrchestrator = useCaptain((s) => s.orchestratorId === terminalId);
  const summonCaptain = useCaptain((s) => s.summonCaptain);
  const setDeckOpen = useCaptain((s) => s.setDeckOpen);

  return (
    <button
      type="button"
      onClick={() => {
        setDeckOpen(false);
        summonCaptain(terminalId);
      }}
      title={`Go to captain - ${identity}`}
      className="group flex min-h-[120px] flex-col gap-2 rounded-lg border p-3 text-left transition-colors hover:bg-neutral-800/30"
      style={{
        backgroundColor: "var(--th-tile-bg)",
        borderColor: isOrchestrator ? "var(--th-accent)" : "var(--th-border)",
      }}
      data-deck-tile={terminalId}
      data-orchestrator={isOrchestrator || undefined}
    >
      <div className="flex items-center gap-2">
        <CaptainStatusDot terminalId={terminalId} size={12} />
        <span
          className="min-w-0 flex-1 truncate text-sm font-semibold"
          style={{ color: "var(--th-fg)" }}
        >
          {identity}
        </span>
        {isOrchestrator && (
          <span
            className="shrink-0 rounded px-1.5 py-0.5 text-[9px] font-semibold uppercase tracking-wide"
            style={{
              color: "var(--th-accent)",
              backgroundColor:
                "color-mix(in srgb, var(--th-accent) 15%, transparent)",
            }}
          >
            orchestrator
          </span>
        )}
      </div>

      <span
        className="min-w-0 truncate text-[11px]"
        style={{ color: "var(--th-fg-muted)" }}
      >
        {workspaceName ?? "tile not available"}
      </span>

      {crew.members.length > 0 && (
        <span
          className="mt-auto text-[11px]"
          style={{ color: "var(--th-fg-muted)" }}
          title="Crew: the sessions this captain spawned (registry spawnedBy links)."
        >
          crew: {crew.running} running · {crew.done} done
        </span>
      )}
    </button>
  );
}
