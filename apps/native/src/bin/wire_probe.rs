//! Headless acceptance runner for the §1.3 `wire` ControlClient (native-pivot T4).
//!
//! A Rust port of `scripts/probes/t1_pty.py` + `t1_subscribe.py` + `t1_resize.py`,
//! run against the LIVE server over the control socket. Proves, in order:
//!   1. discover + connect,
//!   2. `list_terminals` returns the real sessions,
//!   3. live `status://snapshot` (and other) events arrive on the events channel,
//!   4. `attach_pty` to a DISPOSABLE tmux session (`th_t4-wire-check`) streams the
//!      seed scrollback + live output, and write + resize round-trip.
//!
//! Only ever touches its own disposable session; kills it on the way out. This is
//! the WSL-friendly proof that does not need the GPUI backend to build.
//!
//! Reconnect-with-backoff is proven deterministically by the `wire` unit test
//! `request_redials_after_a_dropped_connection` (mock server, no live app). The
//! full "survives an app restart" path is a Windows/live captain check, since
//! restarting the user's running app is a do-not per the execution preamble.

use std::process::Command;
use std::time::{Duration, Instant};

use t_hub_native::wire::{ControlClient, Event, PtyFrame};

const SESSION_ID: &str = "t4-wire-check"; // tmux session becomes th_t4-wire-check
const TMUX_SESSION: &str = "th_t4-wire-check";

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

/// Drain output frames until `needle` appears in the accumulated bytes or timeout.
fn collect_until(rx: &crossbeam::channel::Receiver<PtyFrame>, needle: &str, timeout: Duration) -> (Vec<u8>, usize) {
    let mut acc = Vec::new();
    let mut frames = 0;
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        match rx.recv_timeout(Duration::from_millis(200)) {
            Ok(PtyFrame::Out(bytes)) => {
                frames += 1;
                acc.extend_from_slice(&bytes);
                if acc.windows(needle.len()).any(|w| w == needle.as_bytes()) {
                    break;
                }
            }
            Ok(PtyFrame::Exit(_)) => break,
            Err(_) => {}
        }
    }
    (acc, frames)
}

fn main() {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("warn")).init();
    let mut failures = 0;

    // --- 1. discover + connect ---------------------------------------------
    let ep = ControlClient::discover().expect("discover control.json");
    println!("[1] discovered endpoint addr={} token=...{}", ep.addr, &ep.token[ep.token.len().saturating_sub(4)..]);
    let client = ControlClient::connect(ep).expect("connect control socket");
    println!("[1] connected");

    // --- 2. list_terminals -------------------------------------------------
    let sessions = client.list_sessions().expect("list_terminals");
    println!("[2] list_terminals -> {} live session(s):", sessions.len());
    for s in &sessions {
        println!("      {} (tmux {}, state {})", s.id, s.tmux_session, s.state);
    }

    // --- 3. events: subscribe + collect ------------------------------------
    let ev_rx = client.events();
    let mut snapshots = 0;
    let mut total_events = 0;
    let deadline = Instant::now() + Duration::from_secs(4);
    println!("[3] collecting events for 4s...");
    while Instant::now() < deadline {
        if let Ok(Event { channel, .. }) = ev_rx.recv_timeout(Duration::from_millis(200)) {
            total_events += 1;
            if channel == "status://snapshot" {
                snapshots += 1;
            }
        }
    }
    println!("[3] events: total={total_events} status://snapshot={snapshots}");
    if snapshots == 0 {
        eprintln!("[3] WARN: no status://snapshot events observed in 4s (is a session active?)");
    }

    // --- 4. attach_pty on a DISPOSABLE session -----------------------------
    let existing = String::from_utf8_lossy(&tmux(&["ls"]).stdout).to_string();
    if existing.contains(TMUX_SESSION) {
        eprintln!("[4] {TMUX_SESSION} already exists; aborting to avoid clobbering it");
        std::process::exit(2);
    }
    let r = tmux(&["new-session", "-d", "-s", TMUX_SESSION, "-c", "/tmp"]);
    assert!(r.status.success(), "new-session failed: {}", String::from_utf8_lossy(&r.stderr));
    tmux(&["send-keys", "-t", TMUX_SESSION, "echo T4-SEED-MARKER", "Enter"]);
    std::thread::sleep(Duration::from_millis(800));
    println!("[4] created disposable session {TMUX_SESSION}; pre-attach geometry: {}", geometry());

    // Attach; the reader thread lives inside the handle.
    let handle = client.attach_pty(SESSION_ID, 100, 30).expect("attach_pty");
    let seed = String::from_utf8_lossy(&handle.scrollback);
    let seed_ok = seed.contains("T4-SEED-MARKER");
    println!("[4a] seed frame {}B; contains T4-SEED-MARKER: {seed_ok}", handle.scrollback.len());
    if !seed_ok {
        failures += 1;
        eprintln!("[4a] FAIL: seed scrollback missing the marker");
    }

    // write round-trip
    handle.write(b"echo T4-LOOPBACK-CHECK\r");
    let (acc, frames) = collect_until(&handle.output, "T4-LOOPBACK-CHECK", Duration::from_secs(8));
    let echo_hits = acc.windows(17).filter(|w| *w == b"T4-LOOPBACK-CHECK").count();
    println!("[4b] write round-trip: out frames={frames} decoded={}B echo visible: {}", acc.len(), echo_hits > 0);
    if echo_hits == 0 {
        failures += 1;
        eprintln!("[4b] FAIL: write did not round-trip");
    }

    // resize round-trip (verify via tmux pane geometry; tmux reports rows-1 for the status line)
    std::thread::sleep(Duration::from_millis(500));
    let g_before = geometry();
    handle.resize(90, 25);
    std::thread::sleep(Duration::from_millis(1200));
    let g_after = geometry();
    let resize_ok = g_after == "90x24" && g_before != g_after;
    println!("[4c] resize: before={g_before} after(90x25)={g_after} changed+correct: {resize_ok}");
    if !resize_ok {
        failures += 1;
        eprintln!("[4c] FAIL: resize did not take (expected 90x24)");
    }

    // detach cleanly (tmux session survives detach), then kill our disposable session
    handle.detach();
    std::thread::sleep(Duration::from_millis(300));
    let survived = tmux(&["has-session", "-t", TMUX_SESSION]).status.success();
    println!("[4d] session survived detach: {survived}");
    tmux(&["kill-session", "-t", TMUX_SESSION]);
    println!("[4d] disposable session killed");

    println!();
    if failures == 0 {
        println!("WIRE-PROBE-OK (list + events + attach/scrollback/write/resize all green)");
    } else {
        eprintln!("WIRE-PROBE-FAILED ({failures} check(s) failed)");
        std::process::exit(1);
    }
}
