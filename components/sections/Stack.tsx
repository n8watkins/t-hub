"use client";

import { motion } from "framer-motion";
import {
  AppWindow,
  Cog,
  Code2,
  SquareTerminal,
  Layers,
  Webhook,
  Plug,
  Database,
} from "lucide-react";
import Reveal from "@/components/ui/Reveal";
import Marquee from "@/components/ui/Marquee";
import { techStack, stackLayers } from "@/lib/site";

const notes: Record<string, string> = {
  "Tauri 2": "frameless native shell",
  Rust: "PTY + tmux backend",
  WebView2: "fast web UI on Windows",
  React: "the cockpit UI",
  TypeScript: "end-to-end typed IPC",
  Tailwind: "the design system",
  "xterm.js": "the terminal tiles",
  tmux: "the persistent spine",
  SQLite: "session recovery",
  WSL2: "where your shells live",
  "Claude Code": "the agents it supervises",
  MCP: "so Claude can drive it",
};

// Icons paired by layer index to the stackLayers data in lib/site.ts.
const layerIcons = [
  <AppWindow key="tauri" className="h-5 w-5" />,
  <Cog key="rust" className="h-5 w-5" />,
  <Code2 key="ui" className="h-5 w-5" />,
  <SquareTerminal key="xterm" className="h-5 w-5" />,
  <Layers key="tmux" className="h-5 w-5" />,
  <Webhook key="hooks" className="h-5 w-5" />,
  <Plug key="mcp" className="h-5 w-5" />,
  <Database key="sqlite" className="h-5 w-5" />,
];

function Chip({ name }: { name: string }) {
  return (
    <div className="group/chip flex h-24 min-w-[11rem] flex-col items-center justify-center gap-1 rounded-xl border border-white/[0.08] bg-white/[0.03] px-5 transition-all duration-300 hover:border-cyan-400/30 hover:bg-white/[0.07]">
      <span className="text-base font-semibold text-slate-100">{name}</span>
      <span className="text-center text-xs text-slate-500 transition-colors group-hover/chip:text-cyan-300/80">
        {notes[name]}
      </span>
    </div>
  );
}

export default function Stack() {
  return (
    <section id="stack" className="relative py-24 sm:py-32">
      <div className="container-page">
        <Reveal className="mx-auto max-w-2xl text-center">
          <p className="eyebrow">The stack</p>
          <h2 className="mt-3 text-3xl font-bold tracking-tight text-slate-50 sm:text-4xl">
            What T-Hub is actually built on
          </h2>
          <p className="mt-4 text-haze">
            A native{" "}
            <span className="font-semibold text-slate-200">Tauri 2</span> shell
            (Rust + a frameless WebView2 window) over a Rust PTY backend driving
            a real <span className="font-mono text-slate-300">tmux</span> spine —
            with a React/TypeScript/Tailwind cockpit, xterm.js tiles, deep Claude
            Code hooks, and its own MCP server. Open source, top to bottom.
          </p>
        </Reveal>

        {/* layer-by-layer architecture grid */}
        <div className="mt-14 grid grid-cols-1 gap-4 sm:grid-cols-2 lg:grid-cols-4">
          {stackLayers.map((layer, i) => (
            <motion.div
              key={layer.name}
              initial={{ opacity: 0, y: 24 }}
              whileInView={{ opacity: 1, y: 0 }}
              viewport={{ once: true, margin: "-60px" }}
              transition={{
                duration: 0.5,
                delay: (i % 4) * 0.08,
                ease: [0.25, 0.1, 0.25, 1],
              }}
              className="group relative flex flex-col overflow-hidden rounded-2xl border border-white/[0.08] bg-white/[0.025] p-5 transition-all duration-300 hover:-translate-y-1 hover:border-cyan-400/30 hover:bg-white/[0.05] hover:shadow-glow"
            >
              <div className="pointer-events-none absolute -right-10 -top-10 h-28 w-28 rounded-full bg-cyan-500/0 blur-2xl transition-all duration-500 group-hover:bg-cyan-500/20" />
              <span className="mb-4 flex h-11 w-11 items-center justify-center rounded-xl border border-white/10 bg-gradient-to-br from-white/[0.08] to-transparent text-cyan-300 transition-colors group-hover:text-cyan-200">
                {layerIcons[i]}
              </span>
              <h3 className="text-base font-bold leading-snug text-slate-100">
                {layer.name}
              </h3>
              <p className="mt-1 text-[0.7rem] font-semibold uppercase tracking-wider text-cyan-300/70">
                {layer.role}
              </p>
              <p className="mt-3 text-sm leading-relaxed text-haze">
                {layer.detail}
              </p>
            </motion.div>
          ))}
        </div>
      </div>

      <Reveal className="relative mt-16" y={16}>
        <div className="relative flex flex-col gap-4 overflow-hidden rounded-3xl border border-white/[0.06] bg-gradient-to-br from-ink-700 via-ink-600 to-ink-700 py-8">
          <Marquee duration="48s">
            {techStack.slice(0, 6).map((t) => (
              <Chip key={t} name={t} />
            ))}
          </Marquee>
          <Marquee reverse duration="48s">
            {techStack.slice(6).map((t) => (
              <Chip key={t} name={t} />
            ))}
          </Marquee>
          <div className="pointer-events-none absolute inset-y-0 left-0 w-1/5 bg-gradient-to-r from-ink-700 to-transparent" />
          <div className="pointer-events-none absolute inset-y-0 right-0 w-1/5 bg-gradient-to-l from-ink-700 to-transparent" />
        </div>
      </Reveal>
    </section>
  );
}
