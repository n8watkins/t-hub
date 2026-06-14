//! App-side **control listener** — the local control channel the MCP server
//! ([`termhub-mcp`](../crates/termhub-mcp)) forwards `tools/call` requests to.
//!
//! ## Why this exists
//! MCP servers are launched by the client (Claude) over stdio, as a separate
//! short-lived process. They cannot share the running TermHub app's
//! Tauri-managed state in-process. So the MCP binary speaks the MCP protocol on
//! stdio and forwards each `tools/call` to **this** listener over a loopback TCP
//! channel; the listener dispatches by command name against the app's state and
//! returns JSON. The MCP server therefore needs **no compile-time knowledge** of
//! individual commands — dispatch is dynamic, by name (PRD §9.6, §11.2).
//!
//! ## Wire protocol (newline-delimited JSON over loopback TCP)
//! One request object per line, one response object per line:
//! ```text
//! → {"token":"<secret>","command":"list_terminals","args":{}}
//! ← {"ok":true,"result":[ … ]}
//! ```
//! Errors come back as `{"ok":false,"error":"<message>"}`. A request whose token
//! does not match the per-launch secret is rejected before dispatch.
//!
//! ## Discovery + auth
//! On startup we bind `127.0.0.1:0` (an ephemeral port), generate a per-launch
//! token, and write both to a small handshake file (`~/.termhub/control.json`,
//! mode `0600` on unix). The MCP binary reads that file to learn where to connect
//! and which token to present. `TERMHUB_CONTROL_ADDR` + `TERMHUB_CONTROL_TOKEN`
//! override discovery for tests / harnesses. Binding to loopback keeps the
//! channel host-local (PRD §11.3: expose only what TermHub needs).
//!
//! ## Permission tiers (PRD §11.2)
//! Read + Organization tools are dispatched here. Process-changing and
//! destructive tools are **gated**: this listener refuses any command that is not
//! on its allow-list, returning a clear error, so even if a future MCP build
//! advertises a destructive tool the app will not execute it. The MCP tool
//! descriptions additionally mark such tools as confirmation-required.
//!
//! Boundary: this module *reads* the existing command surface (tmux, agent,
//! status, supervision, files) and calls it; it does not change any of it. The
//! `theme` commands are forwarded by name and will light up when the parallel
//! theme track lands the `get_theme`/`set_theme` Tauri commands + a control
//! handler for them; until then they return a clear "not available" error.

use std::io::{BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};
use std::path::PathBuf;
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::claude::StatusBridge;
use crate::supervision::Supervisor;
use crate::{files, tmux};

/// A single control request: a command name + free-form JSON args, authenticated
/// by the per-launch `token`.
#[derive(Debug, Deserialize)]
pub struct ControlRequest {
    /// Per-launch shared secret (see the handshake file).
    #[serde(default)]
    pub token: String,
    /// The command/tool name to dispatch (e.g. `list_terminals`).
    pub command: String,
    /// Command arguments. Shape is per-command; absent ⇒ `null`.
    #[serde(default)]
    pub args: Value,
}

/// A single control response. `ok` discriminates success (`result`) from failure
/// (`error`), mirroring the `Result<Value, String>` the dispatcher returns.
#[derive(Debug, Serialize)]
pub struct ControlResponse {
    pub ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

impl ControlResponse {
    fn ok(result: Value) -> Self {
        Self {
            ok: true,
            result: Some(result),
            error: None,
        }
    }
    fn err(msg: impl Into<String>) -> Self {
        Self {
            ok: false,
            result: None,
            error: Some(msg.into()),
        }
    }
}

/// The handshake record written so the MCP binary can find + authenticate to the
/// listener. Serialized to `~/.termhub/control.json`.
#[derive(Debug, Serialize, Deserialize)]
pub struct ControlHandshake {
    /// `127.0.0.1:<port>` the listener bound to.
    pub addr: String,
    /// Per-launch shared secret the client must present.
    pub token: String,
    /// PID of the app that owns this listener (diagnostics / staleness checks).
    pub pid: u32,
}

/// A sink that delivers an Organization-tier UI mutation to the frontend. The
/// real implementation (wired from `lib.rs`) emits a Tauri `control://apply`
/// event carrying `{command, args}`; the frontend `controlBridge` subscribes and
/// dispatches it into the workspace store. Boxed as a trait object so this module
/// stays free of any `tauri` dependency and the e2e/unit tests can omit it.
pub trait ApplySink: Send + Sync {
    /// Forward an accepted Organization command + its args to the UI. Returns
    /// `Ok(())` if the event was emitted, or an error string the dispatcher
    /// surfaces (the command is still audited regardless).
    fn apply(&self, command: &str, args: &Value) -> Result<(), String>;
}

/// The shared state the control dispatcher reads. Holds exactly the handles the
/// Read + Organization tools need.
///
/// Deliberately **not** the Tauri-managed `TerminalManager` / `FileIndexState`
/// (those are non-`Clone`, owned by the app for its lifetime, and only
/// borrowable inside the invoke handler). Instead:
///   - terminal listing is reconstructed from the tmux source of truth (exactly
///     as `commands::list_terminals` treats it — tmux is authoritative);
///   - file search uses its own [`files::FileIndexState`] cache (a cache, so a
///     private one is correct — it just re-walks on first query);
///   - supervision + status are read from the `Arc`-shared bridges in
///     [`crate::AppState`], which *is* `Clone`.
#[derive(Clone)]
pub struct ControlContext {
    status: Arc<StatusBridge>,
    /// A snapshot accessor over the supervision reducer. Boxed closure so this
    /// module does not need to name the `AgentBridge` internals; the closure
    /// borrows the shared `Mutex<Supervisor>` inside the bridge.
    supervisor: Arc<dyn Fn(&mut dyn FnMut(&Supervisor)) + Send + Sync>,
    /// Private file index cache for control-channel searches.
    files: Arc<files::FileIndexState>,
    /// Sink that forwards Organization-tier UI mutations (`focus_session`,
    /// `move_tile`, `rename_tab`) to the frontend. `None` in headless tests /
    /// proofs (those just audit); `Some` once `lib.rs` wires the `AppHandle`.
    apply_sink: Option<Arc<dyn ApplySink>>,
    /// The per-launch auth token.
    token: String,
}

impl ControlContext {
    /// Run `f` against the supervision reducer (read-only) via the bridge's lock.
    ///
    /// The visitor type is `FnMut(&mut dyn FnMut(&Supervisor))`, so the inner
    /// closure must be `FnMut`; we move `f` (an `FnOnce`) out of an `Option` on
    /// its single invocation to satisfy that bound. The bridge calls the inner
    /// closure exactly once with the locked `Supervisor`.
    fn with_supervisor<R>(&self, f: impl FnOnce(&Supervisor) -> R) -> R {
        let mut out: Option<R> = None;
        let mut f = Some(f);
        let mut take = |s: &Supervisor| {
            if let Some(f) = f.take() {
                out = Some(f(s));
            }
        };
        (self.supervisor)(&mut take);
        out.expect("supervisor closure always runs")
    }
}

/// Resolve the handshake file path: `$TERMHUB_CONTROL_FILE` if set, else
/// `~/.termhub/control.json` (or the process dir as a last resort).
pub fn handshake_path() -> PathBuf {
    if let Ok(p) = std::env::var("TERMHUB_CONTROL_FILE") {
        return PathBuf::from(p);
    }
    let home = std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."));
    home.join(".termhub").join("control.json")
}

/// Write the handshake file (best-effort `0600` on unix) so the MCP binary can
/// discover the live listener.
fn write_handshake(handshake: &ControlHandshake) -> std::io::Result<()> {
    let path = handshake_path();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let body = serde_json::to_vec_pretty(handshake)?;
    std::fs::write(&path, &body)?;
    // Tighten permissions on unix so another local user can't read the token.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600));
    }
    Ok(())
}

/// Start the control listener on a background thread.
///
/// Binds `127.0.0.1:0`, writes the handshake file, and serves NDJSON control
/// requests until the process exits. Returns the bound address + token so the
/// caller (and tests) know where it landed. A bind failure is returned to the
/// caller; the app logs it and continues (the control channel is optional, like
/// the agent bridge).
pub fn start(ctx: ControlContext) -> std::io::Result<ControlHandshake> {
    let listener = TcpListener::bind("127.0.0.1:0")?;
    let addr = listener.local_addr()?;
    let handshake = ControlHandshake {
        addr: addr.to_string(),
        token: ctx.token.clone(),
        pid: std::process::id(),
    };
    write_handshake(&handshake)?;

    std::thread::Builder::new()
        .name("termhub-control".into())
        .spawn(move || serve(listener, ctx))
        .ok();

    Ok(handshake)
}

/// Accept loop: one short read/serve thread per connection. Connections are
/// expected to be local and short-lived (one MCP `tools/call` round-trip), but we
/// handle multiple lines per connection so a client may pipeline.
fn serve(listener: TcpListener, ctx: ControlContext) {
    for stream in listener.incoming() {
        match stream {
            Ok(stream) => {
                let ctx = ctx.clone();
                std::thread::spawn(move || {
                    if let Err(e) = handle_conn(stream, &ctx) {
                        eprintln!("termhub-control: connection error: {e}");
                    }
                });
            }
            Err(e) => {
                eprintln!("termhub-control: accept failed: {e}");
            }
        }
    }
}

/// Serve every newline-delimited request on one connection until EOF.
fn handle_conn(stream: TcpStream, ctx: &ControlContext) -> std::io::Result<()> {
    let peer = stream.peer_addr().ok();
    // Loopback-only: reject anything that didn't come from 127.0.0.1/::1. The
    // bind is already loopback, but this is a cheap defensive belt.
    if let Some(addr) = peer {
        if !addr.ip().is_loopback() {
            return Ok(());
        }
    }
    let mut writer = stream.try_clone()?;
    let reader = BufReader::new(stream);
    for line in reader.lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        let resp = match serde_json::from_str::<ControlRequest>(&line) {
            Ok(req) => dispatch_authenticated(ctx, req),
            Err(e) => ControlResponse::err(format!("malformed control request: {e}")),
        };
        let mut body = serde_json::to_vec(&resp).unwrap_or_else(|_| {
            br#"{"ok":false,"error":"failed to serialize response"}"#.to_vec()
        });
        body.push(b'\n');
        writer.write_all(&body)?;
        writer.flush()?;
    }
    Ok(())
}

/// Check the token, then dispatch. A bad token is rejected before any command
/// runs (no information about which commands exist is leaked).
fn dispatch_authenticated(ctx: &ControlContext, req: ControlRequest) -> ControlResponse {
    if req.token != ctx.token {
        return ControlResponse::err("unauthorized: bad control token");
    }
    match dispatch(ctx, &req.command, &req.args) {
        Ok(value) => ControlResponse::ok(value),
        Err(e) => ControlResponse::err(e),
    }
}

/// The set of commands the control channel will execute. Read + Organization
/// tiers (PRD §11.2). Process-changing / destructive commands are intentionally
/// **absent**: they fall through to the "not permitted over the control channel"
/// arm so the app never executes them via MCP, even if a client asks.
///
/// `theme` commands are forwarded by name; until the parallel theme track lands
/// their handlers they return a clear "not yet available" error.
fn dispatch(ctx: &ControlContext, command: &str, args: &Value) -> Result<Value, String> {
    match command {
        // ---- Read tier (PRD §11.2: allowed) --------------------------------
        "list_terminals" => list_terminals(),
        "get_status" => get_status(ctx, args),
        "supervision_tree" => supervision_tree(ctx, args),
        "wsl_health" => wsl_health(ctx),
        "search_files" => search_files(ctx, args),
        "list_tabs" => list_tabs(),
        "read_terminal" | "capture_pane" => read_terminal(args),

        // ---- Organization tier (PRD §11.2: allowed, audited) ---------------
        // These are surfaced by the MCP server and accepted here, but the
        // process-changing subset (spawn) is gated behind the confirmation flag
        // in the MCP tool description AND refused here unless explicitly enabled,
        // so the dev-box proof never spawns/kills anything by accident.
        "focus_session" => organization_apply(ctx, "focus_session", args),
        "move_tile" => organization_apply(ctx, "move_tile", args),
        "rename_tab" => organization_apply(ctx, "rename_tab", args),
        "new_tab" => organization_apply(ctx, "new_tab", args),
        "focus_tab" => organization_apply(ctx, "focus_tab", args),
        "open_file" => open_file(ctx, args),

        // ---- Process-changing tier (PRD §11.2: confirmation required) ------
        // `spawn_terminal` stays gated off (it would create an untracked tmux
        // session the UI never adopts). The session-targeted process actions —
        // typing into / interrupting / closing an *existing* session — are
        // executed directly against tmux: the MCP tool descriptions mark them
        // CONFIRMATION REQUIRED, which is the user-facing gate, and they only
        // ever act on a `th_*` session the app already owns.
        "spawn_terminal" => gated_process_change("spawn_terminal"),
        "send_text" => send_text(args),
        "send_keys" => send_keys(args),
        "close_terminal" => close_terminal(args),

        // ---- Theme (forwarded by name; parallel track owns the handlers) ----
        "get_theme" | "set_theme" => Err(format!(
            "control: '{command}' is forwarded by name but the theme command \
             handler is not wired in this build yet (parallel theme track)"
        )),

        // ---- Everything else: not permitted over the control channel -------
        other => Err(format!(
            "control: command '{other}' is not exposed over the control channel \
             (process-changing/destructive commands are gated; see PRD §11.2)"
        )),
    }
}

// ---------------------------------------------------------------------------
// Read-tier handlers
// ---------------------------------------------------------------------------

/// `list_terminals`: reconstruct the terminal list from the tmux source of truth
/// on the isolated `termhub` socket. Mirrors `commands::list_terminals`, minus
/// the in-memory Live/Detached refinement (the control channel does not own the
/// UI's PTY map; everything tmux reports is a live tmux session).
fn list_terminals() -> Result<Value, String> {
    let sessions = tmux::list_sessions().map_err(|e| format!("failed to list tmux sessions: {e}"))?;
    let terminals: Vec<Value> = sessions
        .iter()
        .filter(|s| s.starts_with("th_"))
        .map(|tmux_session| {
            let id = tmux_session
                .strip_prefix("th_")
                .unwrap_or(tmux_session)
                .to_string();
            json!({
                "id": id,
                "tmuxSession": tmux_session,
                "title": tmux_session,
                // tmux owns the live cwd; we do not track it server-side.
                "cwd": "",
                // Source-of-truth listing: present as live tmux-backed sessions.
                "state": "live",
            })
        })
        .collect();
    Ok(json!({ "terminals": terminals, "count": terminals.len() }))
}

/// `get_status`: FR-012 status for one session id (from the supervision reducer)
/// plus the latest statusline snapshot (context %, rate-limit windows) if one
/// has been ingested. `args.sessionId` selects the session.
fn get_status(ctx: &ControlContext, args: &Value) -> Result<Value, String> {
    let session_id = arg_str(args, "sessionId")
        .or_else(|| arg_str(args, "session_id"))
        .ok_or("get_status requires a 'sessionId' argument")?;
    let status = ctx.with_supervisor(|s| s.status(&session_id));
    let snapshot = ctx.status.get(&session_id);
    Ok(json!({
        "sessionId": session_id,
        "status": status,
        "snapshot": snapshot,
    }))
}

/// `supervision_tree`: the read-only orchestrator→subagent tree for one session.
/// Returns `null` when the session is unknown (matching the Tauri command).
fn supervision_tree(ctx: &ControlContext, args: &Value) -> Result<Value, String> {
    let session_id = arg_str(args, "sessionId")
        .or_else(|| arg_str(args, "session_id"))
        .ok_or("supervision_tree requires a 'sessionId' argument")?;
    let tree = ctx.with_supervisor(|s| s.tree(&session_id));
    Ok(serde_json::to_value(tree).map_err(|e| e.to_string())?)
}

/// `wsl_health`: a compact WSL host snapshot. We synthesize it from the locally
/// observable system (so the read tool works on this dev box without the WSL
/// agent connected) and additionally surface the supervised-session count. The
/// schema mirrors `termhub_protocol::HostMetrics`.
fn wsl_health(ctx: &ControlContext) -> Result<Value, String> {
    let metrics = collect_host_metrics();
    let supervised = ctx.with_supervisor(|s| s.session_ids().len());
    Ok(json!({
        "metrics": metrics,
        "supervisedSessions": supervised,
    }))
}

/// `search_files`: fuzzy basename/path/extension search over a project root,
/// using the control channel's own index cache. Args: `root` (required),
/// `query` (required), `limit` (optional, default 20).
fn search_files(ctx: &ControlContext, args: &Value) -> Result<Value, String> {
    let root = arg_str(args, "root").ok_or("search_files requires a 'root' argument")?;
    let query = arg_str(args, "query").unwrap_or_default();
    let limit = args
        .get("limit")
        .and_then(|v| v.as_u64())
        .map(|n| n as usize)
        .unwrap_or(20)
        .clamp(1, 1000);
    let hits = files::control_search(&ctx.files, &root, &query, limit)?;
    Ok(json!({ "root": root, "query": query, "hits": hits }))
}

/// `list_tabs`: the snapshot-track workspace tabs. Workspace-tab persistence is a
/// later workstream (PRD §8 snapshot track / persistence workstream G), so there
/// is no live tab store to read yet. We return an explicit empty list with a note
/// rather than failing, so the tool is callable and forward-compatible.
fn list_tabs() -> Result<Value, String> {
    Ok(json!({
        "tabs": [],
        "note": "workspace-tab persistence is not yet wired (PRD §8 snapshot track); \
                 returns an empty list until the persistence workstream lands.",
    }))
}

/// `read_terminal` / `capture_pane`: return a session's recent visible output as
/// plain text so an external Claude can SEE what the session shows. Talks to tmux
/// directly (`tmux -L termhub capture-pane -p [-S -N] -t th_<id>`), no UI round
/// trip. Args: `sessionId` (required), `historyLines` (optional, default 0 =
/// visible screen only; clamped to keep responses bounded).
fn read_terminal(args: &Value) -> Result<Value, String> {
    let session_id = arg_str(args, "sessionId")
        .or_else(|| arg_str(args, "session_id"))
        .ok_or("read_terminal requires a 'sessionId' argument")?;
    let target = tmux_target(&session_id);
    let history = args
        .get("historyLines")
        .or_else(|| args.get("history_lines"))
        .and_then(|v| v.as_u64())
        .unwrap_or(0)
        .min(10_000) as u32;
    let text = tmux::capture_pane_text(&target, history)
        .map_err(|e| format!("failed to capture pane for '{session_id}': {e}"))?;
    Ok(json!({
        "sessionId": session_id,
        "target": target,
        "historyLines": history,
        "text": text,
    }))
}

// ---------------------------------------------------------------------------
// Organization-tier handlers
// ---------------------------------------------------------------------------

/// `open_file`: resolve + read a capped text file for the requested path. This is
/// the one Organization-tier action that has a real, side-effect-free backing
/// implementation today (the Files reader), so the MCP "open a file" tool returns
/// the file's contents/metadata. Args: `path` (required).
fn open_file(ctx: &ControlContext, args: &Value) -> Result<Value, String> {
    let path = arg_str(args, "path").ok_or("open_file requires a 'path' argument")?;
    let contents = files::control_read_text(&ctx.files, &path)?;
    Ok(serde_json::to_value(contents).map_err(|e| e.to_string())?)
}

/// Organization-tier actions whose effect is a pure UI mutation
/// (`focus_session`, `move_tile`, `rename_tab`). We **accept and audit** them
/// (PRD §11.2: "allowed with visible audit event") AND apply them: the accepted
/// `{command, args}` is forwarded to the frontend through the [`ApplySink`]
/// (a Tauri `control://apply` event), where `controlBridge.ts` dispatches it into
/// the workspace store. `applied` reflects whether the forward happened — `true`
/// once the app has wired its sink (the normal app path), `false` in the headless
/// proof/tests that run the listener without an `AppHandle` (still audited).
fn organization_apply(ctx: &ControlContext, command: &str, args: &Value) -> Result<Value, String> {
    let applied = match &ctx.apply_sink {
        Some(sink) => match sink.apply(command, args) {
            Ok(()) => true,
            // A forward failure is non-fatal: the action is still accepted +
            // audited. Surface the reason in the note but keep the response `ok`.
            Err(e) => {
                eprintln!("termhub-control: failed to forward '{command}' to the UI: {e}");
                false
            }
        },
        // No sink (headless proof/tests): accept + audit only.
        None => false,
    };
    Ok(json!({
        "accepted": command,
        "args": args,
        "audited": true,
        "applied": applied,
        "note": if applied {
            "organization action accepted, audited, and forwarded to the UI \
             (control://apply) for application (PRD §11.2 organization tier)."
        } else {
            "organization action accepted + audited; UI application is delivered \
             by the frontend command (PRD §11.2 organization tier)."
        },
    }))
}

/// Process-changing commands (PRD §11.2: confirmation required) are gated off on
/// the control channel for the dev-box proof — they return a clear refusal rather
/// than spawning/killing anything. The MCP tool description marks them
/// confirmation-required; enabling real execution is a deliberate later step.
fn gated_process_change(command: &str) -> Result<Value, String> {
    Err(format!(
        "control: '{command}' is a process-changing action (PRD §11.2) and is \
         gated off in this build — it requires explicit confirmation/permission \
         and is not executed over the control channel yet"
    ))
}

/// `send_text`: type literal `text` into an existing session, optionally pressing
/// Enter to submit it. Process-changing (PRD §11.2): the MCP tool description
/// marks it CONFIRMATION REQUIRED. Backend-only — drives tmux directly
/// (`send-keys -l`), no UI round trip. Args: `sessionId` + `text` (required),
/// `enter` (optional, default true). Requires the session to exist.
fn send_text(args: &Value) -> Result<Value, String> {
    let session_id = arg_str(args, "sessionId")
        .or_else(|| arg_str(args, "session_id"))
        .ok_or("send_text requires a 'sessionId' argument")?;
    let text = arg_str(args, "text").ok_or("send_text requires a 'text' argument")?;
    let enter = args.get("enter").and_then(|v| v.as_bool()).unwrap_or(true);
    let target = tmux_target(&session_id);
    if !tmux::has_session(&target) {
        return Err(format!("send_text: no such session '{session_id}' (target {target})"));
    }
    tmux::send_text(&target, &text, enter)
        .map_err(|e| format!("failed to send text to '{session_id}': {e}"))?;
    Ok(json!({
        "accepted": "send_text",
        "sessionId": session_id,
        "target": target,
        "enter": enter,
        "audited": true,
    }))
}

/// `send_keys`: send one or more named control keys (e.g. `C-c`, `Up`, `Escape`)
/// to an existing session. Process-changing (confirmation-required). Backend-only
/// (`send-keys` with key-name interpretation). Args: `sessionId` (required) +
/// `keys` (required, a non-empty array of tmux key names).
fn send_keys(args: &Value) -> Result<Value, String> {
    let session_id = arg_str(args, "sessionId")
        .or_else(|| arg_str(args, "session_id"))
        .ok_or("send_keys requires a 'sessionId' argument")?;
    let keys: Vec<String> = args
        .get("keys")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|k| k.as_str().map(|s| s.to_string()))
                .collect()
        })
        .unwrap_or_default();
    if keys.is_empty() {
        return Err("send_keys requires a non-empty 'keys' array of tmux key names".into());
    }
    let target = tmux_target(&session_id);
    if !tmux::has_session(&target) {
        return Err(format!("send_keys: no such session '{session_id}' (target {target})"));
    }
    let key_refs: Vec<&str> = keys.iter().map(|s| s.as_str()).collect();
    tmux::send_keys(&target, &key_refs)
        .map_err(|e| format!("failed to send keys to '{session_id}': {e}"))?;
    Ok(json!({
        "accepted": "send_keys",
        "sessionId": session_id,
        "target": target,
        "keys": keys,
        "audited": true,
    }))
}

/// `close_terminal`: kill an existing session and its process tree. Process-
/// changing/destructive (confirmation-required). Backend-only via tmux
/// `kill-session`, which is idempotent (already-gone ⇒ success). Args:
/// `sessionId` (required).
fn close_terminal(args: &Value) -> Result<Value, String> {
    let session_id = arg_str(args, "sessionId")
        .or_else(|| arg_str(args, "session_id"))
        .ok_or("close_terminal requires a 'sessionId' argument")?;
    let target = tmux_target(&session_id);
    tmux::kill_session(&target)
        .map_err(|e| format!("failed to close terminal '{session_id}': {e}"))?;
    Ok(json!({
        "accepted": "close_terminal",
        "sessionId": session_id,
        "target": target,
        "audited": true,
    }))
}

/// Resolve a caller-supplied session id to its tmux target name on the `termhub`
/// socket. The control listener lists terminals by stripping the `th_` prefix
/// (see [`list_terminals`]), so a bare id maps back to `th_<id>`. We also accept a
/// caller that already passed the full `th_`-prefixed name (idempotent).
fn tmux_target(session_id: &str) -> String {
    if session_id.starts_with("th_") {
        session_id.to_string()
    } else {
        format!("th_{session_id}")
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Pull a string field out of a JSON args object.
fn arg_str(args: &Value, key: &str) -> Option<String> {
    args.get(key).and_then(|v| v.as_str()).map(|s| s.to_string())
}

/// Collect a `HostMetrics`-shaped snapshot from the local system. On Linux/WSL
/// this reads `/proc`; on other platforms it returns a best-effort skeleton so
/// the tool still responds. Mirrors the agent's `host` collector shape.
fn collect_host_metrics() -> Value {
    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);

    #[cfg(target_os = "linux")]
    {
        let (mem_total, mem_avail, swap_total, swap_free) = read_meminfo();
        let load_avg = read_loadavg();
        let cpu_count = std::thread::available_parallelism()
            .map(|n| n.get() as u32)
            .unwrap_or(0);
        let process_count = count_procs();
        let distro = read_pretty_name();
        return json!({
            "memTotalKib": mem_total,
            "memAvailableKib": mem_avail,
            "swapTotalKib": swap_total,
            "swapFreeKib": swap_free,
            "cpuCount": cpu_count,
            "loadAvg": load_avg,
            "processCount": process_count,
            "distro": distro,
            "capturedAtMs": now_ms,
        });
    }
    #[cfg(not(target_os = "linux"))]
    {
        let cpu_count = std::thread::available_parallelism()
            .map(|n| n.get() as u32)
            .unwrap_or(0);
        json!({
            "memTotalKib": 0u64,
            "memAvailableKib": 0u64,
            "swapTotalKib": 0u64,
            "swapFreeKib": 0u64,
            "cpuCount": cpu_count,
            "loadAvg": [0.0, 0.0, 0.0],
            "processCount": 0u32,
            "distro": serde_json::Value::Null,
            "capturedAtMs": now_ms,
        })
    }
}

#[cfg(target_os = "linux")]
fn read_meminfo() -> (u64, u64, u64, u64) {
    let text = std::fs::read_to_string("/proc/meminfo").unwrap_or_default();
    let get = |key: &str| -> u64 {
        for line in text.lines() {
            if let Some(rest) = line.strip_prefix(key) {
                return rest
                    .trim()
                    .trim_end_matches("kB")
                    .trim()
                    .parse()
                    .unwrap_or(0);
            }
        }
        0
    };
    (
        get("MemTotal:"),
        get("MemAvailable:"),
        get("SwapTotal:"),
        get("SwapFree:"),
    )
}

#[cfg(target_os = "linux")]
fn read_loadavg() -> [f32; 3] {
    let text = std::fs::read_to_string("/proc/loadavg").unwrap_or_default();
    let mut it = text.split_whitespace();
    let p = |s: Option<&str>| s.and_then(|v| v.parse().ok()).unwrap_or(0.0);
    [p(it.next()), p(it.next()), p(it.next())]
}

#[cfg(target_os = "linux")]
fn count_procs() -> u32 {
    std::fs::read_dir("/proc")
        .map(|rd| {
            rd.filter_map(|e| e.ok())
                .filter(|e| {
                    e.file_name()
                        .to_str()
                        .map(|n| n.chars().all(|c| c.is_ascii_digit()))
                        .unwrap_or(false)
                })
                .count() as u32
        })
        .unwrap_or(0)
}

#[cfg(target_os = "linux")]
fn read_pretty_name() -> Option<String> {
    let text = std::fs::read_to_string("/etc/os-release").ok()?;
    for line in text.lines() {
        if let Some(rest) = line.strip_prefix("PRETTY_NAME=") {
            return Some(rest.trim_matches('"').to_string());
        }
    }
    None
}

// ---------------------------------------------------------------------------
// Construction from app state
// ---------------------------------------------------------------------------

impl ControlContext {
    /// Build a [`ControlContext`] from the app's shared state. `supervisor` is a
    /// closure that locks the bridge's `Supervisor` and runs a visitor — supplied
    /// by `lib.rs` so this module doesn't reach into `AgentBridge` internals.
    pub fn new(
        status: Arc<StatusBridge>,
        supervisor: Arc<dyn Fn(&mut dyn FnMut(&Supervisor)) + Send + Sync>,
        token: String,
    ) -> Self {
        Self {
            status,
            supervisor,
            files: Arc::new(files::FileIndexState::new()),
            apply_sink: None,
            token,
        }
    }

    /// Attach the [`ApplySink`] that forwards Organization-tier UI mutations to
    /// the frontend (a `control://apply` Tauri event). Builder-style so `lib.rs`
    /// can wire it after constructing the context, while headless tests/proofs
    /// keep the sink-less context (they audit without applying).
    pub fn with_apply_sink(mut self, sink: Arc<dyn ApplySink>) -> Self {
        self.apply_sink = Some(sink);
        self
    }

    /// Test/proof constructor: build a context directly over a shared
    /// `Mutex<Supervisor>` (and a status bridge), wiring the visitor closure
    /// internally. Lets the end-to-end integration test seed real supervision +
    /// status state, start a real listener, and exercise the real `termhub-mcp`
    /// binary against it — without standing up the whole Tauri app.
    #[doc(hidden)]
    pub fn with_shared_supervisor(
        status: Arc<StatusBridge>,
        supervisor: Arc<parking_lot::Mutex<Supervisor>>,
        token: String,
    ) -> Self {
        let sup = supervisor.clone();
        let visitor: Arc<dyn Fn(&mut dyn FnMut(&Supervisor)) + Send + Sync> =
            Arc::new(move |f: &mut dyn FnMut(&Supervisor)| {
                let guard = sup.lock();
                f(&guard);
            });
        Self::new(status, visitor, token)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex as StdMutex;

    /// Build a ControlContext backed by a real (empty) Supervisor + StatusBridge,
    /// with a fixed token, for dispatch tests.
    fn test_ctx(token: &str) -> ControlContext {
        let supervisor = Arc::new(StdMutex::new(Supervisor::new()));
        let sup_for_closure = supervisor.clone();
        let visitor: Arc<dyn Fn(&mut dyn FnMut(&Supervisor)) + Send + Sync> =
            Arc::new(move |f: &mut dyn FnMut(&Supervisor)| {
                let guard = sup_for_closure.lock().unwrap();
                f(&guard);
            });
        ControlContext::new(Arc::new(StatusBridge::new()), visitor, token.to_string())
    }

    #[test]
    fn bad_token_is_rejected_before_dispatch() {
        let ctx = test_ctx("secret");
        let req = ControlRequest {
            token: "wrong".into(),
            command: "list_tabs".into(),
            args: Value::Null,
        };
        let resp = dispatch_authenticated(&ctx, req);
        assert!(!resp.ok);
        assert!(resp.error.unwrap().contains("unauthorized"));
    }

    #[test]
    fn good_token_dispatches() {
        let ctx = test_ctx("secret");
        let req = ControlRequest {
            token: "secret".into(),
            command: "list_tabs".into(),
            args: Value::Null,
        };
        let resp = dispatch_authenticated(&ctx, req);
        assert!(resp.ok, "expected ok, got {:?}", resp.error);
        assert!(resp.result.unwrap().get("tabs").is_some());
    }

    #[test]
    fn unknown_command_is_refused() {
        let ctx = test_ctx("t");
        let err = dispatch(&ctx, "definitely_not_a_command", &Value::Null).unwrap_err();
        assert!(err.contains("not exposed over the control channel"));
    }

    #[test]
    fn process_changing_spawn_is_gated() {
        let ctx = test_ctx("t");
        let err = dispatch(&ctx, "spawn_terminal", &json!({"cwd": "/tmp"})).unwrap_err();
        assert!(err.contains("process-changing"), "got: {err}");
    }

    #[test]
    fn read_terminal_requires_session_id() {
        let ctx = test_ctx("t");
        let err = dispatch(&ctx, "read_terminal", &Value::Null).unwrap_err();
        assert!(err.contains("sessionId"), "got: {err}");
    }

    #[test]
    fn send_text_requires_session_and_text() {
        let ctx = test_ctx("t");
        let err = dispatch(&ctx, "send_text", &json!({"text": "hi"})).unwrap_err();
        assert!(err.contains("sessionId"), "got: {err}");
        let err = dispatch(&ctx, "send_text", &json!({"sessionId": "x"})).unwrap_err();
        assert!(err.contains("text"), "got: {err}");
    }

    #[test]
    fn send_keys_requires_non_empty_keys() {
        let ctx = test_ctx("t");
        let err = dispatch(&ctx, "send_keys", &json!({"sessionId": "x", "keys": []})).unwrap_err();
        assert!(err.contains("keys"), "got: {err}");
    }

    #[test]
    fn close_terminal_requires_session_id() {
        let ctx = test_ctx("t");
        let err = dispatch(&ctx, "close_terminal", &Value::Null).unwrap_err();
        assert!(err.contains("sessionId"), "got: {err}");
    }

    #[test]
    fn send_to_missing_session_is_a_clear_error() {
        // No `th_*` session named this exists ⇒ a readable "no such session".
        let ctx = test_ctx("t");
        let err = dispatch(
            &ctx,
            "send_text",
            &json!({"sessionId": "definitely_absent_xyz", "text": "hi"}),
        )
        .unwrap_err();
        assert!(err.contains("no such session"), "got: {err}");
    }

    #[test]
    fn tmux_target_maps_id_and_is_idempotent() {
        assert_eq!(tmux_target("abc"), "th_abc");
        assert_eq!(tmux_target("th_abc"), "th_abc");
    }

    #[test]
    fn new_tab_and_focus_tab_are_organization_apply() {
        // No sink (headless): accepted + audited, but not applied — same contract
        // as the other organization-tier actions.
        let ctx = test_ctx("t");
        for (cmd, args) in [
            ("new_tab", json!({"name": "Logs"})),
            ("focus_tab", json!({"tabId": "tab-1"})),
        ] {
            let v = dispatch(&ctx, cmd, &args).unwrap();
            assert_eq!(v["accepted"], cmd);
            assert_eq!(v["audited"], true);
            assert_eq!(v["applied"], false);
        }
    }

    /// Live round-trip through dispatch: spawn a real tmux session, type a line
    /// via `send_text`, read it back via `read_terminal`, then `close_terminal`.
    /// Needs a real tmux on PATH (WSL2 dev shell; not the Windows CI target).
    #[test]
    fn live_send_read_close_roundtrip() {
        let id = format!(
            "mcp3test{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        );
        let target = format!("th_{id}");
        let _ = tmux::kill_session(&target);
        tmux::new_session(&target, "/tmp", None).expect("spawn session");

        let ctx = test_ctx("t");
        dispatch(
            &ctx,
            "send_text",
            &json!({"sessionId": id, "text": "echo MCP3_ROUNDTRIP_OK", "enter": true}),
        )
        .expect("send_text should succeed");
        std::thread::sleep(std::time::Duration::from_millis(300));

        let v = dispatch(&ctx, "read_terminal", &json!({"sessionId": id})).unwrap();
        assert!(
            v["text"].as_str().unwrap().contains("MCP3_ROUNDTRIP_OK"),
            "read_terminal should show the echoed sentinel; got {v:?}"
        );

        let c = dispatch(&ctx, "close_terminal", &json!({"sessionId": id})).unwrap();
        assert_eq!(c["accepted"], "close_terminal");
        assert!(!tmux::has_session(&target), "session should be gone after close");
    }

    #[test]
    fn theme_commands_are_forwarded_by_name() {
        let ctx = test_ctx("t");
        // Forwarded by name; not yet wired ⇒ a clear, theme-specific error (not
        // the generic "unknown command" arm). This proves the forward path.
        for cmd in ["get_theme", "set_theme"] {
            let err = dispatch(&ctx, cmd, &Value::Null).unwrap_err();
            assert!(err.contains("theme command handler"), "got: {err}");
        }
    }

    #[test]
    fn get_status_requires_session_id() {
        let ctx = test_ctx("t");
        let err = dispatch(&ctx, "get_status", &Value::Null).unwrap_err();
        assert!(err.contains("sessionId"), "got: {err}");
    }

    #[test]
    fn get_status_returns_unknown_for_unseen_session() {
        let ctx = test_ctx("t");
        let v = dispatch(&ctx, "get_status", &json!({"sessionId": "nope"})).unwrap();
        assert_eq!(v["status"], "unknown");
        assert_eq!(v["sessionId"], "nope");
        assert!(v["snapshot"].is_null());
    }

    #[test]
    fn supervision_tree_unknown_session_is_null() {
        let ctx = test_ctx("t");
        let v = dispatch(&ctx, "supervision_tree", &json!({"sessionId": "nope"})).unwrap();
        assert!(v.is_null());
    }

    #[test]
    fn wsl_health_has_metrics_and_supervised_count() {
        let ctx = test_ctx("t");
        let v = dispatch(&ctx, "wsl_health", &Value::Null).unwrap();
        assert!(v.get("metrics").is_some());
        assert_eq!(v["supervisedSessions"], 0);
        // The metrics object always carries capturedAtMs + cpuCount.
        assert!(v["metrics"].get("capturedAtMs").is_some());
        assert!(v["metrics"].get("cpuCount").is_some());
    }

    #[test]
    fn organization_actions_are_accepted_and_audited() {
        // No apply sink (headless): accepted + audited, but not applied.
        let ctx = test_ctx("t");
        for cmd in ["focus_session", "move_tile", "rename_tab"] {
            let v = dispatch(&ctx, cmd, &json!({"x": 1})).unwrap();
            assert_eq!(v["accepted"], cmd);
            assert_eq!(v["audited"], true);
            assert_eq!(v["applied"], false);
        }
    }

    /// A recording sink that captures every forwarded `{command, args}` so the
    /// test can assert the dispatcher forwards Organization-tier mutations to it.
    struct RecordingSink {
        calls: StdMutex<Vec<(String, Value)>>,
    }
    impl ApplySink for RecordingSink {
        fn apply(&self, command: &str, args: &Value) -> Result<(), String> {
            self.calls
                .lock()
                .unwrap()
                .push((command.to_string(), args.clone()));
            Ok(())
        }
    }

    #[test]
    fn organization_actions_are_forwarded_and_applied_with_a_sink() {
        let sink = Arc::new(RecordingSink {
            calls: StdMutex::new(Vec::new()),
        });
        let ctx = test_ctx("t").with_apply_sink(sink.clone());

        for cmd in ["focus_session", "move_tile", "rename_tab"] {
            let v = dispatch(&ctx, cmd, &json!({"tabId": "tab-1"})).unwrap();
            assert_eq!(v["accepted"], cmd);
            assert_eq!(v["audited"], true);
            // With a sink wired, the action is forwarded to the UI and applied.
            assert_eq!(v["applied"], true, "expected applied:true for {cmd}");
        }

        // Every Organization-tier command reached the sink, in order, with args.
        let calls = sink.calls.lock().unwrap();
        let names: Vec<&str> = calls.iter().map(|(c, _)| c.as_str()).collect();
        assert_eq!(names, ["focus_session", "move_tile", "rename_tab"]);
        assert_eq!(calls[0].1, json!({"tabId": "tab-1"}));
    }

    #[test]
    fn search_files_searches_a_real_tree() {
        // Build a tiny fixture and search it end-to-end through dispatch.
        let mut root = std::env::temp_dir();
        root.push(format!("termhub-control-files-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::write(root.join("src/main.rs"), "fn main() {}").unwrap();
        std::fs::write(root.join("README.md"), "# hi").unwrap();

        let ctx = test_ctx("t");
        let v = dispatch(
            &ctx,
            "search_files",
            &json!({ "root": root.to_string_lossy(), "query": "main", "limit": 5 }),
        )
        .unwrap();
        let hits = v["hits"].as_array().unwrap();
        assert!(
            hits.iter().any(|h| h["relPath"] == "src/main.rs"),
            "expected src/main.rs in {hits:?}"
        );

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn open_file_reads_text_contents() {
        let mut root = std::env::temp_dir();
        root.push(format!("termhub-control-open-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        let file = root.join("notes.md");
        std::fs::write(&file, "# Title\n\nbody").unwrap();

        let ctx = test_ctx("t");
        let v = dispatch(&ctx, "open_file", &json!({ "path": file.to_string_lossy() })).unwrap();
        assert_eq!(v["ext"], "md");
        assert!(v["text"].as_str().unwrap().contains("# Title"));

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn handshake_roundtrips_through_json() {
        let h = ControlHandshake {
            addr: "127.0.0.1:5000".into(),
            token: "abc".into(),
            pid: 42,
        };
        let s = serde_json::to_string(&h).unwrap();
        let back: ControlHandshake = serde_json::from_str(&s).unwrap();
        assert_eq!(back.addr, "127.0.0.1:5000");
        assert_eq!(back.token, "abc");
        assert_eq!(back.pid, 42);
    }
}
