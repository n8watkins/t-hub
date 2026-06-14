"use client";

import type { ReactNode } from "react";

type MarqueeProps = {
  children: ReactNode;
  reverse?: boolean;
  duration?: string;
  className?: string;
};

// CSS-driven marquee. Renders the children twice so the loop is seamless.
export default function Marquee({
  children,
  reverse = false,
  duration = "40s",
  className = "",
}: MarqueeProps) {
  return (
    <div className={`group flex overflow-hidden ${className}`}>
      {[0, 1].map((i) => (
        <div
          key={i}
          aria-hidden={i === 1}
          className={`flex shrink-0 items-center gap-3 pr-3 ${
            reverse ? "animate-marquee-rev" : "animate-marquee"
          } group-hover:[animation-play-state:paused]`}
          style={{ ["--duration" as string]: duration }}
        >
          {children}
        </div>
      ))}
    </div>
  );
}
