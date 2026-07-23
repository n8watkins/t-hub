// WAV playback for voice announcements: the backend TTS proxy returns base64
// WAV bytes; play them through an Audio element data URI. Distinct from
// lib/notify.ts's WebAudio-synthesized chimes - a WAV needs decoding, and the
// Audio element handles that (plus per-clip volume) in one line. Delivery
// failures reject with a stable kind so the caller can surface them.

export type VoiceAudioFailureKind = "playback" | "device";

export class VoiceAudioError extends Error {
  constructor(
    public readonly kind: VoiceAudioFailureKind,
    message: string,
  ) {
    super(message);
    this.name = "VoiceAudioError";
  }
}

function audioFailure(error: unknown): VoiceAudioError {
  const name =
    error && typeof error === "object" && "name" in error
      ? String(error.name)
      : "";
  const kind: VoiceAudioFailureKind =
    name === "NotFoundError" || name === "NotReadableError"
      ? "device"
      : "playback";
  const detail = error instanceof Error ? error.message : String(error);
  return new VoiceAudioError(kind, detail || "Audio playback failed");
}

/** Play base64 WAV bytes at `volume` (clamped 0..=1). */
export async function playWavBase64(
  b64: string,
  volume: number,
): Promise<void> {
  let audio: HTMLAudioElement;
  try {
    audio = new Audio(`data:audio/wav;base64,${b64}`);
    audio.volume = Math.max(0, Math.min(1, volume));
  } catch (error) {
    throw audioFailure(error);
  }
  try {
    await audio.play();
  } catch (error) {
    throw audioFailure(error);
  }
}
