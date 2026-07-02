//! T9 acceptance runner (headless, like `wire-probe`): connect to the REAL
//! running server, spin the [`OverlayFeed`] for a few seconds, and print a text
//! rendering of every sidebar section - proving each overlay's data source
//! round-trips end to end without gpui.
//!
//! Read-only: only issues read commands (`recent_sessions`, `host_metrics`,
//! `codex_usage`, `supervision_session_ids`/`supervision_tree`,
//! `list_terminals`, and at most one gated `claude_usage`) plus the event
//! subscription. It never spawns, archives, or writes.
//!
//! `THN_PROBE_SECS` (default 8) controls the observation window. Prints
//! `OVERLAY-PROBE-OK` when every section answered.

use std::sync::Arc;
use std::thread;
use std::time::Duration;

use t_hub_native::overlays::model::now_ms;
use t_hub_native::overlays::OverlayFeed;
use t_hub_native::wire::ControlClient;

fn main() {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();

    let client = match ControlClient::connect_discovered() {
        Ok(c) => Arc::new(c),
        Err(e) => {
            eprintln!("overlay-probe: could not connect to the control socket: {e}");
            eprintln!("  is the T-Hub app running? (control.json handshake missing/unreadable)");
            std::process::exit(1);
        }
    };
    println!("overlay-probe: connected");

    let feed = OverlayFeed::spawn(client);
    let secs: u64 =
        std::env::var("THN_PROBE_SECS").ok().and_then(|s| s.parse().ok()).unwrap_or(8);
    println!("overlay-probe: observing for {secs}s...");
    thread::sleep(Duration::from_secs(secs));

    let now = now_ms();
    let state = feed.state();
    let mut st = state.lock();
    st.toasts.tick(now);

    let mut ok = true;

    // Recents.
    let open = st.index.open_cwds();
    let rows = st.recents.rows(&open, now);
    println!("\n== recents (loaded={}, {} open project(s) filtered) ==", st.recents.loaded, open.len());
    if let Some(e) = &st.recents.error {
        println!("  error: {e}");
        ok = false;
    }
    for r in rows.iter().take(6) {
        println!(
            "  {} [{}] {}{}",
            r.title,
            r.age,
            r.folder,
            r.worktree.as_deref().map(|w| format!(" (wt {w})")).unwrap_or_default()
        );
    }
    if !st.recents.loaded {
        ok = false;
    }

    // Usage.
    let usage = st.usage.rows(now);
    println!("== usage ==");
    for m in usage.claude.iter().chain(usage.codex.iter()) {
        println!(
            "  {}: used {} left {} {}",
            m.label,
            m.used_pct.map(|v| format!("{v:.0}%")).unwrap_or_else(|| "-".into()),
            m.left_pct.map(|v| format!("{v:.0}%")).unwrap_or_else(|| "-".into()),
            m.resets.as_deref().unwrap_or("")
        );
    }
    if let Some(cost) = usage.total_cost_usd {
        println!("  session cost: ${cost:.2}");
    }
    if usage.claude.is_empty() {
        println!("  (no claude rows - impossible: rows() always emits 2)");
        ok = false;
    }

    // Host metrics.
    println!("== wsl health ==");
    match (st.metrics.summary(), &st.metrics.error) {
        (Some(s), _) => {
            println!("  {s}{}", if st.metrics.is_stale(now) { " (stale)" } else { "" });
            for line in st.metrics.rows() {
                println!("  {}: {}", line.label, line.value);
            }
        }
        (None, Some(e)) => println!("  unavailable (expected pre-agent-bridge): {e}"),
        (None, None) => {
            println!("  no reading and no error - host_metrics never answered");
            ok = false;
        }
    }

    // Supervision.
    let trees = st.supervision.active();
    println!("== supervision ({} active tree(s)) ==", trees.len());
    for tv in trees.iter().take(6) {
        println!(
            "  [{}] {} - {} running, {} done, {} task(s)",
            tv.status.label(),
            tv.label,
            tv.running,
            tv.done,
            tv.outstanding_tasks
        );
        for c in tv.children.iter().take(8) {
            println!(
                "    {} {}{}",
                if c.running { "●" } else { "○" },
                c.label,
                c.duration.as_deref().map(|d| format!(" ({d})")).unwrap_or_default()
            );
        }
    }

    // Toasts + events.
    println!("== toasts ==");
    println!(
        "  queued now: {} (warmup active: {})",
        st.toasts.visible().len(),
        st.toasts.in_warmup(now)
    );
    for toast in st.toasts.visible() {
        println!("  [{:?}] {} - {}", toast.kind, toast.title, toast.body);
    }
    println!("== events ==");
    println!(
        "  folded: {} (agent connection: {})",
        st.events_folded,
        st.agent_connection.as_deref().unwrap_or("(none seen)")
    );

    if ok {
        println!("\nOVERLAY-PROBE-OK");
    } else {
        println!("\noverlay-probe: FAILED (see above)");
        std::process::exit(1);
    }
}
