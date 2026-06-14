"use client";

import { motion } from "framer-motion";
import { MonitorCheck } from "lucide-react";
import Reveal from "@/components/ui/Reveal";
import { stackLayers } from "@/lib/site";
import {
  TauriIcon,
  RustIcon,
  ReactIcon,
  TypeScriptIcon,
  TailwindIcon,
  XtermIcon,
  TmuxIcon,
  SqliteIcon,
  ClaudeIcon,
  WindowsIcon,
  LinuxIcon,
} from "@/components/ui/BrandIcons";

const iconFor: Record<string, (c: string) => React.ReactNode> = {
  tauri: (c) => <TauriIcon className={c} />,
  rust: (c) => <RustIcon className={c} />,
  react: (c) => <ReactIcon className={c} />,
  tailwind: (c) => <TailwindIcon className={c} />,
  xterm: (c) => <XtermIcon className={c} />,
  tmux: (c) => <TmuxIcon className={c} />,
  sqlite: (c) => <SqliteIcon className={c} />,
  claude: (c) => <ClaudeIcon className={c} />,
};

// Clean icon row replacing the old tech marquee.
const techRow: { label: string; node: React.ReactNode }[] = [
  { label: "Tauri", node: <TauriIcon className="h-7 w-7" /> },
  { label: "Rust", node: <RustIcon className="h-7 w-7" /> },
  { label: "React", node: <ReactIcon className="h-7 w-7" /> },
  { label: "TypeScript", node: <TypeScriptIcon className="h-7 w-7" /> },
  { label: "Tailwind", node: <TailwindIcon className="h-7 w-7" /> },
  { label: "xterm.js", node: <XtermIcon className="h-7 w-7" /> },
  { label: "tmux", node: <TmuxIcon className="h-7 w-7" /> },
  { label: "SQLite", node: <SqliteIcon className="h-7 w-7" /> },
  { label: "Claude", node: <ClaudeIcon className="h-7 w-7" /> },
];

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
            A native <span className="font-semibold text-slate-200">Tauri 2</span>{" "}
            shell over a Rust PTY backend driving a real{" "}
            <span className="font-mono text-slate-300">tmux</span> spine — with a
            React / TypeScript / Tailwind cockpit, xterm.js tiles, deep Claude
            Code hooks, and its own MCP server. Open source, top to bottom.
          </p>
        </Reveal>

        {/* clean brand-icon row */}
        <Reveal className="mt-12" y={16}>
          <div className="mx-auto flex max-w-4xl flex-wrap items-center justify-center gap-3">
            {techRow.map((t, i) => (
              <motion.div
                key={t.label}
                initial={{ opacity: 0, scale: 0.9 }}
                whileInView={{ opacity: 1, scale: 1 }}
                viewport={{ once: true }}
                transition={{ duration: 0.35, delay: i * 0.04 }}
                className="group flex items-center gap-2.5 rounded-xl border border-white/[0.08] bg-white/[0.03] px-4 py-2.5 text-slate-300 transition-all hover:border-cyan-400/30 hover:bg-white/[0.06] hover:text-slate-100"
              >
                {t.node}
                <span className="text-sm font-semibold">{t.label}</span>
              </motion.div>
            ))}
          </div>
        </Reveal>

        {/* layer-by-layer architecture grid */}
        <div className="mt-12 grid grid-cols-1 gap-4 sm:grid-cols-2 lg:grid-cols-4">
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
                {iconFor[layer.icon]?.("h-5 w-5")}
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

        {/* platform note */}
        <Reveal className="mx-auto mt-10 max-w-3xl" y={16}>
          <div className="flex flex-col items-start gap-4 rounded-2xl border border-white/[0.08] bg-white/[0.025] p-6 sm:flex-row sm:items-center">
            <span className="flex h-11 w-11 shrink-0 items-center justify-center rounded-xl bg-gradient-to-br from-cyan-400/20 to-blue-600/10 text-cyan-300">
              <MonitorCheck className="h-5 w-5" />
            </span>
            <div>
              <div className="flex items-center gap-2 text-sm font-bold text-slate-100">
                <WindowsIcon className="h-4 w-4 text-cyan-300" />
                Windows + WSL-first today
                <LinuxIcon className="ml-1 h-4 w-4 text-slate-400" />
              </div>
              <p className="mt-1 text-sm leading-relaxed text-haze">
                The shipping build targets Windows + WSL, but the architecture
                isn&apos;t WSL-locked — the Rust core and tmux spine are already
                cross-platform, so native macOS / Linux are squarely on the table.
              </p>
            </div>
          </div>
        </Reveal>
      </div>
    </section>
  );
}
