"use client";

import { motion } from "framer-motion";
import { HardDrive, Wallet, ShieldCheck } from "lucide-react";
import Reveal from "@/components/ui/Reveal";

const pillars = [
  {
    icon: <Wallet className="h-5 w-5" />,
    title: "Don't pay for the obvious",
    body: "AI makes building software easy — so don't get baited by dumb upsells for things you can do for free. The cockpit shouldn't have a meter on it.",
  },
  {
    icon: <HardDrive className="h-5 w-5" />,
    title: "100% local",
    body: "It runs on your machine, on a tmux spine. No account, no server in the middle, nothing routed through someone else's box.",
  },
  {
    icon: <ShieldCheck className="h-5 w-5" />,
    title: "Own your tools",
    body: "Source on GitHub, state in your own SQLite file. Read it, fork it, change it. No vendor can pull the rug or raise the price.",
  },
];

export default function Why() {
  return (
    <section id="why" className="relative py-20 sm:py-24">
      {/* subtle dot field */}
      <div
        aria-hidden
        className="pointer-events-none absolute inset-0 bg-dots opacity-40 [mask-image:radial-gradient(ellipse_60%_50%_at_50%_50%,black,transparent)]"
      />
      <div className="container-page relative">
        <Reveal className="mx-auto max-w-2xl text-center">
          <p className="eyebrow">Why it&apos;s free</p>
          <h2 className="mt-3 text-3xl font-bold tracking-tight text-slate-50 sm:text-4xl">
            Use free tools. Own your tools.
          </h2>
          <p className="mt-4 text-haze">
            AI tooling makes building software easy — make it even easier. Use
            free tools like this one. 100% local, free &amp; open source. That&apos;s
            the whole pitch.
          </p>
        </Reveal>

        {/* three pillars */}
        <div className="mt-12 grid grid-cols-1 gap-4 md:grid-cols-3">
          {pillars.map((p, i) => (
            <motion.div
              key={p.title}
              initial={{ opacity: 0, y: 24 }}
              whileInView={{ opacity: 1, y: 0 }}
              viewport={{ once: true, margin: "-60px" }}
              transition={{ duration: 0.5, delay: i * 0.1 }}
              className="relative overflow-hidden rounded-2xl border border-white/[0.08] bg-gradient-to-b from-white/[0.05] to-transparent p-6"
            >
              <span className="mb-4 flex h-11 w-11 items-center justify-center rounded-xl bg-gradient-to-br from-cyan-400/20 to-blue-600/10 text-cyan-300">
                {p.icon}
              </span>
              <h3 className="text-lg font-bold text-slate-100">{p.title}</h3>
              <p className="mt-2 text-sm leading-relaxed text-haze">{p.body}</p>
            </motion.div>
          ))}
        </div>
      </div>
    </section>
  );
}
