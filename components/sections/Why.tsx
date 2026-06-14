"use client";

import { motion } from "framer-motion";
import { Check, X, ShieldCheck, HardDrive, Wallet } from "lucide-react";
import Reveal from "@/components/ui/Reveal";

const pillars = [
  {
    icon: <Wallet className="h-5 w-5" />,
    title: "Free & open source",
    body: "No subscription, no seats, no usage meter on the dashboard itself, and the full source is on GitHub. You already pay for Claude — the cockpit shouldn't cost extra.",
  },
  {
    icon: <HardDrive className="h-5 w-5" />,
    title: "100% local",
    body: "It runs on your machine, over your WSL2, on a tmux spine. Nothing routes through someone else's servers.",
  },
  {
    icon: <ShieldCheck className="h-5 w-5" />,
    title: "You own your tools",
    body: "Your sessions, your data, your themes, your layout — stored locally in SQLite. No vendor can pull the rug or change the price.",
  },
];

const compare: { label: string; cloud: boolean; termhub: boolean }[] = [
  { label: "Runs fully on your own machine", cloud: false, termhub: true },
  { label: "Your code never leaves your box", cloud: false, termhub: true },
  { label: "No monthly subscription", cloud: false, termhub: true },
  { label: "No per-seat / per-agent pricing", cloud: false, termhub: true },
  { label: "Fully open source — read every line", cloud: false, termhub: true },
  { label: "Persistent terminals you can drag live", cloud: false, termhub: true },
  { label: "Attention queue across many agents", cloud: true, termhub: true },
  { label: "Works offline / behind your firewall", cloud: false, termhub: true },
  { label: "Open the hood — it's your install", cloud: false, termhub: true },
];

export default function Why() {
  return (
    <section id="why" className="relative py-24 sm:py-32">
      {/* subtle dot field */}
      <div
        aria-hidden
        className="pointer-events-none absolute inset-0 bg-dots opacity-40 [mask-image:radial-gradient(ellipse_60%_50%_at_50%_50%,black,transparent)]"
      />
      <div className="container-page relative">
        <Reveal className="mx-auto max-w-2xl text-center">
          <p className="eyebrow">Why it&apos;s free</p>
          <h2 className="mt-3 text-3xl font-bold tracking-tight text-slate-50 sm:text-4xl">
            A cockpit you own — not a dashboard you rent
          </h2>
          <p className="mt-4 text-haze">
            Cloud &ldquo;AI agent dashboard&rdquo; services want a monthly fee to
            watch agents run on their infrastructure. Stop paying for that. T-Hub
            does it on your own machine, for free, with the source right there to
            read.
          </p>
        </Reveal>

        {/* three pillars */}
        <div className="mt-14 grid grid-cols-1 gap-4 md:grid-cols-3">
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

        {/* comparison table */}
        <Reveal className="mx-auto mt-12 max-w-3xl" y={20}>
          <div className="overflow-hidden rounded-2xl border border-white/[0.08]">
            <div className="grid grid-cols-[1fr_auto_auto] items-center bg-white/[0.04] px-5 py-3.5 text-[0.72rem] font-bold uppercase tracking-wider text-slate-400">
              <span></span>
              <span className="w-24 text-center sm:w-32">Cloud service</span>
              <span className="w-24 text-center sm:w-32">
                <span className="gradient-text">T-Hub</span>
              </span>
            </div>
            {compare.map((c, i) => (
              <div
                key={c.label}
                className={`grid grid-cols-[1fr_auto_auto] items-center px-5 py-3 text-sm ${
                  i % 2 ? "bg-white/[0.015]" : ""
                }`}
              >
                <span className="text-slate-300">{c.label}</span>
                <span className="flex w-24 justify-center sm:w-32">
                  {c.cloud ? (
                    <Check className="h-4 w-4 text-slate-500" />
                  ) : (
                    <X className="h-4 w-4 text-rose-500/70" />
                  )}
                </span>
                <span className="flex w-24 justify-center sm:w-32">
                  {c.termhub ? (
                    <span className="flex h-5 w-5 items-center justify-center rounded-full bg-cyan-400/15">
                      <Check
                        className="h-3.5 w-3.5 text-cyan-300"
                        strokeWidth={3}
                      />
                    </span>
                  ) : (
                    <X className="h-4 w-4 text-rose-500/70" />
                  )}
                </span>
              </div>
            ))}
          </div>
          <p className="mt-3 text-center text-xs text-slate-600">
            Comparison reflects the typical hosted &ldquo;agent dashboard&rdquo;
            model. Like it? A Ko-fi tip keeps it free for everyone.
          </p>
        </Reveal>
      </div>
    </section>
  );
}
