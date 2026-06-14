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
//! - Ingest the Claude hook → journal spine: `--hook <EVENT>` mode reads the
//!   hook's JSON from stdin, appends a durable journal entry, and exits 0 so
//!   Claude's turn is never blocked ([`hook`]).
//! - Stream new journal entries live to the connected core ([`transport`]).
//!
//! ## Concurrency / head-of-line blocking
//! The protocol tags every frame with a [`termhub_protocol::Channel`] and every
//! request with a [`termhub_protocol::Priority`] so the writer can interleave
//! control/metrics ahead of bulk payloads on the single pipe (REVIEW). The
//! transport scheduler that exploits this is implemented in [`transport`]
//! (filled in by a subagent); `main` wires the pieces together.

mod dispatch;
mod hook;
mod host;
mod journal;
mod registry;
mod transport;

use std::io::Write;
use std::sync::Arc;

/// CLI surface.
///
/// ## Modes (mutually exclusive)
/// - `--stdio`         Run the NDJSON bridge on stdin/stdout (long-lived agent).
/// - `--hook <EVENT>`  Hook ingest: read one JSON object from stdin, append it
///                     to the journal, exit 0.  Never blocks; never fails Claude.
///
/// ## Shared flags
/// - `--journal-dir <PATH>`  Override the journal directory (default:
///                           `~/.termhub/journal`). Used by tests and by the
///                           core when it relocates the store.
struct Args {
    /// Which mode to run in.
    mode: Mode,
    /// Override the journal directory (default: `~/.termhub/journal`).
    journal_dir: Option<String>,
}

enum Mode {
    Stdio,
    Hook { event: String },
    None,
}

fn parse_args() -> Args {
    let mut mode = Mode::None;
    let mut journal_dir = None;
    let mut it = std::env::args().skip(1);
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--stdio" => mode = Mode::Stdio,
            "--hook" => {
                match it.next() {
                    Some(event) => mode = Mode::Hook { event },
                    None => {
                        eprintln!("termhub-agent: --hook requires an EVENT name argument");
                        std::process::exit(1);
                    }
                }
            }
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
    Args { mode, journal_dir }
}

/// Human-readable agent build string sent in the handshake.
pub fn agent_version() -> String {
    format!("termhub-agent {}", env!("CARGO_PKG_VERSION"))
}

fn main() {
    let args = parse_args();

    match args.mode {
        // ------------------------------------------------------------------
        // --hook <EVENT>: short-lived hook ingest.  MUST exit 0 always.
        // ------------------------------------------------------------------
        Mode::Hook { event } => {
            if let Err(e) = hook::run(&event, args.journal_dir.as_deref()) {
                eprintln!("termhub-agent --hook {event}: unexpected error: {e:#}");
            }
            // Always exit 0 — never fail Claude's turn.
            std::process::exit(0);
        }

        // ------------------------------------------------------------------
        // --stdio: long-lived NDJSON bridge.
        // ------------------------------------------------------------------
        Mode::Stdio => {
            let journal_dir = journal::resolve_journal_dir(args.journal_dir.as_deref());
            let journal = match journal::Journal::open(&journal_dir) {
                Ok(j) => j,
                Err(e) => {
                    eprintln!(
                        "termhub-agent: failed to open journal at {journal_dir:?}: {e:#}"
                    );
                    std::process::exit(1);
                }
            };

            if let Err(e) = transport::serve_stdio(Arc::new(journal)) {
                // A clean EOF on stdin (core closed the pipe) is a normal
                // shutdown, not an error; `serve_stdio` returns Ok in that
                // case. A real error here means the loop itself failed.
                eprintln!("termhub-agent: stdio bridge exited with error: {e:#}");
                let _ = std::io::stderr().flush();
                std::process::exit(1);
            }
        }

        // ------------------------------------------------------------------
        // No mode selected.
        // ------------------------------------------------------------------
        Mode::None => {
            eprintln!(
                "termhub-agent {}: no mode selected; pass --stdio or --hook <EVENT>.",
                env!("CARGO_PKG_VERSION")
            );
            std::process::exit(2);
        }
    }
}
