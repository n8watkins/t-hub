import { afterEach, describe, expect, it, vi } from "vitest";
import {
  playWavBase64,
  VOICE_PLAYBACK_TIMEOUT_MS,
} from "./voiceAudio";

class FakeAudio extends EventTarget {
  volume = 0;
  error: { code: number; message: string } | null = null;
  play = vi.fn(() => Promise.resolve());
}

afterEach(() => {
  vi.useRealTimers();
  vi.unstubAllGlobals();
});

function installAudio(audio: FakeAudio): void {
  vi.stubGlobal(
    "Audio",
    class {
      constructor() {
        return audio;
      }
    },
  );
}

describe("voice audio delivery", () => {
  it("resolves only after ended and applies clamped volume", async () => {
    const audio = new FakeAudio();
    installAudio(audio);
    let resolved = false;
    const playback = playWavBase64("d2F2", 2).then(() => {
      resolved = true;
    });
    await Promise.resolve();
    expect(resolved).toBe(false);
    expect(audio.play).toHaveBeenCalledTimes(1);
    expect(audio.volume).toBe(1);

    audio.dispatchEvent(new Event("ended"));
    await playback;
    expect(resolved).toBe(true);
  });

  it("classifies output device rejection separately", async () => {
    const audio = new FakeAudio();
    audio.play.mockRejectedValue(
      Object.assign(new Error("device missing"), { name: "NotFoundError" }),
    );
    installAudio(audio);

    await expect(playWavBase64("d2F2", 0.5)).rejects.toMatchObject({
      kind: "device",
      message: "device missing",
    });
  });

  it("observes media error and abort events", async () => {
    const errored = new FakeAudio();
    errored.error = { code: 3, message: "decode failed" };
    installAudio(errored);
    const errorPlayback = playWavBase64("bad", 0.5);
    errored.dispatchEvent(new Event("error"));
    await expect(errorPlayback).rejects.toMatchObject({
      kind: "playback",
      message: "decode failed",
    });

    const aborted = new FakeAudio();
    installAudio(aborted);
    const abortedPlayback = playWavBase64("d2F2", 0.5);
    aborted.dispatchEvent(new Event("abort"));
    await expect(abortedPlayback).rejects.toMatchObject({
      kind: "playback",
      message: "Audio playback was aborted",
    });
  });

  it("rejects playback that never reaches a terminal event", async () => {
    vi.useFakeTimers();
    const audio = new FakeAudio();
    installAudio(audio);
    const playback = playWavBase64("d2F2", 0.5);
    const rejection = expect(playback).rejects.toMatchObject({
      kind: "playback",
      message: expect.stringContaining("did not end"),
    });
    await vi.advanceTimersByTimeAsync(VOICE_PLAYBACK_TIMEOUT_MS);
    await rejection;
  });
});
