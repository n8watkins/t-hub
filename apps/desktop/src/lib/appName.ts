// The app's user-facing name, read from the Tauri config's `productName`:
// "T-Hub" for production, "T-Hub Dev" for the side-by-side dev build. Used for the
// titlebar wordmark + the About line so the dev build is visibly distinct (the tray
// + window title get the same name from `brand_name()` on the Rust side).

import { useEffect, useState } from "react";
import { getName, getVersion } from "@tauri-apps/api/app";

// Resolved once for the app lifetime (the name can't change at runtime).
let cached: string | null = null;
// Same idea for the version string (from tauri.conf.json → Cargo/package version).
let cachedVersion: string | null = null;

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

/**
 * The app version (e.g. `0.3.4`) from the Tauri config, for a small build-stamp in
 * the titlebar. Empty string until the async fetch resolves (and if it ever fails),
 * so nothing flashes. Surfaced in the top-left so the running build is always
 * identifiable at a glance (which build am I actually on?).
 */
export function useAppVersion(): string {
  const [version, setVersion] = useState<string>(cachedVersion ?? "");
  useEffect(() => {
    if (cachedVersion) return;
    let alive = true;
    void getVersion()
      .then((v) => {
        cachedVersion = v;
        if (alive) setVersion(v);
      })
      .catch(() => {});
    return () => {
      alive = false;
    };
  }, []);
  return version;
}
