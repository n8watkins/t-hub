//! `th` — T-Hub's canonical agent+human CLI.
//!
//! A thin socket CLIENT of the app's existing control protocol (the same
//! NDJSON-over-loopback-TCP the MCP server uses). It lets the captain and
//! crewmates drive T-Hub from anywhere — inside Claude Code (via Bash), a raw
//! terminal, Tailscale SSH, the phone, or scripts — with no MCP runtime.
//!
//! Design language is AXI: `th` with no args prints a fleet home view, every
//! command ends with suggested next commands, and read commands take `--json`
//! for machine-mode output.

mod control;
mod render;

use std::collections::{HashMap, HashSet};
use std::process::ExitCode;

use serde_json::{json, Value};

use control::Endpoint;

const VERSION: &str = env!("CARGO_PKG_VERSION");

fn main() -> ExitCode {
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

    match run(&args) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("th: {e}");
            ExitCode::FAILURE
        }
    }
}

fn run(args: &[String]) -> Result<(), String> {
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
        other => Err(format!(
            "unknown command '{other}'. Run `th --help` for the command list."
        )),
    }
}

// ---- commands ---------------------------------------------------------------

fn cmd_home(args: &[String]) -> Result<(), String> {
    let f = Flags::parse(args, &[])?;
    let ep = endpoint()?;
    let terminals = control::call(&ep, "list_terminals", json!({}))?;
    let tabs = control::call(&ep, "list_tabs", json!({}))?;
    if f.json {
        print_json(&json!({ "terminals": terminals, "tabs": tabs }));
    } else {
        render::home(&terminals, &tabs);
    }
    Ok(())
}

fn cmd_ls(args: &[String]) -> Result<(), String> {
    let f = Flags::parse(args, &[])?;
    let ep = endpoint()?;
    let result = control::call(&ep, "list_terminals", json!({}))?;
    if f.json {
        print_json(&result);
    } else {
        render::terminals(&result);
    }
    Ok(())
}

fn cmd_read(args: &[String]) -> Result<(), String> {
    let f = Flags::parse(args, &["--history"])?;
    let session = f.positional(0, "read", "<session>")?;
    let history: i64 = match f.opts.get("--history") {
        Some(v) => v
            .parse()
            .map_err(|_| format!("--history expects an integer, got '{v}'"))?,
        None => 0,
    };
    let ep = endpoint()?;
    let result = control::call(
        &ep,
        "read_terminal",
        json!({ "sessionId": session, "historyLines": history }),
    )?;
    if f.json {
        print_json(&result);
    } else {
        render::read_output(&result);
    }
    Ok(())
}

fn cmd_status(args: &[String]) -> Result<(), String> {
    let f = Flags::parse(args, &[])?;
    let ep = endpoint()?;

    // Single-session drill-down.
    if let Some(session) = f.pos.first() {
        let status = control::call(&ep, "get_status", json!({ "sessionId": session }))?;
        let tree = control::call(&ep, "supervision_tree", json!({ "sessionId": session }))?;
        if f.json {
            print_json(&json!({ "status": status, "supervisionTree": tree }));
        } else {
            render::status_one(session, &status, &tree);
        }
        return Ok(());
    }

    // Fleet-wide: one row per live terminal.
    let terminals = control::call(&ep, "list_terminals", json!({}))?;
    let ids: Vec<String> = terminals
        .get("terminals")
        .and_then(|t| t.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|t| t.get("id").and_then(|i| i.as_str()).map(String::from))
                .collect()
        })
        .unwrap_or_default();

    let mut rows = Vec::new();
    let mut raw = Vec::new();
    for id in &ids {
        let status = control::call(&ep, "get_status", json!({ "sessionId": id }))?;
        let st = status
            .get("status")
            .and_then(|s| s.as_str())
            .unwrap_or("unknown")
            .to_string();
        let ctx = status
            .get("snapshot")
            .and_then(|s| s.get("contextUsedPct"))
            .and_then(|p| p.as_f64())
            .map(|p| format!("{p:.0}%"))
            .unwrap_or_else(|| "-".to_string());
        rows.push(render::StatusRow {
            session: id.clone(),
            status: st,
            ctx,
        });
        raw.push(status);
    }

    if f.json {
        print_json(&json!(raw));
    } else {
        render::status_table(&rows);
    }
    Ok(())
}

fn cmd_send(args: &[String]) -> Result<(), String> {
    // Everything after the session id is literal text; `--no-enter` suppresses
    // the trailing Enter. We parse the flag out but keep the rest verbatim.
    let mut enter = true;
    let mut positionals: Vec<String> = Vec::new();
    for a in args {
        if a == "--no-enter" {
            enter = false;
        } else {
            positionals.push(a.clone());
        }
    }
    if positionals.len() < 2 {
        return Err("usage: th send <session> <text...>  [--no-enter]".to_string());
    }
    let session = &positionals[0];
    let text = positionals[1..].join(" ");

    let ep = endpoint()?;
    match control::call(
        &ep,
        "send_text",
        json!({ "sessionId": session, "text": text, "enter": enter }),
    ) {
        Ok(result) => {
            println!("sent to {session}: {:?}{}", text, if enter { " ⏎" } else { "" });
            if result.get("accepted").is_some() {
                println!("  {}", compact(&result));
            }
            render::next(&[
                ("th read <session>", "see the session's response"),
                ("th send <session> <text>", "keep the conversation going"),
            ]);
            Ok(())
        }
        Err(e) => Err(format!(
            "{e}\n  (send_text is process-changing; it acts only on an existing th_<id> session)"
        )),
    }
}

fn cmd_spawn(args: &[String]) -> Result<(), String> {
    let f = Flags::parse(args, &["--name"])?;
    let cwd = f.positional(0, "spawn", "<cwd>")?;
    let mut spawn_args = json!({ "cwd": cwd });
    if let Some(name) = f.opts.get("--name") {
        spawn_args["name"] = json!(name);
    }
    let ep = endpoint()?;
    match control::call(&ep, "spawn_terminal", spawn_args) {
        Ok(result) => {
            println!("spawned: {}", compact(&result));
            render::next(&[("th ls", "list live terminals")]);
            Ok(())
        }
        // spawn_terminal is gated off in the running build (PRD §11.2). Surface
        // the server's message plus the operator fallback — do NOT try to bypass.
        Err(e) => Err(format!(
            "{e}\n\
             fallback: create the terminal from the T-Hub app UI, or use \
             `th worktree new <repoRoot> <branch>` to open a worktree tab with a \
             spawned terminal."
        )),
    }
}

fn cmd_worktree(args: &[String]) -> Result<(), String> {
    let sub = args.first().map(String::as_str).unwrap_or("");
    let rest = if args.is_empty() { &[][..] } else { &args[1..] };
    match sub {
        "new" | "add" => cmd_worktree_new(rest),
        "rm" | "remove" => cmd_worktree_rm(rest),
        "" => Err("usage: th worktree <new|rm> ...".to_string()),
        other => Err(format!("unknown worktree subcommand '{other}' (expected new|rm)")),
    }
}

fn cmd_worktree_new(args: &[String]) -> Result<(), String> {
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

    let mut wt_args = json!({
        "repoRoot": repo_root,
        "worktreePath": worktree_path,
        "branch": branch,
    });
    if let Some(tab) = f.opts.get("--tab") {
        wt_args["tabName"] = json!(tab);
    }

    let ep = endpoint()?;
    match control::call(&ep, "create_worktree", wt_args) {
        Ok(result) => {
            println!("worktree created: {}", compact(&result));
            render::next(&[
                ("th", "fleet home view (find the new tab's terminal)"),
                ("th worktree rm <repoRoot> <path>", "remove it when done"),
            ]);
            Ok(())
        }
        Err(e) => Err(e),
    }
}

fn cmd_worktree_rm(args: &[String]) -> Result<(), String> {
    let f = Flags::parse(args, &["--branch"])?;
    let repo_root = f.positional(0, "worktree rm", "<repoRoot>")?;
    let worktree_path = f.positional(1, "worktree rm", "<path>")?;
    if f.opts.contains_key("--branch") {
        eprintln!("th: note — remove_worktree keys off the path, not a branch; --branch is ignored.");
    }
    let force = f.bools.contains("--force");

    let ep = endpoint()?;
    match control::call(
        &ep,
        "remove_worktree",
        json!({ "repoRoot": repo_root, "worktreePath": worktree_path, "force": force }),
    ) {
        Ok(result) => {
            println!("worktree removed: {}", compact(&result));
            render::next(&[("th", "fleet home view")]);
            Ok(())
        }
        Err(e) => Err(e),
    }
}

fn cmd_tabs(args: &[String]) -> Result<(), String> {
    let f = Flags::parse(args, &[])?;
    let ep = endpoint()?;
    let result = control::call(&ep, "list_tabs", json!({}))?;
    if f.json {
        print_json(&result);
    } else {
        render::tabs(&result);
    }
    Ok(())
}

fn cmd_health(args: &[String]) -> Result<(), String> {
    let f = Flags::parse(args, &[])?;
    let ep = endpoint()?;
    let result = control::call(&ep, "wsl_health", json!({}))?;
    if f.json {
        print_json(&result);
    } else {
        render::health(&result);
    }
    Ok(())
}

fn cmd_events(args: &[String]) -> Result<(), String> {
    let _ = Flags::parse(args, &[])?;
    let ep = endpoint()?;
    eprintln!("th: subscribing to control://event — Ctrl-C to stop");
    control::subscribe(&ep, |frame| {
        println!("{}", compact(&frame));
    })
}

// ---- helpers ----------------------------------------------------------------

fn endpoint() -> Result<Endpoint, String> {
    control::resolve_endpoint()
}

fn print_json(v: &Value) {
    println!("{}", serde_json::to_string_pretty(v).unwrap_or_default());
}

fn compact(v: &Value) -> String {
    serde_json::to_string(v).unwrap_or_default()
}

/// A tiny hand-rolled arg parser (AXI CLIs favor a minimal, dependency-free
/// surface). Splits tokens into positionals, value options (`--flag v` or
/// `--flag=v` for names in `value_flags`), and boolean flags (everything else
/// starting with `--`). `--json` is always recognized as the machine-mode flag.
struct Flags {
    pos: Vec<String>,
    opts: HashMap<String, String>,
    bools: HashSet<String>,
    json: bool,
}

impl Flags {
    fn parse(args: &[String], value_flags: &[&str]) -> Result<Flags, String> {
        let value_set: HashSet<&str> = value_flags.iter().copied().collect();
        let mut pos = Vec::new();
        let mut opts = HashMap::new();
        let mut bools = HashSet::new();
        let mut json = false;

        let mut i = 0;
        while i < args.len() {
            let a = &args[i];
            if let Some(rest) = a.strip_prefix("--") {
                // `--flag=value` form.
                if let Some((name, val)) = rest.split_once('=') {
                    let name = format!("--{name}");
                    if name == "--json" {
                        json = true;
                    } else {
                        opts.insert(name, val.to_string());
                    }
                } else if a == "--json" {
                    json = true;
                } else if value_set.contains(a.as_str()) {
                    // `--flag value` form.
                    let val = args
                        .get(i + 1)
                        .ok_or_else(|| format!("{a} expects a value"))?;
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

        Ok(Flags {
            pos,
            opts,
            bools,
            json,
        })
    }

    fn positional(&self, idx: usize, cmd: &str, name: &str) -> Result<String, String> {
        self.pos
            .get(idx)
            .cloned()
            .ok_or_else(|| format!("th {cmd}: missing required argument {name}"))
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
  send <session> <text...>  type text into a session          [--no-enter]\n\
  spawn <cwd>               spawn a terminal (gated in this build) [--name N]\n\
  worktree new <repoRoot> <branch>   create a git worktree + tab  [--path P] [--tab T]\n\
  worktree rm  <repoRoot> <path>     remove a git worktree        [--force]\n\
  tabs                      list workspace tabs                [--json]\n\
  health                    WSL host snapshot                  [--json]\n\
  events                    stream the control event bus (Ctrl-C to stop)\n\
\n\
flags:\n\
  --json        machine-mode output on read commands\n\
  -h, --help    this help\n\
  -V, --version version\n\
\n\
discovery: $T_HUB_CONTROL_ADDR + $T_HUB_CONTROL_TOKEN, else $T_HUB_CONTROL_FILE,\n\
           else ~/.t-hub/control.json (honored like the MCP server).\n\
\n\
examples:\n\
  th\n\
  th ls\n\
  th read 052ccbb2 --history 200\n\
  th status\n\
  th send 052ccbb2 'ls -la'\n\
  th health --json"
    );
}
