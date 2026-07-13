//! Headless control listener for the T13 binary-framing probe.
//!
//! Stands up the REAL `t_hub_lib::control` listener (same dispatch + attach path
//! the app uses) with no Tauri GUI, so `scripts/probes/t13_binframe.py` can prove
//! v2 binary PTY framing + v1 fallback end-to-end over a real socket against real
//! tmux — WITHOUT touching the user's live app or its `~/.t-hub/control.json`.
//!
//! Discovery is redirected via `T_HUB_CONTROL_FILE` (the probe sets it to a temp
//! path, launches this, waits for the handshake file, then connects). The token is
//! whatever the probe passes as argv[1] (default below). It parks until killed.
//!
//! Run: `T_HUB_CONTROL_FILE=/tmp/th-t13/control.json cargo run --example control_probe_server -- <token>`

use std::sync::Arc;

use parking_lot::Mutex;
use t_hub_lib::{control, status_bridge_for_test, supervision_for_test};

fn main() {
    let token = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "t13-probe-token".to_string());

    // Empty-but-real supervision + status bridges. The PTY attach path this probe
    // exercises needs neither; they're required only to construct the context.
    let supervisor = Arc::new(Mutex::new(supervision_for_test()));
    let status = Arc::new(status_bridge_for_test());

    let ctx = control::ControlContext::with_shared_supervisor(status, supervisor, token);
    let handshake = control::start(ctx).expect("control listener starts");

    // The probe waits on the handshake FILE (start() wrote it, honoring
    // T_HUB_CONTROL_FILE); this line is just an operator breadcrumb.
    println!("T13-PROBE-SERVER-READY {}", handshake.addr);

    // Park forever — the probe kills this process when it's done.
    loop {
        std::thread::sleep(std::time::Duration::from_secs(3600));
    }
}
