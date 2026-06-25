// Keyboard-chord parsing/formatting for the hybrid keymap (WS-3).
//
// A chord is a normalized lowercase string of the form `"<mod>+...+<key>"`,
// modifiers first in a fixed order, e.g. `"ctrl+t"`, `"ctrl+shift+tab"`,
// `"ctrl+1"`, `"ctrl+="`. The ONLY modifiers are ctrl / shift / alt:
//   - Cmd (Meta) is folded into `ctrl` so a single binding string works on both
//     macOS and Windows/Linux — mirroring the app's long-standing
//     `e.ctrlKey || e.metaKey` convention. There is intentionally no `meta`/`cmd`
//     token.
// The non-modifier key is lowercased; named keys keep their lowercased name
// (`tab`, `escape`, `arrowup`, …). Letters/digits/punctuation are their literal
// character (`t`, `1`, `=`).

/** Fixed modifier order so a chord has exactly one canonical spelling. */
const MOD_ORDER = ["ctrl", "shift", "alt"] as const;

/** Build the canonical chord string for a KeyboardEvent. Returns null when the
 *  event carries no usable key (a lone modifier press, IME composition, etc.) so
 *  callers can ignore it. */
export function chordFromEvent(e: KeyboardEvent): string | null {
  const key = e.key;
  if (!key) return null;
  // A bare modifier keydown (Control/Shift/Alt/Meta) isn't itself a chord.
  if (
    key === "Control" ||
    key === "Shift" ||
    key === "Alt" ||
    key === "Meta" ||
    key === "OS"
  ) {
    return null;
  }
  const parts: string[] = [];
  if (e.ctrlKey || e.metaKey) parts.push("ctrl"); // fold Cmd into ctrl
  if (e.shiftKey) parts.push("shift");
  if (e.altKey) parts.push("alt");
  parts.push(normalizeKeyName(key));
  return parts.join("+");
}

/** The bare key (no modifiers) for a KeyboardEvent — used by the prefix tier,
 *  where the second keystroke is matched on its plain key alone. Null for a lone
 *  modifier press. */
export function bareKeyFromEvent(e: KeyboardEvent): string | null {
  const key = e.key;
  if (
    !key ||
    key === "Control" ||
    key === "Shift" ||
    key === "Alt" ||
    key === "Meta" ||
    key === "OS"
  ) {
    return null;
  }
  return normalizeKeyName(key);
}

/** Lowercase + canonicalize a KeyboardEvent.key. Single chars lowercase to their
 *  literal; named keys lowercase too (Tab -> tab, Escape -> escape). `+` is the
 *  joiner so a literal "+" key is spelled "plus" to stay parseable. */
function normalizeKeyName(key: string): string {
  if (key === "+") return "plus";
  if (key === " ") return "space";
  return key.toLowerCase();
}

/** Normalize an arbitrary chord string into canonical form (modifier order,
 *  lowercasing, ctrl/cmd folding). Returns "" for an empty/meaningless chord
 *  (e.g. only modifiers). Used to clean persisted/user-typed bindings. */
export function normalizeChord(input: string): string {
  if (!input) return "";
  const tokens = input
    .split("+")
    .map((t) => t.trim().toLowerCase())
    .filter(Boolean);
  if (tokens.length === 0) return "";
  const mods = new Set<string>();
  let key = "";
  for (const t of tokens) {
    if (t === "ctrl" || t === "control" || t === "cmd" || t === "meta") {
      mods.add("ctrl"); // fold cmd/meta into ctrl
    } else if (t === "shift") {
      mods.add("shift");
    } else if (t === "alt" || t === "option") {
      mods.add("alt");
    } else {
      key = t === "+" ? "plus" : t === " " ? "space" : t;
    }
  }
  if (!key) return ""; // modifiers only — not a usable chord
  const parts: string[] = MOD_ORDER.filter((m) => mods.has(m));
  parts.push(key);
  return parts.join("+");
}

/** Pretty-print a chord for the UI: "ctrl+shift+tab" -> "Ctrl + Shift + Tab".
 *  ctrl renders as "Ctrl/Cmd" to reflect the cross-platform folding. */
export function formatChord(chord: string): string {
  if (!chord) return "—";
  return chord
    .split("+")
    .map((t) => {
      switch (t) {
        case "ctrl":
          return "Ctrl/Cmd";
        case "shift":
          return "Shift";
        case "alt":
          return "Alt";
        case "tab":
          return "Tab";
        case "escape":
          return "Esc";
        case "plus":
          return "+";
        case "space":
          return "Space";
        default:
          return t.length === 1 ? t.toUpperCase() : capitalize(t);
      }
    })
    .join(" + ");
}

function capitalize(s: string): string {
  return s.charAt(0).toUpperCase() + s.slice(1);
}
