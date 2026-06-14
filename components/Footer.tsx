import { Github, Heart, Twitter, Terminal } from "lucide-react";
import { site } from "@/lib/site";

export default function Footer() {
  return (
    <footer className="relative border-t border-white/[0.07] py-12">
      <div className="container-page flex flex-col items-center gap-8 sm:flex-row sm:items-start sm:justify-between">
        <div className="max-w-sm text-center sm:text-left">
          <a href="#" className="flex items-center justify-center gap-2.5 sm:justify-start">
            <span className="flex h-8 w-8 items-center justify-center rounded-lg bg-gradient-to-br from-cyan-400 to-blue-600 text-ink-900">
              <Terminal className="h-4.5 w-4.5" strokeWidth={2.5} />
            </span>
            <span className="text-[1.05rem] font-extrabold tracking-tight">
              Term<span className="gradient-text">Hub</span>
            </span>
          </a>
          <p className="mt-3 text-sm text-slate-500">
            A free, local terminal cockpit for supervising many Claude Code
            agents. Built in public by{" "}
            <a
              href={site.builderSite}
              target="_blank"
              rel="noopener noreferrer"
              className="font-semibold text-slate-400 hover:text-cyan-400"
            >
              n8builds
            </a>
            .
          </p>
        </div>

        <div className="flex flex-col items-center gap-4 sm:items-end">
          <div className="flex items-center gap-2">
            <a
              href={site.github}
              target="_blank"
              rel="noopener noreferrer"
              aria-label="GitHub"
              className="flex h-9 w-9 items-center justify-center rounded-lg border border-white/10 bg-white/[0.04] text-slate-400 transition-all hover:scale-105 hover:text-slate-100"
            >
              <Github className="h-4.5 w-4.5" />
            </a>
            <a
              href={site.kofi}
              target="_blank"
              rel="noopener noreferrer"
              aria-label="Ko-fi"
              className="flex h-9 w-9 items-center justify-center rounded-lg border border-white/10 bg-white/[0.04] text-pink-400 transition-all hover:scale-105 hover:text-pink-300"
            >
              <Heart className="h-4.5 w-4.5" />
            </a>
            <a
              href={site.twitter}
              target="_blank"
              rel="noopener noreferrer"
              aria-label="X / Twitter"
              className="flex h-9 w-9 items-center justify-center rounded-lg border border-white/10 bg-white/[0.04] text-slate-400 transition-all hover:scale-105 hover:text-slate-100"
            >
              <Twitter className="h-4.5 w-4.5" />
            </a>
          </div>
          <p className="text-xs text-slate-600">
            © {new Date().getFullYear()} {site.author} · n8builds
          </p>
        </div>
      </div>
    </footer>
  );
}
