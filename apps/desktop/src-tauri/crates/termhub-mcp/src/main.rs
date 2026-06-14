//! `termhub-mcp` — TermHub's local MCP server (PRD §9.6, §11.2).
//!
//! Launched by the MCP client (Claude) over **stdio**. It speaks the MCP
//! protocol (JSON-RPC 2.0, newline-delimited on stdin/stdout) and forwards every
//! `tools/call` to the running TermHub app over a **loopback control channel**
//! (the app listens on `127.0.0.1:<port>`; this binary discovers the port +
//! per-launch token from `~/.termhub/control.json`). Because forwarding is by
//! command name, this binary has **no compile-time coupling** to the app's
//! individual commands — the tool catalog ([`tools`]) is the only declaration
//! site and the app dispatches dynamically.
//!
//! ## CLI
//! - (default)        Run the stdio MCP server (what the client launches).
//! - `--list-tools`   Print the tool catalog as JSON and exit (offline; no app
//!                    needed). Handy for docs / debugging / the `.mcp.json` proof.
//! - `--version`      Print the version and exit.
//!
//! stderr is reserved for human-readable diagnostics so it never corrupts the
//! JSON-RPC frame stream on stdout.

mod control_client;
mod protocol;
mod server;
mod tools;

use std::io::Write;

fn main() {
    let mode = parse_args();
    match mode {
        Mode::Serve => {
            // Lock stdin/stdout once; the server loop owns them for its lifetime.
            let stdin = std::io::stdin();
            let stdout = std::io::stdout();
            let reader = stdin.lock();
            let writer = stdout.lock();
            if let Err(e) = server::run(reader, writer) {
                eprintln!("termhub-mcp: server loop exited with error: {e}");
                let _ = std::io::stderr().flush();
                std::process::exit(1);
            }
        }
        Mode::ListTools => {
            // Offline catalog dump (no app/control channel needed).
            let tools: Vec<serde_json::Value> =
                tools::catalog().iter().map(|t| t.to_mcp()).collect();
            let doc = serde_json::json!({ "tools": tools });
            println!(
                "{}",
                serde_json::to_string_pretty(&doc).expect("catalog serializes")
            );
        }
        Mode::Version => {
            println!("termhub-mcp {}", env!("CARGO_PKG_VERSION"));
        }
    }
}

enum Mode {
    Serve,
    ListTools,
    Version,
}

fn parse_args() -> Mode {
    for arg in std::env::args().skip(1) {
        match arg.as_str() {
            "--list-tools" => return Mode::ListTools,
            "--version" | "-V" => return Mode::Version,
            "--stdio" => return Mode::Serve, // explicit; the default anyway
            other => {
                eprintln!("termhub-mcp: ignoring unknown argument {other:?}");
            }
        }
    }
    Mode::Serve
}
