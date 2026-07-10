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
//
// Health visibility (never silent): a kokoro death once fell back to piper/SAPI
// with zero surfacing - the general noticed only by ear. So the section probes
// BOTH engines' /health (bounded, on open + a slow periodic tick) and shows a
// live reachability line for each; when the SELECTED engine is unreachable it
// raises a prominent error (announcements will not be spoken) with a switch/
// start prompt. The "we just tried and couldn't" event surfaces separately as a
// chime+toast from the announce path (lib/voiceAnnounce.ts).
import { useEffect, useState } from "react";
import { useVoice, type EngineHealthStatus } from "../store/voice";
import { useEngineRuntime } from "../store/engineRuntime";
import { effectiveTarget } from "../ipc/engine";
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

/** Re-probe both engines' health this often while the panel is open. Slow on
 *  purpose - this is an ambient "is it still up?" check, not a hot loop, and
 *  each probe is already bounded backend-side (2s/engine). */
export const HEALTH_PROBE_INTERVAL_MS = 15000;

/** Engine display label for the managed banner. */
function engineLabel(id: VoiceEngine): string {
  return ENGINES.find((e) => e.id === id)?.label ?? id;
}

/** Human label + dot color for an engine's reachability state. */
function healthPresentation(status: EngineHealthStatus): {
  label: string;
  color: string;
} {
  switch (status) {
    case "up":
      return { label: "reachable", color: "var(--th-dot-live, #4ade80)" };
    case "down":
      return { label: "unreachable", color: "var(--th-dot-error, #f87171)" };
    default:
      return { label: "checking…", color: "var(--th-dot-detached, #9ca3af)" };
  }
}

export function VoiceSection() {
  const enabled = useVoice((s) => s.enabled);
  const engine = useVoice((s) => s.engine);
  const voice = useVoice((s) => s.voice);
  const volume = useVoice((s) => s.volume);
  const announceOnAttention = useVoice((s) => s.announceOnAttention);
  const voices = useVoice((s) => s.voices);
  const voicesUnavailable = useVoice((s) => s.voicesUnavailable);
  const health = useVoice((s) => s.health);
  // Managed lifecycle: when the supervisor is running (flag on) it is the
  // authoritative source for the active-vs-selected engine + degraded level, so
  // its banner supersedes the #52 direct-probe error below. null / managed:false
  // = unmanaged, and the #52 UI stands unchanged.
  const runtime = useEngineRuntime((s) => s.status);
  const managed = !!runtime?.managed;

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
  // Probe BOTH engines' health on open, then on a slow interval while the panel
  // stays mounted - so a silent engine death is SEEN here, not heard later. The
  // probe fans out one bounded backend call per engine; the interval is cleared
  // on unmount so nothing polls once Settings closes.
  useEffect(() => {
    const probe = () => void useVoice.getState().probeHealth();
    probe();
    const id = setInterval(probe, HEALTH_PROBE_INTERVAL_MS);
    return () => clearInterval(id);
  }, []);
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
  const selLabel = ENGINES.find((e) => e.id === engine)?.label ?? engine;
  const otherEngine = ENGINES.find((e) => e.id !== engine);

  // The SELECTED engine is unreachable: announcements can't be spoken. Keyed on
  // the direct /health probe, with the /voices failure as a same-signal fallback
  // for the window before the first health probe resolves. Suppressed while the
  // managed lifecycle runs - its banner is the authoritative degraded surface,
  // so the two never double-message.
  const selectedDown =
    !managed && (health[engine] === "down" || voicesUnavailable);
  // Offer the other engine as an escape hatch only when it's actually up.
  const otherUp = otherEngine ? health[otherEngine.id] === "up" : false;

  // Every control except the master toggle AND the engine selector dims while
  // the selected engine's server is down (so the user can switch engines) or
  // while voice is off entirely (dependent-setting convention).
  const controlsDisabled = voicesUnavailable || !enabled;

  const testVoice = () => {
    setTesting(true);
    setTestError(null);
    const s = useVoice.getState();
    // Test what the general would actually HEAR: if the managed lifecycle has
    // fallen back, route to the active engine + its valid voice (unmanaged =
    // the selected engine + voice, unchanged).
    const target = effectiveTarget(runtime, s.engine, s.voice);
    void synthesizeVoice(VOICE_TEST_PHRASE, target.voice, target.engine)
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

      {/* Managed lifecycle banner (green/amber/red): the authoritative degraded
          surface when T-Hub owns the engine lifecycle. Amber = voice is flowing
          on the fallback engine; it auto-returns when the selected engine
          recovers. Absent entirely when the managed lifecycle is off. */}
      {enabled && managed && runtime && (
        <p
          role={runtime.level === "green" ? "status" : "alert"}
          className="text-xs leading-snug"
          style={{
            color:
              runtime.level === "green"
                ? "var(--th-dot-live, #4ade80)"
                : runtime.level === "red"
                  ? "var(--th-dot-error, #f87171)"
                  : "var(--th-dot-starting, #fbbf24)",
          }}
        >
          {runtime.level === "green" &&
            `Voice engine healthy — ${engineLabel(runtime.activeEngine)} is active.`}
          {runtime.level === "amber" &&
            `Running on fallback — ${engineLabel(runtime.activeEngine)} is carrying voice while ${engineLabel(runtime.selectedEngine)} is unreachable. It will switch back automatically once ${engineLabel(runtime.selectedEngine)} recovers.`}
          {runtime.level === "red" &&
            "Voice unavailable — both engines are down. T-Hub is retrying."}
          {runtime.level === "unknown" && "Checking engine status…"}
        </p>
      )}

      {/* Dual-engine health: BOTH engines' reachability, always visible while
          voice is on, so a silent death of EITHER server is seen here. */}
      {enabled && (
        <div
          role="status"
          aria-label="TTS engine health"
          className="flex flex-col gap-1 text-xs leading-snug"
        >
          {ENGINES.map((e) => {
            const { label, color } = healthPresentation(health[e.id]);
            const isSelected = e.id === engine;
            return (
              <div key={e.id} className="flex items-center gap-2">
                <span
                  aria-hidden
                  className="inline-block h-2 w-2 shrink-0 rounded-full"
                  style={{ backgroundColor: color }}
                />
                <span style={{ opacity: 0.85 }}>
                  {e.label} (127.0.0.1:{e.port}): {label}
                  {isSelected && " — selected"}
                </span>
              </div>
            );
          })}
        </div>
      )}

      {/* Prominent error when the SELECTED engine is unreachable: nothing will
          be spoken. This is the hard "never a silent default" requirement. */}
      {enabled && selectedDown && (
        <p
          role="alert"
          className="text-xs leading-snug"
          style={{ color: "var(--th-dot-error, #f87171)" }}
        >
          {selLabel} (the selected engine) is unreachable on 127.0.0.1:
          {activePort} — attention announcements will NOT be spoken.{" "}
          {otherUp
            ? `Switch to ${otherEngine?.label} above (it's reachable), or start the ${engine} TTS server on port ${activePort}.`
            : `Start the ${engine} TTS server on port ${activePort}.`}
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
