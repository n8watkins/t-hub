"use client";

import { useEffect, useState } from "react";
import { motion, AnimatePresence } from "framer-motion";

// A GIF-style animated mock of T-Hub's live theming + a file/preview rail.
// No real capture needed — it cycles themes on a timer with a framed window
// chrome, four mini terminals, and a file rail that "previews" a file.

const THEMES = [
  { name: "Cyan", accent: "#22d3ee", soft: "rgba(34,211,238,0.14)" },
  { name: "Violet", accent: "#a78bfa", soft: "rgba(167,139,250,0.16)" },
  { name: "Emerald", accent: "#34d399", soft: "rgba(52,211,153,0.16)" },
  { name: "Amber", accent: "#fbbf24", soft: "rgba(251,191,36,0.16)" },
];

const FILES = [
  "app/page.tsx",
  "route.ts",
  "schema.sql",
  "Hero.tsx",
  "site.ts",
];

const TILES = ["feat/auth", "refactor/db", "docs/site", "fix/race"];

export default function ThemeShift() {
  const [t, setT] = useState(0);
  useEffect(() => {
    const id = setInterval(() => setT((p) => (p + 1) % THEMES.length), 2200);
    return () => clearInterval(id);
  }, []);
  const theme = THEMES[t];

  return (
    <motion.div
      initial={{ opacity: 0, y: 30, rotateX: 8 }}
      whileInView={{ opacity: 1, y: 0, rotateX: 0 }}
      viewport={{ once: true, margin: "-60px" }}
      transition={{ duration: 0.8, ease: [0.25, 0.1, 0.25, 1] }}
      className="group relative overflow-hidden rounded-xl border border-white/10 bg-ink-700/60 shadow-2xl shadow-black/50"
    >
      {/* window chrome */}
      <div className="flex items-center gap-1.5 border-b border-white/[0.06] bg-white/[0.03] px-3.5 py-2.5">
        <span className="h-2.5 w-2.5 rounded-full bg-[#ff5f57]" />
        <span className="h-2.5 w-2.5 rounded-full bg-[#febc2e]" />
        <span className="h-2.5 w-2.5 rounded-full bg-[#28c840]" />
        <span className="ml-3 font-mono text-[0.68rem] text-slate-500">T-Hub</span>
        <div className="ml-auto flex items-center gap-1.5">
          <AnimatePresence mode="wait">
            <motion.span
              key={theme.name}
              initial={{ opacity: 0, y: -4 }}
              animate={{ opacity: 1, y: 0 }}
              exit={{ opacity: 0, y: 4 }}
              transition={{ duration: 0.25 }}
              className="flex items-center gap-1.5 rounded-full px-2 py-0.5 text-[0.6rem] font-bold uppercase tracking-wider"
              style={{ background: theme.soft, color: theme.accent }}
            >
              <span
                className="h-1.5 w-1.5 rounded-full"
                style={{ background: theme.accent }}
              />
              {theme.name}
            </motion.span>
          </AnimatePresence>
        </div>
      </div>

      <div className="flex" style={{ aspectRatio: "1440 / 900" }}>
        {/* file rail */}
        <div className="hidden w-40 shrink-0 flex-col gap-0.5 border-r border-white/[0.06] bg-ink-900/40 p-2 sm:flex">
          <div className="mb-1 px-1.5 font-mono text-[0.55rem] uppercase tracking-wider text-slate-600">
            files
          </div>
          {FILES.map((f, i) => {
            const active = i === t % FILES.length;
            return (
              <div
                key={f}
                className="flex items-center gap-1.5 rounded-md px-1.5 py-1 font-mono text-[0.6rem] transition-colors"
                style={
                  active
                    ? { background: theme.soft, color: theme.accent }
                    : { color: "#64748b" }
                }
              >
                <span
                  className="h-1 w-1 rounded-full"
                  style={{ background: active ? theme.accent : "#475569" }}
                />
                {f}
              </div>
            );
          })}
          {/* preview chip */}
          <div className="mt-auto rounded-md border border-white/[0.06] bg-ink-900/60 p-2">
            <div className="font-mono text-[0.5rem] uppercase tracking-wider text-slate-600">
              preview
            </div>
            <div className="mt-1 space-y-1">
              {[0.9, 0.6, 0.75].map((w, i) => (
                <div
                  key={i}
                  className="h-1 rounded-full"
                  style={{
                    width: `${w * 100}%`,
                    background: i === 0 ? theme.accent : "#1f2c45",
                  }}
                />
              ))}
            </div>
          </div>
        </div>

        {/* terminal grid */}
        <div className="grid flex-1 grid-cols-2 gap-1.5 p-2">
          {TILES.map((label, i) => (
            <div
              key={label}
              className="relative flex flex-col overflow-hidden rounded-lg border bg-ink-900/80 transition-colors duration-500"
              style={{ borderColor: i === 1 ? theme.accent : "rgba(255,255,255,0.07)" }}
            >
              <div className="flex items-center gap-1.5 border-b border-white/[0.06] px-2 py-1">
                <span
                  className="h-1.5 w-1.5 rounded-full transition-colors duration-500"
                  style={{ background: theme.accent }}
                />
                <span className="font-mono text-[0.55rem] text-slate-400">
                  {label}
                </span>
              </div>
              <div className="flex-1 space-y-1 p-2">
                {[0.85, 0.55, 0.7, 0.4].map((w, j) => (
                  <div
                    key={j}
                    className="h-1 rounded-full transition-colors duration-500"
                    style={{
                      width: `${w * 100}%`,
                      background: j === 0 ? theme.accent : "#1b2840",
                      opacity: j === 0 ? 0.9 : 1,
                    }}
                  />
                ))}
                <span
                  className="mt-1 inline-block h-2 w-[5px] animate-blink"
                  style={{ background: theme.accent }}
                />
              </div>
            </div>
          ))}
        </div>
      </div>

      {/* status bar */}
      <div className="flex items-center justify-between border-t border-white/[0.07] bg-white/[0.02] px-3 py-1.5 font-mono text-[0.55rem] text-slate-500">
        <span className="flex items-center gap-1.5">
          <span className="h-1.5 w-1.5 rounded-full bg-emerald-400" />
          theme: {theme.name.toLowerCase()} · applied live, no reload
        </span>
        <span style={{ color: theme.accent }}>4 sessions</span>
      </div>

      {/* sheen */}
      <div className="pointer-events-none absolute inset-0 -translate-x-full bg-gradient-to-r from-transparent via-white/[0.07] to-transparent transition-transform duration-700 group-hover:translate-x-full" />
    </motion.div>
  );
}
