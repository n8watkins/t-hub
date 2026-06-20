// usePersistedToggle — a boolean toggle that persists to localStorage (#9).
//
// The sidebar hand-rolled the SAME collapse-and-remember idiom several times:
//   const [open, setOpen] = useState(() => localStorage.getItem(key) !== "0");
//   const persist = (v) => { setOpen(v); localStorage.setItem(key, v ? "1":"0"); };
// (Section, BottomStatus, UsageSection.) This collapses that into one hook with
// the IDENTICAL storage convention — "0" means off, anything else (incl. absent)
// means on — so existing persisted values keep working and behavior is unchanged.
import { useState } from "react";

/**
 * A persisted boolean toggle. Seeds from `localStorage[key]` ("0" => false, any
 * other value or absence => `defaultValue`), and writes "1"/"0" back on every
 * set. With an UNDEFINED key (a collapsible-but-not-remembered case) or no
 * `localStorage` (SSR/test) it behaves as plain in-memory state seeded from
 * `defaultValue`. Returns the value and a setter that persists.
 */
export function usePersistedToggle(
  key: string | undefined,
  defaultValue = true,
): [boolean, (value: boolean) => void] {
  const [value, setValue] = useState<boolean>(() => {
    if (!key || typeof localStorage === "undefined") return defaultValue;
    const raw = localStorage.getItem(key);
    if (raw === null) return defaultValue;
    return raw !== "0";
  });
  const set = (next: boolean): void => {
    setValue(next);
    if (!key || typeof localStorage === "undefined") return;
    try {
      localStorage.setItem(key, next ? "1" : "0");
    } catch {
      /* ignore quota / serialization errors — the in-memory state still updates */
    }
  };
  return [value, set];
}
