// ThemeProvider — the runtime glue that makes themes live + MCP-aware.
//
// It renders no chrome of its own beyond hosting the (self-toggling) ThemeEditor
// overlay. On mount it runs the one-time theme bridge:
//   - applies the persisted/active theme to :root immediately (no flash),
//   - reconciles with the backend's persisted theme (the MCP-writable copy),
//   - subscribes to `theme://changed` so a theme set via MCP/another window
//     applies here live.
// Mounted by the self-contained bootstrap (src/themeBootstrap.tsx) into its own
// React root, so App.tsx never has to know the theming system exists.
import { useEffect } from "react";
import { initThemeBridge } from "../store/theme";
import { ThemeEditor } from "./ThemeEditor";

export function ThemeProvider() {
  useEffect(() => {
    let dispose: (() => void) | undefined;
    let cancelled = false;
    void initThemeBridge().then((d) => {
      if (cancelled) d();
      else dispose = d;
    });
    return () => {
      cancelled = true;
      dispose?.();
    };
  }, []);

  // The editor mounts here but is invisible until the user hits Ctrl/Cmd+,.
  return <ThemeEditor />;
}
