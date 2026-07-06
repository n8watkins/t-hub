// Settings > Voice (its own file so tests can render it without the whole
// ThemeEditor): controls for spoken announcements backed by ~/.t-hub/voice.json
// (the FILE is the source of truth - external captain tooling reads it too)
// and the local Piper TTS server, reached ONLY through the backend proxy
// (src-tauri/src/voice.rs) because the server rejects browser-Origin requests.
//
// Degradation contract: when the /voices proxy fails (server down) the section
// shows an "unavailable" hint and disables every control EXCEPT the master
// enable toggle, so the persisted intent can still be flipped while the server
// is offline.
import { useEffect, useState } from "react";
import { useVoice } from "../store/voice";
import { synthesizeVoice } from "../ipc/voice";
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

export function VoiceSection() {
  const enabled = useVoice((s) => s.enabled);
  const voice = useVoice((s) => s.voice);
  const volume = useVoice((s) => s.volume);
  const announceOnAttention = useVoice((s) => s.announceOnAttention);
  const voices = useVoice((s) => s.voices);
  const voicesUnavailable = useVoice((s) => s.voicesUnavailable);

  // One test synthesis in flight at a time; surface a failure inline (the
  // server can vanish between the voices fetch and the click).
  const [testing, setTesting] = useState(false);
  const [testError, setTestError] = useState<string | null>(null);

  // (Re)hydrate from the file and re-probe the server each time the section
  // mounts - the panel is the natural "is it up NOW?" checkpoint. load() is
  // idempotent; refreshVoices flips the degradation state both ways.
  useEffect(() => {
    const s = useVoice.getState();
    void s.load().then(() => s.refreshVoices());
  }, []);

  // Every control except the master toggle dims while the server is down
  // (spec) or while voice is off entirely (dependent-setting convention).
  const controlsDisabled = voicesUnavailable || !enabled;

  const testVoice = () => {
    setTesting(true);
    setTestError(null);
    void synthesizeVoice(VOICE_TEST_PHRASE, voice)
      .then((b64) => playWavBase64(b64, useVoice.getState().volume))
      .catch(() => setTestError("Synthesis failed - is the voice server running?"))
      .finally(() => setTesting(false));
  };

  return (
    <Group
      title="Voice announcements"
      description={
        "Spoken cues synthesized by the local Piper TTS server (127.0.0.1:7477). " +
        "Settings persist to ~/.t-hub/voice.json and are shared with the captain's announce tooling."
      }
    >
      <SettingToggleRow
        label="Enable voice"
        hint="Master switch - nothing speaks while this is off."
        value={enabled}
        onChange={(v) => useVoice.getState().setEnabled(v)}
      />

      {voicesUnavailable && (
        <p
          role="status"
          className="text-xs leading-snug"
          style={{ color: "var(--th-dot-starting, #fbbf24)" }}
        >
          Voice server unavailable - start the local Piper TTS server to pick a
          voice and test playback.
        </p>
      )}

      <Row label="Voice">
        <ThemeSelect
          value={voice}
          onChange={(v) => useVoice.getState().setVoice(v)}
          title="Installed Piper voice used for announcements"
          disabled={controlsDisabled}
        >
          {/* Keep the persisted voice selectable even when the server list is
              absent OR no longer contains it (voice uninstalled), so the
              closed control always shows the value actually in use. */}
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
