// Clipboard helpers, shared across the app (the terminal copy/paste path and the
// tile header's copy-an-id menu items).
//
// WebView2 silently blocks `navigator.clipboard` (copy/paste "did nothing"), so
// prefer the Tauri clipboard plugin and fall back to the web API only if the
// plugin isn't available (e.g. plain `pnpm dev` in a browser / a headless test).
import {
  readText as tauriReadText,
  writeText as tauriWriteText,
} from "@tauri-apps/plugin-clipboard-manager";

/** Write `text` to the system clipboard (Tauri plugin first, web API fallback).
 *  Never throws: a fully unavailable clipboard is a silent no-op. */
export async function clipboardWrite(text: string): Promise<void> {
  try {
    await tauriWriteText(text);
    return;
  } catch {
    /* fall through to the web API */
  }
  try {
    await navigator.clipboard.writeText(text);
  } catch {
    /* nothing more we can do */
  }
}

/** Read text from the system clipboard (Tauri plugin first, web API fallback).
 *  Returns "" when the clipboard is empty or unavailable. */
export async function clipboardRead(): Promise<string> {
  try {
    return (await tauriReadText()) ?? "";
  } catch {
    /* fall through */
  }
  try {
    return await navigator.clipboard.readText();
  } catch {
    return "";
  }
}
