// Side-effect module (mirrors notifyMount/statusMount): hydrate the voice
// store from ~/.t-hub/voice.json and arm the announce-on-attention watcher
// once at startup. main.tsx imports this for its side effect.
import { useVoice } from "../store/voice";
import { mountVoiceAnnounce } from "./voiceAnnounce";

// Load first so the watcher's gates reflect the file, then arm. Outside Tauri
// (plain `pnpm dev`) load() falls back to defaults (everything off).
void useVoice.getState().load();
mountVoiceAnnounce();
