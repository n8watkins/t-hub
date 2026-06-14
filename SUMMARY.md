# TermHub promo site — build summary

A modern, animation-heavy marketing site for **TermHub**, the free, local,
terminal-first cockpit for supervising many persistent Claude Code sessions.
Brand: **n8builds** / Nathan Watkins.

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
3. **Hero** — animated grid backdrop, live "Free & local" pill, gradient
   headline, and a **fully animated mock TermHub cockpit** (`TerminalGrid`):
   workspace tabs, 2x2 live-typing terminals, an attention badge, status bar.
   Plus a 4-up stat strip.
4. **Features** — bento grid of standout features (tiled grid, attention queue,
   workspaces/tear-off, live theming, usage readouts, supervision tree, SQLite
   recovery, file/web preview, MCP server).
5. **Showcase** — alternating image rows using **real product screenshots**.
6. **HowItWorks** — the spawn -> supervise -> unblock -> ship loop, timeline style.
7. **Why** — "a cockpit you own, not a dashboard you rent": three pillars
   (free / local / you own it) + a comparison table vs hosted cloud agent dashboards.
8. **Stack** — two marquees of the underlying tech (Tauri, Rust, tmux, etc.).
9. **CTA** — final GitHub + Ko-fi call to action.
10. **Footer**.

All marketing copy is first-draft and written to be accurate to the product
(cross-checked against the TermHub README in the live app).

## Screenshots used

Two **real** TermHub screenshots were found and copied in (no placeholder needed):

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
3. **GitHub repo** — `https://github.com/n8watkins/termhub`. The repo is **private
   today**; the site notes this. Make it public (or update the link) before launch.
4. **Production URL** — `app/layout.tsx` uses `https://termhub.n8builds.dev` for
   metadataBase / OpenGraph. Set the real domain.
5. **OG / social share image** — none set yet. Add a 1200x630 image and wire it into
   `openGraph.images` / `twitter.images` in `app/layout.tsx`.
6. **Optional richer screenshots** — Features and HowItWorks are icon-driven by
   design; more app shots could be added if available.

## Open questions

- Confirm Ko-fi handle (see #2).
- Want a download/installer/release section once the repo is public, or GitHub-only?
- Preferred domain for metadata (`termhub.n8builds.dev` vs standalone)?
- Tone check on the "fleet of agents" framing — easy to dial up/down.

## Notes

- `git init`'d locally; **not** pushed and **not** deployed, per instructions.
- The live app at `/home/natkins/n8builds/tools` was **not** touched.
- `npm audit` advisories live in the Next.js 14.x framework itself (DoS /
  cache-poisoning classes relevant to self-hosted servers handling untrusted
  traffic — low risk for a static marketing page on Vercel). They only fully clear
  on Next 16 (breaking migration); pinned to the latest patched **14.2.35**.
