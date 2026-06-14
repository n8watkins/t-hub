"use client";

import Reveal from "@/components/ui/Reveal";
import Screenshot from "@/components/ui/Screenshot";
import ThemeShift from "@/components/ui/ThemeShift";
import { Check } from "lucide-react";

type Row = {
  eyebrow: string;
  title: string;
  body: string;
  bullets: string[];
  shot?: {
    src: string;
    alt: string;
    width: number;
    height: number;
  };
  animated?: boolean;
  flip?: boolean;
};

const rows: Row[] = [
  {
    eyebrow: "The grid",
    title: "Your whole fleet, tiled in one cockpit",
    body: "Workspace tabs across the top, attention badges where it matters, and a grid of live terminals below. Drag any tile to a new slot and the session keeps running — no reload, no lost scrollback.",
    bullets: [
      "Persistent xterm pool positioned over the grid",
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
    body: "A live session supervision tree and a cross-agent attention queue, fed by deep Claude Code hooks. When an agent stops to ask or hits a wall, it surfaces to the top instead of getting buried.",
    bullets: [
      "Supervision tree of every agent and its children",
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
    eyebrow: "Live theming",
    title: "Recolor the whole cockpit, instantly",
    body: "Switch themes and the entire UI — every terminal, the file rail, the status bar — recolors live with no reload. Browse files in the rail and preview them without leaving your sessions.",
    bullets: [
      "Instant, no-reload theme switching across all terminals",
      "File rail with inline file preview",
      "Frameless Tauri window — native feel, web speed",
    ],
    animated: true,
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
              <Reveal className={row.flip ? "lg:order-2" : ""} y={20}>
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

              <div className={`[perspective:1600px] ${row.flip ? "lg:order-1" : ""}`}>
                {row.animated ? (
                  <ThemeShift />
                ) : (
                  row.shot && <Screenshot {...row.shot} />
                )}
              </div>
            </div>
          ))}
        </div>
      </div>
    </section>
  );
}
