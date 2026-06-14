import { Github, Heart, Terminal, ArrowUpRight } from "lucide-react";
import { site } from "@/lib/site";

// X (Twitter) glyph — lucide dropped the bird, so inline the current mark.
function XIcon({ className }: { className?: string }) {
  return (
    <svg
      viewBox="0 0 24 24"
      className={className ?? "h-4.5 w-4.5"}
      fill="currentColor"
      aria-hidden
    >
      <path d="M18.244 2.25h3.308l-7.227 8.26 8.502 11.24H16.17l-5.214-6.817L4.99 21.75H1.68l7.73-8.835L1.254 2.25H8.08l4.713 6.231zm-1.161 17.52h1.833L7.084 4.126H5.117z" />
    </svg>
  );
}

export default function Footer() {
  return (
    <footer className="relative border-t border-white/[0.07] py-12">
      <div className="container-page flex flex-col items-center gap-8 sm:flex-row sm:items-start sm:justify-between">
        <div className="max-w-sm text-center sm:text-left">
          <a
            href="#"
            className="flex items-center justify-center gap-2.5 sm:justify-start"
          >
            <span className="flex h-8 w-8 items-center justify-center rounded-lg bg-gradient-to-br from-cyan-400 to-blue-600 text-ink-900">
              <Terminal className="h-4.5 w-4.5" strokeWidth={2.5} />
            </span>
            <span className="text-[1.05rem] font-extrabold tracking-tight">
              T-<span className="gradient-text">Hub</span>
            </span>
          </a>
          <p className="mt-3 text-sm text-slate-500">
            A free, open-source, 100% local session-first terminal IDE for
            supervising many Claude Code sessions. A tool by{" "}
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

          {/* prominent n8builds.dev link */}
          <a
            href={site.builderSite}
            target="_blank"
            rel="noopener noreferrer"
            className="group mt-4 inline-flex items-center gap-1.5 rounded-lg border border-cyan-400/20 bg-cyan-400/[0.06] px-3 py-1.5 text-sm font-semibold text-cyan-300 transition-all hover:bg-cyan-400/[0.12]"
          >
            n8builds.dev — more tools
            <ArrowUpRight className="h-3.5 w-3.5 transition-transform group-hover:translate-x-0.5 group-hover:-translate-y-0.5" />
          </a>
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
              href={site.x}
              target="_blank"
              rel="noopener noreferrer"
              aria-label="X"
              className="flex h-9 w-9 items-center justify-center rounded-lg border border-white/10 bg-white/[0.04] text-slate-400 transition-all hover:scale-105 hover:text-slate-100"
            >
              <XIcon className="h-4 w-4" />
            </a>
          </div>
          <p className="text-xs text-slate-600">
            © {new Date().getFullYear()}{" "}
            <a
              href={site.builderSite}
              target="_blank"
              rel="noopener noreferrer"
              className="hover:text-cyan-400"
            >
              n8builds
            </a>
          </p>
        </div>
      </div>
    </footer>
  );
}
