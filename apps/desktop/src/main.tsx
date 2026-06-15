import ReactDOM from "react-dom/client";
import App from "./App";
import "./index.css";
// Side-effect import: mounts session-event notification sounds/desktop toasts
// once at startup (idempotent). See src/lib/notifyMount.ts.
import "./lib/notifyMount";
// Side-effect import: schedules a one-shot on-launch update check (and, if the
// user opted in, a silent install + relaunch). Idempotent + best-effort — see
// src/lib/updateMount.ts.
import "./lib/updateMount";

// Note: React.StrictMode is intentionally omitted. Its double-invoke of effects
// in development breaks xterm.js terminals (double `open()` / disposed addons).

ReactDOM.createRoot(document.getElementById("root") as HTMLElement).render(
  <App />,
);
