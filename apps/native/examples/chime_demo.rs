//! Manual QA for the T-B chimes: synthesize the three notify.ts recipes with
//! the REAL production synth ([`alerts::chime_samples`] + [`alerts::wav_bytes`])
//! and play each through the same player chain the unix `alert()` path uses.
//! Headless (no gpui) - runs from WSL, where WSLg's Pulse socket makes the
//! chimes audible on the Windows side.
//!
//! Run: `cargo run --example chime_demo --no-default-features`

use std::io::Write as _;

use t_hub_native::overlays::alerts::{chime_samples, wav_bytes, SAMPLE_RATE};
use t_hub_native::overlays::toasts::ToastKind;

fn main() {
    for (kind, name) in [
        (ToastKind::Attention, "attention"),
        (ToastKind::Done, "done"),
        (ToastKind::Error, "error"),
    ] {
        let samples = chime_samples(kind);
        let wav = wav_bytes(&samples, SAMPLE_RATE);
        let path = std::env::temp_dir().join(format!("t-hub-chime-demo-{name}.wav"));
        std::fs::File::create(&path)
            .and_then(|mut f| f.write_all(&wav))
            .expect("write demo wav");
        let mut played = false;
        for player in ["paplay", "pw-play", "aplay"] {
            match std::process::Command::new(player).arg(&path).status() {
                Ok(st) if st.success() => {
                    println!("CHIME-OK {name}: {} samples via {player}", samples.len());
                    played = true;
                    break;
                }
                _ => continue,
            }
        }
        if !played {
            println!("CHIME-SILENT {name}: no player succeeded (wav at {})", path.display());
        }
        std::thread::sleep(std::time::Duration::from_millis(250));
    }
}
