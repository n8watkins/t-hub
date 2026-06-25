// Cross-component "open this file in the reader" bus (WS-1, open-file-on-click).
//
// Ctrl/Cmd-clicking a file path in a terminal tile (Terminal.tsx) should surface
// that file in the SAME tile's Files reader. The terminal and the FilePanel are
// separate component subtrees (the xterm lives in the persistent pool; the Files
// panel mounts inside the tile body only when its Files tab is active), so they
// can't call each other directly. This tiny store is the hand-off: the terminal
// publishes a pending { terminalId, path } open-request here (and switches the
// tile to its Files tab via usePanels), and the matching FilePanel — once it
// mounts — consumes the request and clears it.
//
// Only ONE pending request is held at a time: a fresh Ctrl+click supersedes an
// unconsumed one (the user clicked something newer). FilePanel clears it the
// instant it opens the file, so a request is consumed exactly once.
import { create } from "zustand";
import type { TerminalId } from "../ipc/types";

/** A pending request to open a file in a tile's Files reader. */
interface OpenRequest {
  /** Which tile's Files reader should open the file. */
  terminalId: TerminalId;
  /** Absolute path to open (already validated by the requester). */
  path: string;
}

interface FileOpenState {
  /** The single outstanding open-request, or null when there's nothing pending. */
  pending: OpenRequest | null;
  /** Publish a request to open `path` in `terminalId`'s Files reader. Replaces
   *  any unconsumed prior request (the newest click wins). */
  requestOpen: (terminalId: TerminalId, path: string) => void;
  /** Clear the pending request (called by FilePanel once it has opened the file). */
  clear: () => void;
}

export const useFileOpen = create<FileOpenState>((set) => ({
  pending: null,
  requestOpen: (terminalId, path) => set({ pending: { terminalId, path } }),
  clear: () => set({ pending: null }),
}));
