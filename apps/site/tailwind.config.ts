import type { Config } from "tailwindcss";

const config: Config = {
  darkMode: "class",
  content: [
    "./app/**/*.{ts,tsx}",
    "./components/**/*.{ts,tsx}",
    "./lib/**/*.{ts,tsx}",
  ],
  theme: {
    extend: {
      spacing: {
        "4.5": "1.125rem",
      },
      colors: {
        ink: {
          DEFAULT: "#050812",
          900: "#050812",
          800: "#06101f",
          700: "#081427",
          600: "#0a1a33",
        },
        haze: "#9cadc5",
      },
      fontFamily: {
        sans: ["var(--font-inter)", "system-ui", "sans-serif"],
        mono: ["var(--font-mono)", "ui-monospace", "monospace"],
      },
      boxShadow: {
        glow: "0 0 60px -15px rgba(34,211,238,0.35)",
        "glow-soft": "0 0 80px -20px rgba(59,130,246,0.4)",
      },
      keyframes: {
        marquee: {
          from: { transform: "translateX(0)" },
          to: { transform: "translateX(calc(-50% - 0.75rem))" },
        },
        "marquee-rev": {
          from: { transform: "translateX(calc(-50% - 0.75rem))" },
          to: { transform: "translateX(0)" },
        },
        shimmer: {
          "0%": { backgroundPosition: "-200% 0" },
          "100%": { backgroundPosition: "200% 0" },
        },
        "pulse-cell": {
          "0%, 100%": { opacity: "0.55", fill: "#0c1836" },
          "50%": { opacity: "1", fill: "#173061" },
        },
        float: {
          "0%, 100%": { transform: "translateY(0)" },
          "50%": { transform: "translateY(-10px)" },
        },
        "gradient-x": {
          "0%, 100%": { backgroundPosition: "0% 50%" },
          "50%": { backgroundPosition: "100% 50%" },
        },
        blink: {
          "0%, 49%": { opacity: "1" },
          "50%, 100%": { opacity: "0" },
        },
      },
      animation: {
        marquee: "marquee var(--duration,40s) linear infinite",
        "marquee-rev": "marquee-rev var(--duration,40s) linear infinite",
        shimmer: "shimmer 2.5s linear infinite",
        float: "float 6s ease-in-out infinite",
        "gradient-x": "gradient-x 6s ease infinite",
        blink: "blink 1.1s step-end infinite",
      },
    },
  },
  plugins: [],
};

export default config;
