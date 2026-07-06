// WAV playback for voice announcements: the backend TTS proxy returns base64
// WAV bytes; play them through an Audio element data URI. Distinct from
// lib/notify.ts's WebAudio-synthesized chimes - a WAV needs decoding, and the
// Audio element handles that (plus per-clip volume) in one line. Best-effort:
// no audio device / jsdom / autoplay refusal all degrade silently.

/** Play base64 WAV bytes at `volume` (clamped 0..=1). Fire-and-forget. */
export function playWavBase64(b64: string, volume: number): void {
  try {
    const audio = new Audio(`data:audio/wav;base64,${b64}`);
    audio.volume = Math.max(0, Math.min(1, volume));
    void audio.play().catch(() => {});
  } catch {
    // No Audio constructor (test env) or malformed data: stay silent.
  }
}
