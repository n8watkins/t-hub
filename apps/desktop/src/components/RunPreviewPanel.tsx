import { useCallback, type ReactElement } from "react";
import type { TerminalId } from "../ipc/types";
import { usePanels } from "../store/panels";
import { DevTab } from "./DevTab";
import { WebPreview } from "./WebPreview";

interface RunPreviewPanelProps {
  terminalId: TerminalId;
  cwd: string;
}

/**
 * One guided surface for starting a managed dev server, reviewing its output,
 * and loading the URL that runner reports.
 *
 * Only the managed runner may auto-select a URL.
 * Arbitrary URLs printed by an interactive agent terminal are never promoted
 * into the preview because they do not prove process ownership or intent.
 */
export function RunPreviewPanel({
  terminalId,
  cwd,
}: RunPreviewPanelProps): ReactElement {
  const devUrl = usePanels((state) => state.devUrl[terminalId]);
  const previewUrl = usePanels((state) => state.previewUrl[terminalId]);
  const setPreviewUrl = usePanels((state) => state.setPreviewUrl);
  const rememberPreviewUrl = useCallback(
    (url: string) => setPreviewUrl(terminalId, url),
    [setPreviewUrl, terminalId],
  );

  return (
    <section
      aria-label="Run and Preview"
      className="grid h-full min-h-0 grid-rows-[minmax(10rem,38%)_minmax(0,1fr)]"
    >
      <div
        className="min-h-0 overflow-hidden border-b"
        style={{ borderColor: "var(--th-border)" }}
      >
        <DevTab terminalId={terminalId} cwd={cwd} />
      </div>
      <div className="min-h-0 overflow-hidden">
        <WebPreview
          initialUrl={devUrl ?? previewUrl ?? undefined}
          onNavigate={rememberPreviewUrl}
        />
      </div>
    </section>
  );
}
