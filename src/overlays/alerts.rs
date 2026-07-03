//! Sound chimes + OS notifications (T-B): the native half of the webview's
//! `lib/notify.ts`, closing the two §1.4 parity rows the T9 toasts left open.
//!
//! The webview SYNTHESIZES its chimes with WebAudio (no bundled assets) and
//! sends OS toasts through the Tauri notification plugin. Native port, same
//! shape, zero new build dependencies:
//!  - [`chime_samples`] is a pure sample synthesizer (the exact notify.ts tone
//!    recipes: sine oscillators, 12ms exponential attack, exponential release),
//!    unit-tested under `--no-default-features`;
//!  - [`wav_bytes`] wraps samples in a 16-bit PCM WAV image (pure, tested);
//!  - playback + notification are std-only shell-outs / FFI per platform:
//!    unix plays through `paplay` (WSLg's Pulse socket) with `pw-play`/`aplay`
//!    fallbacks and notifies via `notify-send`; Windows plays in-memory WAV
//!    through winmm's `PlaySoundW` and toasts via the PowerShell WinRT
//!    one-liner (the same APP_ID trick the Tauri plugin's backend uses).
//!
//! Trigger: [`super::feed::OverlayFeed`]'s event thread calls [`alert`] for
//! every FRESH toast the fold enqueues, so chimes/notifications inherit the
//! toasts' dedup, warmup replay-suppression, and active-tab suppression (the
//! last is a small parity deviation - the webview chimes even for the session
//! you are looking at - documented in §5). Env gates until the T-C settings
//! hub exists: `THN_SOUND=0` mutes chimes, `THN_NOTIFY=0` mutes OS toasts
//! (webview `soundsEnabled`/`notificationsEnabled` equivalents, default on).
//! Headless builds (`--no-default-features`) compile [`alert`] to a no-op so
//! the probe binaries never beep.

use super::toasts::ToastKind;

/// Synth sample rate. 44.1kHz mono - small buffers (~12k samples per chime).
pub const SAMPLE_RATE: u32 = 44_100;

/// One short tone in a chime: frequency (Hz), start offset (s), duration (s)
/// (webview `Tone`).
struct Tone {
    freq: f32,
    at: f32,
    dur: f32,
}

/// Per-kind chime recipes - notify.ts `CHIMES`, verbatim. Short and quiet:
/// ambient cues, not alarms.
fn tones(kind: ToastKind) -> [Tone; 2] {
    match kind {
        // Soft "someone needs you" - gentle two-note rise.
        ToastKind::Attention => [
            Tone { freq: 660.0, at: 0.0, dur: 0.12 },
            Tone { freq: 880.0, at: 0.1, dur: 0.16 },
        ],
        // Pleasant "turn finished" - bright major third.
        ToastKind::Done => [
            Tone { freq: 784.0, at: 0.0, dur: 0.12 },
            Tone { freq: 988.0, at: 0.1, dur: 0.18 },
        ],
        // Alert "something failed" - lower descending pair, a touch louder.
        ToastKind::Error => [
            Tone { freq: 440.0, at: 0.0, dur: 0.16 },
            Tone { freq: 330.0, at: 0.14, dur: 0.22 },
        ],
    }
}

/// notify.ts `PEAK_GAIN`.
fn peak_gain(kind: ToastKind) -> f32 {
    match kind {
        ToastKind::Attention => 0.18,
        ToastKind::Done => 0.2,
        ToastKind::Error => 0.28,
    }
}

/// The WebAudio envelope floor (`setValueAtTime(0.0001, ...)`).
const ENV_FLOOR: f32 = 0.0001;
/// Attack length (`exponentialRampToValueAtTime(peak, start + 0.012)`).
const ATTACK_S: f32 = 0.012;
/// Post-release tail (`osc.stop(end + 0.02)`).
const TAIL_S: f32 = 0.02;

/// WebAudio `exponentialRampToValueAtTime` between (t0,v0) and (t1,v1):
/// v(t) = v0 * (v1/v0)^((t-t0)/(t1-t0)). Both values must be positive.
fn exp_ramp(v0: f32, v1: f32, frac: f32) -> f32 {
    v0 * (v1 / v0).powf(frac)
}

/// Synthesize the chime for `kind` as mono f32 samples in [-1, 1] at
/// [`SAMPLE_RATE`] - the notify.ts recipe: per tone a sine oscillator through a
/// gain with a 12ms exponential attack to the kind's peak and an exponential
/// release to the floor at the tone's end, all tones summed.
pub fn chime_samples(kind: ToastKind) -> Vec<f32> {
    let tones = tones(kind);
    let peak = peak_gain(kind);
    let total_s = tones
        .iter()
        .map(|t| t.at + t.dur)
        .fold(0.0_f32, f32::max)
        + TAIL_S;
    let n = (total_s * SAMPLE_RATE as f32).ceil() as usize;
    let mut out = vec![0.0_f32; n];
    for tone in &tones {
        let attack = ATTACK_S.min(tone.dur);
        for (i, sample) in out.iter_mut().enumerate() {
            let t = i as f32 / SAMPLE_RATE as f32;
            let dt = t - tone.at;
            if dt < 0.0 || dt >= tone.dur {
                continue;
            }
            let env = if dt < attack {
                exp_ramp(ENV_FLOOR, peak, dt / attack)
            } else {
                exp_ramp(peak, ENV_FLOOR, (dt - attack) / (tone.dur - attack))
            };
            *sample += (std::f32::consts::TAU * tone.freq * dt).sin() * env;
        }
    }
    out
}

/// Wrap mono f32 samples as a complete in-memory 16-bit PCM WAV image
/// (RIFF/WAVE/fmt/data) - what `paplay` reads from a file and `PlaySoundW`
/// plays straight from memory.
pub fn wav_bytes(samples: &[f32], sample_rate: u32) -> Vec<u8> {
    let data_len = (samples.len() * 2) as u32;
    let mut out = Vec::with_capacity(44 + samples.len() * 2);
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&(36 + data_len).to_le_bytes());
    out.extend_from_slice(b"WAVE");
    out.extend_from_slice(b"fmt ");
    out.extend_from_slice(&16_u32.to_le_bytes()); // PCM fmt chunk size
    out.extend_from_slice(&1_u16.to_le_bytes()); // audio format: PCM
    out.extend_from_slice(&1_u16.to_le_bytes()); // channels: mono
    out.extend_from_slice(&sample_rate.to_le_bytes());
    out.extend_from_slice(&(sample_rate * 2).to_le_bytes()); // byte rate
    out.extend_from_slice(&2_u16.to_le_bytes()); // block align
    out.extend_from_slice(&16_u16.to_le_bytes()); // bits per sample
    out.extend_from_slice(b"data");
    out.extend_from_slice(&data_len.to_le_bytes());
    for s in samples {
        out.extend_from_slice(&((s.clamp(-1.0, 1.0) * i16::MAX as f32) as i16).to_le_bytes());
    }
    out
}

/// The env-gate rule shared by both toggles: unset/anything => on, `"0"` => off
/// (mirrors the other `THN_*` opt-outs). Pure for testability.
pub fn enabled_from(var: Option<&str>) -> bool {
    var != Some("0")
}

/// `THN_SOUND` gate (webview `soundsEnabled`, default on).
pub fn sounds_enabled() -> bool {
    enabled_from(std::env::var("THN_SOUND").ok().as_deref())
}

/// `THN_NOTIFY` gate (webview `notificationsEnabled`, default on).
pub fn notifications_enabled() -> bool {
    enabled_from(std::env::var("THN_NOTIFY").ok().as_deref())
}

/// Fire the chime + OS notification for one fresh toast (webview `notify()`:
/// both halves independently respect their toggle). Runs on a short-lived
/// background thread - playback blocks for the chime's ~0.3s and the notifier
/// can block on its transport; the caller is the feed's event drainer, which
/// must never stall. Headless builds compile this away entirely.
#[cfg(feature = "gui")]
pub fn alert(kind: ToastKind, title: &str, body: &str) {
    let (title, body) = (title.to_string(), body.to_string());
    let sound = sounds_enabled();
    let notify = notifications_enabled();
    if !sound && !notify {
        return;
    }
    std::thread::Builder::new()
        .name("t-hub-native-alert".into())
        .spawn(move || {
            if sound {
                io::play_chime(kind);
            }
            if notify {
                io::os_notify(&title, &body);
            }
        })
        .ok();
}

/// Headless twin: the probe binaries fold the same toasts but never beep.
#[cfg(not(feature = "gui"))]
pub fn alert(_kind: ToastKind, _title: &str, _body: &str) {}

/// The platform I/O half (gui builds only). Failures are logged at debug and
/// swallowed - a missing player/daemon must never take the feed down, exactly
/// like the webview's graceful sound-only / silent fallbacks.
#[cfg(feature = "gui")]
mod io {
    use super::{chime_samples, wav_bytes, ToastKind, SAMPLE_RATE};

    #[cfg(unix)]
    pub fn play_chime(kind: ToastKind) {
        use std::io::Write as _;
        // Write the WAV once per (kind, process) and reuse it: the recipes are
        // static, and paplay wants a file (no reliable stdin format sniffing
        // across fallback players).
        let name = match kind {
            ToastKind::Attention => "attention",
            ToastKind::Done => "done",
            ToastKind::Error => "error",
        };
        let path = std::env::temp_dir()
            .join(format!("t-hub-native-chime-{name}-{}.wav", std::process::id()));
        if !path.exists() {
            let bytes = wav_bytes(&chime_samples(kind), SAMPLE_RATE);
            let ok = std::fs::File::create(&path)
                .and_then(|mut f| f.write_all(&bytes))
                .is_ok();
            if !ok {
                log::debug!("chime: could not write {}", path.display());
                return;
            }
        }
        // WSLg routes Pulse to the Windows side; pw-play/aplay cover native
        // Linux boxes. First player that exits 0 wins.
        for player in ["paplay", "pw-play", "aplay"] {
            match std::process::Command::new(player)
                .arg(&path)
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .status()
            {
                Ok(st) if st.success() => return,
                _ => continue,
            }
        }
        log::debug!("chime: no audio player succeeded (tried paplay/pw-play/aplay)");
    }

    #[cfg(windows)]
    pub fn play_chime(kind: ToastKind) {
        // winmm ships with Windows; SND_MEMORY plays the WAV image directly
        // from the buffer (kept alive by the synchronous call).
        #[link(name = "winmm")]
        extern "system" {
            fn PlaySoundW(psz: *const u8, hmod: *mut core::ffi::c_void, fdw: u32) -> i32;
        }
        const SND_SYNC: u32 = 0x0000;
        const SND_NODEFAULT: u32 = 0x0002;
        const SND_MEMORY: u32 = 0x0004;
        let bytes = wav_bytes(&chime_samples(kind), SAMPLE_RATE);
        unsafe {
            PlaySoundW(
                bytes.as_ptr(),
                core::ptr::null_mut(),
                SND_SYNC | SND_NODEFAULT | SND_MEMORY,
            );
        }
    }

    #[cfg(unix)]
    pub fn os_notify(title: &str, body: &str) {
        // Desktop-standard; degrades silently where no notification daemon
        // exists (e.g. WSLg today - the toast cards remain the visible cue).
        let _ = std::process::Command::new("notify-send")
            .args(["-a", "T-Hub", title, body])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status();
    }

    #[cfg(windows)]
    pub fn os_notify(title: &str, body: &str) {
        // The PowerShell WinRT toast one-liner, under the PowerShell APP_ID -
        // the same registered-AppId trick tauri-winrt-notification (the Tauri
        // plugin's backend) uses, without adding the dependency. Title/body are
        // XML-escaped and single-quote-doubled for the PS string literal.
        fn esc(s: &str) -> String {
            s.replace('&', "&amp;")
                .replace('<', "&lt;")
                .replace('>', "&gt;")
                .replace('\'', "''")
        }
        let script = format!(
            "$null = [Windows.UI.Notifications.ToastNotificationManager, Windows.UI.Notifications, ContentType = WindowsRuntime]; \
             $null = [Windows.Data.Xml.Dom.XmlDocument, Windows.Data.Xml.Dom.XmlDocument, ContentType = WindowsRuntime]; \
             $xml = New-Object Windows.Data.Xml.Dom.XmlDocument; \
             $xml.LoadXml('<toast><visual><binding template=\"ToastGeneric\"><text>{}</text><text>{}</text></binding></visual></toast>'); \
             $toast = New-Object Windows.UI.Notifications.ToastNotification($xml); \
             [Windows.UI.Notifications.ToastNotificationManager]::CreateToastNotifier('{{1AC14E77-02E7-4E5D-B744-2EB1AE5198B7}}\\WindowsPowerShell\\v1.0\\powershell.exe').Show($toast)",
            esc(title),
            esc(body)
        );
        let _ = std::process::Command::new("powershell.exe")
            .args(["-NoProfile", "-NonInteractive", "-WindowStyle", "Hidden", "-Command", &script])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chimes_match_the_notify_ts_recipes() {
        // Lengths: last tone's end + the 20ms stop tail, at 44.1kHz.
        for (kind, end_s) in [
            (ToastKind::Attention, 0.1 + 0.16),
            (ToastKind::Done, 0.1 + 0.18),
            (ToastKind::Error, 0.14 + 0.22),
        ] {
            let samples = chime_samples(kind);
            let expect = ((end_s + TAIL_S) * SAMPLE_RATE as f32).ceil() as usize;
            assert_eq!(samples.len(), expect, "{kind:?} length");
        }
    }

    #[test]
    fn chime_envelope_peaks_at_the_kind_gain_and_starts_ends_silent() {
        for kind in [ToastKind::Attention, ToastKind::Done, ToastKind::Error] {
            let samples = chime_samples(kind);
            let peak = samples.iter().fold(0.0_f32, |m, s| m.max(s.abs()));
            let gain = peak_gain(kind);
            // Two overlapping tones can sum slightly above one tone's peak,
            // but never above 2x; the envelope keeps each tone at its gain.
            assert!(peak <= 2.0 * gain, "{kind:?} peak {peak} vs gain {gain}");
            assert!(peak >= 0.5 * gain, "{kind:?} inaudible: peak {peak}");
            // Starts at the envelope floor and ends released.
            assert!(samples[0].abs() < 0.002, "{kind:?} clicks at start");
            assert!(samples[samples.len() - 1].abs() < 0.002, "{kind:?} clicks at end");
        }
    }

    #[test]
    fn error_chime_is_the_loudest_and_kinds_differ() {
        let quiet = chime_samples(ToastKind::Attention);
        let loud = chime_samples(ToastKind::Error);
        let p = |v: &[f32]| v.iter().fold(0.0_f32, |m, s| m.max(s.abs()));
        assert!(p(&loud) > p(&quiet));
        assert_ne!(quiet.len(), loud.len());
    }

    #[test]
    fn wav_image_is_valid_pcm16_mono() {
        let samples = [0.0_f32, 0.5, -0.5, 1.0, -1.0, 2.0]; // 2.0 clamps
        let wav = wav_bytes(&samples, SAMPLE_RATE);
        assert_eq!(wav.len(), 44 + samples.len() * 2);
        assert_eq!(&wav[0..4], b"RIFF");
        assert_eq!(&wav[8..12], b"WAVE");
        assert_eq!(&wav[12..16], b"fmt ");
        assert_eq!(&wav[36..40], b"data");
        let data_len = u32::from_le_bytes(wav[40..44].try_into().unwrap());
        assert_eq!(data_len as usize, samples.len() * 2);
        let riff_len = u32::from_le_bytes(wav[4..8].try_into().unwrap());
        assert_eq!(riff_len, 36 + data_len);
        // Sample encoding: 0 -> 0, 1.0 -> i16::MAX, out-of-range clamps.
        let s = |i: usize| i16::from_le_bytes(wav[44 + 2 * i..46 + 2 * i].try_into().unwrap());
        assert_eq!(s(0), 0);
        assert_eq!(s(3), i16::MAX);
        assert_eq!(s(4), -i16::MAX);
        assert_eq!(s(5), i16::MAX);
    }

    #[test]
    fn env_gates_default_on_and_zero_disables() {
        assert!(enabled_from(None));
        assert!(enabled_from(Some("1")));
        assert!(enabled_from(Some("")));
        assert!(!enabled_from(Some("0")));
    }
}
