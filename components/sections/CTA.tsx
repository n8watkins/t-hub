"use client";

import { motion } from "framer-motion";
import { Github, Heart, Star } from "lucide-react";
import { site } from "@/lib/site";

export default function CTA() {
  return (
    <section className="relative py-24 sm:py-32">
      <div className="container-page">
        <motion.div
          initial={{ opacity: 0, y: 30 }}
          whileInView={{ opacity: 1, y: 0 }}
          viewport={{ once: true, margin: "-80px" }}
          transition={{ duration: 0.7, ease: [0.25, 0.1, 0.25, 1] }}
          className="relative overflow-hidden rounded-3xl border border-white/10 bg-gradient-to-br from-ink-700 to-ink-800 px-6 py-16 text-center sm:px-16"
        >
          {/* animated glow ring */}
          <div
            aria-hidden
            className="pointer-events-none absolute inset-0"
          >
            <div className="absolute left-1/2 top-0 h-72 w-72 -translate-x-1/2 -translate-y-1/3 rounded-full bg-cyan-500/20 blur-[120px]" />
            <div className="absolute bottom-0 right-1/4 h-64 w-64 translate-y-1/3 rounded-full bg-blue-600/20 blur-[120px]" />
            <div className="absolute inset-0 bg-grid opacity-[0.4] [mask-image:radial-gradient(ellipse_60%_60%_at_50%_50%,black,transparent)]" />
          </div>

          <div className="relative">
            <span className="mx-auto mb-6 flex w-fit items-center gap-2 rounded-full border border-cyan-400/20 bg-cyan-400/10 px-4 py-1.5 text-[0.7rem] font-bold uppercase tracking-widest text-cyan-300">
              <Star className="h-3.5 w-3.5" />
              Free &amp; open source
            </span>
            <h2 className="mx-auto max-w-2xl text-3xl font-extrabold tracking-tight text-slate-50 sm:text-5xl">
              Stop babysitting one terminal.
              <br />
              <span className="gradient-text">Command the whole fleet.</span>
            </h2>
            <p className="mx-auto mt-5 max-w-xl text-haze">
              Download T-Hub free from GitHub and run your Claude Code agents the
              way they deserve — locally, on your machine, no subscription. If it
              saves you time, a Ko-fi tip keeps it going.
            </p>

            <div className="mt-9 flex flex-wrap items-center justify-center gap-3">
              <a
                href={site.github}
                target="_blank"
                rel="noopener noreferrer"
                className="flex items-center gap-2 rounded-xl bg-gradient-to-r from-cyan-400 to-blue-600 px-7 py-3.5 text-sm font-bold text-ink-900 shadow-lg shadow-blue-900/40 transition-all hover:scale-[1.04] hover:shadow-glow"
              >
                <Github className="h-5 w-5" />
                Download free on GitHub
              </a>
              <a
                href={site.kofi}
                target="_blank"
                rel="noopener noreferrer"
                className="flex items-center gap-2 rounded-xl border border-white/12 bg-white/[0.05] px-7 py-3.5 text-sm font-bold text-slate-100 transition-all hover:scale-[1.04] hover:bg-white/[0.1]"
              >
                <Heart className="h-5 w-5 text-pink-400" />
                Support on Ko-fi
              </a>
            </div>
            <p className="mt-5 text-xs text-slate-600">
              Windows-only for now (Windows 11 + WSL2) · open source · a tool by
              n8builds
            </p>
          </div>
        </motion.div>
      </div>
    </section>
  );
}
