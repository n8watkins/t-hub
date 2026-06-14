// Side-effect mount for session notifications.
//
// Importing THIS module once at app startup wires up notification sounds +
// desktop notifications — no function call needed at the import site. It exists
// so the orchestrator can mount the feature with a single side-effect import in
// src/main.tsx (next to `import "./index.css";`):
//
//     import "./lib/notifyMount";
//
// The actual logic lives in ./notify; this file only triggers the idempotent
// mount so the import site stays a one-liner and App.tsx is untouched.
import { mountSessionNotifications } from "./notify";

mountSessionNotifications();
