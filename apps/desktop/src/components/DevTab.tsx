// DevTab — the per-project "Dev" view: a managed `npm run dev` runner.
//
// SCAFFOLD / CONTRACT. This stub exists so the per-tile panel can import and
// mount <DevTab terminalId cwd/> and the app compiles before the runner lands.
// It is OWNED by the Dev-runner agent — the panel agent only mounts it and must
// not edit this file (keeps the parallel branches conflict-free).
//
// The real implementation should:
//   - run the project's dev server (auto-detect pnpm/npm; command editable),
//     scoped to `cwd`, in its own pane/child process;
//   - stream its output here with run/stop controls + a status indicator;
//   - detect the localhost URL it prints and publish it via
//       usePanels.getState().setDevUrl(terminalId, url)
//     so the Preview tab loads it automatically.
// Keep these props stable.
import type { TerminalId } from "../ipc/types";

export interface DevTabProps {
  /** The project/terminal this dev runner belongs to. */
  terminalId: TerminalId;
  /** The project's working directory (where the dev server runs). */
  cwd: string;
}

/** Placeholder until the Dev-runner agent implements it (see file header). */
export function DevTab({ cwd }: DevTabProps) {
  return (
    <div
      className="flex h-full flex-col items-center justify-center gap-1 px-6 text-center text-sm"
      style={{ color: "var(--th-fg-muted)" }}
    >
      <div>Dev runner coming soon.</div>
      <div className="text-[11px]">
        Will run the dev server in {cwd || "this project"} and feed its URL to
        Preview.
      </div>
    </div>
  );
}
