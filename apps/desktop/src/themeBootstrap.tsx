// Self-contained bootstrap for the theming system.
//
// The theming workstream owns its wiring end-to-end and must NOT modify App.tsx
// or main.tsx. So instead of being rendered by App, the ThemeProvider (which
// applies the theme to :root, bridges the backend/MCP, and hosts the
// Ctrl/Cmd+, ThemeEditor overlay) mounts itself into its OWN React root on a
// dedicated, fixed/inert container appended to <body>.
//
// This module is pulled into the bundle by a single <script type="module"> in
// index.html (the one integration seam outside App/main), and runs its side
// effect on import. The container is pointer-events:none and zero-size so it
// never intercepts input; the editor panel sets its own pointer-events when open.
import ReactDOM from "react-dom/client";
import { ThemeProvider } from "./components/ThemeProvider";

const CONTAINER_ID = "t-hub-theme-root";

function mount(): void {
  if (typeof document === "undefined") return;
  if (document.getElementById(CONTAINER_ID)) return; // idempotent (HMR-safe)

  const host = document.createElement("div");
  host.id = CONTAINER_ID;
  // Inert host: it must never steal clicks/layout from the app. The editor
  // overlay re-enables pointer events on itself (it's `fixed` + `z-50`).
  host.style.position = "fixed";
  host.style.inset = "0";
  host.style.pointerEvents = "none";
  host.style.zIndex = "2147483000";
  document.body.appendChild(host);

  ReactDOM.createRoot(host).render(<ThemeProvider />);
}

// React StrictMode is intentionally not used app-wide here (the app omits it to
// keep xterm happy); the overlay has no such constraint but we match the app.
mount();
