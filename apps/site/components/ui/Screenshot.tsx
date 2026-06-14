"use client";

import Image from "next/image";
import { motion } from "framer-motion";

type ScreenshotProps = {
  src: string;
  alt: string;
  /** Width/height of the underlying asset for layout ratio. */
  width: number;
  height: number;
  /** Shown when src is a placeholder path that doesn't exist yet. */
  placeholderLabel?: string;
  priority?: boolean;
  className?: string;
};

// A framed "app window" wrapper for product screenshots.
// If `placeholderLabel` is set, it renders a clearly-labeled empty frame instead
// of an <Image>, so the human knows exactly which shot to drop in.
export default function Screenshot({
  src,
  alt,
  width,
  height,
  placeholderLabel,
  priority,
  className = "",
}: ScreenshotProps) {
  return (
    <motion.div
      initial={{ opacity: 0, y: 30, rotateX: 8 }}
      whileInView={{ opacity: 1, y: 0, rotateX: 0 }}
      viewport={{ once: true, margin: "-60px" }}
      transition={{ duration: 0.8, ease: [0.25, 0.1, 0.25, 1] }}
      className={`group relative overflow-hidden rounded-xl border border-white/10 bg-ink-700/60 shadow-2xl shadow-black/50 ${className}`}
    >
      {/* faux window chrome */}
      <div className="flex items-center gap-1.5 border-b border-white/[0.06] bg-white/[0.03] px-3.5 py-2.5">
        <span className="h-2.5 w-2.5 rounded-full bg-[#ff5f57]" />
        <span className="h-2.5 w-2.5 rounded-full bg-[#febc2e]" />
        <span className="h-2.5 w-2.5 rounded-full bg-[#28c840]" />
        <span className="ml-3 font-mono text-[0.68rem] text-slate-500">
          T-Hub
        </span>
      </div>

      {placeholderLabel ? (
        <div
          className="flex flex-col items-center justify-center gap-2 bg-ink-800 p-10 text-center"
          style={{ aspectRatio: `${width} / ${height}` }}
        >
          <div className="rounded-md border border-dashed border-cyan-400/30 px-3 py-1 font-mono text-[0.7rem] uppercase tracking-widest text-cyan-400/80">
            Screenshot placeholder
          </div>
          <p className="max-w-xs text-sm text-haze">{placeholderLabel}</p>
        </div>
      ) : (
        <Image
          src={src}
          alt={alt}
          width={width}
          height={height}
          priority={priority}
          className="h-auto w-full"
        />
      )}

      {/* sheen on hover */}
      <div className="pointer-events-none absolute inset-0 -translate-x-full bg-gradient-to-r from-transparent via-white/[0.07] to-transparent transition-transform duration-700 group-hover:translate-x-full" />
    </motion.div>
  );
}
