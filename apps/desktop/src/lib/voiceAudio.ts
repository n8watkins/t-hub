// WAV playback for voice announcements: the backend TTS proxy returns base64
// WAV bytes; play them through an Audio element data URI. Distinct from
// lib/notify.ts's WebAudio-synthesized chimes - a WAV needs decoding, and the
// Audio element handles that (plus per-clip volume) in one line. Delivery
// failures reject with a stable kind so the caller can surface them.

export type VoiceAudioFailureKind = "playback" | "device";
export const VOICE_PLAYBACK_TIMEOUT_MS = 30000;

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
  timeoutMs: number = VOICE_PLAYBACK_TIMEOUT_MS,
): Promise<void> {
  let audio: HTMLAudioElement;
  try {
    audio = new Audio(`data:audio/wav;base64,${b64}`);
    audio.volume = Math.max(0, Math.min(1, volume));
  } catch (error) {
    throw audioFailure(error);
  }
  await new Promise<void>((resolve, reject) => {
    let settled = false;
    const finish = (error?: VoiceAudioError) => {
      if (settled) return;
      settled = true;
      clearTimeout(timeout);
      audio.removeEventListener("ended", onEnded);
      audio.removeEventListener("error", onError);
      audio.removeEventListener("abort", onAbort);
      if (error) reject(error);
      else resolve();
    };
    const onEnded = () => finish();
    const onError = () => {
      const code = audio.error?.code;
      const detail = audio.error?.message || `Audio media error${code ? ` ${code}` : ""}`;
      finish(new VoiceAudioError("playback", detail));
    };
    const onAbort = () =>
      finish(new VoiceAudioError("playback", "Audio playback was aborted"));
    const timeout = setTimeout(
      () =>
        finish(
          new VoiceAudioError(
            "playback",
            `Audio playback did not end within ${timeoutMs}ms`,
          ),
        ),
      timeoutMs,
    );
    audio.addEventListener("ended", onEnded, { once: true });
    audio.addEventListener("error", onError, { once: true });
    audio.addEventListener("abort", onAbort, { once: true });
    try {
      void audio.play().catch((error) => finish(audioFailure(error)));
    } catch (error) {
      finish(audioFailure(error));
    }
  });
}
