# T-Hub — Feature Backlog

Requested features not yet built, captured from working sessions. Each item lists
what it is, the approach, a rough size, and open questions. Parked/abandoned items
live in [BACKBURNER.md](./BACKBURNER.md); the phased architecture plan is in
[PLAN.md](./PLAN.md).

**Size key:** **S** = a few edits in one area · **M** = multi-file, one subsystem ·
**L** = new subsystem / cross-cutting / needs a research spike first.

---

## A. Client identity — Claude vs Codex (icons + detection)

Underpins group B's usage split. The app must know which agent a tile is running
(claude / codex / plain shell). This is the "adapter-based core" the README
promised ("so other terminal agents can be added later").

- **A1 · Replace the "Claude" text with the Claude icon** in the tile header. **S.**
  The real `ClaudeIcon` already exists (`src/components/ClaudeIcon.tsx`) — drop the
  word, keep just the icon (tooltip "Claude").
- **A2 · Codex icon asset.** **S.** The blue OpenAI Codex glyph is provided at
  `/mnt/c/Users/natha/Downloads/codex-img-removebg-preview.png` — copy into the app
  (`src/assets/` or a `CodexIcon.tsx`) and show it for Codex tiles.
- **A3 · Detect the client per tile** (claude / codex / shell). **M.** Required for
  A1/A2 and all of B to show the right icon/usage. Infer from the spawn command
  (the tile knows what it launched) and/or a per-client marker. The architectural
  hook for multi-agent support.

## B. Usage & WSL strips — compact + dual-provider

- **B1 · Compact collapsed WSL.** **S–M.** Collapsing the WSL strip currently shows
  nothing useful; keep a one-line summary (e.g. RAM, maybe CPU%) in the collapsed
  bar. (`Sidebar.tsx` `BottomStatus` / `WslHealth.tsx`.)
- **B2 · Collapsed Usage stats.** **S–M.** When Usage is collapsed it should still
  show basic numbers in the bar, not nothing. (`Sidebar.tsx` `UsageSection` /
  `UsageStrip.tsx`.)
- **B3 · Codex usage tracking + Claude/Codex split.** **L — research spike first.**
  Track Codex usage distinctly from Claude and show both with a clear visual
  distinction (icons from A). Open question: how does the Codex CLI expose
  usage/limits? Claude's arrives via the statusline hook TermHub installs; Codex
  almost certainly differs (its own command / output / API). Spike before design.

## C. Drag & paste into the terminal

- **C1 · Drag a file / folder / image onto a tile → insert its path** as terminal
  input. **M.** Tauri webview file-drop → write the path to the PTY. Translate
  `C:\…` → `/mnt/c/…`, quote spaces, support multiple drops.
- **C2 · Paste an image into the terminal** (like Claude Code's native image paste).
  **M–L.** On an image-bearing clipboard paste, save it to a temp file and insert
  the path (or speak Claude's paste protocol). Needs clipboard-image access +
  temp-file handling; normal text paste already works.

## D. Workspace / sidebar interactions

- **D1 · Workspace color cascades to ALL tiles in the workspace.** **S–M (bug-ish).**
  Today setting a workspace color does not recolor the tiles inside it (only the
  focus ring, and not reliably while you're in the workspace). Make the tile
  chrome (header/border/accent) derive from the workspace color and update live.
- **D2 · Drag a sidebar terminal row into another workspace.** **M.** Make the
  `WorkspacesList` terminal rows drag sources; dropping on another workspace row
  reuses the existing `moveTileToTab`.

## E. Resilience — auto-resume on rate-limit

- **E1 · Auto-restart when out of credits.** **L.** A per-terminal toggle that
  detects the agent's "limit reached — resets at <time>" output and auto-resumes
  when that time passes. Pieces: (1) scan PTY output for the rate-limit pattern
  (reuse the existing output-scan path that already detects localhost URLs);
  (2) capture the EXACT text for Claude (and Codex); (3) a per-tile scheduler/timer
  keyed to the reset time; (4) the toggle UI; (5) optional manual time entry as a
  fallback. Provider-aware via A3.

---

## Carried-over follow-ups (earlier sessions)

- **PR status on the git chip** — `gh pr list --head <branch>` → open/merged badge
  next to the branch chip. **M.**
- **Status off the durable journal** — route ephemeral statusline snapshots through
  a non-cursor-advancing channel so the journal never regrows (startup compaction
  is the current stopgap). **L.**
- **Detached, EDITABLE Files window** — the last open item from the older UI batch.
  **M.**
- **Page-up copy-mode polish** — tune if the feel is off (pending a live test). **S.**
