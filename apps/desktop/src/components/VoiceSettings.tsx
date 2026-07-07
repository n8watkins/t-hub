// Settings > Voice (its own file so tests can render it without the whole
// ThemeEditor): controls for spoken announcements backed by ~/.t-hub/voice.json
// (the FILE is the source of truth - external captain tooling reads it too)
// and a local TTS server, reached ONLY through the backend proxy
// (src-tauri/src/voice.rs) because the servers reject browser-Origin requests.
//
// Two selectable ENGINES: Piper (127.0.0.1:7477) and Kokoro (127.0.0.1:7478).
// Switching the engine dropdown re-queries that engine's /voices live (the two
// have disjoint voice sets); a persisted voice that isn't in the new engine's
// list self-heals to the first available voice.
//
// Degradation contract: when the SELECTED engine's /voices proxy fails (its
// server down) the section shows an "unavailable" hint and disables every
// control EXCEPT the master enable toggle and the engine selector, so the user
// can still switch to the other engine (or flip intent) while one is offline.
import { useEffect, useState } from "react";
import { useVoice } from "../store/voice";
import { synthesizeVoice, type VoiceEngine } from "../ipc/voice";
import { playWavBase64 } from "../lib/voiceAudio";
import {
  Btn,
  Group,
  Opt,
  Row,
  SettingSliderRow,
  SettingToggleRow,
  ThemeSelect,
} from "./settingRows";

/** The short phrase the Test button speaks. */
export const VOICE_TEST_PHRASE = "T-Hub voice check";

/** Engine dropdown options + their loopback ports (shown in the description). */
const ENGINES: { id: VoiceEngine; label: string; port: number }[] = [
  { id: "piper", label: "Piper", port: 7477 },
  { id: "kokoro", label: "Kokoro", port: 7478 },
];

export function VoiceSection() {
  const enabled = useVoice((s) => s.enabled);
  const engine = useVoice((s) => s.engine);
  const voice = useVoice((s) => s.voice);
  const volume = useVoice((s) => s.volume);
  const announceOnAttention = useVoice((s) => s.announceOnAttention);
  const voices = useVoice((s) => s.voices);
  const voicesUnavailable = useVoice((s) => s.voicesUnavailable);

  // One test synthesis in flight at a time; surface a failure inline (the
  // server can vanish between the voices fetch and the click).
  const [testing, setTesting] = useState(false);
  const [testError, setTestError] = useState<string | null>(null);

  // (Re)hydrate from the file each time the section mounts - the panel is the
  // natural "is it up NOW?" checkpoint. load() is idempotent.
  useEffect(() => {
    void useVoice.getState().load();
  }, []);
  // Re-probe the SELECTED engine whenever it changes (and on mount, and after
  // load() adopts the file's engine): the one seam that keeps the voices list
  // in lockstep with the engine dropdown, flipping the degradation state both
  // ways.
  useEffect(() => {
    void useVoice.getState().refreshVoices();
  }, [engine]);
  // Self-heal a voice that isn't in the selected engine's list (switching
  // engines leaves a foreign voice behind, or a voice was uninstalled): adopt
  // the first available one so Test/announce target a real voice. Only fires
  // when the list actually loaded (server up), so a down server preserves the
  // persisted voice untouched.
  useEffect(() => {
    if (voices && voices.length > 0 && !voices.includes(voice)) {
      useVoice.getState().setVoice(voices[0]);
    }
  }, [voices, voice]);

  const activePort = ENGINES.find((e) => e.id === engine)?.port ?? 7477;

  // Every control except the master toggle AND the engine selector dims while
  // the selected engine's server is down (so the user can switch engines) or
  // while voice is off entirely (dependent-setting convention).
  const controlsDisabled = voicesUnavailable || !enabled;

  const testVoice = () => {
    setTesting(true);
    setTestError(null);
    const s = useVoice.getState();
    void synthesizeVoice(VOICE_TEST_PHRASE, s.voice, s.engine)
      .then((b64) => playWavBase64(b64, useVoice.getState().volume))
      .catch(() => setTestError("Synthesis failed - is the voice server running?"))
      .finally(() => setTesting(false));
  };

  return (
    <Group
      title="Voice announcements"
      description={
        `Spoken cues synthesized by a local TTS server (${engine}, 127.0.0.1:${activePort}). ` +
        "Settings persist to ~/.t-hub/voice.json and are shared with the captain's announce tooling."
      }
    >
      <SettingToggleRow
        label="Enable voice"
        hint="Master switch - nothing speaks while this is off."
        value={enabled}
        onChange={(v) => useVoice.getState().setEnabled(v)}
      />

      <Row label="Engine">
        <ThemeSelect
          value={engine}
          onChange={(v) => useVoice.getState().setEngine(v as VoiceEngine)}
          title="TTS backend - Piper (7477) or Kokoro (7478)"
          disabled={!enabled}
        >
          {ENGINES.map((e) => (
            <Opt key={e.id} value={e.id}>
              {e.label}
            </Opt>
          ))}
        </ThemeSelect>
      </Row>

      {voicesUnavailable && (
        <p
          role="status"
          className="text-xs leading-snug"
          style={{ color: "var(--th-dot-starting, #fbbf24)" }}
        >
          {engine === "kokoro" ? "Kokoro" : "Piper"} server unavailable - start
          the local {engine} TTS server on port {activePort} to pick a voice and
          test playback, or switch engines above.
        </p>
      )}

      <Row label="Voice">
        <ThemeSelect
          value={voice}
          onChange={(v) => useVoice.getState().setVoice(v)}
          title={`Installed ${engine} voice used for announcements`}
          disabled={controlsDisabled}
        >
          {/* Keep the persisted voice selectable even when the server list is
              absent OR no longer contains it (voice uninstalled / other
              engine), so the closed control always shows the value in use. */}
          {(voices && voices.length > 0
            ? voices.includes(voice)
              ? voices
              : [...voices, voice]
            : [voice]
          ).map((v) => (
            <Opt key={v} value={v}>
              {v}
            </Opt>
          ))}
        </ThemeSelect>
      </Row>

      <SettingSliderRow
        label="Volume"
        hint="Playback volume for announcements and the test phrase."
        value={Math.round(volume * 100)}
        min={0}
        max={100}
        step={5}
        suffix="%"
        onChange={(v) => useVoice.getState().setVolume(v / 100)}
        disabled={controlsDisabled}
      />

      <div className="flex items-center gap-3">
        <Btn
          onClick={testVoice}
          disabled={controlsDisabled || testing}
          title={`Speak "${VOICE_TEST_PHRASE}" with the selected voice`}
        >
          {testing ? "Speaking…" : "Test voice"}
        </Btn>
        {testError && (
          <span
            className="text-xs"
            style={{ color: "var(--th-dot-error, #f87171)" }}
          >
            {testError}
          </span>
        )}
      </div>

      <SettingToggleRow
        label="Announce when a session needs attention"
        hint={
          "Opt-in: speaks one short cue when a session enters needs-permission " +
          "or needs-question (bursts are debounced). Off by default."
        }
        value={announceOnAttention}
        onChange={(v) => useVoice.getState().setAnnounceOnAttention(v)}
        disabled={controlsDisabled}
      />
    </Group>
  );
}
