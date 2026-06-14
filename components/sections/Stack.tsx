"use client";

import Reveal from "@/components/ui/Reveal";
import Marquee from "@/components/ui/Marquee";
import { techStack } from "@/lib/site";

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
          <p className="eyebrow">Under the hood</p>
          <h2 className="mt-3 text-3xl font-bold tracking-tight text-slate-50 sm:text-4xl">
            Boring-reliable stack, ambitious result
          </h2>
          <p className="mt-4 text-haze">
            A native Tauri shell over a Rust PTY backend driving a real{" "}
            <span className="font-mono text-slate-300">tmux</span> spine — so your
            terminals are genuinely persistent, not a clever illusion.
          </p>
        </Reveal>
      </div>

      <Reveal className="relative mt-14" y={16}>
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
