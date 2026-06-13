// STUB — implemented by the Canvas subagent (task #11).
// A tile is a thin header (status dot, title, cwd, close) wrapping a TerminalView.
import type { TerminalId } from "../ipc/types";

export interface TileProps {
  terminalId: TerminalId;
  focused: boolean;
  onFocus: () => void;
  onClose: () => void;
}

export function Tile(_props: TileProps) {
  return null;
}
