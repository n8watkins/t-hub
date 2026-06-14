"use client";

import { motion } from "framer-motion";
import {
  Bell,
  GitBranch,
  DatabaseBackup,
  Gauge,
  PanelsTopLeft,
  Plug,
  Move,
  Activity,
  Sparkles,
} from "lucide-react";
import Reveal from "@/components/ui/Reveal";
import { ClaudeIcon } from "@/components/ui/BrandIcons";

type Feature = {
  icon: React.ReactNode;
  title: string;
  body: string;
  span?: string;
  tag?: string;
};

// Lead with the unique differentiators. Spans are tuned so the 3-column bento
// grid fills exactly — every row sums to 3 columns, no empty cells, no overflow.
const features: Feature[] = [
  {
    icon: <GitBranch className="h-5 w-5" />,
    title: "Session supervision tree",
    body: "A live tree of every agent and its child sessions — supervise the whole fleet from one place instead of one prompt at a time.",
    span: "md:col-span-2",
    tag: "Signature",
  },
  {
    icon: <Bell className="h-5 w-5" />,
    title: "Cross-agent attention queue",
    body: "The one agent that's blocked and waiting on you jumps to the top — no hunting across tiles for who asked a question.",
    tag: "Hooks",
  },
  {
    icon: <Gauge className="h-5 w-5" />,
    title: "Live usage readouts",
    body: "Per-session context fill, spend, and rate-limit headroom at a glance — know which agent is about to stall before it does.",
    tag: "Quality of life",
  },
  {
    icon: <Move className="h-5 w-5" />,
    title: "Persistent terminals you drag with zero reload",
    body: "Tiles come from a real xterm pool on a tmux spine. Drag, resize, and reorder live sessions — scrollback never blinks out, processes never restart.",
    span: "md:col-span-2",
  },
  {
    icon: <Plug className="h-5 w-5" />,
    title: "Built-in MCP server",
    body: "T-Hub ships its own MCP server, so Claude can spawn terminals, move tiles, switch themes, and read the supervision tree.",
    tag: "MCP",
  },
  {
    icon: <Sparkles className="h-5 w-5" />,
    title: "Configure it by just asking Claude",
    body: "“Spin up three sessions and theme them blue.” Because Claude drives the app over MCP, you set up your cockpit in plain English.",
  },
  {
    icon: <DatabaseBackup className="h-5 w-5" />,
    title: "SQLite snapshot recovery",
    body: "Sessions, layouts, and themes snapshot to SQLite and survive crashes and restarts. Close a tile and the process detaches via tmux instead of dying.",
  },
  {
    icon: <Activity className="h-5 w-5" />,
    title: "Monitor WSL usage",
    body: "WSL health, memory, and disk live right in the status bar, so you catch a thrashing distro before it takes your agents down.",
  },
  {
    icon: <PanelsTopLeft className="h-5 w-5" />,
    title: "Workspaces + tear-off windows",
    body: "Group sessions into workspace tabs and pop any of them into its own window across monitors — it supports as many windows as you want.",
    span: "md:col-span-2",
  },
];

export default function Features() {
  return (
    <section id="features" className="relative py-24 sm:py-32">
      <div className="container-page">
        <Reveal className="mx-auto max-w-2xl text-center">
          <p className="eyebrow">The cockpit</p>
          <h2 className="mt-3 text-3xl font-bold tracking-tight text-slate-50 sm:text-4xl">
            Built for supervising many agents at once
          </h2>
          <p className="mt-4 text-haze">
            One persistent UI for the way real agent work actually happens: many
            sessions, long-running, all needing a human at the right moment.
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
                  <span className="flex items-center gap-1 rounded-full border border-cyan-400/20 bg-cyan-400/10 px-2.5 py-0.5 text-[0.6rem] font-bold uppercase tracking-wider text-cyan-300">
                    {(f.tag === "Hooks" || f.tag === "MCP") && (
                      <ClaudeIcon className="h-3 w-3" />
                    )}
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
