// ESLint 9 flat config for the T-Hub desktop app.
//
// This is the JavaScript/TypeScript counterpart to the Rust gate
// (`cargo fmt --check` + `cargo clippy --workspace --all-targets -- -D warnings`)
// that `.github/workflows/test.yml` already enforces on every PR.
//
// It runs UNTYPED on purpose - no `parserOptions.projectService`. Type-aware
// linting has to build the whole program on every run, and `tsc --noEmit`
// already runs as its own CI step and its own local lane, so the type errors a
// typed lint would surface are covered. Keeping the lint pass untyped is what
// lets it sit next to `typecheck` in the parallel `fast` lane without adding
// meaningful wall time.
//
// Severity policy:
//   error - real defects, and mechanical cleanups with no judgement involved.
//   warn  - a real signal with an existing backlog. Warnings do NOT fail CI
//           (the lint step deliberately does not pass `--max-warnings 0`), so
//           the backlog can be burned down without gating unrelated PRs.

import js from "@eslint/js";
import globals from "globals";
import tseslint from "typescript-eslint";
import reactHooks from "eslint-plugin-react-hooks";
import reactRefresh from "eslint-plugin-react-refresh";

export default tseslint.config(
  {
    ignores: [
      // Build output.
      "dist/**",
      "dist-ssr/**",
      "node_modules/**",
      // Cargo/Tauri build trees and Tauri's generated sources.
      "src-tauri/target/**",
      "src-tauri/target-agent-resource/**",
      "src-tauri/gen/**",
      ".next/**",
      // Playwright output.
      "playwright-report/**",
      "test-results/**",
      // Ambient declaration shims for untyped third-party packages.
      "src/vite-env.d.ts",
      "src/vscode-icons-js.d.ts",
      // Machine-specific symlinks to bundled binaries and vendored libs.
      "bin/**",
      "lib/**",
    ],
  },

  js.configs.recommended,
  // Untyped preset. `recommendedTypeChecked` is a deliberate non-goal here; see
  // the header note about `tsc --noEmit` already covering type errors.
  ...tseslint.configs.recommended,

  {
    languageOptions: {
      ecmaVersion: 2022,
      sourceType: "module",
      globals: {
        ...globals.browser,
      },
      parserOptions: {
        ecmaFeatures: { jsx: true },
      },
    },
    rules: {
      // `_`-prefixed bindings are the intentional "unused on purpose" marker,
      // matching the Rust side's `_name` convention.
      "@typescript-eslint/no-unused-vars": [
        "error",
        {
          argsIgnorePattern: "^_",
          varsIgnorePattern: "^_",
          caughtErrorsIgnorePattern: "^_",
          destructuredArrayIgnorePattern: "^_",
        },
      ],

      // `catch {}` is an idiom here, not an oversight: persistence, clipboard,
      // and Tauri-plugin calls are all best-effort and must never take down a
      // terminal tile. Every other empty block stays an error.
      "no-empty": ["error", { allowEmptyCatch: true }],
    },
  },

  // React sources. Scoped to `src` so the Vite/Vitest/Playwright configs and
  // the Node build scripts are not measured against component rules.
  {
    files: ["src/**/*.{ts,tsx}"],
    plugins: {
      "react-hooks": reactHooks,
      "react-refresh": reactRefresh,
    },
    rules: {
      // The reason this config exists. A shipped bug listed `focusedId` in a
      // `useEffect` dependency array in Canvas.tsx, so every single tile click
      // tore down and rebuilt a 15s poll interval and spawned a `wsl.exe`
      // subprocess. `exhaustive-deps` is precisely the rule that surfaces that
      // class of defect. It is a WARNING only because the tree already carries
      // a backlog and erroring would gate every PR on unrelated cleanup - it is
      // not optional signal. Burn the backlog down; do not switch it off.
      "react-hooks/exhaustive-deps": "warn",
      // A conditional or looped hook call is always a real bug, never style.
      "react-hooks/rules-of-hooks": "error",

      // Vite fast refresh only preserves state when a module exports
      // components and nothing else. `allowConstantExport` keeps the
      // component-plus-literals modules quiet, since constants do not break
      // refresh the way exported functions do.
      "react-refresh/only-export-components": [
        "warn",
        { allowConstantExport: true },
      ],
    },
  },

  // Node-side tooling: Vite/Vitest/Playwright/Tailwind/PostCSS configs, this
  // config itself, and the build scripts all run under Node, not the browser.
  {
    files: [
      "*.config.{js,ts,mjs}",
      "eslint.config.js",
      "scripts/**/*.{js,mjs,cjs}",
    ],
    languageOptions: {
      globals: {
        ...globals.node,
      },
    },
  },

  // Vitest suites and the Playwright e2e specs reach for Node built-ins
  // (process, Buffer) alongside the jsdom/browser globals above.
  {
    files: ["**/*.test.{ts,tsx}", "e2e/**/*.ts"],
    languageOptions: {
      globals: {
        ...globals.node,
      },
    },
  },
);
