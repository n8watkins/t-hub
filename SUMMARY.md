# T-Hub promo site — build summary

A modern, animation-heavy marketing site for **T-Hub** (capital T, hyphen): a
free, open-source, **100% local session-first terminal IDE for agentic coding**
— an opinionated cockpit to run and supervise MANY Claude Code / agent terminal
sessions at once, instead of seeding through a dozen project windows. See every
session and its files in one tree, click any file, jump to any session. **A tool
by n8builds** (GitHub user `n8watkins`). Windows + WSL-first build; the
architecture is **not** WSL-locked.

## Owner revisions applied (v2 pass — session-first reframe)

Copy rewritten around the session-first positioning above. Brand is **n8builds**
(NOT "Nathan Watkins"), pushed hard toward **n8builds.dev** (~11 mentions).

1. **"Why free" moved to the top** (right after the hero), rewritten tighter +
   homelier: "Use free tools. Own your tools." Three curt pillars (don't pay for
   the obvious / 100% local / own your tools). The old vs-cloud comparison table
   and the false **"works offline / behind firewall"** claim were removed.
   (`components/sections/Why.tsx`.)
2. **Features reworked** to lead with the unique differentiators and drop
   table-stakes ones. In order: session supervision tree, cross-agent attention
   queue, live usage readouts, persistent terminals you drag with zero reload,
   built-in MCP server, **configure it by just asking Claude**, SQLite snapshot
   recovery, **monitor WSL usage**, workspaces + tear-off. File-tree / web-preview
   demoted out of the headline grid. No un-built claims. A **Claude icon** sits on
   the Hooks + MCP tags. (`components/sections/Features.tsx`.)
3. **Cuts:** the "T-Hub session flow" loop diagram (`HowItWorks.tsx` deleted),
   the tech marquee (replaced with a clean brand-icon row in Stack), and the
   "one window / one cockpit" line. Hero pipeline is now
   **"Spawn. Supervise. Ship. Repeat."**
4. **Stack sells like a README** with real brand SVG icons (Tauri, Rust, React,
   TypeScript, Tailwind, xterm.js, tmux, SQLite, Claude) inlined in
   `components/ui/BrandIcons.tsx`. A platform note states it's a
   **Windows + WSL-first build but the architecture is not WSL-locked**.
   (`components/sections/Stack.tsx`.)
5. **New Roadmap section** (`components/sections/Roadmap.tsx`), README-style:
   current state + roadmap (becoming **agent-agnostic**, beyond Windows+WSL,
   yours to shape) + contributor CTA ("star it, open issues, become a
   contributor — or fork it and build it your way").
6. **Bento grid fits perfectly:** 9 cards in a 3-col grid with 3 col-span-2 cards
   = exactly 4 full rows, no overflow / empty cells. Includes the fun
   **"configure your T-Hub by just asking Claude"** card.
7. **Screenshots:** the two real shots kept (`grid.png`, `supervision.png`). The
   old MISSING `theming.png` placeholder is replaced with a **CSS/Framer animated
   "GIF-style" mock** (`components/ui/ThemeShift.tsx`) cycling live themes across
   mini terminals + a file/preview rail — no real capture required.
8. **CTAs:** "Download free on GitHub" links directly to `.../termhub/releases`;
   a **Star on GitHub** button sits beside it (hero, final CTA, navbar).
9. **Branding:** brand = **n8builds** everywhere. Footer swaps **Twitter → X**
   (inline X glyph) and adds a prominent **"n8builds.dev — more tools"** link.
10. **More icons throughout**, incl. a **Claude icon** next to "Claude Code".

## What this is

A single-page Next.js (App Router) marketing site, production-ready and
Vercel-deployable. Dark navy + cyan/blue aesthetic matching the n8builds style
(ambient glows, grid backdrops, gradient text, Framer Motion throughout).
`npm run build` passes; the page is fully static (~154 kB first load).

## Tech

- **Next.js 14.2.35** (App Router) + **TypeScript**
- **Tailwind CSS 3** (custom theme: ink palette, marquee/float/gradient keyframes, `4.5` spacing)
- **Framer Motion** for scroll reveals, the hero, and the scroll-progress bar
- **lucide-react** icons + inlined brand SVGs (`components/ui/BrandIcons.tsx`),
  **sharp** for image optimization
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

1. **ScrollProgress / Navbar / AmbientGlow** — sticky nav with Star + Download
   (→ releases) CTAs and an "a tool by n8builds" sub-label.
2. **Hero** — session-first headline ("Run many agents. Supervise them all."),
   Claude-icon pill, animated mock cockpit (`TerminalGrid`), 4-up stat strip,
   "Spawn. Supervise. Ship. Repeat." pipeline, Download + Star CTAs.
3. **Why** (moved to the top) — "Use free tools. Own your tools." 3 curt pillars.
4. **Features** — perfectly-fitting bento grid of differentiators (incl.
   "configure by asking Claude" + "monitor WSL usage").
5. **Showcase** — 2 real screenshots + 1 animated theming mock (`ThemeShift`).
6. **Stack** — brand-icon row + layer-by-layer architecture grid + platform note.
7. **Roadmap** — current state + agent-agnostic roadmap + contributor CTA.
8. **CTA** — Download (→ releases) + Star buttons.
9. **Footer** — n8builds branding, X / Ko-fi / GitHub, "n8builds.dev — more tools".

## Screenshots used

Two **real** T-Hub screenshots are in place; the third row is an animated mock:

- `public/screenshots/grid.png` — the cockpit: workspace tabs + 2x2 terminal grid
  + status bar. (Source: `Screenshot 2026-06-14 005854.png`.)
- `public/screenshots/supervision.png` — sidebar supervision tree / attention
  queue. (Source: `Screenshot 2026-06-14 005913.png`.)
- Third Showcase row — `components/ui/ThemeShift.tsx`, a CSS/Framer animated
  "GIF-style" mock cycling live themes across mini terminals + a file/preview
  rail. No real capture required (the older Screenshots folder had no clean
  live-theming / file-preview T-Hub shot — only `grid` and `supervision` qualify).

## Placeholders the human must replace / confirm

1. **GitHub repo** — `https://github.com/n8watkins/termhub` (`lib/site.ts`). Repo
   is **private today** and uses the old `termhub` slug. Make it public (and/or
   rename to a `t-hub` slug). The Download CTA points at `.../termhub/releases` —
   confirm a releases page with downloadable builds exists before launch.
2. **Ko-fi handle** — `https://ko-fi.com/n8watkins` (`lib/site.ts`). **Confirm.**
3. **X handle** — `https://x.com/n8watkins` (`lib/site.ts`). **Confirm.**
4. **OG / social share image** — none yet. Add a 1200x630 image at
   `public/og.png` and uncomment the prepared `openGraph.images` /
   `twitter.images` lines in `app/layout.tsx`. `metadataBase` currently uses the
   public alias `https://termhub-site.vercel.app`.
5. **Optional real "theming" screenshot** — the third Showcase row is an animated
   mock by design. To use a real capture, swap `ThemeShift` back to a `Screenshot`
   in `components/sections/Showcase.tsx`.
6. **Project folder / package name** — still `termhub-site` on disk / in
   `package.json`. Cosmetic.
7. **Custom domain** — currently the `termhub-site.vercel.app` alias; point a real
   domain and update `siteUrl` in `app/layout.tsx` if desired.

## Deploy

Deployed to Vercel (project `termhub-site`, org authed as natkins23) via
`vercel deploy --prod --yes` from the project dir. Public alias:
https://termhub-site.vercel.app.

## Open questions

- Confirm Ko-fi + X handles (see #2, #3).
- Make the repo public + ensure /releases has builds before launch.
- Preferred custom domain (e.g. a t-hub.n8builds.dev subdomain)?

## Notes

- The live app at `/home/natkins/n8builds/tools` and the Remotion project at
  `/home/natkins/n8builds/termhub-videos` were **not** touched.
- `npm audit` advisories live in the Next.js 14.x framework itself (DoS /
  cache-poisoning classes relevant to self-hosted servers handling untrusted
  traffic — low risk for a static marketing page on Vercel). They only fully clear
  on Next 16 (breaking migration); pinned to the latest patched **14.2.35**.
