// WebPreview — an in-overlay webpage viewer. A URL bar (defaulting to a local
// dev server) drives an <iframe>; the main use case is previewing a localhost
// dev server next to your terminals without leaving TermHub.
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
import { open as shellOpen } from "@tauri-apps/plugin-shell";

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

/** Default URL — the conventional local dev server. */
const DEFAULT_URL = "http://localhost:3000";

/** How long to wait for a `load` before assuming the site refused framing. A
 *  localhost page resolves in well under this; the watchdog is only the safety
 *  net for silently-blocked (X-Frame-Options/CSP) navigations. */
const LOAD_WATCHDOG_MS = 6000;

type LoadState =
  | { status: "idle" }
  | { status: "loading" }
  | { status: "loaded" }
  // "blocked": the watchdog fired with no load — almost always framing refusal.
  // "error": the iframe fired an explicit error (bad URL / connection refused).
  | { status: "blocked" }
  | { status: "error" };

export interface WebPreviewProps {
  /** Optional starting URL (defaults to the local dev server). */
  initialUrl?: string;
}

export function WebPreview({ initialUrl = DEFAULT_URL }: WebPreviewProps) {
  // `url` is the committed (submitted) address driving the iframe; `draft` is
  // the editable input value. Splitting them means typing doesn't reload on
  // every keystroke — only Enter / the Go button navigates.
  const [url, setUrl] = useState(initialUrl);
  const [draft, setDraft] = useState(initialUrl);
  const [load, setLoad] = useState<LoadState>({ status: "idle" });
  // Bumped on every (re)navigation so the iframe remounts and the watchdog
  // effect re-runs even when the same URL is re-submitted (a manual retry).
  const [nav, setNav] = useState(0);
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

  // Watchdog: when a navigation starts, assume framing was refused if no `load`
  // event lands within the window. The iframe's onLoad clears this by moving us
  // to "loaded" (which cancels the timer on the next effect run).
  useEffect(() => {
    if (load.status !== "loading") return;
    const handle = window.setTimeout(() => {
      setLoad((cur) => (cur.status === "loading" ? { status: "blocked" } : cur));
    }, LOAD_WATCHDOG_MS);
    return () => window.clearTimeout(handle);
  }, [load.status, nav]);

  const openInBrowser = useCallback(() => {
    if (url) void openExternal(url);
  }, [url]);
  // (openExternal is async with its own internal fallback; we fire-and-forget.)

  return (
    <div className="flex h-full min-h-0 flex-col">
      {/* URL bar: address input + Go + an always-available external-open. */}
      <form
        className="flex shrink-0 items-center gap-2 border-b px-3 py-2"
        style={{ borderColor: "var(--th-border)" }}
        onSubmit={(e) => {
          e.preventDefault();
          navigate(draft);
        }}
      >
        <input
          value={draft}
          onChange={(e) => setDraft(e.target.value)}
          placeholder="http://localhost:3000"
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
        <button
          type="button"
          onClick={openInBrowser}
          className="shrink-0 px-3 py-1.5 text-sm"
          style={{
            borderRadius: "var(--th-radius)",
            border: "1px solid var(--th-border)",
            background: "transparent",
            color: "var(--th-fg-muted)",
          }}
          title="Open this URL in your external browser"
        >
          Open externally
        </button>
      </form>

      {/* Body: the framed page, with overlays for the blocked/error cases. The
          iframe stays mounted under a blocked/error notice so a successful late
          load can still clear it; the notice just covers it until then. */}
      <div className="relative min-h-0 flex-1" style={{ background: "#fff" }}>
        {url ? (
          <iframe
            // Remount on each navigation so a refused load doesn't leave a stale
            // frame and the watchdog effect re-arms cleanly.
            key={nav}
            ref={iframeRef}
            src={url}
            title="Web preview"
            className="h-full w-full border-0"
            // Permissive enough for typical dev servers (scripts, same-origin,
            // forms, popups) while still sandboxed.
            sandbox="allow-scripts allow-same-origin allow-forms allow-popups allow-modals"
            onLoad={() => setLoad({ status: "loaded" })}
            onError={() => setLoad({ status: "error" })}
          />
        ) : (
          <div
            className="flex h-full items-center justify-center text-sm"
            style={{ color: "var(--th-fg-muted)" }}
          >
            Enter a URL to preview.
          </div>
        )}

        {(load.status === "blocked" || load.status === "error") && (
          <FramingNotice
            kind={load.status}
            url={url}
            onOpenExternal={openInBrowser}
            onRetry={() => navigate(url)}
          />
        )}
      </div>
    </div>
  );
}

/**
 * The friendly fallback shown when a page won't frame (blocked by
 * X-Frame-Options/CSP) or failed to load. Offers the external-browser open and a
 * retry. Themed to sit over the (white) iframe surface.
 */
function FramingNotice({
  kind,
  url,
  onOpenExternal,
  onRetry,
}: {
  kind: "blocked" | "error";
  url: string;
  onOpenExternal: () => void;
  onRetry: () => void;
}) {
  const blocked = kind === "blocked";
  return (
    <div
      className="absolute inset-0 flex flex-col items-center justify-center gap-3 px-8 text-center"
      style={{ background: "var(--th-sidebar-bg)", color: "var(--th-fg)" }}
    >
      <div className="text-sm font-semibold">
        {blocked
          ? "This page can’t be previewed here"
          : "Couldn’t load this page"}
      </div>
      <div
        className="max-w-md text-xs leading-relaxed"
        style={{ color: "var(--th-fg-muted)" }}
      >
        {blocked ? (
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
        title={url}
      >
        {url}
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
