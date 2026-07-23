import { afterEach, describe, expect, it, vi } from "vitest";
import { playWavBase64 } from "./voiceAudio";

afterEach(() => {
  vi.unstubAllGlobals();
});

describe("voice audio delivery", () => {
  it("awaits playback and applies clamped volume", async () => {
    const play = vi.fn(() => Promise.resolve());
    const instances: Array<{ volume: number }> = [];
    vi.stubGlobal(
      "Audio",
      class {
        volume = 0;
        play = play;

        constructor() {
          instances.push(this);
        }
      },
    );

    await playWavBase64("d2F2", 2);
    expect(play).toHaveBeenCalledTimes(1);
    expect(instances[0].volume).toBe(1);
  });

  it("classifies output device errors separately", async () => {
    const error = Object.assign(new Error("device missing"), {
      name: "NotFoundError",
    });
    vi.stubGlobal(
      "Audio",
      class {
        volume = 0;
        play = vi.fn(() => Promise.reject(error));
      },
    );

    await expect(playWavBase64("d2F2", 0.5)).rejects.toMatchObject({
      kind: "device",
      message: "device missing",
    });
  });

  it("classifies decoding and autoplay errors as playback failures", async () => {
    const error = Object.assign(new Error("unsupported clip"), {
      name: "NotSupportedError",
    });
    vi.stubGlobal(
      "Audio",
      class {
        volume = 0;
        play = vi.fn(() => Promise.reject(error));
      },
    );

    await expect(playWavBase64("bad", 0.5)).rejects.toMatchObject({
      kind: "playback",
      message: "unsupported clip",
    });
  });
});
