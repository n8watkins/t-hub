"use client";

import { useEffect, useState } from "react";
import { motion } from "framer-motion";
import { Github, Star, Terminal } from "lucide-react";
import { site } from "@/lib/site";

const links = [
  { href: "#why", label: "Why free" },
  { href: "#features", label: "Features" },
  { href: "#cockpit", label: "Cockpit" },
  { href: "#stack", label: "Stack" },
  { href: "#roadmap", label: "Roadmap" },
];

export default function Navbar() {
  const [scrolled, setScrolled] = useState(false);

  useEffect(() => {
    const onScroll = () => setScrolled(window.scrollY > 20);
    onScroll();
    window.addEventListener("scroll", onScroll, { passive: true });
    return () => window.removeEventListener("scroll", onScroll);
  }, []);

  return (
    <motion.header
      initial={{ y: -80, opacity: 0 }}
      animate={{ y: 0, opacity: 1 }}
      transition={{ duration: 0.5, ease: [0.25, 0.1, 0.25, 1] }}
      className={`fixed inset-x-0 top-0 z-50 transition-all duration-300 ${
        scrolled
          ? "border-b border-white/[0.07] bg-ink-900/80 backdrop-blur-xl"
          : "border-b border-transparent bg-transparent"
      }`}
    >
      <nav className="container-page flex h-16 items-center justify-between">
        <a href="#" className="group flex items-center gap-2.5">
          <span className="flex h-8 w-8 items-center justify-center rounded-lg bg-gradient-to-br from-cyan-400 to-blue-600 text-ink-900 shadow-glow">
            <Terminal className="h-4.5 w-4.5" strokeWidth={2.5} />
          </span>
          <span className="flex flex-col leading-none">
            <span className="text-[1.05rem] font-extrabold tracking-tight">
              T-<span className="gradient-text">Hub</span>
            </span>
            <span className="mt-0.5 text-[0.6rem] font-medium uppercase tracking-wider text-slate-500">
              a tool by{" "}
              <span className="text-slate-400 group-hover:text-cyan-400">
                {site.brand}
              </span>
            </span>
          </span>
        </a>

        <div className="hidden items-center gap-1 md:flex">
          {links.map((l) => (
            <a
              key={l.href}
              href={l.href}
              className="rounded-lg px-3 py-2 text-sm font-medium text-slate-400 transition-colors hover:text-slate-100"
            >
              {l.label}
            </a>
          ))}
        </div>

        <div className="flex items-center gap-2">
          <a
            href={site.github}
            target="_blank"
            rel="noopener noreferrer"
            className="group hidden items-center gap-1.5 rounded-lg border border-white/10 bg-white/[0.04] px-3.5 py-2 text-sm font-semibold text-slate-200 transition-all hover:scale-[1.03] hover:bg-white/[0.08] sm:flex"
          >
            <Star className="h-4 w-4 text-amber-300 transition-transform group-hover:rotate-12" />
            Star
          </a>
          <a
            href={site.releases}
            target="_blank"
            rel="noopener noreferrer"
            className="flex items-center gap-1.5 rounded-lg bg-gradient-to-r from-cyan-400 to-blue-600 px-3.5 py-2 text-sm font-bold text-ink-900 transition-all hover:scale-[1.03] hover:shadow-glow"
          >
            <Github className="h-4 w-4" />
            Download
          </a>
        </div>
      </nav>
    </motion.header>
  );
}
