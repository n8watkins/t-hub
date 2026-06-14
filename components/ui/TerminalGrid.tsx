"use client";

import { useEffect, useState } from "react";
import { motion } from "framer-motion";

// A self-contained, animated mock of the T-Hub cockpit:
// a 2x2 tiled grid of "terminals" that type Claude-Code-style output,
// with workspace tabs, attention badges, and a status bar.
// Pure presentational eye-candy for the hero — no real terminals.

type Tile = {
  id: string;
  label: string;
  lines: string[];
  accent: string;
  attention?: boolean;
};

const TILES: Tile[] = [
  {
    id: "6440d820",
    label: "feat/auth",
    accent: "#22d3ee",
    lines: [
      "$ claude",
      "● Wiring up the OAuth callback...",
      "  Editing app/api/auth/route.ts",
      "✓ 3 files changed, tests green",
    ],
  },
  {
    id: "364f9987",
    label: "refactor/db",
    accent: "#60a5fa",
    attention: true,
    lines: [
      "$ claude",
      "● Migrating schema to Drizzle...",
      "? Drop the legacy column? (y/n)",
    ],
  },
  {
    id: "883cd3ce",
    label: "docs/site",
    accent: "#2dd4bf",
    lines: [
      "$ claude",
      "● Drafting the landing copy...",
      "  Rewrote hero + features",
      "✓ committed: punchier headline",
    ],
  },
  {
    id: "7470da50",
    label: "fix/race",
    accent: "#818cf8",
    lines: [
      "$ claude",
      "● Tracing the startup race...",
      "  Found it: pool init ordering",
      "✓ patched, re-running suite",
    ],
  },
];

function useTypedLines(lines: string[], speed = 28, start = 0) {
  const [text, setText] = useState("");
  useEffect(() => {
    const full = lines.join("\n");
    let i = 0;
    let timer: ReturnType<typeof setTimeout>;
    const startTimer = setTimeout(function tick() {
      setText(full.slice(0, i));
      i += 1;
      if (i <= full.length) {
        timer = setTimeout(tick, speed + (full[i] === "\n" ? 180 : 0));
      }
    }, start);
    return () => {
      clearTimeout(startTimer);
      clearTimeout(timer);
    };
  }, [lines, speed, start]);
  return text;
}

function colorize(line: string, accent: string) {
  if (line.startsWith("$")) return "text-slate-500";
  if (line.startsWith("●")) return "text-cyan-300";
  if (line.startsWith("✓")) return "text-emerald-400";
  if (line.startsWith("?")) return "text-amber-300";
  return "text-slate-300";
}

function Tile({ tile, index }: { tile: Tile; index: number }) {
  const typed = useTypedLines(tile.lines, 26, 400 + index * 650);
  const renderLines = typed.split("\n");
  return (
    <motion.div
      initial={{ opacity: 0, scale: 0.96 }}
      animate={{ opacity: 1, scale: 1 }}
      transition={{ duration: 0.5, delay: 0.15 * index, ease: [0.25, 0.1, 0.25, 1] }}
      className="relative flex flex-col overflow-hidden rounded-lg border border-white/[0.07] bg-ink-900/80"
      style={
        tile.attention
          ? { boxShadow: "0 0 0 1px rgba(251,191,36,0.35) inset" }
          : undefined
      }
    >
      <div className="flex items-center justify-between border-b border-white/[0.06] px-2.5 py-1.5">
        <div className="flex items-center gap-1.5">
          <span
            className="h-1.5 w-1.5 rounded-full"
            style={{ background: tile.accent }}
          />
          <span className="font-mono text-[0.62rem] text-slate-400">
            {tile.id} · {tile.label}
          </span>
        </div>
        {tile.attention ? (
          <span className="flex items-center gap-1 rounded-full bg-amber-400/15 px-1.5 py-0.5 text-[0.55rem] font-bold uppercase tracking-wide text-amber-300">
            <span className="h-1 w-1 animate-pulse rounded-full bg-amber-400" />
            needs you
          </span>
        ) : (
          <span className="font-mono text-[0.55rem] text-slate-600">running</span>
        )}
      </div>
      <div className="flex-1 px-2.5 py-2 font-mono text-[0.62rem] leading-relaxed">
        {renderLines.map((ln, i) => (
          <div key={i} className={colorize(ln, tile.accent)}>
            {ln || " "}
            {i === renderLines.length - 1 && (
              <span className="ml-0.5 inline-block h-2.5 w-[5px] translate-y-0.5 animate-blink bg-cyan-300" />
            )}
          </div>
        ))}
      </div>
    </motion.div>
  );
}

export default function TerminalGrid() {
  return (
    <div className="relative overflow-hidden rounded-2xl border border-white/10 bg-ink-800/70 shadow-2xl shadow-black/60 backdrop-blur-sm">
      {/* window title bar with workspace tabs */}
      <div className="flex items-center gap-1 border-b border-white/[0.07] bg-white/[0.02] px-3 py-2">
        <div className="mr-2 flex gap-1.5">
          <span className="h-2.5 w-2.5 rounded-full bg-[#ff5f57]" />
          <span className="h-2.5 w-2.5 rounded-full bg-[#febc2e]" />
          <span className="h-2.5 w-2.5 rounded-full bg-[#28c840]" />
        </div>
        {[
          { name: "Workspace 5", n: 4, active: true },
          { name: "Workspace 1", n: 1 },
          { name: "Workspace 2", n: 6 },
        ].map((w) => (
          <div
            key={w.name}
            className={`flex items-center gap-1.5 rounded-md px-2.5 py-1 text-[0.65rem] font-medium ${
              w.active
                ? "bg-white/[0.07] text-slate-100"
                : "text-slate-500"
            }`}
          >
            <span
              className={`h-1.5 w-1.5 rounded-full ${
                w.active ? "bg-cyan-400" : "bg-slate-600"
              }`}
            />
            {w.name}
            <span
              className={`rounded-full px-1.5 text-[0.55rem] font-bold ${
                w.n > 1
                  ? "bg-cyan-400/15 text-cyan-300"
                  : "bg-white/10 text-slate-400"
              }`}
            >
              {w.n}
            </span>
          </div>
        ))}
        <span className="ml-1 text-slate-600">+</span>
      </div>

      {/* terminal grid */}
      <div className="grid grid-cols-2 gap-2 p-2.5">
        {TILES.map((t, i) => (
          <Tile key={t.id} tile={t} index={i} />
        ))}
      </div>

      {/* status bar */}
      <div className="flex items-center justify-between border-t border-white/[0.07] bg-white/[0.02] px-3 py-2 font-mono text-[0.6rem] text-slate-500">
        <span className="flex items-center gap-1.5">
          <span className="h-1.5 w-1.5 rounded-full bg-emerald-400" />
          wsl: healthy · tmux -L termhub
        </span>
        <span className="flex items-center gap-3">
          <span>ctx 41%</span>
          <span>$0.18</span>
          <span className="text-amber-300">1 needs input</span>
        </span>
      </div>
    </div>
  );
}
