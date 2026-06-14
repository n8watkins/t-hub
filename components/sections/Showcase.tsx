"use client";

import Reveal from "@/components/ui/Reveal";
import Screenshot from "@/components/ui/Screenshot";
import { Check } from "lucide-react";

type Row = {
  eyebrow: string;
  title: string;
  body: string;
  bullets: string[];
  shot: {
    src: string;
    alt: string;
    width: number;
    height: number;
    placeholderLabel?: string;
  };
  flip?: boolean;
};

const rows: Row[] = [
  {
    eyebrow: "The grid",
    title: "Your whole fleet, tiled in one window",
    body: "Workspace tabs across the top, attention badges where it matters, and a 2×2 (or denser) grid of live terminals below. Drag any tile to a new slot and the session keeps running — no reload, no lost scrollback.",
    bullets: [
      "Persistent xterm pool positioned over placeholder cells",
      "Workspace tabs with per-workspace attention counts",
      "WSL health, context %, and live spend in the status bar",
    ],
    shot: {
      src: "/screenshots/grid.png",
      alt: "T-Hub cockpit: workspace tabs and a 2x2 grid of live terminal tiles",
      width: 955,
      height: 720,
    },
  },
  {
    eyebrow: "Supervision",
    title: "Know which agent needs you — instantly",
    body: "A live session supervision tree and an attention queue, fed by deep Claude Code hooks. When an agent stops to ask a question or hits a wall, it surfaces to the top instead of getting buried.",
    bullets: [
      "Session supervision tree of every agent and its children",
      "Attention queue: who is blocked and waiting on input",
      "Per-session context, cost, and rate-limit usage",
    ],
    shot: {
      src: "/screenshots/supervision.png",
      alt: "T-Hub sidebar showing the session supervision tree and attention queue",
      width: 955,
      height: 720,
    },
    flip: true,
  },
  {
    eyebrow: "Theming",
    title: "Live theming and a file/web preview overlay",
    body: "Recolor the entire cockpit instantly with no reload, browse the repo with the file tree, and pop a file or a web page into an overlay without leaving your terminals.",
    bullets: [
      "Instant, no-reload theme switching across all terminals",
      "File tree with file + web preview overlay",
      "Frameless Tauri window — native feel, web speed",
    ],
    shot: {
      src: "/screenshots/theming.png",
      alt: "T-Hub live theming and file preview overlay",
      width: 1440,
      height: 900,
      placeholderLabel:
        "Needed: a shot of the live-theming picker open and/or the file/web preview overlay. Save to public/screenshots/theming.png.",
    },
  },
];

export default function Showcase() {
  return (
    <section id="cockpit" className="relative py-24 sm:py-32">
      <div className="container-page">
        <Reveal className="mx-auto max-w-2xl text-center">
          <p className="eyebrow">See it move</p>
          <h2 className="mt-3 text-3xl font-bold tracking-tight text-slate-50 sm:text-4xl">
            Built to be lived in
          </h2>
          <p className="mt-4 text-haze">
            Real screens from the app. This is what supervising a dozen agents
            looks like when it&apos;s calm instead of chaotic.
          </p>
        </Reveal>

        <div className="mt-20 flex flex-col gap-24">
          {rows.map((row) => (
            <div
              key={row.title}
              className="grid grid-cols-1 items-center gap-10 lg:grid-cols-2 lg:gap-14"
            >
              <Reveal
                className={row.flip ? "lg:order-2" : ""}
                y={20}
              >
                <p className="eyebrow">{row.eyebrow}</p>
                <h3 className="mt-3 text-2xl font-bold tracking-tight text-slate-50 sm:text-3xl">
                  {row.title}
                </h3>
                <p className="mt-4 leading-relaxed text-haze">{row.body}</p>
                <ul className="mt-6 space-y-3">
                  {row.bullets.map((b) => (
                    <li key={b} className="flex items-start gap-3">
                      <span className="mt-0.5 flex h-5 w-5 shrink-0 items-center justify-center rounded-full bg-cyan-400/15 text-cyan-300">
                        <Check className="h-3 w-3" strokeWidth={3} />
                      </span>
                      <span className="text-sm text-slate-300">{b}</span>
                    </li>
                  ))}
                </ul>
              </Reveal>

              <div className={row.flip ? "lg:order-1" : ""}>
                <Screenshot {...row.shot} />
              </div>
            </div>
          ))}
        </div>
      </div>
    </section>
  );
}
