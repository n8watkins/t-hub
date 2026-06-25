import ReactDOM from "react-dom/client";
import App from "./App";
import "./index.css";
// Side-effect import: mounts session-event notification sounds/desktop toasts
// once at startup (idempotent). See src/lib/notifyMount.ts.
import "./lib/notifyMount";
// Side-effect import: mounts the always-on Claude USAGE feed — subscribes
// status://snapshot into the supervision store so the sidebar USAGE strip
// populates app-wide (not just while the Sidebar is mounted). Idempotent.
// See src/lib/statusMount.ts.
import "./lib/statusMount";
// Side-effect import: schedules a one-shot on-launch update check (and, if the
// user opted in, a silent install + relaunch). Idempotent + best-effort — see
// src/lib/updateMount.ts.
import "./lib/updateMount";
// Side-effect import: arms "auto-continue on usage reset" — for terminals the
// user opted into, waits for a rate-limited Claude session's window to reset and
// injects the continue command so it resumes on its own. Idempotent. See
// src/lib/autoContinueMount.ts.
import "./lib/autoContinueMount";
// Side-effect import: arms the event→action RULES engine (WS-5b) — when a
// supervised session's FR-012 status transitions, runs the user-configured action
// (notify / send text / spawn / restart / run). Loop-guarded + startup-warmed.
// Idempotent. See src/lib/rulesMount.ts.
import "./lib/rulesMount";

// Note: React.StrictMode is intentionally omitted. Its double-invoke of effects
// in development breaks xterm.js terminals (double `open()` / disposed addons).

ReactDOM.createRoot(document.getElementById("root") as HTMLElement).render(
  <App />,
);
