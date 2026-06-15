// Strip ANSI / VT control sequences from a string.
//
// Terminal output interleaves escape codes (SGR colors, cursor moves, line
// erase) with the visible text. When we scan that RAW pty stream for things like
// a dev-server's localhost URL, those codes get captured into the match — e.g.
// `http://localhost:7788/preview` arrives as `...preview\x1b[K\x1b[m\x1b[28;1H`
// (erase-line + SGR-reset + cursor-move). Stripping the escapes first leaves
// just the text the user actually sees on screen.
//
// Covers the sequence classes pty output realistically emits:
//   - CSI  e.g. \x1b[0m, \x1b[2K, \x1b[28;1H
//          (params 0x30–0x3F, intermediates 0x20–0x2F, final byte 0x40–0x7E)
//   - OSC  e.g. \x1b]0;window-title\x07  (terminated by BEL or ST `\x1b\`)
//   - two-char escapes  e.g. \x1bM (reverse index), \x1bD
//
// eslint-disable-next-line no-control-regex
const ANSI_RE =
  // eslint-disable-next-line no-control-regex
  /\x1b(?:[@-Z\\-_]|\[[0-?]*[ -/]*[@-~]|\][^\x07\x1b]*(?:\x07|\x1b\\))/g;

/** Remove ANSI/VT escape sequences, returning only the printable text. */
export function stripAnsi(input: string): string {
  return input.replace(ANSI_RE, "");
}
