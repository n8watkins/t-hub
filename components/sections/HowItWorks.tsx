"use client";

import { motion } from "framer-motion";
import Reveal from "@/components/ui/Reveal";

const steps = [
  {
    n: "01",
    title: "Spawn",
    body: "Open terminals into your WSL2 distro. Each tile gets its own PTY on an isolated tmux socket — your sessions, persistent and detachable.",
    code: "tmux -L termhub new -s feat/auth",
  },
  {
    n: "02",
    title: "Supervise",
    body: "Run a Claude Code agent in any tile. Hooks feed the supervision tree, the attention queue, and live context / cost / rate-limit readouts.",
    code: "● editing route.ts  · ctx 41% · $0.18",
  },
  {
    n: "03",
    title: "Unblock",
    body: "When an agent stops to ask, it jumps to the top of the attention queue. Jump in, answer, and move on to the next one — nothing gets lost.",
    code: "? Drop the legacy column? (y/n)",
  },
  {
    n: "04",
    title: "Ship",
    body: "Reorder, tear off to a second monitor, recover after a crash from SQLite. Drive the whole thing by hand — or let Claude drive it over MCP.",
    code: "✓ 3 files changed, tests green",
  },
];

export default function HowItWorks() {
  return (
    <section className="relative py-24 sm:py-32">
      <div className="container-page">
        <Reveal className="mx-auto max-w-2xl text-center">
          <p className="eyebrow">The loop</p>
          <h2 className="mt-3 text-3xl font-bold tracking-tight text-slate-50 sm:text-4xl">
            How a TermHub session flows
          </h2>
        </Reveal>

        <div className="relative mt-16">
          {/* connecting line */}
          <div
            aria-hidden
            className="absolute left-1/2 top-0 hidden h-full w-px -translate-x-1/2 bg-gradient-to-b from-transparent via-cyan-400/25 to-transparent lg:block"
          />
          <div className="flex flex-col gap-8 lg:gap-2">
            {steps.map((s, i) => (
              <motion.div
                key={s.n}
                initial={{ opacity: 0, x: i % 2 ? 30 : -30 }}
                whileInView={{ opacity: 1, x: 0 }}
                viewport={{ once: true, margin: "-80px" }}
                transition={{ duration: 0.6, ease: [0.25, 0.1, 0.25, 1] }}
                className={`flex flex-col gap-4 lg:w-1/2 ${
                  i % 2
                    ? "lg:ml-auto lg:pl-12"
                    : "lg:pr-12 lg:text-right lg:items-end"
                }`}
              >
                <div
                  className={`relative w-full max-w-md rounded-2xl border border-white/[0.08] bg-white/[0.025] p-6 ${
                    i % 2 ? "" : "lg:ml-auto"
                  }`}
                >
                  <div
                    className={`flex items-center gap-3 ${
                      i % 2 ? "" : "lg:flex-row-reverse"
                    }`}
                  >
                    <span className="font-mono text-3xl font-extrabold text-cyan-400/30">
                      {s.n}
                    </span>
                    <h3 className="text-xl font-bold text-slate-100">
                      {s.title}
                    </h3>
                  </div>
                  <p className="mt-3 text-sm leading-relaxed text-haze">
                    {s.body}
                  </p>
                  <div className="mt-4 overflow-x-auto rounded-lg border border-white/[0.06] bg-ink-900/80 px-3 py-2 text-left font-mono text-[0.72rem] text-cyan-200/90">
                    <span className="text-slate-600">› </span>
                    {s.code}
                  </div>
                </div>
              </motion.div>
            ))}
          </div>
        </div>
      </div>
    </section>
  );
}
