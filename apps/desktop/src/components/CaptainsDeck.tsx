// The CAPTAINS DECK - the AGENTS surface (deck-as-agents model): a full-screen
// view whose grid renders the orchestrator + captains each as a LIVE TERMINAL
// PANEL, with the identity/status/crew shrunk to a slim header strip per panel.
// The orchestrator sits at the top of the hierarchy (first panel); the focused
// agent (deckFocusId) is spotlighted (enlarged) while every agent stays visible
// and live. A persistent bottom input talks to the designated orchestrator.
//
// RENDERING CONTRACT: this mounts INSIDE TerminalPoolLayer (co-located with the
// captain overlay), so each panel body is a `useTerminalSlot` placeholder the
// pool paints the agent's ONE pooled <TerminalView> into - no second attach.
// While the deck is open the pool shows every agent terminal on every tab and
// lifts each agent's wrapper (z-2) above the panel chrome (z-1); the agent's
// workspace tile yields its placeholder (Canvas slotActive) so exactly one
// placeholder is registered per id. The host renders null while the deck is
// closed (or in a satellite window).
import { useEffect, useMemo, useState } from "react";
import { Mic, Send, X } from "lucide-react";
import { useCaptain, agentOrder } from "../store/captain";
import { useWorkspace, isSatelliteWindow } from "../store/workspace";
import { writeTerminal } from "../ipc/client";
import { readTerminalTailLine } from "../lib/terminalTail";
import { useTerminalSlot } from "./TerminalPool";
import {
  CaptainStatusDot,
  useCaptainDisplayLabel,
  useWorkspaceNameForTerminal,
} from "./CaptainOverlay";
import { useCrewSummary } from "../hooks/useCrewSummary";

/** The deck host. Rendered unconditionally inside the pool layer; paints only
 *  while the deck is open, and never in a satellite window. */
export function CaptainsDeck() {
  const deckOpen = useCaptain((s) => s.deckOpen);
  const setDeckOpen = useCaptain((s) => s.setDeckOpen);

  if (!deckOpen || isSatelliteWindow()) return null;

  return (
    <div
      // pointer-events-auto: the pool layer is click-through, but the deck is a
      // modal surface that must capture clicks (the agent xterm wrappers still
      // sit above it and stay interactive).
      className="pointer-events-auto absolute inset-0 flex flex-col"
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
          Agents
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

      {/* The agent panels (live terminals). */}
      <DeckPanels />

      {/* The orchestrator output strip (latest line of its terminal). */}
      <OrchestratorStrip />

      {/* The persistent bottom input, targeting the orchestrator. */}
      <OrchestratorInput />
    </div>
  );
}

/** The panel grid: one live terminal panel per agent (orchestrator first, then
 *  captains), the focused one spotlighted (full-width, taller). */
function DeckPanels() {
  const orchestratorId = useCaptain((s) => s.orchestratorId);
  const captainIds = useCaptain((s) => s.captainIds);
  const deckFocusId = useCaptain((s) => s.deckFocusId);
  const agents = useMemo(
    () => agentOrder({ orchestratorId, captainIds }),
    [orchestratorId, captainIds],
  );

  if (agents.length === 0) {
    return (
      <div
        className="flex flex-1 items-center justify-center px-6 text-center text-sm"
        style={{ color: "var(--th-fg-muted)" }}
      >
        No agents yet. The orchestrator is adopted at launch; pin a session as a
        captain (right-click a tile) to add it to the deck.
      </div>
    );
  }

  return (
    <div className="th-scroll min-h-0 flex-1 overflow-y-auto p-3">
      <div
        className="grid gap-3"
        style={{ gridTemplateColumns: "repeat(auto-fill, minmax(280px, 1fr))" }}
      >
        {agents.map((id) => (
          <DeckPanel key={id} terminalId={id} focused={id === deckFocusId} />
        ))}
      </div>
    </div>
  );
}

/** One agent panel: a slim header strip (status dot + stable identity +
 *  orchestrator badge + crew summary) above the LIVE terminal body (the pool's
 *  placeholder). Clicking focuses this panel in the deck (spotlight). */
function DeckPanel({
  terminalId,
  focused,
}: {
  terminalId: string;
  focused: boolean;
}) {
  const slotRef = useTerminalSlot(terminalId);
  const identity = useCaptainDisplayLabel(terminalId);
  // undefined workspace name = no live tile (popped out / gone) => no pooled
  // terminal to show; render the unavailable affordance instead of a slot.
  const workspaceName = useWorkspaceNameForTerminal(terminalId);
  const hasTile = workspaceName != null;
  const crew = useCrewSummary(terminalId);
  const isOrchestrator = useCaptain((s) => s.orchestratorId === terminalId);
  const setDeckFocus = useCaptain((s) => s.setDeckFocus);

  return (
    <div
      onClick={() => setDeckFocus(terminalId)}
      className="flex flex-col overflow-hidden rounded-lg border"
      style={{
        backgroundColor: "var(--th-tile-bg)",
        borderColor: focused ? "var(--th-accent)" : "var(--th-border)",
        boxShadow: focused ? "0 0 0 1px var(--th-accent)" : undefined,
        // The focused agent spotlights: it spans the full grid width and is
        // taller. CSS-only (grid-column / min-height) so the pooled wrapper just
        // re-syncs to the new rect - no DOM reorder (the mutedbug constraint).
        gridColumn: focused ? "1 / -1" : undefined,
        minHeight: focused ? 380 : 220,
      }}
      data-deck-panel={terminalId}
      data-focused={focused || undefined}
      data-orchestrator={isOrchestrator || undefined}
      data-tile-available={hasTile || undefined}
    >
      {/* Slim header strip. */}
      <div
        className="flex shrink-0 items-center gap-2 border-b px-2 py-1"
        style={{ borderColor: "var(--th-border)" }}
      >
        <CaptainStatusDot terminalId={terminalId} size={9} />
        <span
          className="min-w-0 truncate text-xs font-semibold"
          style={{ color: "var(--th-fg)" }}
        >
          {identity}
        </span>
        {isOrchestrator && (
          <span
            className="shrink-0 rounded px-1 text-[8px] font-semibold uppercase tracking-wide"
            style={{
              color: "var(--th-accent)",
              backgroundColor:
                "color-mix(in srgb, var(--th-accent) 15%, transparent)",
            }}
          >
            orchestrator
          </span>
        )}
        {crew.members.length > 0 && (
          <span
            className="ml-auto shrink-0 text-[10px]"
            style={{ color: "var(--th-fg-muted)" }}
            title="Crew: the sessions this agent spawned (registry spawnedBy links)."
          >
            crew: {crew.running} running · {crew.done} done
          </span>
        )}
      </div>

      {/* Live terminal body (the pool paints the agent's xterm over this
          placeholder) or the unavailable affordance. */}
      {hasTile ? (
        <div
          ref={slotRef}
          className="min-h-0 flex-1 overflow-hidden"
          data-deck-terminal={terminalId}
        />
      ) : (
        <div
          className="flex flex-1 items-center justify-center px-3 text-center text-[11px]"
          style={{ color: "var(--th-fg-muted)" }}
        >
          Terminal not available (tab popped out?)
        </div>
      )}
    </div>
  );
}

/** A thin strip above the input showing the latest visible line of the
 *  designated orchestrator's terminal. Polls the xterm buffer tail (~600ms) so
 *  there is no per-output-chunk cost; renders nothing until an orchestrator is
 *  designated. */
function OrchestratorStrip() {
  const orchestratorId = useCaptain((s) => s.orchestratorId);
  const label = useCaptainDisplayLabel(orchestratorId ?? "");
  const [line, setLine] = useState("");

  useEffect(() => {
    if (!orchestratorId) {
      setLine("");
      return;
    }
    const tick = () => setLine(readTerminalTailLine(orchestratorId));
    tick();
    const timer = setInterval(tick, 600);
    return () => clearInterval(timer);
  }, [orchestratorId]);

  if (!orchestratorId) return null;

  return (
    <div
      className="flex shrink-0 items-center gap-2 border-t px-3 py-1.5"
      style={{ borderColor: "var(--th-border)" }}
      data-orchestrator-strip
    >
      <span
        className="shrink-0 text-[10px] font-semibold uppercase tracking-wide"
        style={{ color: "var(--th-accent)" }}
      >
        {label}
      </span>
      <span
        className="min-w-0 flex-1 truncate font-mono text-[11px]"
        style={{ color: "var(--th-fg-muted)" }}
        title={line}
      >
        {line || "(no output yet)"}
      </span>
    </div>
  );
}

/** The persistent bottom input: on Enter (or Send) it writes the typed line +
 *  carriage return to the DESIGNATED orchestrator terminal via the same
 *  writeTerminal IPC xterm uses. A disabled mic placeholder (voice input is
 *  coming via Scribe) sits to its right, then Send. Disabled when no orchestrator
 *  is designated or its terminal is gone. */
function OrchestratorInput() {
  const orchestratorId = useCaptain((s) => s.orchestratorId);
  const label = useCaptainDisplayLabel(orchestratorId ?? "");
  const state = useWorkspace((s) =>
    orchestratorId ? s.terminals[orchestratorId]?.state : undefined,
  );
  // A live (or detached-but-alive) tile is writable - the pool keeps every
  // session attached, so an orchestrator in any tab receives input. A starting /
  // exited / errored tile is not yet (or no longer) ready.
  const writable =
    orchestratorId != null && (state === "live" || state === "detached");

  const [draft, setDraft] = useState("");
  const canSend = writable && draft.trim().length > 0;

  const send = () => {
    if (!canSend || orchestratorId == null) return;
    // Send the TRIMMED text (we gate on draft.trim(), so trailing/leading
    // whitespace the user didn't mean shouldn't reach the orchestrator).
    const line = draft.trim();
    setDraft("");
    // Append a carriage return - the byte xterm sends for Enter, which the PTY's
    // line discipline turns into a submit (a TUI like Claude reads \r as Enter).
    void writeTerminal(orchestratorId, `${line}\r`).catch((e) => {
      console.error("orchestrator input: writeTerminal failed", e);
    });
  };

  return (
    <div
      className="shrink-0 border-t p-2"
      style={{ borderColor: "var(--th-border)" }}
      data-orchestrator-input
    >
      <form
        className="flex items-end gap-2"
        onSubmit={(e) => {
          e.preventDefault();
          send();
        }}
      >
        <input
          type="text"
          value={draft}
          onChange={(e) => setDraft(e.target.value)}
          disabled={!writable}
          aria-label="Message the orchestrator"
          placeholder={
            orchestratorId == null
              ? "No orchestrator set - right-click a tile and Mark as orchestrator"
              : writable
                ? `Message ${label}...`
                : `${label} - terminal not available`
          }
          className="min-w-0 flex-1 rounded-md border bg-transparent px-3 py-2 text-sm outline-none disabled:opacity-50"
          style={{ color: "var(--th-fg)", borderColor: "var(--th-border)" }}
          data-orchestrator-field
        />

        {/* Voice input placeholder - Scribe backend is a sibling crew's track. */}
        <button
          type="button"
          disabled
          aria-label="Voice input coming via Scribe"
          title="Voice input coming via Scribe"
          className="flex h-9 w-9 shrink-0 items-center justify-center rounded-md border opacity-40"
          style={{ borderColor: "var(--th-border)", color: "var(--th-fg-muted)" }}
        >
          <Mic size={16} aria-hidden />
        </button>

        <button
          type="submit"
          disabled={!canSend}
          aria-label="Send to orchestrator"
          title="Send (Enter)"
          className="flex h-9 w-9 shrink-0 items-center justify-center rounded-md text-white transition-opacity disabled:opacity-40"
          style={{ backgroundColor: "var(--th-accent)" }}
          data-orchestrator-send
        >
          <Send size={16} aria-hidden />
        </button>
      </form>
    </div>
  );
}
