

/**
 * True when the keydown's target is a real text-entry surface that should keep
 * its OWN keystrokes - a form `<input>`/`<textarea>`/`<select>`, or any
 * `contentEditable` host. The global keymap bails out for these so:
 *   - the command palette's search input + its "press new key" rebind capture
 *     actually RECEIVE the chord (the capture-phase keymap used to fire the
 *     direct command - e.g. Ctrl+T - before the palette could capture it, so a
 *     currently-bound chord was unrebindable);
 *   - typing chords into the worktree-path form, rename-tab, or theme fields
 *     can't accidentally trigger app commands.
 *
 * CRITICAL EXCLUSION: xterm renders an OFFSCREEN `<textarea class=
 * "xterm-helper-textarea">` as a focused terminal's keyboard input sink, so when
 * a terminal is focused EVERY keydown's target is that textarea. We must treat
 * it as NON-editable here, or the editable-guard would disable all global
 * hotkeys (Ctrl+T/+W/+Tab/prefix…) the moment a terminal has focus. We
 * explicitly let that one class fall through so terminal-focused hotkeys keep
 * working; the prefix/direct chords still beat xterm via the capture phase.
 */
export function isEditableTarget(t: EventTarget | null): boolean {
  const el = t as HTMLElement | null;
  if (!el || typeof el.tagName !== "string") return false;
  // xterm's hidden input sink is NOT an editable surface for the keymap - global
  // hotkeys must keep firing while a terminal is focused.
  if (el.classList?.contains("xterm-helper-textarea")) return false;
  const tag = el.tagName;
  if (tag === "INPUT" || tag === "TEXTAREA" || tag === "SELECT") return true;
  return el.isContentEditable === true;
}
