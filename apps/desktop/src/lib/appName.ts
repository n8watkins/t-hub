// The app's user-facing name, read from the Tauri config's `productName`:
// "T-Hub" for production, "T-Hub Dev" for the side-by-side dev build. Used for the
// titlebar wordmark + the About line so the dev build is visibly distinct (the tray
// + window title get the same name from `brand_name()` on the Rust side).

import { useEffect, useState } from "react";
import { getName } from "@tauri-apps/api/app";

// Resolved once for the app lifetime (the name can't change at runtime).
let cached: string | null = null;

/**
 * The product name from the Tauri config. Defaults to `"T-Hub"` until the async
 * fetch resolves (and if it ever fails), so the wordmark never flashes empty.
 */
export function useAppName(): string {
  const [name, setName] = useState<string>(cached ?? "T-Hub");
  useEffect(() => {
    if (cached) return;
    let alive = true;
    void getName()
      .then((n) => {
        cached = n;
        if (alive) setName(n);
      })
      .catch(() => {});
    return () => {
      alive = false;
    };
  }, []);
  return name;
}
