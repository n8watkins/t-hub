"use client";

import { motion } from "framer-motion";
import { Check, GitFork, Star, CircleDot, Map } from "lucide-react";
import Reveal from "@/components/ui/Reveal";
import { site, roadmapNow, roadmapNext } from "@/lib/site";

const stepIcon = [
  <CircleDot key="a" className="h-5 w-5" />,
  <Map key="b" className="h-5 w-5" />,
  <GitFork key="c" className="h-5 w-5" />,
];

export default function Roadmap() {
  return (
    <section id="roadmap" className="relative py-24 sm:py-32">
      <div className="container-page">
        <Reveal className="mx-auto max-w-2xl text-center">
          <p className="eyebrow">The roadmap</p>
          <h2 className="mt-3 text-3xl font-bold tracking-tight text-slate-50 sm:text-4xl">
            Where it is, and where it&apos;s going
          </h2>
          <p className="mt-4 text-haze">
            Opinionated, open source, and not finished. Here&apos;s the honest
            state of the project — README-style.
          </p>
        </Reveal>

        <div className="mt-14 grid grid-cols-1 gap-6 lg:grid-cols-2">
          {/* current state */}
          <Reveal y={20}>
            <div className="h-full rounded-2xl border border-white/[0.08] bg-white/[0.025] p-7">
              <div className="flex items-center gap-2 text-[0.7rem] font-bold uppercase tracking-wider text-emerald-300">
                <span className="h-2 w-2 rounded-full bg-emerald-400" />
                Built for today
              </div>
              <h3 className="mt-3 text-xl font-bold text-slate-100">
                What it does right now
              </h3>
              <ul className="mt-5 space-y-3">
                {roadmapNow.map((item) => (
                  <li key={item} className="flex items-start gap-3">
                    <span className="mt-0.5 flex h-5 w-5 shrink-0 items-center justify-center rounded-full bg-emerald-400/15 text-emerald-300">
                      <Check className="h-3 w-3" strokeWidth={3} />
                    </span>
                    <span className="text-sm leading-relaxed text-slate-300">
                      {item}
                    </span>
                  </li>
                ))}
              </ul>
            </div>
          </Reveal>

          {/* what's next */}
          <Reveal y={20} delay={0.1}>
            <div className="h-full rounded-2xl border border-white/[0.08] bg-white/[0.025] p-7">
              <div className="flex items-center gap-2 text-[0.7rem] font-bold uppercase tracking-wider text-cyan-300">
                <span className="h-2 w-2 rounded-full bg-cyan-400" />
                On the roadmap
              </div>
              <h3 className="mt-3 text-xl font-bold text-slate-100">
                Where it&apos;s heading
              </h3>
              <div className="mt-5 space-y-5">
                {roadmapNext.map((r, i) => (
                  <motion.div
                    key={r.title}
                    initial={{ opacity: 0, x: 16 }}
                    whileInView={{ opacity: 1, x: 0 }}
                    viewport={{ once: true }}
                    transition={{ duration: 0.4, delay: i * 0.08 }}
                    className="flex items-start gap-3"
                  >
                    <span className="flex h-9 w-9 shrink-0 items-center justify-center rounded-xl border border-white/10 bg-gradient-to-br from-cyan-400/15 to-blue-600/5 text-cyan-300">
                      {stepIcon[i]}
                    </span>
                    <div>
                      <p className="text-sm font-bold text-slate-100">
                        {r.title}
                      </p>
                      <p className="mt-1 text-sm leading-relaxed text-haze">
                        {r.body}
                      </p>
                    </div>
                  </motion.div>
                ))}
              </div>
            </div>
          </Reveal>
        </div>

        {/* contributor CTA */}
        <Reveal className="mx-auto mt-8 max-w-3xl" y={16}>
          <div className="flex flex-col items-center gap-5 rounded-2xl border border-cyan-400/15 bg-gradient-to-br from-cyan-400/[0.06] to-blue-600/[0.04] p-7 text-center sm:flex-row sm:text-left">
            <div className="flex-1">
              <h3 className="text-lg font-bold text-slate-50">
                Become agent-agnostic with us
              </h3>
              <p className="mt-1.5 text-sm leading-relaxed text-haze">
                Star it, open issues, become a contributor — or fork it and build
                it your way. It&apos;s your tool to shape.
              </p>
            </div>
            <div className="flex shrink-0 gap-3">
              <a
                href={site.github}
                target="_blank"
                rel="noopener noreferrer"
                className="flex items-center gap-2 rounded-xl bg-gradient-to-r from-cyan-400 to-blue-600 px-5 py-2.5 text-sm font-bold text-ink-900 transition-all hover:scale-[1.03] hover:shadow-glow"
              >
                <Star className="h-4 w-4" />
                Star the repo
              </a>
              <a
                href={site.github}
                target="_blank"
                rel="noopener noreferrer"
                className="flex items-center gap-2 rounded-xl border border-white/12 bg-white/[0.05] px-5 py-2.5 text-sm font-bold text-slate-100 transition-all hover:scale-[1.03] hover:bg-white/[0.1]"
              >
                <GitFork className="h-4 w-4" />
                Fork it
              </a>
            </div>
          </div>
        </Reveal>
      </div>
    </section>
  );
}
