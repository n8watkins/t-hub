// A lightweight floating "ghost" that follows the pointer during a drag, so the
// user can see they're carrying something (a tile or a workspace tab) — the
// visual the dimmed-in-place source alone doesn't provide.
//
// It is a plain DOM node appended to <body> (not React) so it composes with the
// imperative pointer-drag controller, and — critically — it is
// `pointer-events: none`, so it NEVER intercepts the cursor and never interferes
// with the `elementFromPoint` drop-target resolution the drag relies on. Colors
// come from the live theme via `var(--th-*)` (with hard fallbacks) so the ghost
// matches the current look.

export interface DragGhostOptions {
  /** Header text (terminal title / tab name). */
  title: string;
  /** Optional muted subline (e.g. a tile's cwd). */
  subtitle?: string;
  /** Ghost width in px. */
  width: number;
  /** Faded body height in px below the header; 0 => a header-only chip (tabs). */
  bodyHeight?: number;
}

export interface DragGhost {
  /** Reposition the ghost to follow the pointer (viewport coords). */
  move: (x: number, y: number) => void;
  /** Remove the ghost from the DOM. */
  destroy: () => void;
}

export function createDragGhost(o: DragGhostOptions): DragGhost {
  const host = document.createElement("div");
  host.setAttribute("aria-hidden", "true");
  host.style.cssText = [
    "position:fixed",
    "left:0",
    "top:0",
    "z-index:2147483600",
    "pointer-events:none",
    "will-change:transform",
    // Start off-screen until the first move() so there's no flash at 0,0.
    "transform:translate(-9999px,-9999px)",
    `width:${o.width}px`,
    "border-radius:var(--th-radius,4px)",
    "border:1px solid var(--th-accent,#10b981)",
    "background:var(--th-tile-bg,#171717)",
    "box-shadow:0 12px 32px -8px rgba(0,0,0,0.6)",
    "opacity:0.9",
    "overflow:hidden",
    "font-family:var(--th-font, ui-sans-serif, system-ui, sans-serif)",
  ].join(";");

  const header = document.createElement("div");
  header.style.cssText = [
    "display:flex",
    "align-items:center",
    "gap:6px",
    "height:22px",
    "padding:0 8px",
    "background:var(--th-header-bg,#0a0a0a)",
    "border-bottom:1px solid var(--th-border,#262626)",
    "color:var(--th-fg,#e5e5e5)",
    "font-size:12px",
    "white-space:nowrap",
  ].join(";");

  const dot = document.createElement("span");
  dot.style.cssText =
    "flex:0 0 auto;width:6px;height:6px;border-radius:9999px;background:var(--th-accent,#10b981)";
  const titleEl = document.createElement("span");
  titleEl.textContent = o.title;
  titleEl.style.cssText =
    "overflow:hidden;text-overflow:ellipsis;white-space:nowrap";
  header.appendChild(dot);
  header.appendChild(titleEl);
  host.appendChild(header);

  if (o.bodyHeight && o.bodyHeight > 0) {
    const body = document.createElement("div");
    const lines: string[] = [`height:${o.bodyHeight}px`, "padding:6px 8px"];
    if (o.subtitle) {
      body.textContent = o.subtitle;
      lines.push(
        "color:var(--th-fg-muted,#737373)",
        "font-size:11px",
        "white-space:nowrap",
        "overflow:hidden",
        "text-overflow:ellipsis",
      );
    }
    body.style.cssText = lines.join(";");
    host.appendChild(body);
  }

  document.body.appendChild(host);

  return {
    move(x, y) {
      // Offset down-right of the cursor so the ghost never sits on the
      // elementFromPoint hit-point and the drop target stays resolvable.
      host.style.transform = `translate(${x + 14}px, ${y + 12}px)`;
    },
    destroy() {
      host.remove();
    },
  };
}
