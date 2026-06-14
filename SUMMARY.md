# T-Hub promo site — build summary

A modern, animation-heavy marketing site for **T-Hub** (capital T, hyphen), the
free, **open-source**, local terminal-first cockpit for supervising many
persistent Claude Code sessions. **A tool by n8builds** (Nathan Watkins, GitHub
user `n8watkins`). Windows-only for now.

## Owner corrections applied (latest pass)

1. **Product name → "T-Hub"** (capital T, hyphen). Every user-visible
   "TermHub"/"Term Hub" string was replaced across hero, sections, nav/footer
   wordmarks, faux window chrome, page `<title>`/metadata, OG/Twitter tags,
   keywords, and the package description. (`tmux -L termhub` in the mock status
   bar is a real command flag, left as-is. The on-disk folder is still
   `termhub-site` and the GitHub URL keeps the old slug — see placeholders.)
2. **n8builds branding made clear.** Navbar wordmark now carries an
   "a tool by **n8builds**" sub-label; footer, hero sub-note, and final CTA all
   say "a tool by n8builds" with the brand linked to `n8builds.dev`.
3. **Positioning = personal local tool, now free + open source.** Copy leads with
   "the personal tool I built for myself, now free & open source", "download it
   from GitHub", and "Windows-only (for now)". Anti-paid-cloud angle sharpened.
   Added an "MIT / open source" hero stat, a "Fully open source" comparison row,
   and renamed the "Free, forever" pillar to "Free & open source".
4. **Real product-page STACK section.** `components/sections/Stack.tsx` rewritten:
   above the kept tech marquees there is now a dedicated, layer-by-layer
   architecture grid (data: `lib/site.ts` → `stackLayers`) covering **Tauri 2**
   (Rust + frameless WebView2), the **Rust core**, **React/TS/Tailwind**,
   **xterm.js**, the **tmux spine**, **Claude Code hooks**, the **MCP server**,
   and **SQLite**. Animations kept and extended.

## What this is

A single-page Next.js (App Router) marketing site, production-ready and
Vercel-deployable. Dark navy + cyan/blue aesthetic matching the n8builds style
(ambient glows, grid backdrops, gradient text, marquees, Framer Motion
throughout). `npm run build` passes; the page is fully static (~145 kB first load).

## Tech

- **Next.js 14.2.35** (App Router) + **TypeScript**
- **Tailwind CSS 3** (custom theme: ink palette, marquee/float/gradient keyframes, `4.5` spacing)
- **Framer Motion** for scroll reveals, the hero, and the scroll-progress bar
- **lucide-react** icons, **sharp** for image optimization
- Fonts: Inter + JetBrains Mono via `next/font`

## How to run

```bash
cd /home/natkins/n8builds/termhub-site
npm install
npm run dev      # http://localhost:3000
# or
npm run build && npm run start
```

## Page structure (`app/page.tsx`)

1. **ScrollProgress** — gradient bar tracking scroll.
2. **Navbar** — sticky, blurs on scroll; GitHub + Ko-fi CTAs.
3. **Hero** — animated grid backdrop, live "Free & open source" pill, gradient
   headline, and a **fully animated mock T-Hub cockpit** (`TerminalGrid`):
   workspace tabs, 2x2 live-typing terminals, an attention badge, status bar.
   Plus a 4-up stat strip (now incl. "MIT open source"). "Download free on
   GitHub" CTA.
4. **Features** — bento grid of standout features (tiled grid, attention queue,
   workspaces/tear-off, live theming, usage readouts, supervision tree, SQLite
   recovery, file/web preview, MCP server).
5. **Showcase** — alternating image rows using **real product screenshots**.
6. **HowItWorks** — the spawn -> supervise -> unblock -> ship loop, timeline style.
7. **Why** — "a cockpit you own, not a dashboard you rent": three pillars
   (free & open source / local / you own it) + a comparison table vs hosted cloud
   agent dashboards (incl. an open-source row).
8. **Stack** — NEW dedicated layer-by-layer architecture grid + the two tech
   marquees (Tauri, Rust, tmux, etc.).
9. **CTA** — final "Download free on GitHub" + Ko-fi call to action.
10. **Footer** — wordmark + "A tool by n8builds".

All marketing copy is written to be accurate to the product (cross-checked
against the live app).

## Screenshots used

Two **real** T-Hub screenshots are in place (no placeholder needed):

- `public/screenshots/grid.png` — the cockpit: workspace tabs + 2x2 terminal grid
  + status bar. (Hero-quality. Source: `Screenshot 2026-06-14 005854.png`.)
- `public/screenshots/supervision.png` — sidebar supervision tree / attention
  queue. (Source: `Screenshot 2026-06-14 005913.png`.)

## Placeholders the human must replace / confirm

1. **`public/screenshots/theming.png`** — MISSING. The third Showcase row renders
   a clearly-labeled placeholder frame. Needed: a shot of the **live-theming
   picker open** and/or the **file/web preview overlay**. Drop it at that path and
   it renders automatically (1440x900 assumed; adjust width/height in
   `components/sections/Showcase.tsx` if different).
2. **Ko-fi handle** — using `https://ko-fi.com/n8builds` as a placeholder
   (`lib/site.ts`). **Confirm the real handle.**
3. **GitHub repo** — `https://github.com/n8watkins/termhub` (`lib/site.ts`). Repo
   is **private today** and the URL still uses the old `termhub` slug. Make it
   public (and/or rename to a `t-hub` slug) and update the link before launch.
4. **Production URL** — `app/layout.tsx` now uses `https://t-hub.n8builds.dev`
   for metadataBase / OpenGraph. **Set the real domain.**
5. **OG / social share image** — none yet. Add a 1200x630 image at
   `public/og.png` and uncomment the prepared `openGraph.images` /
   `twitter.images` lines in `app/layout.tsx`.
6. **Project folder / package name** — still `termhub-site` on disk and in
   `package.json` `name`. Cosmetic; rename if desired (not required to ship).
7. **Optional richer screenshots** — Features/HowItWorks/Stack are icon-driven by
   design; more app shots could be added if available.

## Open questions

- Confirm Ko-fi handle (see #2).
- Want a download/installer/release section once the repo is public, or GitHub-only?
- Preferred domain for metadata (`t-hub.n8builds.dev` vs standalone)?
- Tone check on the "fleet of agents" framing — easy to dial up/down.

## Notes

- `git init`'d locally; **not** pushed and **not** deployed, per instructions.
- The live app at `/home/natkins/n8builds/tools` was **not** touched.
- `npm audit` advisories live in the Next.js 14.x framework itself (DoS /
  cache-poisoning classes relevant to self-hosted servers handling untrusted
  traffic — low risk for a static marketing page on Vercel). They only fully clear
  on Next 16 (breaking migration); pinned to the latest patched **14.2.35**.
