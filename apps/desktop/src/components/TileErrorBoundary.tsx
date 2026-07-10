// Per-tile render fault isolation (adopt-harden).
//
// The terminal pool renders EVERY placed tile at once, each as a <TerminalView>.
// React has no built-in isolation between siblings: an uncaught throw during one
// tile's render (or one of its own layout/effect commits) unwinds the whole pool
// subtree, so a SINGLE bad/dead/weird session blanks the ENTIRE grid and no tile
// attaches its PTY. That is exactly the incident shape - 13 leaked ghost sessions
// were adopted onto the active tab, and with no per-tile boundary the crowded /
// malformed render tore down the pool, leaving the app blank with zero attaches.
//
// This boundary wraps each tile so a throw is contained: the failing tile shows a
// small inline error (with a retry), and every OTHER tile renders and attaches
// normally. Error boundaries only catch RENDER/commit-phase throws - the attach
// path itself already isolates per tile via try/catch + a reattach loop in
// Terminal.tsx - so together adoption and attach are both per-tile isolated.
import { Component, type ErrorInfo, type ReactNode } from "react";
import { tlog } from "../lib/diag";

interface TileErrorBoundaryProps {
  /** The terminal id this boundary guards - for the error label + diagnostics. */
  terminalId: string;
  children: ReactNode;
}

interface TileErrorBoundaryState {
  error: Error | null;
}

export class TileErrorBoundary extends Component<
  TileErrorBoundaryProps,
  TileErrorBoundaryState
> {
  constructor(props: TileErrorBoundaryProps) {
    super(props);
    this.state = { error: null };
  }

  static getDerivedStateFromError(error: Error): TileErrorBoundaryState {
    return { error };
  }

  componentDidCatch(error: Error, info: ErrorInfo): void {
    // Mirror the failure to the diag file so a RELEASE build (no devtools) still
    // records WHICH tile broke and why - the rest of the grid stays alive.
    tlog(
      "pool",
      `tile ${this.props.terminalId} render crashed (isolated, siblings unaffected): ${String(
        error,
      )}${info.componentStack ? `\n${info.componentStack}` : ""}`,
    );
  }

  private readonly retry = (): void => {
    this.setState({ error: null });
  };

  render(): ReactNode {
    const { error } = this.state;
    if (error) {
      return (
        <div
          role="alert"
          data-th-tile-error={this.props.terminalId}
          className="flex h-full w-full flex-col items-center justify-center gap-2 bg-neutral-900/80 p-4 text-center text-xs text-neutral-300"
        >
          <div className="font-medium text-red-400">This tile failed to render</div>
          <div className="max-w-full break-words text-neutral-400">
            {error.message || String(error)}
          </div>
          <button
            type="button"
            onClick={this.retry}
            className="mt-1 rounded border border-neutral-600 px-2 py-1 text-neutral-200 hover:bg-neutral-700"
          >
            Retry
          </button>
        </div>
      );
    }
    return this.props.children;
  }
}
