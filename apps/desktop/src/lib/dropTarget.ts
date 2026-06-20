// Shared pointer-drop-target resolver (#6).
//
// Every TermHub drag interaction is built on POINTER events + `document.
// elementFromPoint`, not HTML5 DnD (which is unreliable over xterm's WebGL canvas
// in WebView2 — see src/lib/pointerDrag.ts). Each call site historically rolled
// its OWN `elementFromPoint(x,y)?.closest("[data-...]")` plus an
// `getAttribute(...)` to map a viewport point to the owning tile / tab / row.
// This collapses that into ONE helper so the resolution logic lives in one place;
// each call site still owns its own PRECEDENCE by passing its selectors in order.
//
// elementFromPoint returns the topmost element under the point (often the xterm
// canvas), so we walk UP to the nearest matching ancestor with `closest`. The
// attribute whose value we return is read straight from the matched selector, so
// `[data-tab-id]` yields the value of `data-tile-id`'s sibling attribute, etc.

/** The resolved drop target: the matched element, WHICH selector matched (so the
 *  caller can branch on it, e.g. a pool-body hit vs a tile-chrome hit), and the
 *  value of that selector's data attribute (null if the attribute is value-less). */
export interface DropTargetHit {
  el: HTMLElement;
  selector: string;
  value: string | null;
}

/** The attribute name a single `[data-...]` selector targets, or null if the
 *  selector isn't of that shape. Only the leading `[data-...]` bracket is read —
 *  enough for our call sites, which all pass plain attribute selectors. */
function attrNameOf(selector: string): string | null {
  const m = /^\[\s*([a-zA-Z0-9-]+)/.exec(selector.trim());
  return m ? m[1] : null;
}

/**
 * Resolve which drop target sits under viewport point (`x`, `y`). The `selectors`
 * are tried IN ORDER as one combined `closest(...)` — but because a single
 * `closest` returns the nearest ancestor matching ANY of them (DOM-distance, not
 * list order), this finds the closest matching ancestor and then reports WHICH of
 * the passed selectors it matched (first match in list order wins on a tie). The
 * value returned is that selector's data-attribute value on the matched element.
 *
 * Call sites that need strict list precedence (try selector A's nearest match
 * before selector B's, regardless of DOM distance) should call this once PER
 * selector and take the first hit — see Tile.tsx (tab-over-tile) and dropPaste.ts
 * (pool-over-tile). For the simple single-anchor sites one call is enough.
 */
export function resolveDropTarget(
  x: number,
  y: number,
  selectors: string[],
): DropTargetHit | null {
  const el = document.elementFromPoint(x, y) as HTMLElement | null;
  if (!el) return null;
  const hit = el.closest<HTMLElement>(selectors.join(", "));
  if (!hit) return null;
  // Report the FIRST passed selector the matched element satisfies (list order),
  // so a caller's precedence is honored when an element carries two of them.
  for (const sel of selectors) {
    if (hit.matches(sel)) {
      const name = attrNameOf(sel);
      return {
        el: hit,
        selector: sel,
        value: name ? hit.getAttribute(name) : null,
      };
    }
  }
  // Shouldn't happen (closest matched the union), but stay defensive.
  return { el: hit, selector: selectors[0], value: null };
}
