"use client";

import { motion } from "framer-motion";
import {
  LayoutGrid,
  Bell,
  Palette,
  GitBranch,
  DatabaseBackup,
  Gauge,
  PanelsTopLeft,
  Plug,
  FolderTree,
} from "lucide-react";
import Reveal from "@/components/ui/Reveal";

type Feature = {
  icon: React.ReactNode;
  title: string;
  body: string;
  span?: string;
  tag?: string;
};

const features: Feature[] = [
  {
    icon: <LayoutGrid className="h-5 w-5" />,
    title: "A tiled terminal grid with zero reloads",
    body: "Drag, resize, and reorder live terminals like windows. A persistent xterm pool sits over placeholder cells, so layout never tears down and your scrollback never blinks out.",
    span: "md:col-span-2 md:row-span-1",
    tag: "Signature",
  },
  {
    icon: <Bell className="h-5 w-5" />,
    title: "An attention queue",
    body: "Deep Claude Code hooks surface exactly which agents are blocked and waiting on you — no hunting across tiles for the one that asked a question.",
    tag: "Hooks",
  },
  {
    icon: <PanelsTopLeft className="h-5 w-5" />,
    title: "Workspaces + tear-off windows",
    body: "Group sessions into workspace tabs and pop any of them into its own window across monitors.",
  },
  {
    icon: <Palette className="h-5 w-5" />,
    title: "Live theming, no reload",
    body: "Recolor the whole cockpit instantly. Themes apply across every terminal in real time.",
  },
  {
    icon: <Gauge className="h-5 w-5" />,
    title: "Live context, cost & rate-limit usage",
    body: "See per-session context fill, spend, and rate-limit headroom at a glance — know when an agent is about to stall before it does.",
    span: "md:col-span-2",
    tag: "Supervision",
  },
  {
    icon: <GitBranch className="h-5 w-5" />,
    title: "Session supervision tree",
    body: "A live tree of every agent and its descendants — see the whole fleet, not just one prompt.",
  },
  {
    icon: <DatabaseBackup className="h-5 w-5" />,
    title: "SQLite-backed recovery",
    body: "Sessions survive crashes and restarts. Close a tile and the process detaches via tmux instead of dying.",
  },
  {
    icon: <FolderTree className="h-5 w-5" />,
    title: "File tree + file/web preview",
    body: "Browse the repo and pop a file or a web page into an overlay without leaving the cockpit.",
  },
  {
    icon: <Plug className="h-5 w-5" />,
    title: "An MCP server — Claude drives the app",
    body: "T-Hub ships its own MCP server, so Claude itself can spawn terminals, move tiles, switch themes, and read the supervision tree.",
    span: "md:col-span-2",
    tag: "MCP",
  },
];

export default function Features() {
  return (
    <section id="features" className="relative py-24 sm:py-32">
      <div className="container-page">
        <Reveal className="mx-auto max-w-2xl text-center">
          <p className="eyebrow">The cockpit</p>
          <h2 className="mt-3 text-3xl font-bold tracking-tight text-slate-50 sm:text-4xl">
            Everything you need to fly a fleet of agents
          </h2>
          <p className="mt-4 text-haze">
            One persistent UI built for the way real agent work actually
            happens: many sessions, long-running, all needing a human at the
            right moment.
          </p>
        </Reveal>

        <div className="mt-14 grid auto-rows-[1fr] grid-cols-1 gap-4 md:grid-cols-3">
          {features.map((f, i) => (
            <motion.article
              key={f.title}
              initial={{ opacity: 0, y: 24 }}
              whileInView={{ opacity: 1, y: 0 }}
              viewport={{ once: true, margin: "-60px" }}
              transition={{
                duration: 0.5,
                delay: (i % 3) * 0.08,
                ease: [0.25, 0.1, 0.25, 1],
              }}
              className={`group relative flex flex-col overflow-hidden rounded-2xl border border-white/[0.08] bg-white/[0.025] p-6 transition-all duration-300 hover:-translate-y-1 hover:border-cyan-400/30 hover:bg-white/[0.05] hover:shadow-glow ${
                f.span ?? ""
              }`}
            >
              {/* corner glow */}
              <div className="pointer-events-none absolute -right-12 -top-12 h-32 w-32 rounded-full bg-cyan-500/0 blur-2xl transition-all duration-500 group-hover:bg-cyan-500/20" />

              <div className="mb-4 flex items-center justify-between">
                <span className="flex h-11 w-11 items-center justify-center rounded-xl border border-white/10 bg-gradient-to-br from-white/[0.08] to-transparent text-cyan-300 transition-colors group-hover:text-cyan-200">
                  {f.icon}
                </span>
                {f.tag && (
                  <span className="rounded-full border border-cyan-400/20 bg-cyan-400/10 px-2.5 py-0.5 text-[0.6rem] font-bold uppercase tracking-wider text-cyan-300">
                    {f.tag}
                  </span>
                )}
              </div>

              <h3 className="text-lg font-bold leading-snug text-slate-100">
                {f.title}
              </h3>
              <p className="mt-2 text-sm leading-relaxed text-haze">{f.body}</p>
            </motion.article>
          ))}
        </div>
      </div>
    </section>
  );
}
