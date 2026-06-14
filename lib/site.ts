// Central place for links, copy fragments, and brand constants.
// Flagged placeholders are noted in SUMMARY.md.

export const site = {
  name: "TermHub",
  tagline: "Run a hundred agents. Lose none of them.",
  brand: "n8builds",
  author: "Nathan Watkins",
  description:
    "A free, local, terminal-first cockpit for running and supervising many persistent Claude Code sessions at once. Tiled terminals, workspace tabs, live theming, deep hooks integration — you own all of it.",
  // PLACEHOLDER: repo is private today. Confirm before publishing.
  github: "https://github.com/n8watkins/termhub",
  // PLACEHOLDER: confirm the real Ko-fi handle.
  kofi: "https://ko-fi.com/n8builds",
  twitter: "https://x.com/n8watkins",
  builderSite: "https://n8builds.dev",
};

export const stats = [
  { value: "1", label: "window", sub: "every agent, one cockpit" },
  { value: "0", label: "reloads", sub: "drag, resize, reorder live" },
  { value: "100%", label: "local", sub: "your machine, your data" },
  { value: "$0", label: "forever", sub: "free, no account" },
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
