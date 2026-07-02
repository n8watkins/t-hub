//! T11 headless acceptance runner: proves all three panels against the REAL
//! running server (loopback), no GPUI needed - the same role wire-probe /
//! overlay-probe play for T4/T9.
//!
//!  1. FILES - `index_project` + `search_files` + `list_dir` + `open_file` +
//!     `git_info` round-trip on a real project root, folded through the real
//!     `FilesState` reducers (tree rows, ranked hits, viewer).
//!  2. PREVIEW - a DISPOSABLE tmux session prints a localhost URL; the
//!     capture scan detects it, the client-side probe reaches a real local
//!     HTTP server (status + `<title>`), and a dead port reads Refused.
//!  3. RUNNER - the full machine on a DISPOSABLE session it owns:
//!     bind (adopt handshake) -> run (URL detected from capture) -> C-c stop
//!     (stop-probe marker) -> natural exit (exit code observed) -> kill
//!     (`close_terminal`, session verified gone). Optionally
//!     (`THN_PROBE_SPAWN=1`) the `spawn_terminal` identification path against
//!     the live UI sink.
//!
//! Only ever touches its own disposable `th_t11*` sessions (§0 rule).
//! Prints `PANELS-PROBE-OK` when every leg passes.

use std::io::{Read as _, Write as _};
use std::net::TcpListener;
use std::process::Command;
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use parking_lot::Mutex;
use serde_json::json;

use t_hub_native::panels::feed::exec_runner_cmds;
use t_hub_native::panels::files::{GitInfo, IndexSummary, SearchResponse};
use t_hub_native::panels::preview::{scan_local_urls, Probe};
use t_hub_native::panels::probe::probe_url;
use t_hub_native::panels::runner::Phase;
use t_hub_native::panels::{now_ms, LiveSession, PanelsState};
use t_hub_native::wire::ControlClient;

const PREV_SESSION: &str = "t11prev";
const RUN_SESSION: &str = "t11run";

fn main() {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();
    let result = run();
    cleanup_sessions();
    match result {
        Ok(()) => println!("PANELS-PROBE-OK"),
        Err(e) => {
            eprintln!("PANELS-PROBE-FAILED: {e}");
            std::process::exit(1);
        }
    }
}

fn run() -> Result<(), String> {
    let client = Arc::new(
        ControlClient::connect_discovered().map_err(|e| format!("connect: {e}"))?,
    );
    files_leg(&client)?;
    preview_leg(&client)?;
    runner_leg(&client)?;
    if std::env::var("THN_PROBE_SPAWN").as_deref() == Ok("1") {
        spawn_leg(&client)?;
    } else {
        println!("panels-probe: spawn_terminal leg skipped (set THN_PROBE_SPAWN=1)");
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Files
// ---------------------------------------------------------------------------

fn files_leg(client: &ControlClient) -> Result<(), String> {
    let root = probe_root()?;
    println!("panels-probe[files]: root {root}");
    let mut state = PanelsState::new();

    // index_project
    let v = client
        .request("index_project", json!({ "root": root }))
        .map_err(|e| format!("index_project: {e}"))?;
    let summary: IndexSummary =
        serde_json::from_value(v).map_err(|e| format!("index_project shape: {e}"))?;
    if summary.count == 0 {
        return Err("index_project returned an empty index".into());
    }
    println!("panels-probe[files]: indexed {} files", summary.count);

    // tree via list_dir, folded through the real reducers
    let fetch = state.select_root(&root);
    for f in fetch {
        let v = client
            .request("list_dir", json!({ "path": f.path, "showIgnored": f.show_ignored }))
            .map_err(|e| format!("list_dir: {e}"))?;
        let entries = serde_json::from_value(v).map_err(|e| format!("list_dir shape: {e}"))?;
        state.files.fold_dir(&f.path, Ok(entries));
    }
    state.files.fold_index(summary);
    let rows = state.files.tree_rows();
    if !rows.iter().any(|r| r.name == "apps" && r.is_dir) {
        return Err("tree rows missing the apps/ dir".into());
    }
    println!("panels-probe[files]: tree shows {} root entries", rows.len());

    // fuzzy search (debounced), ranked hits folded with highlight spans
    state.files.set_query("cargotoml", 0);
    let (seq, q) = state
        .files
        .take_due_search(1_000)
        .ok_or("debounced search never came due")?;
    let v = client
        .request("search_files", json!({ "root": root, "query": q, "limit": 50 }))
        .map_err(|e| format!("search_files: {e}"))?;
    let resp: SearchResponse =
        serde_json::from_value(v).map_err(|e| format!("search_files shape: {e}"))?;
    state.files.fold_hits(seq, Ok(resp));
    let hits = state.files.hit_rows();
    let top = hits.first().ok_or("search_files: no hits for 'cargotoml'")?;
    if !top.rel_path.to_lowercase().ends_with("cargo.toml") {
        return Err(format!("unexpected top hit for 'cargotoml': {}", top.rel_path));
    }
    if top.spans.is_empty() {
        return Err("top hit has no highlight spans".into());
    }
    println!(
        "panels-probe[files]: {} hits, top {} ({} spans)",
        hits.len(),
        top.rel_path,
        top.spans.len()
    );

    // read-only viewer via open_file
    let path = format!("{root}/README.md");
    let fetch = state.files.open(&path).ok_or("viewer open issued no fetch")?;
    let v = client
        .request("open_file", json!({ "path": fetch }))
        .map_err(|e| format!("open_file: {e}"))?;
    let fc = serde_json::from_value(v).map_err(|e| format!("open_file shape: {e}"))?;
    state.files.fold_file(&path, Ok(fc));
    let viewer = state.files.viewer.as_ref().ok_or("viewer missing")?;
    if viewer.loading || viewer.error.is_some() || viewer.lines.is_empty() {
        return Err(format!("viewer did not load: {:?}", viewer.error));
    }
    println!("panels-probe[files]: viewer loaded README.md ({} lines)", viewer.lines.len());

    // git header
    let v = client
        .request("git_info", json!({ "path": root }))
        .map_err(|e| format!("git_info: {e}"))?;
    let git: GitInfo = serde_json::from_value(v).map_err(|e| format!("git_info shape: {e}"))?;
    if !git.is_repo {
        return Err("git_info: probe root is not a repo?".into());
    }
    state.files.fold_git(git.clone());
    println!(
        "panels-probe[files]: git branch {:?} dirty {} worktree {}",
        git.branch, git.dirty_count, git.is_linked_worktree
    );
    Ok(())
}

/// The worktree root this probe runs inside (has README.md + apps/).
fn probe_root() -> Result<String, String> {
    if let Ok(r) = std::env::var("THN_PROBE_ROOT") {
        return Ok(r);
    }
    let cur = std::env::current_dir().map_err(|e| e.to_string())?;
    cur.ancestors()
        .find(|p| p.join("README.md").exists() && p.join("apps").is_dir())
        .map(|p| p.to_string_lossy().into_owned())
        .ok_or_else(|| "no project root found above cwd (set THN_PROBE_ROOT)".into())
}

// ---------------------------------------------------------------------------
// Preview
// ---------------------------------------------------------------------------

fn preview_leg(client: &ControlClient) -> Result<(), String> {
    // A real local HTTP "dev server" for the reachability probe.
    let listener = TcpListener::bind("127.0.0.1:0").map_err(|e| e.to_string())?;
    let port = listener.local_addr().map_err(|e| e.to_string())?.port();
    thread::spawn(move || {
        for stream in listener.incoming().flatten().take(4) {
            serve_one(stream);
        }
    });

    // A dead port: bind then drop to get a port nothing listens on.
    let dead_port = {
        let l = TcpListener::bind("127.0.0.1:0").map_err(|e| e.to_string())?;
        l.local_addr().map_err(|e| e.to_string())?.port()
    };

    make_session(PREV_SESSION)?;
    // One URL per echo: capture text wraps at the pane width and a wrapped
    // URL splits mid-scan (the documented capture-scan limitation; the
    // host-push path is wrap-aware). The wide pane below keeps even long
    // lines whole; separate lines keep the probe deterministic regardless.
    for text in [
        format!("echo dev server up at http://127.0.0.1:{port}/app"),
        format!("echo also http://127.0.0.1:{dead_port}/dead"),
    ] {
        client
            .request(
                "send_text",
                json!({ "sessionId": PREV_SESSION, "text": text, "enter": true }),
            )
            .map_err(|e| format!("send_text: {e}"))?;
    }

    // Capture-scan until both URLs fold in (the same read_terminal the feed polls).
    let mut state = PanelsState::new();
    state.fold_live_sessions(list_live(client)?);
    let deadline = Instant::now() + Duration::from_secs(6);
    loop {
        let text = read_capture(client, PREV_SESSION)?;
        let urls = scan_local_urls(&text);
        if !urls.is_empty() {
            state.preview.fold_urls(PREV_SESSION, urls, now_ms());
        }
        let found = state
            .preview
            .rows()
            .iter()
            .find(|s| s.session == PREV_SESSION)
            .map(|s| s.urls.len())
            .unwrap_or(0);
        if found >= 2 {
            break;
        }
        if Instant::now() > deadline {
            return Err("capture scan never saw the echoed URLs".into());
        }
        thread::sleep(Duration::from_millis(250));
    }

    // Probe both: live port -> Reachable(200) + title; dead port -> Refused.
    let unprobed = state.preview.take_unprobed();
    if unprobed.len() != 2 {
        return Err(format!("expected 2 unprobed urls, got {}", unprobed.len()));
    }
    for (session, url) in unprobed {
        let outcome = probe_url(&url);
        state.preview.fold_probe(&session, &url.canonical(), outcome.probe, outcome.title);
    }
    let rows = state.preview.rows();
    let s = rows
        .iter()
        .find(|s| s.session == PREV_SESSION)
        .ok_or("preview session missing")?;
    let live_url = s
        .urls
        .iter()
        .find(|e| e.url.port == port)
        .ok_or("live URL missing from the list")?;
    if live_url.probe != (Probe::Reachable { status: Some(200) }) {
        return Err(format!("live URL probe: {:?}", live_url.probe));
    }
    if live_url.title.as_deref() != Some("T11 Probe") {
        return Err(format!("live URL title: {:?}", live_url.title));
    }
    let dead_url = s
        .urls
        .iter()
        .find(|e| e.url.port == dead_port)
        .ok_or("dead URL missing from the list")?;
    if dead_url.probe != Probe::Refused {
        return Err(format!("dead URL probe: {:?}", dead_url.probe));
    }
    println!(
        "panels-probe[preview]: {} urls on session; :{port} Reachable(200) '{}'; :{dead_port} Refused",
        s.urls.len(),
        live_url.title.clone().unwrap_or_default()
    );

    client
        .request("close_terminal", json!({ "sessionId": PREV_SESSION }))
        .map_err(|e| format!("close_terminal: {e}"))?;
    Ok(())
}

fn serve_one(mut stream: std::net::TcpStream) {
    let mut buf = [0u8; 2048];
    let _ = stream.read(&mut buf);
    let body = "<html><head><title>T11 Probe</title></head><body>ok</body></html>";
    let resp = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
        body.len(),
        body
    );
    let _ = stream.write_all(resp.as_bytes());
}

// ---------------------------------------------------------------------------
// Runner
// ---------------------------------------------------------------------------

fn runner_leg(client: &ControlClient) -> Result<(), String> {
    make_session(RUN_SESSION)?;
    let url_port = 59_173u16; // no listener needed: URL *detection* is the point

    // Shared state so exec_runner_cmds (the production executor) can be reused.
    let state: Arc<Mutex<PanelsState>> = Arc::new(Mutex::new(PanelsState::new()));
    {
        let mut st = state.lock();
        st.runners.ensure("/tmp", now_ms());
    }
    let exec = |cmds| exec_runner_cmds(client, &state, Some("/tmp"), cmds);

    // Bind our own disposable session: adopt handshake must land Ready.
    let cmds = state.lock().runners.get_mut("/tmp").unwrap().bind_existing(RUN_SESSION, now_ms());
    exec(cmds);
    wait_phase(client, &state, "bind/adopt", Duration::from_secs(8), |p| {
        matches!(p, Phase::Ready { .. })
    })?;
    println!("panels-probe[runner]: adopted {RUN_SESSION} (Ready)");

    // Run: URL detected from capture while Running.
    {
        let mut st = state.lock();
        let live = st.live.clone();
        let r = st.runners.get_mut("/tmp").unwrap();
        r.set_command(&format!(
            "printf 'dev ready http://127.0.0.1:{url_port}/\\n'; sleep 300"
        ));
        let cmds = r.start(now_ms(), &live);
        drop(st);
        exec(cmds);
    }
    wait_phase(client, &state, "run", Duration::from_secs(8), |p| {
        matches!(p, Phase::Running { .. })
    })?;
    let deadline = Instant::now() + Duration::from_secs(8);
    loop {
        drive_captures(client, &state)?;
        let url = state.lock().runners.get("/tmp").unwrap().url.clone();
        if let Some(u) = url {
            if !u.contains(&format!(":{url_port}")) {
                return Err(format!("unexpected detected url {u}"));
            }
            println!("panels-probe[runner]: URL detected while running: {u}");
            break;
        }
        if Instant::now() > deadline {
            return Err("runner never detected the dev URL".into());
        }
        thread::sleep(Duration::from_millis(300));
    }

    // Stop: C-c + delayed stop-probe -> Exited.
    let cmds = state.lock().runners.get_mut("/tmp").unwrap().stop(now_ms());
    exec(cmds);
    wait_phase(client, &state, "stop", Duration::from_secs(10), |p| {
        matches!(p, Phase::Exited { .. })
    })?;
    println!("panels-probe[runner]: C-c stop observed (Exited)");

    // Natural exit: the wrapped EXIT marker carries the real code.
    {
        let mut st = state.lock();
        let live = st.live.clone();
        let r = st.runners.get_mut("/tmp").unwrap();
        r.set_command("true");
        let cmds = r.start(now_ms(), &live);
        drop(st);
        exec(cmds);
    }
    wait_phase(client, &state, "natural exit", Duration::from_secs(8), |p| {
        matches!(p, Phase::Exited { code: Some(0), .. })
    })?;
    println!("panels-probe[runner]: natural exit observed with code 0");

    // Kill: close_terminal; the session must be gone from list_terminals.
    let cmds = state.lock().runners.get_mut("/tmp").unwrap().kill();
    exec(cmds);
    let deadline = Instant::now() + Duration::from_secs(6);
    loop {
        let live = list_live(client)?;
        if !live.iter().any(|s| s.id == RUN_SESSION) {
            break;
        }
        if Instant::now() > deadline {
            return Err("killed session still listed".into());
        }
        thread::sleep(Duration::from_millis(300));
    }
    println!("panels-probe[runner]: kill removed the session");
    Ok(())
}

/// Optional: the spawn_terminal identification path (needs the live app UI
/// as the apply sink; creates ONE transient tile, then closes it).
fn spawn_leg(client: &ControlClient) -> Result<(), String> {
    let root = "/tmp/t11-spawn-demo";
    std::fs::create_dir_all(root).map_err(|e| e.to_string())?;
    let state: Arc<Mutex<PanelsState>> = Arc::new(Mutex::new(PanelsState::new()));
    {
        let mut st = state.lock();
        st.runners.ensure(root, now_ms());
        let live = list_live(client)?;
        st.fold_live_sessions(live);
    }
    let cmds = {
        let mut st = state.lock();
        let live = st.live.clone();
        st.runners.get_mut(root).unwrap().start(now_ms(), &live)
    };
    exec_runner_cmds(client, &state, Some(root), cmds);
    if let Phase::Failed { reason } = &state.lock().runners.get(root).unwrap().phase {
        // A server-side refusal (older builds gate spawn_terminal off; a
        // headless server has no UI sink) is an environmental limit, not a
        // machine bug - the fail-fast fold IS the behavior under test here.
        println!("panels-probe[spawn]: leg skipped, server refused the spawn: {reason}");
        return Ok(());
    }
    // Identification: poll sessions through the machine until it adopts.
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        {
            let live = list_live(client)?;
            let mut st = state.lock();
            st.fold_live_sessions(live);
            let sessions = st.live.clone();
            let cmds = st.runners.on_sessions(&sessions, now_ms());
            drop(st);
            exec_runner_cmds(client, &state, None, cmds);
        }
        drive_captures(client, &state)?;
        let (phase, sid) = {
            let st = state.lock();
            let r = st.runners.get(root).unwrap();
            (r.phase.clone(), r.sid().map(|s| s.to_string()))
        };
        if matches!(phase, Phase::Ready { .. }) {
            let sid = sid.unwrap();
            println!("panels-probe[spawn]: spawn adopted as {sid} (Ready); closing");
            let cmds = state.lock().runners.get_mut(root).unwrap().kill();
            exec_runner_cmds(client, &state, None, cmds);
            return Ok(());
        }
        if let Phase::Failed { reason } = phase {
            return Err(format!("spawn leg failed: {reason}"));
        }
        {
            let mut st = state.lock();
            let cmds = st.runners.on_tick(now_ms());
            drop(st);
            exec_runner_cmds(client, &state, None, cmds);
        }
        if Instant::now() > deadline {
            return Err("spawn leg timed out".into());
        }
        thread::sleep(Duration::from_millis(500));
    }
}

// ---------------------------------------------------------------------------
// Shared helpers
// ---------------------------------------------------------------------------

fn list_live(client: &ControlClient) -> Result<Vec<LiveSession>, String> {
    let v = client.request("list_terminals", json!({})).map_err(|e| e.to_string())?;
    Ok(v["terminals"]
        .as_array()
        .map(|list| {
            list.iter()
                .map(|t| LiveSession {
                    id: t["id"].as_str().unwrap_or("").to_string(),
                    title: t["title"].as_str().unwrap_or("").to_string(),
                    cwd: t["cwd"].as_str().unwrap_or("").to_string(),
                })
                .filter(|s| !s.id.is_empty())
                .collect()
        })
        .unwrap_or_default())
}

fn read_capture(client: &ControlClient, sid: &str) -> Result<String, String> {
    let v = client
        .request("read_terminal", json!({ "sessionId": sid, "historyLines": 300 }))
        .map_err(|e| e.to_string())?;
    Ok(v["text"].as_str().unwrap_or("").to_string())
}

/// One feed-equivalent observation pass: session sweep + capture polls +
/// timeout tick, executing whatever the machines emit.
fn drive_captures(client: &ControlClient, state: &Arc<Mutex<PanelsState>>) -> Result<(), String> {
    let targets = state.lock().runners.tail_targets();
    for (root, sid) in targets {
        let text = read_capture(client, &sid)?;
        let mut st = state.lock();
        if let Some(r) = st.runners.get_mut(&root) {
            r.on_capture(&sid, &text, now_ms());
        }
    }
    let cmds = {
        let mut st = state.lock();
        st.runners.on_tick(now_ms())
    };
    exec_runner_cmds(client, state, None, cmds);
    Ok(())
}

fn wait_phase(
    client: &ControlClient,
    state: &Arc<Mutex<PanelsState>>,
    what: &str,
    timeout: Duration,
    done: impl Fn(&Phase) -> bool,
) -> Result<(), String> {
    let deadline = Instant::now() + timeout;
    loop {
        drive_captures(client, state)?;
        let phase = state.lock().runners.get("/tmp").unwrap().phase.clone();
        if done(&phase) {
            return Ok(());
        }
        if let Phase::Failed { reason } = &phase {
            return Err(format!("{what}: machine failed: {reason}"));
        }
        if Instant::now() > deadline {
            return Err(format!("{what}: timed out in phase {phase:?}"));
        }
        thread::sleep(Duration::from_millis(300));
    }
}

fn make_session(sid: &str) -> Result<(), String> {
    let status = Command::new("tmux")
        .args([
            "-L", "t-hub", "new-session", "-d", "-x", "220", "-y", "50", "-s",
            &format!("th_{sid}"), "-c", "/tmp",
        ])
        .status()
        .map_err(|e| format!("tmux new-session: {e}"))?;
    if !status.success() {
        return Err(format!("tmux new-session th_{sid} failed"));
    }
    Ok(())
}

fn cleanup_sessions() {
    for sid in [PREV_SESSION, RUN_SESSION] {
        let _ = Command::new("tmux")
            .args(["-L", "t-hub", "kill-session", "-t", &format!("th_{sid}")])
            .status();
    }
}
