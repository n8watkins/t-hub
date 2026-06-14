// Central place for links, copy fragments, and brand constants.
// Flagged placeholders are noted in SUMMARY.md.

export const site = {
  name: "T-Hub",
  tagline: "Run a hundred agents. Lose none of them.",
  brand: "n8builds",
  author: "Nathan Watkins",
  githubUser: "n8watkins",
  description:
    "T-Hub is a free, open-source, local terminal cockpit for running and supervising many persistent Claude Code sessions at once. Tiled terminals, workspace tabs, live theming, deep hooks integration — download it from GitHub and own all of it. Windows-only for now.",
  // PLACEHOLDER: repo is private today. Make public (or update link) before publishing.
  github: "https://github.com/n8watkins/termhub",
  // PLACEHOLDER: confirm the real Ko-fi handle.
  kofi: "https://ko-fi.com/n8builds",
  twitter: "https://x.com/n8watkins",
  builderSite: "https://n8builds.dev",
};

export const stats = [
  { value: "1", label: "window", sub: "every agent, one cockpit" },
  { value: "100%", label: "local", sub: "your machine, your data" },
  { value: "$0", label: "forever", sub: "free, no account" },
  { value: "MIT", label: "open source", sub: "read it, fork it, own it" },
];

export const techStack = [
  "Tauri 2",
  "Rust",
  "WebView2",
  "React",
  "TypeScript",
  "Tailwind",
  "xterm.js",
  "tmux",
  "SQLite",
  "WSL2",
  "Claude Code",
  "MCP",
];

// The dedicated product "stack" section — the real architecture, layer by layer.
export const stackLayers: {
  name: string;
  role: string;
  detail: string;
}[] = [
  {
    name: "Tauri 2",
    role: "Frameless native shell",
    detail:
      "A Rust-backed Tauri 2 app rendering a frameless WebView2 window — native window, custom chrome, web speed. Tiny footprint compared to an Electron build.",
  },
  {
    name: "Rust core",
    role: "PTY + process backend",
    detail:
      "The Rust side owns the PTYs, spawns processes, and brokers IPC to the UI over a typed bridge. The heavy, persistent work lives in native code.",
  },
  {
    name: "React · TypeScript · Tailwind",
    role: "The cockpit UI",
    detail:
      "The whole interface is React + TypeScript with a Tailwind design system — typed end to end, from the IPC payloads to the components you see.",
  },
  {
    name: "xterm.js",
    role: "The terminal tiles",
    detail:
      "Every tile is a real xterm.js terminal from a persistent pool positioned over the grid, so dragging and reordering never tears down your scrollback.",
  },
  {
    name: "tmux spine",
    role: "Genuinely persistent sessions",
    detail:
      "Each session lives on an isolated tmux socket, so terminals truly persist and detach instead of dying. Close a tile, the process keeps running.",
  },
  {
    name: "Claude Code hooks",
    role: "Deep supervision integration",
    detail:
      "T-Hub installs deep Claude Code hooks that feed the attention queue, the supervision tree, and live context / cost / rate-limit readouts per session.",
  },
  {
    name: "MCP server",
    role: "Claude can drive the app",
    detail:
      "T-Hub ships its own MCP server, so Claude itself can spawn terminals, move tiles, switch themes, and read the supervision tree — the cockpit drives back.",
  },
  {
    name: "SQLite",
    role: "Crash-safe recovery",
    detail:
      "Sessions, layouts, and themes are stored locally in SQLite, so the whole cockpit survives crashes and restarts. Your state is on your disk, not a server.",
  },
];
