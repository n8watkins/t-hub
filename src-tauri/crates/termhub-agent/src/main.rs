//! `termhub-agent` — the WSL-side control agent (PLAN.md Workstream A).
//!
//! Launched by the TermHub core as:
//! ```text
//! wsl.exe -d <distro> -- termhub-agent --stdio
//! ```
//! or directly on a unix dev box (`termhub-agent --stdio`). It speaks the
//! versioned NDJSON protocol from `termhub-protocol` over **stdin/stdout**
//! (stderr is reserved for human-readable diagnostics so it never corrupts the
//! frame stream).
//!
//! ## Responsibilities (0.5)
//! - Maintain a durable, append-only **event journal** on the WSL VHDX
//!   ([`journal`]) — the reconstruction-intent authority that survives the
//!   Windows app closing, replayed to the core on connect.
//! - Serve control RPCs: tmux/session registry ([`registry`]), host metrics +
//!   git/worktree queries ([`host`]).
//! - Ingest the Claude hook → journal spine (hook handler scripts append to the
//!   journal file; the agent tails it and forwards new entries — wired in a
//!   later round; the journal append API is defined now).
//!
//! ## Concurrency / head-of-line blocking
//! The protocol tags every frame with a [`termhub_protocol::Channel`] and every
//! request with a [`termhub_protocol::Priority`] so the writer can interleave
//! control/metrics ahead of bulk payloads on the single pipe (REVIEW). The
//! transport scheduler that exploits this is implemented in [`transport`]
//! (filled in by a subagent); `main` wires the pieces together.

mod dispatch;
mod host;
mod journal;
mod registry;
mod transport;

use std::io::Write;

/// CLI surface. Only `--stdio` is meaningful in 0.5 (run the NDJSON bridge on
/// stdin/stdout); other modes are reserved.
struct Args {
    stdio: bool,
    /// Override the journal directory (default: `~/.termhub/journal`). Used by
    /// tests and by the core when it relocates the store.
    journal_dir: Option<String>,
}

fn parse_args() -> Args {
    let mut stdio = false;
    let mut journal_dir = None;
    let mut it = std::env::args().skip(1);
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--stdio" => stdio = true,
            "--journal-dir" => journal_dir = it.next(),
            "--version" | "-V" => {
                println!("termhub-agent {}", env!("CARGO_PKG_VERSION"));
                std::process::exit(0);
            }
            other => {
                eprintln!("termhub-agent: ignoring unknown argument {other:?}");
            }
        }
    }
    Args { stdio, journal_dir }
}

/// Human-readable agent build string sent in the handshake.
pub fn agent_version() -> String {
    format!("termhub-agent {}", env!("CARGO_PKG_VERSION"))
}

fn main() {
    let args = parse_args();
    if !args.stdio {
        eprintln!(
            "termhub-agent {}: no mode selected; pass --stdio to run the NDJSON bridge.",
            env!("CARGO_PKG_VERSION")
        );
        std::process::exit(2);
    }

    // Open (or create) the durable journal before serving so we can report its
    // head sequence in the handshake.
    let journal_dir = journal::resolve_journal_dir(args.journal_dir.as_deref());
    let journal = match journal::Journal::open(&journal_dir) {
        Ok(j) => j,
        Err(e) => {
            eprintln!("termhub-agent: failed to open journal at {journal_dir:?}: {e:#}");
            std::process::exit(1);
        }
    };

    if let Err(e) = transport::serve_stdio(journal) {
        // A clean EOF on stdin (core closed the pipe) is a normal shutdown, not
        // an error; `serve_stdio` returns Ok in that case. A real error here
        // means the loop itself failed.
        eprintln!("termhub-agent: stdio bridge exited with error: {e:#}");
        let _ = std::io::stderr().flush();
        std::process::exit(1);
    }
}
