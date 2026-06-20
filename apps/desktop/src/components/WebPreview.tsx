// WebPreview — an in-overlay webpage viewer. A URL bar drives an <iframe>; the
// main use case is previewing a localhost dev server next to your terminals
// without leaving TermHub. With NO URL yet (nothing detected / typed / fed from
// the Dev runner) it sits on a helpful empty state instead of pointing the
// iframe at a dead default address — see the empty-body branch below.
//
// Framing reality: many production sites refuse to be embedded via
// `X-Frame-Options: DENY/SAMEORIGIN` or a `frame-ancestors` CSP. The browser
// blocks the load WITHOUT firing the iframe's `error` event and (cross-origin)
// without letting us read the frame — so we can't detect a refusal by listening
// for `error`. Instead we arm a short watchdog on each navigation: if neither a
// `load` event nor a manual confirmation arrives in time, we assume the site
// refused framing and surface a friendly message plus an "Open in browser"
// fallback that hands the URL to the OS default browser via the shell plugin.
// Localhost dev servers (the target use case) virtually always frame fine, so
// the happy path is the common path.
//
// This is a self-contained surface meant to live inside <PreviewOverlay>; it
// owns only its URL/loading/error state and the one shell `open()` side effect.

import { useCallback, useEffect, useRef, useState } from "react";
import type { ReactElement } from "react";
import { open as shellOpen } from "@tauri-apps/plugin-shell";
import { ExternalLink, SquareArrowOutUpRight } from "lucide-react";
import {
  reachablePreviewUrl,
  probePreviewReachable,
} from "../ipc/devserver";
import { popOutPreview } from "../store/preview";

/**
 * Hand a URL to the OS default browser. Primary path is the Tauri shell
 * plugin's `open()` (PRD-blessed, already a JS dependency). If that rejects —
 * e.g. the native `tauri-plugin-shell` isn't registered / lacks the
 * `shell:allow-open` capability in this build — fall back to `window.open`,
 * which WebView2 routes to the external browser for a `_blank` target. Either
 * way the user gets out to a real browser; we never leave them stuck.
 */
async function openExternal(url: string): Promise<void> {
  try {
    await shellOpen(url);
  } catch {
    try {
      window.open(url, "_blank", "noopener,noreferrer");
    } catch {
      /* nothing more we can do from the frontend */
    }
  }
}

/** Placeholder shown in the empty URL bar — the conventional local dev server.
 *  This is ONLY a hint/quick-fill: it is never auto-loaded (see the empty-state
 *  branch), so a tile with nothing running doesn't flash a connection error. */
const URL_PLACEHOLDER = "http://localhost:3000";

/** How long to wait for a `load` before assuming the site refused framing. A
 *  localhost page resolves in well under this; the watchdog is only the safety
 *  net for silently-blocked (X-Frame-Options/CSP) navigations. */
const LOAD_WATCHDOG_MS = 6000;

type LoadState =
  | { status: "idle" }
  | { status: "loading" }
  | { status: "loaded" }
  // "blocked": the watchdog fired with no load. Could be framing refusal OR the
  // server being unreachable; a TCP probe (below) refines which. `reachable`
  // is filled in after the probe resolves: false => the server isn't accepting
  // connections (the WSL2 case), true/undefined => it's up but refused framing.
  // "error": the iframe fired an explicit error (bad URL / connection refused).
  | { status: "blocked"; reachable?: boolean }
  | { status: "error" };

export interface WebPreviewProps {
  /** Optional starting URL (the Dev runner's detected URL, or the user's last
   *  Preview URL). Absent/empty => no auto-load; we show the empty state with
   *  the URL bar still available rather than loading a dead default address. */
  initialUrl?: string;
  /** localhost URLs scraped from the tile's terminal output (newest-first).
   *  Rendered as one-click chips under the URL bar; clicking one navigates. */
  detectedUrls?: string[];
}

export function WebPreview({
  initialUrl = "",
  detectedUrls = [],
}: WebPreviewProps): ReactElement {
  // `url` is the committed (submitted) address driving the iframe; `draft` is
  // the editable input value. Splitting them means typing doesn't reload on
  // every keystroke — only Enter / the Go button navigates.
  const [url, setUrl] = useState(initialUrl);
  const [draft, setDraft] = useState(initialUrl);
  const [load, setLoad] = useState<LoadState>({ status: "idle" });
  // Bumped on every (re)navigation so the iframe remounts and the watchdog
  // effect re-runs even when the same URL is re-submitted (a manual retry).
  const [nav, setNav] = useState(0);
  // `loadUrl` is the URL the iframe ACTUALLY loads — `url` with any WSL
  // `localhost`/`127.0.0.1` rewritten to a Windows-reachable host (the core
  // connection fix). It lags `url` by one async tick (the host lookup); until it
  // resolves we don't mount the iframe so we never flash a load against the
  // unreachable loopback. On unix / no backend the rewrite is a no-op and this
  // just equals `url`. See ipc/devserver.ts `reachablePreviewUrl`.
  const [loadUrl, setLoadUrl] = useState<string>("");
  const iframeRef = useRef<HTMLIFrameElement>(null);

  // Normalize a typed address into a loadable URL: bare hosts/ports get an
  // http:// scheme so "localhost:3000" and "example.com" both work.
  const normalize = useCallback((raw: string): string => {
    const t = raw.trim();
    if (!t) return "";
    if (/^https?:\/\//i.test(t)) return t;
    return `http://${t}`;
  }, []);

  const navigate = useCallback(
    (raw: string) => {
      const next = normalize(raw);
      if (!next) return;
      setUrl(next);
      setDraft(next);
      setLoad({ status: "loading" });
      setNav((n) => n + 1);
    },
    [normalize],
  );

  // Follow a LIVE `initialUrl`. The per-tile Preview tab passes the project's
  // detected dev-server URL (usePanels.devUrl) as `initialUrl`; when terminal
  // output detection updates it, the prop changes and we should navigate there —
  // but ONLY when it's a genuinely new server URL, never clobbering an address
  // the user typed into the bar. We remember the last `initialUrl` we adopted
  // and re-navigate only when the incoming prop differs from BOTH that adopted
  // value and the currently committed `url` (so a user override sticks). The
  // mount-time value is already adopted via useState above, so we seed the ref
  // with it and this effect is a no-op on the first render.
  const adoptedInitialRef = useRef(initialUrl);
  useEffect(() => {
    if (initialUrl === adoptedInitialRef.current) return; // unchanged prop
    adoptedInitialRef.current = initialUrl;
    if (!initialUrl) return; // cleared (no dev URL yet) — leave the bar as-is
    if (initialUrl === url) return; // already showing it (e.g. user typed it)
    navigate(initialUrl);
    // `navigate` is stable (useCallback); `url` is read to avoid a redundant
    // reload when we're already on the incoming URL.
  }, [initialUrl, navigate, url]);

  // Seed the preview from the newest DETECTED URL, but only as a last resort:
  // when no real dev-server URL was ever passed in (so the committed `url` is
  // still the untouched empty initial) and the user hasn't navigated. This makes
  // the first localhost URL a terminal prints auto-load, without ever clobbering
  // a URL the user typed/is viewing — once `url` diverges from the adopted
  // initial value the guard below is false forever, and this is one-shot anyway.
  const detectedSeededRef = useRef(false);
  const newestDetected = detectedUrls[0];
  useEffect(() => {
    if (detectedSeededRef.current) return; // only auto-seed once
    if (!newestDetected) return; // nothing detected yet
    // Don't seed if a real dev URL was supplied, or if the user has navigated:
    // in both cases the committed `url` no longer equals the adopted initial.
    if (url !== adoptedInitialRef.current) return;
    detectedSeededRef.current = true;
    navigate(newestDetected);
  }, [newestDetected, navigate, url]);

  // Resolve the reachable load URL whenever the committed `url` (or a manual
  // retry via `nav`) changes. On Windows this swaps a WSL `localhost` for the
  // WSL interface IP so the Windows-side iframe can reach the dev server; on unix
  // / plain-browser it's a no-op. We mount the iframe only AFTER this resolves
  // (loadUrl set), so the first load already targets the reachable host.
  useEffect(() => {
    let cancelled = false;
    if (!url) {
      setLoadUrl("");
      return;
    }
    void reachablePreviewUrl(url).then((resolved) => {
      if (cancelled) return;
      setLoadUrl(resolved);
      // Ensure the watchdog can arm for the FIRST load too: on mount `url` is
      // seeded directly (not via navigate, which sets "loading"), so without
      // this a silent initial failure (the WSL case) would sit in "idle" and
      // never surface a notice. Move to "loading" unless the iframe already
      // reported a terminal state for this nav.
      setLoad((cur) => (cur.status === "idle" ? { status: "loading" } : cur));
    });
    return () => {
      cancelled = true;
    };
  }, [url, nav]);

  // Watchdog: when a navigation starts, assume framing was refused if no `load`
  // event lands within the window. The iframe's onLoad clears this by moving us
  // to "loaded" (which cancels the timer on the next effect run).
  useEffect(() => {
    if (load.status !== "loading") return;
    const handle = window.setTimeout(() => {
      setLoad((cur) =>
        cur.status === "loading" ? { status: "blocked" } : cur,
      );
    }, LOAD_WATCHDOG_MS);
    return () => window.clearTimeout(handle);
  }, [load.status, nav]);

  // When we land in "blocked" (the watchdog fired with no load), TCP-probe the
  // target to tell the two causes apart: a refused/timed-out connection means the
  // server isn't up / isn't reachable (the WSL2 localhost case) — a precise,
  // actionable message — whereas a successful connect means it's up but refused
  // framing. We probe `loadUrl` (the address the iframe used) and stash the
  // result on the load state so the notice can specialize. Best-effort: if we
  // can't probe (no backend) we leave `reachable` undefined (generic notice).
  useEffect(() => {
    if (load.status !== "blocked" || load.reachable !== undefined) return;
    if (!loadUrl) return;
    let cancelled = false;
    void probePreviewReachable(loadUrl).then((ok) => {
      if (cancelled || ok === null) return;
      setLoad((cur) =>
        cur.status === "blocked" ? { status: "blocked", reachable: ok } : cur,
      );
    });
    return () => {
      cancelled = true;
    };
  }, [load, loadUrl]);

  const openInBrowser = useCallback(() => {
    // The external browser is ALSO a Windows process, so it hits the same
    // unreachable WSL loopback — hand it the reachable URL, not the raw one.
    if (!url) return;
    void reachablePreviewUrl(url).then((u) => openExternal(u || url));
  }, [url]);
  // (openExternal is async with its own internal fallback; we fire-and-forget.)

  // Pop the current preview out into its own OS window (TASK 3). The window is a
  // top-level load of the (reachable) dev URL — no iframe, so framing CSPs don't
  // apply — and each call opens a NEW window, so multiple previews can coexist
  // (TASK 2). We resolve the reachable URL first for the same WSL reason.
  const popOut = useCallback(() => {
    if (!url) return;
    void reachablePreviewUrl(url).then((u) => popOutPreview(u || url));
  }, [url]);

  return (
    <div className="flex h-full min-h-0 flex-col">
      {/* URL bar: address input + Go + an always-available external-open. The
          detected-URL chips (if any) wrap onto a row directly under it. */}
      <div
        className="shrink-0 border-b"
        style={{ borderColor: "var(--th-border)" }}
      >
      <form
        className="flex shrink-0 items-center gap-2 px-3 py-2"
        onSubmit={(e) => {
          e.preventDefault();
          navigate(draft);
        }}
      >
        <input
          value={draft}
          onChange={(e) => setDraft(e.target.value)}
          placeholder={URL_PLACEHOLDER}
          spellCheck={false}
          autoCorrect="off"
          autoCapitalize="off"
          autoComplete="off"
          className="min-w-0 flex-1 px-2.5 py-1.5 text-sm focus:outline-none"
          style={{
            borderRadius: "var(--th-radius)",
            border: "1px solid var(--th-border)",
            background: "var(--th-tile-bg)",
            color: "var(--th-fg)",
          }}
          onFocus={(e) => {
            e.currentTarget.style.borderColor = "var(--th-focus-ring)";
          }}
          onBlur={(e) => {
            e.currentTarget.style.borderColor = "var(--th-border)";
          }}
        />
        <button
          type="submit"
          className="shrink-0 px-3 py-1.5 text-sm font-medium"
          style={{
            borderRadius: "var(--th-radius)",
            border: "1px solid var(--th-border)",
            background: "var(--th-tile-bg)",
            color: "var(--th-fg)",
          }}
          title="Load this URL"
        >
          Go
        </button>
        {/* Pop out + open-externally as compact ICON buttons (these used to be
            text buttons / a separate Dev panel). Both resolve the reachable URL
            internally; the icons keep the toolbar tight on a narrow tile. */}
        <button
          type="button"
          onClick={popOut}
          className="flex h-8 w-8 shrink-0 items-center justify-center"
          style={{
            borderRadius: "var(--th-radius)",
            border: "1px solid var(--th-border)",
            background: "transparent",
            color: "var(--th-fg-muted)",
          }}
          title="Open this preview in its own window"
          aria-label="Pop out preview"
        >
          <SquareArrowOutUpRight size={16} aria-hidden />
        </button>
        <button
          type="button"
          onClick={openInBrowser}
          className="flex h-8 w-8 shrink-0 items-center justify-center"
          style={{
            borderRadius: "var(--th-radius)",
            border: "1px solid var(--th-border)",
            background: "transparent",
            color: "var(--th-fg-muted)",
          }}
          title="Open this URL in your external browser"
          aria-label="Open externally in browser"
        >
          <ExternalLink size={16} aria-hidden />
        </button>
      </form>

      {/* Detected-URL chips: localhost URLs the tile's terminal printed (e.g. a
          dev server announcing itself). Click to navigate the preview there.
          Newest-first; only shown when there are any. */}
      {detectedUrls.length > 0 && (
        <div className="flex flex-wrap items-center gap-1.5 px-3 pb-2">
          <span
            className="text-[11px]"
            style={{ color: "var(--th-fg-muted)" }}
          >
            Detected:
          </span>
          {detectedUrls.map((u) => {
            const active = u === url;
            return (
              <button
                key={u}
                type="button"
                onClick={() => navigate(u)}
                title={`Preview ${u}`}
                className="max-w-[16rem] truncate px-2 py-0.5 text-[11px]"
                style={{
                  borderRadius: "var(--th-radius)",
                  border: "1px solid var(--th-border)",
                  // The currently-shown URL reads as selected; the rest are
                  // muted "jump to it" affordances.
                  background: active ? "var(--th-tile-bg)" : "transparent",
                  color: active ? "var(--th-fg)" : "var(--th-fg-muted)",
                }}
              >
                {u}
              </button>
            );
          })}
        </div>
      )}
      </div>

      {/* Body: the framed page, with overlays for the blocked/error cases. The
          iframe stays mounted under a blocked/error notice so a successful late
          load can still clear it; the notice just covers it until then. */}
      <div className="relative min-h-0 flex-1" style={{ background: "#fff" }}>
        {url && loadUrl ? (
          <iframe
            // Remount on each navigation so a refused load doesn't leave a stale
            // frame and the watchdog effect re-arms cleanly. `loadUrl` is the
            // reachable (host-rewritten) address; see the resolve effect above.
            key={nav}
            ref={iframeRef}
            src={loadUrl}
            title="Web preview"
            className="h-full w-full border-0"
            // Permissive enough for typical dev servers (scripts, same-origin,
            // forms, popups) while still sandboxed.
            sandbox="allow-scripts allow-same-origin allow-forms allow-popups allow-modals"
            onLoad={() => setLoad({ status: "loaded" })}
            onError={() => setLoad({ status: "error" })}
          />
        ) : (
          // Empty state: no URL detected / typed / fed from the Dev runner yet.
          // We deliberately do NOT load a dead default address (which would flash
          // a connection error) — instead point the user at the Dev tab or the
          // URL bar above, both of which set a real URL when ready.
          <div
            className="flex h-full flex-col items-center justify-center gap-1.5 px-8 text-center"
            style={{ color: "var(--th-fg-muted)" }}
          >
            <div className="text-sm font-medium" style={{ color: "var(--th-fg)" }}>
              Nothing to preview yet
            </div>
            <div className="max-w-xs text-xs leading-relaxed">
              Start a dev server (the <span className="font-medium">Dev</span>{" "}
              tab) or enter a URL above to preview it here.
            </div>
          </div>
        )}

        {(load.status === "blocked" || load.status === "error") && (
          <FramingNotice
            // A blocked load whose TCP probe came back unreachable is the
            // "server isn't up / not reachable" case (incl. the WSL2 one), not a
            // framing refusal — show that precise message instead.
            kind={
              load.status === "blocked" && load.reachable === false
                ? "unreachable"
                : load.status
            }
            url={url}
            loadUrl={loadUrl}
            onOpenExternal={openInBrowser}
            onRetry={() => navigate(url)}
          />
        )}
      </div>
    </div>
  );
}

/**
 * The friendly fallback shown when a preview can't load. Three cases:
 *   - "unreachable": the TCP probe confirmed nothing is accepting connections at
 *     the address (server not started, wrong port, or — the WSL2 case — a server
 *     bound to a loopback the Windows WebView can't reach). The most actionable.
 *   - "blocked": the watchdog fired but the server IS up → it refused framing
 *     (X-Frame-Options / CSP), as many production sites do.
 *   - "error": the iframe fired an explicit error (bad URL).
 * Offers the external-browser open and a retry. Themed over the (white) iframe.
 */
function FramingNotice({
  kind,
  url,
  loadUrl,
  onOpenExternal,
  onRetry,
}: {
  kind: "blocked" | "error" | "unreachable";
  url: string;
  /** The reachable address actually loaded (may differ from `url` on Windows). */
  loadUrl: string;
  onOpenExternal: () => void;
  onRetry: () => void;
}) {
  // Whether the iframe loaded a rewritten host (Windows/WSL); surfaced in the
  // unreachable case so the user can see we already tried the reachable address.
  const rewritten = !!loadUrl && loadUrl !== url;
  const title =
    kind === "unreachable"
      ? "Couldn’t reach this server"
      : kind === "blocked"
        ? "This page can’t be previewed here"
        : "Couldn’t load this page";
  return (
    <div
      className="absolute inset-0 flex flex-col items-center justify-center gap-3 px-8 text-center"
      style={{ background: "var(--th-sidebar-bg)", color: "var(--th-fg)" }}
    >
      <div className="text-sm font-semibold">{title}</div>
      <div
        className="max-w-md text-xs leading-relaxed"
        style={{ color: "var(--th-fg-muted)" }}
      >
        {kind === "unreachable" ? (
          <>
            Nothing is accepting connections at this address. Make sure the dev
            server is running on this port. If it’s running inside WSL, bind it to{" "}
            <code>0.0.0.0</code> (not <code>127.0.0.1</code>) so the preview can
            reach it — e.g. Vite needs <code>--host</code>.
          </>
        ) : kind === "blocked" ? (
          <>
            The site refused to be embedded (it sets{" "}
            <code>X-Frame-Options</code> or a framing <code>CSP</code>). Many
            production sites do this. Localhost dev servers usually preview fine
            — open it in your browser instead.
          </>
        ) : (
          <>
            The address didn’t load. Check the URL and that the server is
            running, then retry — or open it in your browser.
          </>
        )}
      </div>
      <div
        className="max-w-md truncate text-[11px]"
        style={{ color: "var(--th-fg-muted)" }}
        title={rewritten ? `${url}  →  ${loadUrl}` : url}
      >
        {rewritten ? loadUrl : url}
      </div>
      <div className="flex items-center gap-2 pt-1">
        <button
          type="button"
          onClick={onOpenExternal}
          className="px-3 py-1.5 text-sm font-medium"
          style={{
            borderRadius: "var(--th-radius)",
            border: "1px solid var(--th-border)",
            background: "var(--th-tile-bg)",
            color: "var(--th-fg)",
          }}
        >
          Open in browser
        </button>
        <button
          type="button"
          onClick={onRetry}
          className="px-3 py-1.5 text-sm"
          style={{
            borderRadius: "var(--th-radius)",
            border: "1px solid var(--th-border)",
            background: "transparent",
            color: "var(--th-fg-muted)",
          }}
        >
          Retry
        </button>
      </div>
    </div>
  );
}
