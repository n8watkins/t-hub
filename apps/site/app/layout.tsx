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

// Public alias for the deployed site.
const siteUrl = "https://t-hub-site.vercel.app";

export const metadata: Metadata = {
  metadataBase: new URL(siteUrl),
  title: `${site.name} — ${site.tagline} · a free, open-source tool by ${site.brand}`,
  description: site.description,
  applicationName: site.name,
  creator: site.brand,
  publisher: site.brand,
  keywords: [
    "T-Hub",
    "n8builds",
    "Claude Code",
    "agentic coding",
    "session-first terminal IDE",
    "open source",
    "free",
    "terminal multiplexer",
    "AI agent cockpit",
    "agent supervision",
    "local AI agents",
    "tmux",
    "WSL2",
    "Tauri",
    "xterm",
  ],
  authors: [{ name: site.brand, url: site.builderSite }],
  openGraph: {
    title: `${site.name} — ${site.tagline}`,
    description: site.description,
    url: siteUrl,
    siteName: `${site.name} · ${site.brand}`,
    type: "website",
    // PLACEHOLDER: add a 1200x630 OG image at /public/og.png and uncomment.
    // images: [{ url: "/og.png", width: 1200, height: 630, alt: site.name }],
  },
  twitter: {
    card: "summary_large_image",
    title: `${site.name} — ${site.tagline}`,
    description: site.description,
    creator: "@n8watkins",
    // PLACEHOLDER: add the OG image here too once created.
    // images: ["/og.png"],
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
