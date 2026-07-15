import { useEffect, useMemo, useState } from "react";
import type { ReactElement } from "react";
import { open as shellOpen } from "@tauri-apps/plugin-shell";
import { ExternalLink, RefreshCw } from "lucide-react";
import {
  projectBoardSnapshot,
  type ProjectBoardSnapshot,
} from "../ipc/projects";
import type { TerminalId } from "../ipc/types";

interface BoardPanelProps {
  terminalId: TerminalId;
  cwd: string;
}

const STATUS_ORDER = [
  "running",
  "awaiting_input",
  "claimed",
  "ready",
  "blocked",
  "backlog",
  "done",
  "shipped",
  "abandoned",
];

export function BoardPanel({ terminalId, cwd }: BoardPanelProps): ReactElement {
  const [snapshot, setSnapshot] = useState<ProjectBoardSnapshot | null>(null);
  const [loading, setLoading] = useState(true);
  const [requestVersion, setRequestVersion] = useState(0);
  const [externalError, setExternalError] = useState<string | null>(null);

  useEffect(() => {
    let current = true;
    setLoading(true);
    setExternalError(null);
    projectBoardSnapshot({ terminalId, cwd, limit: 1000 })
      .then((result) => {
        if (current) setSnapshot(result);
      })
      .catch(() => {
        if (current) {
          setSnapshot({
            schemaVersion: 1,
            status: "error",
            resolution: "none",
            problem: {
              code: "board_request_failed",
              message: "T-Hub could not load the Project board.",
              retryable: true,
            },
          });
        }
      })
      .finally(() => {
        if (current) setLoading(false);
      });
    return () => {
      current = false;
    };
  }, [terminalId, cwd, requestVersion]);

  const groups = useMemo(() => {
    const grouped = new Map<string, NonNullable<ProjectBoardSnapshot["board"]>["cards"]>();
    for (const card of snapshot?.board?.cards ?? []) {
      const cards = grouped.get(card.status) ?? [];
      cards.push(card);
      grouped.set(card.status, cards);
    }
    return [...grouped.entries()].sort(([left], [right]) => {
      const leftRank = STATUS_ORDER.indexOf(left);
      const rightRank = STATUS_ORDER.indexOf(right);
      return (leftRank < 0 ? STATUS_ORDER.length : leftRank) -
        (rightRank < 0 ? STATUS_ORDER.length : rightRank);
    });
  }, [snapshot]);

  const openFullBoard = async () => {
    if (!snapshot?.external?.url) return;
    setExternalError(null);
    try {
      await shellOpen(snapshot.external.url);
    } catch {
      setExternalError("T-Hub could not open the full Powder board in your browser.");
    }
  };

  if (loading) {
    return (
      <BoardShell title="Board">
        <div role="status" className="m-auto text-sm" style={{ color: "var(--th-fg-muted)" }}>
          Loading the Project board...
        </div>
      </BoardShell>
    );
  }

  if (!snapshot || !snapshot.board || !["ready", "degraded"].includes(snapshot.status)) {
    return (
      <BoardShell title={snapshot?.project ? `${snapshot.project.name} Board` : "Board"}>
        <div className="m-auto flex max-w-md flex-col items-center gap-3 px-6 text-center">
          <div className="text-sm font-semibold">{problemTitle(snapshot?.status)}</div>
          <p className="text-xs leading-relaxed" style={{ color: "var(--th-fg-muted)" }}>
            {snapshot?.problem?.message ?? "T-Hub could not load the Project board."}
          </p>
          {snapshot?.binding && (
            <p className="text-[11px]" style={{ color: "var(--th-fg-muted)" }}>
              {snapshot.binding.repository} via {snapshot.binding.connectionProfile}
            </p>
          )}
          <div className="flex flex-wrap justify-center gap-2">
            {snapshot?.problem?.retryable && (
              <ActionButton onClick={() => setRequestVersion((version) => version + 1)}>
                <RefreshCw size={13} /> Retry
              </ActionButton>
            )}
            {snapshot?.external && (
              <ActionButton onClick={openFullBoard}>
                <ExternalLink size={13} /> Open full Powder board
              </ActionButton>
            )}
          </div>
          {externalError && <p role="alert" className="text-xs text-red-400">{externalError}</p>}
        </div>
      </BoardShell>
    );
  }

  const { board, binding } = snapshot;
  return (
    <BoardShell
      title={`${board.repository.name} Board`}
      actions={
        <>
          <span className="hidden text-[11px] sm:inline" style={{ color: "var(--th-fg-muted)" }}>
            {binding?.connectionProfile}
          </span>
          <ActionButton onClick={() => setRequestVersion((version) => version + 1)} title="Refresh board">
            <RefreshCw size={13} />
          </ActionButton>
          {snapshot.external && (
            <ActionButton onClick={openFullBoard} title="Open full Powder board">
              <ExternalLink size={13} />
              <span className="hidden sm:inline">Full board</span>
            </ActionButton>
          )}
        </>
      }
    >
      {snapshot.status === "degraded" && snapshot.problem && (
        <div role="status" className="border-b px-3 py-2 text-xs text-amber-300" style={{ borderColor: "var(--th-border)" }}>
          {snapshot.problem.message}
        </div>
      )}
      {externalError && <div role="alert" className="border-b px-3 py-2 text-xs text-red-400" style={{ borderColor: "var(--th-border)" }}>{externalError}</div>}
      {board.cards.length === 0 ? (
        <div className="m-auto text-center">
          <div className="text-sm font-semibold">No cards on this board</div>
          <div className="mt-1 text-xs" style={{ color: "var(--th-fg-muted)" }}>
            Powder returned an empty {board.repository.name} board.
          </div>
        </div>
      ) : (
        <div className="th-scroll grid min-h-0 flex-1 auto-cols-[minmax(15rem,1fr)] grid-flow-col gap-3 overflow-auto p-3">
          {groups.map(([status, cards]) => (
            <section key={status} aria-label={`${statusLabel(status)} cards`} className="flex min-h-0 flex-col gap-2">
              <div className="flex items-center justify-between text-xs font-semibold uppercase tracking-wide">
                <span>{statusLabel(status)}</span>
                <span style={{ color: "var(--th-fg-muted)" }}>{cards.length}</span>
              </div>
              <div className="flex flex-col gap-2">
                {cards.map((card) => (
                  <article key={card.id} className="rounded border p-3" style={{ borderColor: "var(--th-border)", background: "var(--th-tile-bg)" }}>
                    <div className="text-sm font-medium leading-snug">{card.title}</div>
                    <div className="mt-2 flex flex-wrap items-center gap-1.5 text-[11px]" style={{ color: "var(--th-fg-muted)" }}>
                      <span className="font-mono">{card.id}</span>
                      <span>{card.priority.toUpperCase()}</span>
                      {card.estimate && <span>{card.estimate.toUpperCase()}</span>}
                      {card.claim && <span title={`Claim expires ${new Date(card.claim.expiresAt * 1000).toLocaleString()}`}>Claimed by {card.claim.agent}</span>}
                    </div>
                    {card.labels.length > 0 && (
                      <div className="mt-2 flex flex-wrap gap-1">
                        {card.labels.map((label) => <span key={label} className="rounded border px-1.5 py-0.5 text-[10px]" style={{ borderColor: "var(--th-border)", color: "var(--th-fg-muted)" }}>{label}</span>)}
                      </div>
                    )}
                  </article>
                ))}
              </div>
            </section>
          ))}
        </div>
      )}
    </BoardShell>
  );
}

function BoardShell({ title, actions, children }: { title: string; actions?: ReactElement; children: React.ReactNode }): ReactElement {
  return (
    <section aria-label="Project Board" className="flex h-full min-h-0 flex-col" style={{ background: "var(--th-sidebar-bg)", color: "var(--th-fg)" }}>
      <header className="flex min-h-10 items-center justify-between gap-2 border-b px-3" style={{ borderColor: "var(--th-border)" }}>
        <h2 className="truncate text-sm font-semibold">{title}</h2>
        {actions && <div className="flex items-center gap-1.5">{actions}</div>}
      </header>
      <div className="flex min-h-0 flex-1 flex-col">{children}</div>
    </section>
  );
}

function ActionButton({ children, ...props }: React.ButtonHTMLAttributes<HTMLButtonElement>): ReactElement {
  return <button type="button" className="inline-flex items-center gap-1.5 rounded border px-2 py-1 text-xs hover:bg-neutral-700/30" style={{ borderColor: "var(--th-border)" }} {...props}>{children}</button>;
}

function problemTitle(status?: ProjectBoardSnapshot["status"]): string {
  switch (status) {
    case "noProject": return "No registered Project";
    case "unbound": return "No Powder board is bound";
    case "unauthorized": return "Powder authorization failed";
    case "unreachable": return "Powder is unreachable";
    case "misconfigured": return "Powder profile needs attention";
    case "repositoryMissing": return "Powder board not found";
    default: return "Board unavailable";
  }
}

function statusLabel(status: string): string {
  return status.replaceAll("_", " ");
}
