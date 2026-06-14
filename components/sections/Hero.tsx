"use client";

import { motion } from "framer-motion";
import { Github, Heart, ArrowRight, Cpu } from "lucide-react";
import { site, stats } from "@/lib/site";
import TerminalGrid from "@/components/ui/TerminalGrid";

const ease = [0.25, 0.1, 0.25, 1] as const;

export default function Hero() {
  return (
    <section
      id="home"
      className="relative flex min-h-screen flex-col items-center justify-center overflow-hidden pt-28 pb-16"
    >
      {/* animated grid backdrop */}
      <div aria-hidden className="pointer-events-none absolute inset-0 -z-0">
        <div className="absolute inset-0 bg-grid [mask-image:radial-gradient(ellipse_70%_60%_at_50%_30%,black_20%,transparent_75%)]" />
        <div className="absolute inset-x-0 top-0 h-[60vh] bg-[radial-gradient(ellipse_50%_50%_at_50%_0%,rgba(34,211,238,0.10),transparent_70%)]" />
      </div>

      <div className="container-page relative z-10">
        {/* live pill */}
        <motion.a
          href={site.github}
          target="_blank"
          rel="noopener noreferrer"
          initial={{ opacity: 0, y: -12 }}
          animate={{ opacity: 1, y: 0 }}
          transition={{ duration: 0.4, ease }}
          className="group mx-auto mb-7 flex w-fit items-center gap-2.5 rounded-full border border-white/12 bg-white/[0.04] px-4 py-1.5 text-sm backdrop-blur-sm transition-all hover:bg-white/[0.07]"
        >
          <span className="flex items-center gap-1.5 text-cyan-400">
            <span className="relative flex h-2 w-2">
              <span className="absolute inline-flex h-full w-full animate-ping rounded-full bg-cyan-400 opacity-70" />
              <span className="relative inline-flex h-2 w-2 rounded-full bg-cyan-400" />
            </span>
            <span className="text-[0.7rem] font-bold uppercase tracking-widest">
              Free &amp; local
            </span>
          </span>
          <span className="text-white/20">·</span>
          <span className="text-slate-400">
            A new home for your Claude Code agents
          </span>
          <ArrowRight className="h-3.5 w-3.5 text-slate-600 transition-transform group-hover:translate-x-0.5" />
        </motion.a>

        {/* headline */}
        <motion.h1
          initial={{ opacity: 0, y: 22 }}
          animate={{ opacity: 1, y: 0 }}
          transition={{ duration: 0.6, delay: 0.1, ease }}
          className="mx-auto max-w-4xl text-center text-[2.6rem] font-extrabold leading-[1.03] tracking-tight text-slate-50 sm:text-6xl lg:text-[4.4rem]"
        >
          Run a hundred agents.
          <br />
          <span className="gradient-text bg-[length:200%_auto] animate-gradient-x">
            Lose none of them.
          </span>
        </motion.h1>

        {/* subhead */}
        <motion.p
          initial={{ opacity: 0, y: 16 }}
          animate={{ opacity: 1, y: 0 }}
          transition={{ duration: 0.55, delay: 0.26, ease }}
          className="mx-auto mt-6 max-w-2xl text-center text-base leading-relaxed text-haze sm:text-lg"
        >
          TermHub is a terminal-first cockpit for running and supervising{" "}
          <span className="font-semibold text-slate-200">
            many persistent Claude Code sessions
          </span>{" "}
          at once — tiled terminals you drag without a reload, an attention queue
          that tells you which agent needs you, and live context, cost &amp;
          rate-limit usage. Local. Yours. Free.
        </motion.p>

        {/* pipeline note */}
        <motion.div
          initial={{ opacity: 0 }}
          animate={{ opacity: 1 }}
          transition={{ duration: 0.5, delay: 0.42 }}
          className="mt-5 flex flex-wrap items-center justify-center gap-2.5 font-mono text-[0.72rem] text-slate-600"
        >
          {["spawn", "supervise", "unblock", "ship", "repeat"].map((w, i, a) => (
            <span key={w} className="flex items-center gap-2.5">
              <span className="text-slate-500">{w}</span>
              {i < a.length - 1 && <span className="text-white/15">→</span>}
            </span>
          ))}
        </motion.div>

        {/* CTAs */}
        <motion.div
          initial={{ opacity: 0, y: 12 }}
          animate={{ opacity: 1, y: 0 }}
          transition={{ duration: 0.5, delay: 0.5, ease }}
          className="mt-8 flex flex-wrap items-center justify-center gap-3"
        >
          <a
            href={site.github}
            target="_blank"
            rel="noopener noreferrer"
            className="flex items-center gap-2 rounded-xl bg-gradient-to-r from-cyan-400 to-blue-600 px-6 py-3 text-sm font-bold text-ink-900 shadow-lg shadow-blue-900/40 transition-all hover:scale-[1.03] hover:shadow-glow"
          >
            <Github className="h-4.5 w-4.5" />
            Get it on GitHub
          </a>
          <a
            href={site.kofi}
            target="_blank"
            rel="noopener noreferrer"
            className="flex items-center gap-2 rounded-xl border border-white/12 bg-white/[0.04] px-6 py-3 text-sm font-bold text-slate-200 transition-all hover:scale-[1.03] hover:bg-white/[0.08]"
          >
            <Heart className="h-4.5 w-4.5 text-pink-400" />
            Support on Ko-fi
          </a>
        </motion.div>

        {/* terminal mock */}
        <motion.div
          initial={{ opacity: 0, y: 40 }}
          animate={{ opacity: 1, y: 0 }}
          transition={{ duration: 0.8, delay: 0.5, ease }}
          className="mx-auto mt-14 max-w-4xl [perspective:1600px]"
        >
          <TerminalGrid />
        </motion.div>

        {/* stat strip */}
        <motion.dl
          initial={{ opacity: 0, y: 20 }}
          animate={{ opacity: 1, y: 0 }}
          transition={{ duration: 0.6, delay: 0.75, ease }}
          className="mx-auto mt-12 grid max-w-3xl grid-cols-2 gap-px overflow-hidden rounded-2xl border border-white/[0.07] bg-white/[0.04] sm:grid-cols-4"
        >
          {stats.map((s) => (
            <div
              key={s.label}
              className="flex flex-col items-center gap-0.5 bg-ink-900/40 px-4 py-5 text-center"
            >
              <dt className="text-2xl font-extrabold text-slate-50">
                {s.value}
                <span className="ml-1 text-sm font-semibold text-cyan-400">
                  {s.label}
                </span>
              </dt>
              <dd className="text-[0.72rem] text-slate-500">{s.sub}</dd>
            </div>
          ))}
        </motion.dl>

        <motion.p
          initial={{ opacity: 0 }}
          animate={{ opacity: 1 }}
          transition={{ delay: 1 }}
          className="mt-8 flex items-center justify-center gap-2 text-center text-xs text-slate-600"
        >
          <Cpu className="h-3.5 w-3.5" />
          Windows 11 + WSL2 · Tauri 2 · built by {" "}
          <a
            href={site.builderSite}
            target="_blank"
            rel="noopener noreferrer"
            className="font-semibold text-slate-400 underline-offset-4 hover:text-cyan-400 hover:underline"
          >
            n8builds
          </a>
        </motion.p>
      </div>
    </section>
  );
}
