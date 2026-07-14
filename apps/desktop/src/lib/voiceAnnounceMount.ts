// Side-effect module (mirrors notifyMount/statusMount): hydrate the voice
// store from ~/.t-hub/voice.json, arm the announce-on-attention watcher, and
// mount the settings-driven Scribe voice-gate lifecycle. main.tsx imports this
// for its side effect.
import { useVoice } from "../store/voice";
import { mountVoiceAnnounce, startScribePoll } from "./voiceAnnounce";

// Load first so the watcher's gates reflect the file, then arm. Outside Tauri
// (plain `pnpm dev`) load() falls back to defaults (everything off).
void useVoice.getState().load();
mountVoiceAnnounce();
// Poll Scribe only while voice announcements are enabled, so announcements HOLD
// while the general dictates and DELIVER when they stop. Fail-open
// (listening=false) when Scribe isn't running.
startScribePoll();
