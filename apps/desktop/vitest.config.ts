import { defineConfig } from "vitest/config";
import react from "@vitejs/plugin-react";

// Frontend test runner config. Mirrors vite.config.ts's React plugin so JSX/TSX
// transforms behave the same under test. The current batch is pure-function only,
// but jsdom + globals are enabled up front so FUTURE component tests
// (@testing-library/react) drop in without re-config.
export default defineConfig({
  plugins: [react()],
  test: {
    environment: "jsdom",
    globals: true,
    include: ["src/**/*.test.{ts,tsx}"],
  },
});
