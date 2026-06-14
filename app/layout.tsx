import type { Metadata } from "next";
import { Inter, JetBrains_Mono } from "next/font/google";
import "./globals.css";
import { site } from "@/lib/site";

const inter = Inter({
  subsets: ["latin"],
  display: "swap",
  variable: "--font-inter",
  weight: ["400", "500", "600", "700", "800"],
});

const mono = JetBrains_Mono({
  subsets: ["latin"],
  display: "swap",
  variable: "--font-mono",
  weight: ["400", "500", "700"],
});

// PLACEHOLDER: set the real production URL before deploying.
const siteUrl = "https://termhub.n8builds.dev";

export const metadata: Metadata = {
  metadataBase: new URL(siteUrl),
  title: `${site.name} — ${site.tagline}`,
  description: site.description,
  keywords: [
    "TermHub",
    "Claude Code",
    "terminal multiplexer",
    "AI agent dashboard",
    "tmux",
    "WSL2",
    "Tauri",
    "xterm",
    "n8builds",
    "Nathan Watkins",
  ],
  authors: [{ name: site.author }],
  openGraph: {
    title: `${site.name} — ${site.tagline}`,
    description: site.description,
    url: siteUrl,
    siteName: site.name,
    type: "website",
  },
  twitter: {
    card: "summary_large_image",
    title: `${site.name} — ${site.tagline}`,
    description: site.description,
    creator: "@n8watkins",
  },
};

export default function RootLayout({
  children,
}: Readonly<{ children: React.ReactNode }>) {
  return (
    <html lang="en" className={`${inter.variable} ${mono.variable} dark`}>
      <body className="font-sans antialiased">
        <a
          href="#main"
          className="sr-only focus:not-sr-only focus:absolute focus:left-4 focus:top-4 focus:z-[999] focus:rounded-md focus:bg-cyan-500 focus:px-4 focus:py-2 focus:text-ink-900"
        >
          Skip to content
        </a>
        {children}
      </body>
    </html>
  );
}
