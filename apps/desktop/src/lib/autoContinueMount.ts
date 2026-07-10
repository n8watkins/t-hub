// Side-effect module (mirrors notifyMount/statusMount/voiceAnnounceMount): arm the
// auto-continue watcher once at startup. main.tsx imports this for its side effect.
//
// The WATCHER LOGIC lives in ./autoContinueWatch (import-side-effect-free so it
// unit-tests in isolation); this file is the single place that actually arms it.
import { installAutoContinue } from "./autoContinueWatch";

installAutoContinue();
