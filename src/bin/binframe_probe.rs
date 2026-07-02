//! Headless acceptance + bandwidth runner for T13b: the native `wire/` speaking
//! v2 BINARY PTY framing, with the v1 path as the A/B baseline.
//!
//! The Rust sibling of `scripts/probes/t13_binframe.py`, driven through the REAL
//! `ControlClient`/`PtyHandle` (not a hand-rolled socket), so it proves the code
//! the GPUI client actually runs. Against a control server (normally the headless
//! `control_probe_server` example - point `T_HUB_CONTROL_JSON` at its handshake),
//! on ONE disposable tmux session it proves:
//!
//!   1. `attach_pty` negotiates V2Binary; the binary scrollback carries the seed;
//!      write and resize round-trip over binary frames.
//!   2. A firehose (`seq 1 50000`) measured on the REAL inbound wire (raw socket
//!      bytes, counted inside the reader) vs the decoded payload bytes.
//!   3. `attach_pty_v1` (forced v1) still works against the same server, and the
//!      same firehose measured over v1 gives the baseline.
//!
//! It prints both runs' wire/payload numbers, each run's framing tax, and the v2
//! reduction - both exact (pricing v1 for the very frames v2 carried) and as the
//! measured cross-check. Only ever touches its own disposable session.

use std::process::Command;
use std::time::{Duration, Instant};

use t_hub_native::wire::{ControlClient, PtyFrame, PtyFraming, PtyHandle};

const SESSION_ID: &str = "t13b-native"; // tmux session becomes th_t13b-native
const TMUX_SESSION: &str = "th_t13b-native";

fn tmux(args: &[&str]) -> std::process::Output {
    Command::new("tmux")
        .args(["-L", "t-hub"])
        .args(args)
        .output()
        .expect("run tmux")
}

fn geometry() -> String {
    let out = tmux(&["list-panes", "-t", TMUX_SESSION, "-F", "#{pane_width}x#{pane_height}"]);
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

/// Type `echo <marker>` and poll capture-pane until it renders twice (command
/// line + output), so a cold shell can't race the attach. Mirrors the python
/// probe's `seed_marker`.
fn seed_marker(marker: &str) -> bool {
    for _ in 0..20 {
        tmux(&["send-keys", "-t", TMUX_SESSION, &format!("echo {marker}"), "Enter"]);
        for _ in 0..5 {
            std::thread::sleep(Duration::from_millis(250));
            let cap = tmux(&["capture-pane", "-p", "-t", TMUX_SESSION]);
            let text = String::from_utf8_lossy(&cap.stdout).to_string();
            if text.matches(marker).count() >= 2 {
                return true;
            }
        }
    }
    false
}

/// Drain output frames until `needle` appears, then keep draining through a
/// short quiet period (the trailing redraw frames). Returns (accumulated bytes,
/// per-frame payload lengths).
fn drain_until(handle: &PtyHandle, needle: &[u8], timeout: Duration) -> (Vec<u8>, Vec<usize>) {
    let mut acc: Vec<u8> = Vec::new();
    let mut lens = Vec::new();
    let deadline = Instant::now() + timeout;
    let mut seen_at: Option<Instant> = None;
    loop {
        if Instant::now() > deadline {
            break;
        }
        if let Some(t) = seen_at {
            if t.elapsed() > Duration::from_secs(1) {
                break; // 1s quiet after the marker: the redraw tail has settled
            }
        }
        match handle.output.recv_timeout(Duration::from_millis(300)) {
            Ok(PtyFrame::Out(bytes)) => {
                lens.push(bytes.len());
                acc.extend_from_slice(&bytes);
                if seen_at.is_none()
                    && acc.windows(needle.len()).any(|w| w == needle)
                {
                    seen_at = Some(Instant::now());
                }
            }
            Ok(PtyFrame::Exit(_)) => break,
            Err(_) => {
                if seen_at.is_some() {
                    break;
                }
            }
        }
    }
    (acc, lens)
}

struct FirehoseRun {
    frames: usize,
    payload: u64,
    wire: u64,
}

/// Run one `seq 1 50000` firehose through `handle`, measuring the inbound wire
/// via the handle's counters. `marker` must be unique per run.
fn firehose(handle: &PtyHandle, marker: &str) -> (FirehoseRun, Vec<usize>) {
    let wire_before = handle.wire_bytes_in();
    let payload_before = handle.payload_bytes_in();
    handle.write(format!("seq 1 50000; echo {marker}\r").as_bytes());
    let (_, lens) = drain_until(handle, marker.as_bytes(), Duration::from_secs(40));
    (
        FirehoseRun {
            frames: lens.len(),
            payload: handle.payload_bytes_in() - payload_before,
            wire: handle.wire_bytes_in() - wire_before,
        },
        lens,
    )
}

fn main() {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("warn")).init();
    let mut failures = 0;
    let mut check = |label: &str, ok: bool| {
        println!("  [{}] {label}", if ok { "PASS" } else { "FAIL" });
        if !ok {
            failures += 1;
        }
    };

    // --- setup: endpoint + one disposable session ---------------------------
    let ep = ControlClient::discover().expect("discover control endpoint");
    println!(
        "[setup] endpoint {} (advertised protocol_version: {:?})",
        ep.addr, ep.protocol_version
    );
    let client = ControlClient::connect(ep).expect("connect control socket");

    let existing = String::from_utf8_lossy(&tmux(&["ls"]).stdout).to_string();
    if existing.contains(TMUX_SESSION) {
        eprintln!("[setup] {TMUX_SESSION} already exists; aborting to avoid clobbering it");
        std::process::exit(2);
    }
    let r = tmux(&["new-session", "-d", "-s", TMUX_SESSION, "-c", "/tmp", "-x", "100", "-y", "30"]);
    assert!(r.status.success(), "new-session failed: {}", String::from_utf8_lossy(&r.stderr));
    let seeded = seed_marker("T13B-SEED-MARKER");
    println!("[setup] created {TMUX_SESSION}; geometry {}; seed rendered: {seeded}", geometry());

    // === (1) v2 binary attach: negotiation + round-trips ====================
    println!("\n[v2] auto-negotiated attach");
    let h2 = client.attach_pty(SESSION_ID, 100, 30).expect("attach_pty (auto)");
    check("negotiated framing is V2Binary", h2.framing() == PtyFraming::V2Binary);
    check(
        "binary scrollback carries the seed marker",
        String::from_utf8_lossy(&h2.scrollback).contains("T13B-SEED-MARKER"),
    );
    println!("       scrollback {}B (raw, no base64)", h2.scrollback.len());

    h2.write(b"echo T13B-V2-LOOPBACK\r");
    let (acc, lens) = drain_until(&h2, b"T13B-V2-LOOPBACK", Duration::from_secs(10));
    check(
        "binary WRITE round-trips as OUT frames",
        !lens.is_empty() && acc.windows(16).any(|w| w == b"T13B-V2-LOOPBACK"),
    );

    let g_before = geometry();
    h2.resize(90, 25);
    std::thread::sleep(Duration::from_millis(1200));
    let g_after = geometry();
    check(
        &format!("binary RESIZE changed geometry {g_before} -> {g_after} (want 90x24)"),
        g_after == "90x24",
    );

    // === (2) v2 firehose: the measured wire =================================
    println!("\n[v2] firehose (seq 1 50000) - measured on the real wire");
    let (v2, v2_lens) = firehose(&h2, "T13B-FIREHOSE-V2-DONE");
    // Price v1 for the SAME frames v2 carried: {"out":"<b64>"}\n = 11B envelope
    // + 4*ceil(L/3) base64, per frame (the python probe's exact formula).
    let v1_priced: u64 = v2_lens.iter().map(|&l| 11 + 4 * (l as u64).div_ceil(3)).sum();
    println!(
        "       frames={} payload={}B wire={}B (tax {:+.1}%)",
        v2.frames,
        v2.payload,
        v2.wire,
        pct(v2.wire, v2.payload)
    );
    println!("       v1 priced for the SAME frames = {v1_priced}B");
    let reduction_exact = 1.0 - v2.wire as f64 / v1_priced as f64;
    println!(
        "       exact reduction = {:.1}%  (v2 is {:.1}% of v1)",
        reduction_exact * 100.0,
        100.0 * v2.wire as f64 / v1_priced as f64
    );
    check("firehose observed and materially smaller (>20%)", reduction_exact > 0.20);
    h2.detach();

    // === (3) forced-v1 attach: regression + measured baseline ===============
    println!("\n[v1] forced-v1 attach (attach_pty_v1) - regression + baseline");
    let h1 = client.attach_pty_v1(SESSION_ID, 90, 25).expect("attach_pty_v1");
    check("forced framing is V1Json", h1.framing() == PtyFraming::V1Json);
    check("v1 scrollback decodes to real content", !h1.scrollback.is_empty());

    h1.write(b"echo T13B-V1-LOOPBACK\r");
    let (acc1, lens1) = drain_until(&h1, b"T13B-V1-LOOPBACK", Duration::from_secs(10));
    check(
        "v1 JSON write round-trips as out frames",
        !lens1.is_empty() && acc1.windows(16).any(|w| w == b"T13B-V1-LOOPBACK"),
    );

    let g1_before = geometry();
    h1.resize(110, 40);
    std::thread::sleep(Duration::from_millis(1200));
    let g1_after = geometry();
    check(
        &format!("v1 JSON resize changed geometry {g1_before} -> {g1_after} (want 110x39)"),
        g1_after == "110x39",
    );

    // Return to the v2 run's geometry so both firehoses redraw the same pane
    // size (payload volume depends on geometry; the A/B should not).
    h1.resize(90, 25);
    std::thread::sleep(Duration::from_millis(1200));

    println!("\n[v1] firehose (seq 1 50000) - measured baseline (same 90x25 geometry as v2)");
    let (v1, _) = firehose(&h1, "T13B-FIREHOSE-V1-DONE");
    println!(
        "       frames={} payload={}B wire={}B (tax {:+.1}%)",
        v1.frames,
        v1.payload,
        v1.wire,
        pct(v1.wire, v1.payload)
    );
    h1.detach();

    // === summary =============================================================
    // The two runs capture independent tmux redraws, so payload totals differ;
    // the normalized comparison is each run's tax (wire/payload).
    let v1_tax = v1.wire as f64 / v1.payload.max(1) as f64;
    let v2_tax = v2.wire as f64 / v2.payload.max(1) as f64;
    let reduction_meas = 1.0 - v2_tax / v1_tax;
    println!("\n[summary]");
    println!(
        "  v1 measured: wire/payload = {}/{} = {:.3}x   v2 measured: {}/{} = {:.3}x",
        v1.wire, v1.payload, v1_tax, v2.wire, v2.payload, v2_tax
    );
    println!(
        "  reduction: exact (same-frames pricing) = {:.1}%; measured (tax-normalized cross-check) = {:.1}%",
        reduction_exact * 100.0,
        reduction_meas * 100.0
    );

    // --- teardown ------------------------------------------------------------
    tmux(&["kill-session", "-t", TMUX_SESSION]);
    println!("\n[teardown] killed {TMUX_SESSION}");

    if failures == 0 {
        println!("\nT13B-BINFRAME-PROBE-OK");
    } else {
        eprintln!("\nT13B-BINFRAME-PROBE-FAILED ({failures} check(s) failed)");
        std::process::exit(1);
    }
}

fn pct(wire: u64, payload: u64) -> f64 {
    100.0 * wire as f64 / payload.max(1) as f64 - 100.0
}
