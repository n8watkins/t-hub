#![allow(clippy::doc_overindented_list_items)]

//! `t-hub-agent` — the WSL-side control agent (PLAN.md Workstream A).
//!
//! Launched by the T-Hub core as:
//! ```text
//! wsl.exe -d <distro> -- t-hub-agent --stdio
//! ```
//! or directly on a unix dev box (`t-hub-agent --stdio`). It speaks the
//! versioned NDJSON protocol from `t-hub-protocol` over **stdin/stdout**
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
//! - Ingest Claude's **statusline** JSON: `--statusline` mode reads the
//!   statusline payload from stdin, appends a durable `StatusSnapshot` journal
//!   entry, prints a one-line readout to stdout (so it's a valid statusline
//!   command), and exits 0 ([`hook::run_statusline`]). This is the data source
//!   for the sidebar's Claude USAGE strip (cost / context % / rate limits).
//!
//! ## Concurrency / head-of-line blocking
//! The protocol tags every frame with a [`t_hub_protocol::Channel`] and every
//! request with a [`t_hub_protocol::Priority`] so the writer can interleave
//! control/metrics ahead of bulk payloads on the single pipe (REVIEW). The
//! transport scheduler that exploits this is implemented in [`transport`]
//! (filled in by a subagent); `main` wires the pieces together.

mod codex;
mod dispatch;
mod gate;
mod hook;
mod host;
mod journal;
mod registry;
mod transport;

use std::io::Write;
use std::process::Command;
use std::sync::Arc;

use anyhow::{bail, Context};
use serde_json::json;
use t_hub_protocol::{EventJournalEntry, JournalEventType, JournalSource};

/// CLI surface.
///
/// ## Modes (mutually exclusive)
/// - `--stdio`         Run the NDJSON bridge on stdin/stdout (long-lived agent).
/// - `--hook <EVENT>`  Hook ingest: read one JSON object from stdin, append it
///                     to the journal, exit 0.  Never blocks; never fails Claude.
/// - `--statusline`    Statusline ingest: read Claude's statusline JSON from
///                     stdin, append a `StatusSnapshot` journal entry, echo a
///                     short readout to stdout, exit 0. Never blocks Claude.
/// - `--codex-tap`     Structured Codex lifecycle ingest: read Codex `exec
///                     --json` or mirrored app-server JSONL from stdin and append
///                     normalized, credential-safe lifecycle events.
/// - `--codex-unobserved` Record one credential-safe degraded marker for an
///                        interactive Codex TUI in its exact owning tmux pane
///                        when structured telemetry is unavailable.
/// - `--gate` item-3 Pillar C: the BLOCKING `PreToolUse` gate - reads the hook JSON
///   on stdin, classifies the Bash command, and DENIES an outward-facing action a
///   crew may not take (fail-closed).
///
/// ## Shared flags
/// - `--journal-dir <PATH>`  Override the journal directory (default:
///                           `~/.t-hub/journal`). Used by tests and by the
///                           core when it relocates the store.
struct Args {
    /// Which mode to run in.
    mode: Mode,
    /// Override the journal directory (then `T_HUB_AGENT_JOURNAL_DIR`, then `~/.t-hub/journal`).
    journal_dir: Option<String>,
}

enum Mode {
    Stdio,
    Hook {
        event: String,
    },
    /// Statusline ingest: read Claude's statusline JSON from stdin, journal a
    /// `StatusSnapshot`, echo a readout, exit 0.
    Statusline,
    /// Structured Codex lifecycle ingest for headless and interactive telemetry.
    CodexTap,
    /// Explicit degraded marker for an interactive Codex TUI without lifecycle
    /// telemetry, bound to its exact owning tmux pane.
    CodexUnobserved,
    /// item-3 Pillar C: the BLOCKING `PreToolUse` gate. Reads the hook JSON on stdin,
    /// classifies the Bash command, resolves the caller's capability class from the
    /// app, and DENIES an outward-facing action a crew may not take (or a significant
    /// deploy/spend lacking a verified general authorization). Fail-closed.
    Gate,
    None,
}

fn parse_args() -> Args {
    let mut mode = Mode::None;
    let mut journal_dir = None;
    let mut it = std::env::args().skip(1);
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--stdio" => mode = Mode::Stdio,
            "--hook" => match it.next() {
                Some(event) => mode = Mode::Hook { event },
                None => {
                    eprintln!("t-hub-agent: --hook requires an EVENT name argument");
                    std::process::exit(1);
                }
            },
            "--statusline" => mode = Mode::Statusline,
            "--codex-tap" => mode = Mode::CodexTap,
            "--codex-unobserved" => mode = Mode::CodexUnobserved,
            "--gate" => mode = Mode::Gate,
            "--journal-dir" => journal_dir = it.next(),
            "--version" | "-V" => {
                println!("t-hub-agent {}", env!("CARGO_PKG_VERSION"));
                std::process::exit(0);
            }
            other => {
                eprintln!("t-hub-agent: ignoring unknown argument {other:?}");
            }
        }
    }
    Args { mode, journal_dir }
}

/// Human-readable agent build string sent in the handshake.
pub fn agent_version() -> String {
    format!("t-hub-agent {}", env!("CARGO_PKG_VERSION"))
}

fn main() {
    let args = parse_args();

    match args.mode {
        // ------------------------------------------------------------------
        // --hook <EVENT>: short-lived hook ingest.  MUST exit 0 always.
        // ------------------------------------------------------------------
        Mode::Hook { event } => {
            if let Err(e) = hook::run(&event, args.journal_dir.as_deref()) {
                eprintln!("t-hub-agent --hook {event}: unexpected error: {e:#}");
            }
            // Always exit 0 — never fail Claude's turn.
            std::process::exit(0);
        }

        // ------------------------------------------------------------------
        // --statusline: short-lived statusline ingest.  MUST exit 0 always so
        // it stays a well-behaved Claude statusline command.
        // ------------------------------------------------------------------
        Mode::Statusline => {
            if let Err(e) = hook::run_statusline(args.journal_dir.as_deref()) {
                eprintln!("t-hub-agent --statusline: unexpected error: {e:#}");
            }
            // Always exit 0 — never fail Claude's statusline render.
            std::process::exit(0);
        }

        // ------------------------------------------------------------------
        // --codex-tap: structured Codex lifecycle ingest.
        // ------------------------------------------------------------------
        Mode::CodexTap => match codex::run(args.journal_dir.as_deref()) {
            Ok(outcome) if outcome.turn_failed => std::process::exit(1),
            Ok(_) => std::process::exit(0),
            Err(error) => {
                eprintln!("t-hub-agent --codex-tap: {error:#}");
                std::process::exit(1);
            }
        },

        // ------------------------------------------------------------------
        // --codex-unobserved: record an explicit, pane-bound degraded marker.
        // This operation must succeed before the shell guard execs Codex.
        // ------------------------------------------------------------------
        Mode::CodexUnobserved => match run_codex_unobserved(args.journal_dir.as_deref()) {
            Ok(()) => std::process::exit(0),
            Err(error) => {
                eprintln!("t-hub-agent --codex-unobserved: {error:#}");
                std::process::exit(1);
            }
        },

        // ------------------------------------------------------------------
        // --gate: the BLOCKING PreToolUse gate (item-3 Pillar C). Emits a deny
        // decision for a blocked outward-facing command, else stays silent so the
        // normal permission flow proceeds. Always exits 0 (the decision is carried
        // in the JSON output, not the exit code).
        // ------------------------------------------------------------------
        Mode::Gate => {
            gate::run();
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
                    eprintln!("t-hub-agent: failed to open journal at {journal_dir:?}: {e:#}");
                    std::process::exit(1);
                }
            };

            // Startup compaction: trim ephemeral status snapshots if the journal
            // has grown past the cap. Done HERE — before the core connects —
            // because compaction renumbers line-position sequences, and doing it
            // mid-connection would push new seqs below the core's cursor and
            // silently stall delivery. The incremental tail keeps live delivery
            // cheap at any size; this only bounds disk between restarts.
            if journal.byte_len() > journal::COMPACT_THRESHOLD_BYTES {
                match journal.compact_dropping_status() {
                    Ok((before, after, kept)) => eprintln!(
                        "t-hub-agent: compacted journal on startup: {before} -> {after} bytes \
                         ({kept} durable entries kept; dropped ephemeral status snapshots)"
                    ),
                    Err(e) => eprintln!(
                        "t-hub-agent: startup journal compaction failed (continuing): {e:#}"
                    ),
                }
            }

            if let Err(e) = transport::serve_stdio(Arc::new(journal)) {
                // A clean EOF on stdin (core closed the pipe) is a normal
                // shutdown, not an error; `serve_stdio` returns Ok in that
                // case. A real error here means the loop itself failed.
                eprintln!("t-hub-agent: stdio bridge exited with error: {e:#}");
                let _ = std::io::stderr().flush();
                std::process::exit(1);
            }
        }

        // ------------------------------------------------------------------
        // No mode selected.
        // ------------------------------------------------------------------
        Mode::None => {
            eprintln!(
                "t-hub-agent {}: no mode selected; pass --stdio, --hook <EVENT>, --statusline, --codex-tap, or --codex-unobserved.",
                env!("CARGO_PKG_VERSION")
            );
            std::process::exit(2);
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct TmuxProvenance {
    session_name: String,
    session_id: String,
    session_created: u64,
    window_id: String,
    pane_id: String,
    pane_pid: u32,
}

fn run_codex_unobserved(journal_dir: Option<&str>) -> anyhow::Result<()> {
    let provenance = observe_tmux_provenance()?;
    let dir = journal::resolve_journal_dir(journal_dir);
    let journal =
        journal::Journal::open(&dir).with_context(|| format!("opening journal at {dir:?}"))?;
    let entry = EventJournalEntry {
        seq: 0,
        timestamp_ms: now_ms(),
        source: JournalSource::Agent,
        entity_id: Some(format!(
            "codex-unobserved:{}:{}",
            provenance.session_id, provenance.pane_id
        )),
        event_type: JournalEventType::AgentCommand,
        payload: json!({
            "schema": "t-hub.codex.unobserved.v1",
            "provider": "codex",
            "provider_version": "0.144.4",
            "session_id": format!(
                "codex-unobserved:{}:{}",
                provenance.session_id, provenance.pane_id
            ),
            "lifecycle": "telemetry_health",
            "tmux_session": provenance.session_name.clone(),
            "runtime_health": "degraded",
            "agent_status": "unknown",
            "transport": "unavailable",
            "telemetry": {
                "transport": "unavailable",
                "quality": "stale",
                "runtime_health": "degraded",
                "detail": "interactive_tui_lifecycle_unsupported",
            },
            "tmux": {
                "session_name": provenance.session_name,
                "session_id": provenance.session_id,
                "session_created": provenance.session_created,
                "window_id": provenance.window_id,
                "pane_id": provenance.pane_id,
                "pane_pid": provenance.pane_pid,
            }
        }),
        result: None,
    };
    journal
        .append(entry)
        .context("appending interactive Codex degraded marker")?;
    Ok(())
}

fn observe_tmux_provenance() -> anyhow::Result<TmuxProvenance> {
    const FORMAT: &str =
        "#{session_name}\t#{session_id}\t#{session_created}\t#{window_id}\t#{pane_id}\t#{pane_pid}";
    const MAX_OUTPUT_BYTES: usize = 512;

    let expected_pane = std::env::var("TMUX_PANE")
        .context("interactive Codex degraded marker requires TMUX_PANE")?;
    validate_prefixed_number("tmux pane id", &expected_pane, '%')?;
    let output = Command::new("tmux")
        .args(["display-message", "-p", "-t", &expected_pane, FORMAT])
        .output()
        .context("reading owning tmux pane")?;
    if !output.status.success() || !output.stderr.is_empty() {
        bail!("owning tmux pane is unavailable");
    }
    if output.stdout.is_empty() || output.stdout.len() > MAX_OUTPUT_BYTES {
        bail!("owning tmux pane returned invalid provenance");
    }
    let line = std::str::from_utf8(&output.stdout)
        .context("owning tmux pane returned non-UTF-8 provenance")?
        .trim_end_matches(['\r', '\n']);
    if line.contains(['\r', '\n']) {
        bail!("owning tmux pane returned ambiguous provenance");
    }
    let fields = line.split('\t').collect::<Vec<_>>();
    let [session_name, session_id, session_created, window_id, pane_id, pane_pid] =
        fields.as_slice()
    else {
        bail!("owning tmux pane returned incomplete provenance");
    };
    if session_name.is_empty()
        || session_name.len() > 128
        || !session_name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
    {
        bail!("owning tmux session name is invalid");
    }
    validate_prefixed_number("tmux session id", session_id, '$')?;
    validate_prefixed_number("tmux window id", window_id, '@')?;
    validate_prefixed_number("tmux pane id", pane_id, '%')?;
    if *pane_id != expected_pane {
        bail!("owning tmux pane identity changed");
    }
    let session_created = parse_positive_number("tmux session creation time", session_created)?;
    let pane_pid = u32::try_from(parse_positive_number("tmux pane pid", pane_pid)?)
        .context("tmux pane pid is out of range")?;
    Ok(TmuxProvenance {
        session_name: (*session_name).to_string(),
        session_id: (*session_id).to_string(),
        session_created,
        window_id: (*window_id).to_string(),
        pane_id: (*pane_id).to_string(),
        pane_pid,
    })
}

fn validate_prefixed_number(field: &str, value: &str, prefix: char) -> anyhow::Result<()> {
    let digits = value
        .strip_prefix(prefix)
        .filter(|digits| !digits.is_empty() && digits.len() <= 20)
        .filter(|digits| digits.bytes().all(|byte| byte.is_ascii_digit()));
    if digits.is_none() {
        bail!("{field} is invalid");
    }
    Ok(())
}

fn parse_positive_number(field: &str, value: &str) -> anyhow::Result<u64> {
    let parsed = value
        .parse::<u64>()
        .ok()
        .filter(|value| *value > 0)
        .with_context(|| format!("{field} is invalid"))?;
    Ok(parsed)
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}
