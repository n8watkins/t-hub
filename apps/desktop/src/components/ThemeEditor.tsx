// ThemeEditor - the always-mounted settings GATE.
//
// Two responsibilities, deliberately kept apart so the boot graph stays small:
//   - `useEditorHotkeys` must always be live, or Ctrl/Cmd+, would not open
//     anything. It is tiny (one keydown listener + the settings store).
//   - The PANEL itself is ~1200 lines that statically reach RecoveryReview,
//     HookInstallPanel, CodexHookInstallPanel, VoiceSection, ThemeSection and the
//     Tauri updater/process plugins. ThemeProvider mounts from an index.html
//     entry, so that whole tier used to be parsed on every launch for UI that is
//     invisible until the user asks for it.
//
// So the panel is React.lazy'd into its own chunk. Nothing renders until `open`,
// and the import only starts then, so the lazy boundary costs no extra work on
// the open path beyond one chunk fetch from the local asset protocol.
import { Suspense, lazy, useEffect } from "react";
import { useSettings } from "../store/settings";

const ThemeEditorPanel = lazy(() => import("./ThemeEditorPanel"));

/**
 * Wire the global `Ctrl/Cmd+,` toggle (and Esc-to-close) onto the settings
 * store, so the panel's open state is shared rather than component-local.
 */
function useEditorHotkeys(): void {
  const toggleSettings = useSettings((s) => s.toggleSettings);
  const closeSettings = useSettings((s) => s.closeSettings);
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      const mod = e.ctrlKey || e.metaKey;
      // Ctrl/Cmd+, opens/closes the panel (matches the conventional "settings"
      // shortcut). `e.key` is "," regardless of layout shifts for this combo.
      if (mod && e.key === "," && !e.altKey && !e.shiftKey) {
        e.preventDefault();
        toggleSettings();
      } else if (e.key === "Escape") {
        // Only consume Escape when we're actually open (don't swallow it
        // globally — terminals/inputs may want it when the panel is closed).
        if (useSettings.getState().settingsOpen) {
          e.preventDefault();
          closeSettings();
        }
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [toggleSettings, closeSettings]);
}

export function ThemeEditor() {
  useEditorHotkeys();
  const open = useSettings((s) => s.settingsOpen);
  const closeSettings = useSettings((s) => s.closeSettings);
  if (!open) return null;
  // No fallback chrome: the chunk is local (Tauri asset protocol), so the gap is
  // a frame or two. Rendering a spinner would flash more than it informs.
  return (
    <Suspense fallback={null}>
      <ThemeEditorPanel onClose={closeSettings} />
    </Suspense>
  );
}
