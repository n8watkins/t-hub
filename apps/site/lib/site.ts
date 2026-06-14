// Central place for links, copy fragments, and brand constants.
// Flagged placeholders are noted in SUMMARY.md.

export const site = {
  name: "T-Hub",
  tagline: "Run many agents. Supervise them all.",
  brand: "n8builds",
  githubUser: "n8watkins",
  description:
    "T-Hub is a free, open-source, 100% local session-first terminal IDE for agentic coding — an opinionated cockpit to run and supervise many Claude Code / agent terminal sessions at once. See every session and its files in one tree, click any file, jump to any session. By n8builds.",
  // PLACEHOLDER: repo is private today. Make public (or update link) before publishing.
  github: "https://github.com/n8watkins/termhub",
  releases: "https://github.com/n8watkins/termhub/releases",
  // PLACEHOLDER: confirm the real Ko-fi handle.
  kofi: "https://ko-fi.com/n8watkins",
  x: "https://x.com/n8watkins",
  builderSite: "https://n8builds.dev",
};

export const stats = [
  { value: "Many", label: "sessions", sub: "one cockpit, not a dozen windows" },
  { value: "100%", label: "local", sub: "your machine, your data" },
  { value: "$0", label: "forever", sub: "free, no account" },
  { value: "MIT", label: "open source", sub: "read it, fork it, own it" },
];

// The dedicated product "stack" section — the real architecture, layer by layer.
// `icon` keys into the brand-icon map in components/sections/Stack.tsx.
export const stackLayers: {
  name: string;
  icon: string;
  role: string;
  detail: string;
}[] = [
  {
    name: "Tauri 2",
    icon: "tauri",
    role: "Frameless native shell",
    detail:
      "A Rust-backed Tauri 2 app in a frameless WebView — native window, custom chrome, web speed, a fraction of an Electron footprint.",
  },
  {
    name: "Rust core",
    icon: "rust",
    role: "PTY + process backend",
    detail:
      "The Rust side owns the PTYs, spawns processes, and brokers typed IPC to the UI. The heavy, persistent work lives in native code.",
  },
  {
    name: "React + TypeScript",
    icon: "react",
    role: "The cockpit UI",
    detail:
      "The whole interface is React + TypeScript — typed end to end, from the IPC payloads down to the components you see.",
  },
  {
    name: "Tailwind",
    icon: "tailwind",
    role: "The design system",
    detail:
      "A Tailwind design system keeps the cockpit consistent and themeable — every color recolors live, with no reload.",
  },
  {
    name: "xterm.js",
    icon: "xterm",
    role: "The terminal tiles",
    detail:
      "Every tile is a real xterm.js terminal from a persistent pool positioned over the grid — drag and reorder without tearing down scrollback.",
  },
  {
    name: "tmux",
    icon: "tmux",
    role: "Persistent session spine",
    detail:
      "Each session lives on its own tmux socket, so terminals truly persist and detach instead of dying. Close a tile, the process keeps running.",
  },
  {
    name: "SQLite",
    icon: "sqlite",
    role: "Crash-safe recovery",
    detail:
      "Sessions, layouts, and themes snapshot to local SQLite, so the whole cockpit survives crashes and restarts. State on your disk, not a server.",
  },
  {
    name: "Claude Code",
    icon: "claude",
    role: "Hooks + MCP integration",
    detail:
      "Deep Claude Code hooks feed the supervision tree, the attention queue, and usage readouts. T-Hub also ships an MCP server so Claude can drive the app.",
  },
];

// README-style roadmap.
export const roadmapNow = [
  "Run and supervise many Claude Code sessions side by side",
  "Session supervision tree with a cross-agent attention queue",
  "Live context, cost & rate-limit usage per session",
  "Persistent terminals you drag with zero reload",
  "SQLite snapshot recovery + workspace tabs and tear-off windows",
  "An MCP server, so Claude can configure the cockpit for you",
];

export const roadmapNext = [
  {
    title: "Agent-agnostic",
    body: "Today it's tuned for Claude Code. Next: a clean adapter layer so any terminal agent — not just Claude — plugs into the supervision tree and attention queue.",
  },
  {
    title: "Beyond Windows + WSL",
    body: "The architecture isn't WSL-locked. The build ships Windows + WSL-first today; native macOS / Linux are on the table as the tmux + Rust spine is already cross-platform.",
  },
  {
    title: "Yours to shape",
    body: "It's open source and opinionated, not finished. Star it, open issues, become a contributor — or fork it and build it your way.",
  },
];
