//! `th` — T-Hub's canonical agent+human CLI.
//!
//! A thin socket CLIENT of the app's existing control protocol (the same
//! NDJSON-over-loopback-TCP the MCP server uses). It lets the captain and
//! crewmates drive T-Hub from anywhere — inside Claude Code (via Bash), a raw
//! terminal, Tailscale SSH, the phone, or scripts — with no MCP runtime.
//!
//! Design language is AXI: `th` with no args prints a fleet home view, every
//! command ends with runnable "Next" hints, read commands take `--json` for a
//! stable machine envelope, output is bounded + sorted, and the shell exit code
//! is a stable taxonomy agents can branch on (see [`exit`]).

mod control;
mod render;

use std::collections::{HashMap, HashSet};
use std::io::IsTerminal;
use std::process::ExitCode;

use serde_json::{json, Value};

use control::{ControlError, Endpoint};
use render::Ui;

const VERSION: &str = env!("CARGO_PKG_VERSION");

/// Stable exit-code taxonomy. Agents branch on `$?`, so these never move:
///   0 success · 2 usage/bad-args · 3 app-not-running (discovery/connect) ·
///   4 server error (`ok:false`) · 5 gated / permission-denied (e.g. the spawn
///   gate) · 6 protocol-version mismatch. A gated action or an app-down case
///   MUST NOT exit 0.
mod exit {
    pub const USAGE: u8 = 2;
    pub const APP_DOWN: u8 = 3;
    pub const SERVER_ERROR: u8 = 4;
    pub const GATED: u8 = 5;
    pub const PROTOCOL: u8 = 6;
}

/// A CLI failure carrying its stable exit code + a machine-friendly `kind`.
struct CliError {
    code: u8,
    kind: &'static str,
    message: String,
}

impl CliError {
    fn usage(message: impl Into<String>) -> Self {
        CliError { code: exit::USAGE, kind: "usage", message: message.into() }
    }
}

/// Map a classified control error onto the exit taxonomy. A server `ok:false`
/// whose message reads as a gate/confirmation becomes GATED (5), not a plain
/// server error (4), so agents can tell "denied by policy" from "it failed".
impl From<ControlError> for CliError {
    fn from(e: ControlError) -> Self {
        match e {
            ControlError::AppDown(m) => CliError { code: exit::APP_DOWN, kind: "app_down", message: m },
            ControlError::Protocol(m) => CliError { code: exit::PROTOCOL, kind: "protocol", message: m },
            ControlError::Server(m) => {
                if is_gated(&m) {
                    CliError { code: exit::GATED, kind: "gated", message: m }
                } else {
                    CliError { code: exit::SERVER_ERROR, kind: "server_error", message: m }
                }
            }
        }
    }
}

/// Does a server error read as a permission/confirmation gate rather than a
/// generic failure? Keyed off the app's own gating language (PRD §11.2).
fn is_gated(message: &str) -> bool {
    let m = message.to_lowercase();
    m.contains("gated") || m.contains("confirmation") || m.contains("process-changing")
}

/// Restore the OS-default SIGPIPE handling so a downstream `head`/`grep` that
/// closes the pipe early kills `th` conventionally (exit 141) instead of Rust
/// panicking on an EPIPE from `println!`. Piping into `head` is routine for an
/// agent-grade CLI, so this must never surface a panic.
#[cfg(unix)]
fn reset_sigpipe() {
    // SAFETY: `signal` is async-signal-safe and we call it once, before any I/O.
    unsafe {
        libc::signal(libc::SIGPIPE, libc::SIG_DFL);
    }
}
#[cfg(not(unix))]
fn reset_sigpipe() {}

fn main() -> ExitCode {
    reset_sigpipe();
    let args: Vec<String> = std::env::args().skip(1).collect();

    // Top-level help / version, before we touch the socket.
    match args.first().map(String::as_str) {
        Some("-h") | Some("--help") | Some("help") => {
            print_help();
            return ExitCode::SUCCESS;
        }
        Some("-V") | Some("-v") | Some("--version") => {
            println!("th {VERSION}");
            return ExitCode::SUCCESS;
        }
        _ => {}
    }

    // Whether the caller asked for the machine envelope decides how we render an
    // error too: JSON errors go to stdout (agents parse them), text to stderr.
    let wants_json = args.iter().any(|a| a == "--json");
    let command = command_label(&args);

    match run(&args) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            if wants_json {
                emit_json_err(&command, &e);
            } else {
                eprintln!("th: {}", e.message);
            }
            ExitCode::from(e.code)
        }
    }
}

/// The command label used in the JSON envelope + error reporting.
fn command_label(args: &[String]) -> String {
    match args.first().map(String::as_str) {
        None | Some("") => "home".to_string(),
        Some("worktree") | Some("wt") => match args.get(1).map(String::as_str) {
            Some(sub) if !sub.starts_with('-') => format!("worktree {sub}"),
            _ => "worktree".to_string(),
        },
        Some(c) => c.to_string(),
    }
}

fn run(args: &[String]) -> Result<(), CliError> {
    let cmd = args.first().map(String::as_str).unwrap_or("");
    let rest = if args.is_empty() { &[][..] } else { &args[1..] };

    match cmd {
        "" => cmd_home(rest),
        "ls" | "list" => cmd_ls(rest),
        "read" => cmd_read(rest),
        "status" => cmd_status(rest),
        "send" => cmd_send(rest),
        "spawn" => cmd_spawn(rest),
        "worktree" | "wt" => cmd_worktree(rest),
        "tabs" => cmd_tabs(rest),
        "health" => cmd_health(rest),
        "events" | "watch" => cmd_events(rest),
        other => Err(CliError::usage(format!(
            "unknown command '{other}'. Run `th --help` for the command list."
        ))),
    }
}

// ---- commands ---------------------------------------------------------------

fn cmd_home(args: &[String]) -> Result<(), CliError> {
    let f = Flags::parse(args, &[])?;
    let ui = f.ui();
    let ep = endpoint()?;
    let terminals = control::call(&ep, "list_terminals", json!({}))?;
    let tabs = control::call(&ep, "list_tabs", json!({}))?;
    if f.json {
        emit_json_ok(
            "home",
            json!({
                "terminals": render::sort_terminals(&terminals),
                "tabs": render::sort_tabs(&tabs),
            }),
        );
    } else {
        render::home(&terminals, &tabs, &ui);
    }
    Ok(())
}

fn cmd_ls(args: &[String]) -> Result<(), CliError> {
    let f = Flags::parse(args, &[])?;
    let ui = f.ui();
    let ep = endpoint()?;
    let result = control::call(&ep, "list_terminals", json!({}))?;
    if f.json {
        let terms = render::sort_terminals(&result);
        emit_json_ok("ls", json!({ "count": terms.len(), "terminals": terms }));
    } else {
        render::terminals(&result, &ui);
    }
    Ok(())
}

fn cmd_read(args: &[String]) -> Result<(), CliError> {
    let f = Flags::parse(args, &["--history"])?;
    let ui = f.ui();
    let session = f.positional(0, "read", "<session>")?;
    let history: i64 = match f.opts.get("--history") {
        Some(v) => v.parse().map_err(|_| CliError::usage(format!("--history expects an integer, got '{v}'")))?,
        None => 0,
    };
    let ep = endpoint()?;
    let result = control::call(&ep, "read_terminal", json!({ "sessionId": session, "historyLines": history }))?;
    if f.json {
        emit_json_ok("read", result);
    } else {
        render::read_output(&result, &ui);
    }
    Ok(())
}

fn cmd_status(args: &[String]) -> Result<(), CliError> {
    let f = Flags::parse(args, &[])?;
    let ui = f.ui();
    let ep = endpoint()?;

    // Single-session drill-down.
    if let Some(session) = f.pos.first() {
        let status = control::call(&ep, "get_status", json!({ "sessionId": session }))?;
        let tree = control::call(&ep, "supervision_tree", json!({ "sessionId": session }))?;
        if f.json {
            emit_json_ok("status", json!({ "status": status, "supervisionTree": tree }));
        } else {
            render::status_one(session, &status, &tree, &ui);
        }
        return Ok(());
    }

    // Fleet-wide: one row per live terminal, sorted by id for stable diffs.
    let terminals = control::call(&ep, "list_terminals", json!({}))?;
    let ids: Vec<String> = render::sort_terminals(&terminals)
        .iter()
        .filter_map(|t| t.get("id").and_then(|i| i.as_str()).map(String::from))
        .collect();

    let mut rows = Vec::new();
    let mut raw = Vec::new();
    for id in &ids {
        let status = control::call(&ep, "get_status", json!({ "sessionId": id }))?;
        let st = status.get("status").and_then(|s| s.as_str()).unwrap_or("unknown").to_string();
        let ctx = status
            .get("snapshot")
            .and_then(|s| s.get("contextUsedPct"))
            .and_then(|p| p.as_f64())
            .map(|p| format!("{p:.0}%"))
            .unwrap_or_else(|| "-".to_string());
        rows.push(render::StatusRow { session: id.clone(), status: st, ctx });
        raw.push(status);
    }

    if f.json {
        emit_json_ok("status", json!({ "count": raw.len(), "sessions": raw }));
    } else {
        render::status_table(&rows, &ui);
    }
    Ok(())
}

fn cmd_send(args: &[String]) -> Result<(), CliError> {
    // Everything after the session id is literal text; `--no-enter` suppresses
    // the trailing Enter, `--json` picks the machine envelope. The rest is kept
    // verbatim (so quoted text passes through unchanged).
    let mut enter = true;
    let mut json_mode = false;
    let mut positionals: Vec<String> = Vec::new();
    for a in args {
        match a.as_str() {
            "--no-enter" => enter = false,
            "--json" => json_mode = true,
            _ => positionals.push(a.clone()),
        }
    }
    if positionals.len() < 2 {
        return Err(CliError::usage("usage: th send <session> <text...>  [--no-enter] [--json]"));
    }
    let session = &positionals[0];
    let text = positionals[1..].join(" ");

    let ep = endpoint()?;
    let result = control::call(&ep, "send_text", json!({ "sessionId": session, "text": text, "enter": enter }))?;
    if json_mode {
        emit_json_ok("send", json!({ "sessionId": session, "text": text, "enter": enter, "result": result }));
    } else {
        let ui = Ui { tty: std::io::stdout().is_terminal(), all: false };
        println!("sent to {session}: {text:?}{}", if enter { "  ⏎" } else { "" });
        render::next(&ui, &[(format!("th read {session}"), "see the session's response")]);
    }
    Ok(())
}

fn cmd_spawn(args: &[String]) -> Result<(), CliError> {
    let f = Flags::parse(args, &["--name"])?;
    let cwd = f.positional(0, "spawn", "<cwd>")?;
    let mut spawn_args = json!({ "cwd": cwd });
    if let Some(name) = f.opts.get("--name") {
        spawn_args["name"] = json!(name);
    }
    let ep = endpoint()?;
    // spawn_terminal is gated off in the running build (PRD §11.2). We surface
    // the server's message verbatim and map it to exit 5 (gated) via the From
    // impl — do NOT try to bypass it — plus an operator fallback in text mode.
    match control::call(&ep, "spawn_terminal", spawn_args) {
        Ok(result) => {
            if f.json {
                emit_json_ok("spawn", result);
            } else {
                let ui = f.ui();
                println!("spawned: {}", compact(&result));
                render::next(&ui, &[("th ls".to_string(), "list live terminals")]);
            }
            Ok(())
        }
        Err(e) => {
            let mut err: CliError = e.into();
            if err.kind == "gated" && !f.json {
                err.message.push_str(
                    "\nfallback: create the terminal from the T-Hub app UI, or use \
                     `th worktree new <repoRoot> <branch>` to open a worktree tab with a \
                     spawned terminal.",
                );
            }
            Err(err)
        }
    }
}

fn cmd_worktree(args: &[String]) -> Result<(), CliError> {
    let sub = args.first().map(String::as_str).unwrap_or("");
    let rest = if args.is_empty() { &[][..] } else { &args[1..] };
    match sub {
        "new" | "add" => cmd_worktree_new(rest),
        "rm" | "remove" => cmd_worktree_rm(rest),
        "" => Err(CliError::usage("usage: th worktree <new|rm> ...")),
        other => Err(CliError::usage(format!("unknown worktree subcommand '{other}' (expected new|rm)"))),
    }
}

fn cmd_worktree_new(args: &[String]) -> Result<(), CliError> {
    let f = Flags::parse(args, &["--path", "--tab"])?;
    let repo_root = f.positional(0, "worktree new", "<repoRoot>")?;
    let branch = f.positional(1, "worktree new", "<branch>")?;

    // The server requires an absolute worktreePath. If --path is omitted we
    // derive one under the repo's `.claude/worktrees/<branchLeaf>` (matching
    // this project's own worktree convention).
    let worktree_path = match f.opts.get("--path") {
        Some(p) => p.clone(),
        None => {
            let leaf = branch.rsplit('/').next().unwrap_or(&branch);
            format!("{}/.claude/worktrees/{}", repo_root.trim_end_matches('/'), leaf)
        }
    };

    let mut wt_args = json!({ "repoRoot": repo_root, "worktreePath": worktree_path, "branch": branch });
    if let Some(tab) = f.opts.get("--tab") {
        wt_args["tabName"] = json!(tab);
    }

    let ep = endpoint()?;
    let result = control::call(&ep, "create_worktree", wt_args)?;
    if f.json {
        emit_json_ok("worktree new", result);
    } else {
        let ui = f.ui();
        println!("worktree created: {}", compact(&result));
        render::next(
            &ui,
            &[
                ("th".to_string(), "fleet home view (find the new tab's terminal)"),
                (format!("th worktree rm {repo_root} {worktree_path}"), "remove it when done"),
            ],
        );
    }
    Ok(())
}

fn cmd_worktree_rm(args: &[String]) -> Result<(), CliError> {
    let f = Flags::parse(args, &["--branch"])?;
    let repo_root = f.positional(0, "worktree rm", "<repoRoot>")?;
    let worktree_path = f.positional(1, "worktree rm", "<path>")?;
    if f.opts.contains_key("--branch") {
        eprintln!("th: note — remove_worktree keys off the path, not a branch; --branch is ignored.");
    }
    let force = f.bools.contains("--force");

    let ep = endpoint()?;
    let result = control::call(
        &ep,
        "remove_worktree",
        json!({ "repoRoot": repo_root, "worktreePath": worktree_path, "force": force }),
    )?;
    if f.json {
        emit_json_ok("worktree rm", result);
    } else {
        let ui = f.ui();
        println!("worktree removed: {}", compact(&result));
        render::next(&ui, &[("th".to_string(), "fleet home view")]);
    }
    Ok(())
}

fn cmd_tabs(args: &[String]) -> Result<(), CliError> {
    let f = Flags::parse(args, &[])?;
    let ui = f.ui();
    let ep = endpoint()?;
    let result = control::call(&ep, "list_tabs", json!({}))?;
    if f.json {
        let tabs = render::sort_tabs(&result);
        emit_json_ok("tabs", json!({ "count": tabs.len(), "tabs": tabs }));
    } else {
        render::tabs(&result, &ui);
    }
    Ok(())
}

fn cmd_health(args: &[String]) -> Result<(), CliError> {
    let f = Flags::parse(args, &[])?;
    let ui = f.ui();
    let ep = endpoint()?;
    let result = control::call(&ep, "wsl_health", json!({}))?;
    if f.json {
        emit_json_ok("health", result);
    } else {
        render::health(&result, &ui);
    }
    Ok(())
}

fn cmd_events(args: &[String]) -> Result<(), CliError> {
    let _ = Flags::parse(args, &[])?;
    let ep = endpoint()?;
    eprintln!("th: subscribing to control://event — Ctrl-C to stop");
    // Each frame is already NDJSON; stream it verbatim, one object per line.
    control::subscribe(&ep, |frame| println!("{}", compact(&frame)))?;
    Ok(())
}

// ---- JSON envelope (stable contract) ---------------------------------------

/// Emit the stable success envelope: `{ ok, command, data, error:null }`.
fn emit_json_ok(command: &str, data: Value) {
    let env = json!({ "ok": true, "command": command, "data": data, "error": Value::Null });
    println!("{}", serde_json::to_string_pretty(&env).unwrap_or_default());
}

/// Emit the stable failure envelope: `{ ok:false, command, data:null, error }`.
/// The `error.code` matches the process exit code, so agents can read either.
fn emit_json_err(command: &str, e: &CliError) {
    let env = json!({
        "ok": false,
        "command": command,
        "data": Value::Null,
        "error": { "code": e.code, "kind": e.kind, "message": e.message },
    });
    println!("{}", serde_json::to_string_pretty(&env).unwrap_or_default());
}

// ---- helpers ----------------------------------------------------------------

fn endpoint() -> Result<Endpoint, CliError> {
    control::resolve_endpoint().map_err(Into::into)
}

fn compact(v: &Value) -> String {
    serde_json::to_string(v).unwrap_or_default()
}

/// A tiny hand-rolled arg parser (AXI CLIs favor a minimal, dependency-free
/// surface). Splits tokens into positionals, value options (`--flag v` or
/// `--flag=v` for names in `value_flags`), and boolean flags (everything else
/// starting with `--`). `--json` and `--all` are always recognized.
struct Flags {
    pos: Vec<String>,
    opts: HashMap<String, String>,
    bools: HashSet<String>,
    json: bool,
    all: bool,
}

impl Flags {
    fn parse(args: &[String], value_flags: &[&str]) -> Result<Flags, CliError> {
        let value_set: HashSet<&str> = value_flags.iter().copied().collect();
        let mut pos = Vec::new();
        let mut opts = HashMap::new();
        let mut bools = HashSet::new();
        let mut json = false;
        let mut all = false;

        let mut i = 0;
        while i < args.len() {
            let a = &args[i];
            if let Some(rest) = a.strip_prefix("--") {
                // `--flag=value` form.
                if let Some((name, val)) = rest.split_once('=') {
                    let name = format!("--{name}");
                    match name.as_str() {
                        "--json" => json = true,
                        "--all" => all = true,
                        _ => {
                            opts.insert(name, val.to_string());
                        }
                    }
                } else if a == "--json" {
                    json = true;
                } else if a == "--all" {
                    all = true;
                } else if value_set.contains(a.as_str()) {
                    // `--flag value` form.
                    let val = args.get(i + 1).ok_or_else(|| CliError::usage(format!("{a} expects a value")))?;
                    opts.insert(a.clone(), val.clone());
                    i += 1;
                } else {
                    bools.insert(a.clone());
                }
            } else {
                pos.push(a.clone());
            }
            i += 1;
        }

        Ok(Flags { pos, opts, bools, json, all })
    }

    fn positional(&self, idx: usize, cmd: &str, name: &str) -> Result<String, CliError> {
        self.pos
            .get(idx)
            .cloned()
            .ok_or_else(|| CliError::usage(format!("th {cmd}: missing required argument {name}")))
    }

    /// The render context: TTY-detected stdout + the `--all` cap override.
    fn ui(&self) -> Ui {
        Ui { tty: std::io::stdout().is_terminal(), all: self.all }
    }
}

fn print_help() {
    println!(
        "th — T-Hub CLI (control-socket client)  v{VERSION}\n\
\n\
usage: th [command] [args] [flags]\n\
\n\
commands:\n\
  (none)                    fleet home view: live terminals, tabs, next commands\n\
  ls                        list live terminals\n\
  read <session>            read a terminal's recent output   [--history N] [--json]\n\
  status [<session>]        FR-012 status (all sessions, or one + its tree) [--json]\n\
  send <session> <text...>  type text into a session          [--no-enter] [--json]\n\
  spawn <cwd>               spawn a terminal (gated in this build) [--name N]\n\
  worktree new <repoRoot> <branch>   create a git worktree + tab  [--path P] [--tab T]\n\
  worktree rm  <repoRoot> <path>     remove a git worktree        [--force]\n\
  tabs                      list workspace tabs                [--json]\n\
  health                    WSL host snapshot                  [--json]\n\
  events                    stream the control event bus (Ctrl-C to stop)\n\
\n\
flags:\n\
  --json        stable machine envelope: {{ok, command, data, error}} (read cmds)\n\
  --all         show every row (human lists are capped at 20 otherwise)\n\
  -h, --help    this help\n\
  -V, --version version\n\
\n\
exit codes (agents branch on $?):\n\
  0  success\n\
  2  usage / bad arguments\n\
  3  app not running (discovery or connect failed)\n\
  4  server error (the app answered ok:false)\n\
  5  gated / permission-denied (e.g. the spawn gate)\n\
  6  control protocol-version mismatch\n\
\n\
discovery: $T_HUB_CONTROL_ADDR + $T_HUB_CONTROL_TOKEN, else $T_HUB_CONTROL_FILE,\n\
           else ~/.t-hub/control.json (honored like the MCP server).\n\
\n\
output: piped (non-TTY) output drops column padding but stays structured; no\n\
        color, spinners, or cursor escapes are ever emitted. --json is full +\n\
        sorted; human lists are sorted + capped for stable, bounded output.\n\
\n\
examples:\n\
  th\n\
  th ls --all\n\
  th read 052ccbb2 --history 200\n\
  th status --json\n\
  th send 052ccbb2 'ls -la'\n\
  th health --json"
    );
}
