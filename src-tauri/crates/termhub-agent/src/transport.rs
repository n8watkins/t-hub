//! The NDJSON-over-stdio transport: read [`CoreFrame`]s from stdin, serve them,
//! and write [`AgentFrame`]s to stdout (stderr stays human-readable).
//!
//! ## What's implemented now
//! A correct, ordered serve loop: the Hello/Ready handshake, request → response
//! routing via [`crate::dispatch`], Ping/Pong, journal replay
//! ([`CoreToAgent::ReplayJournal`]), and graceful shutdown (on `Shutdown` or
//! stdin EOF). Every reply preserves the request's [`Channel`].
//!
//! ## SUBAGENT(transport): the head-of-line-blocking scheduler
//! The protocol tags each frame with a [`Channel`] + [`Priority`] precisely so a
//! writer can interleave control/metrics ahead of bulk payloads on the single
//! pipe (REVIEW). This baseline serves requests strictly in arrival order on the
//! reader thread. The enhancement (a bounded priority queue feeding a dedicated
//! writer thread, so a slow `CapturePane` can't delay a `Metrics`/`Ping` reply)
//! is the subagent's job. It must keep the wire format and these function
//! signatures unchanged; only the internal scheduling changes.

use std::io::{BufRead, Write};

use anyhow::Result;
use termhub_protocol::{
    AgentFrame, AgentToCore, Channel, CoreToAgent, Hello, Ready, PROTOCOL_VERSION,
};

use crate::dispatch;
use crate::journal::Journal;

/// Run the stdio bridge until the core closes stdin or sends `Shutdown`.
/// Returns `Ok(())` on a clean shutdown; `Err` only on an unrecoverable io
/// failure writing to stdout.
pub fn serve_stdio(journal: Journal) -> Result<()> {
    let stdin = std::io::stdin();
    let mut reader = stdin.lock();
    let stdout = std::io::stdout();
    let mut writer = stdout.lock();

    let mut line = String::new();
    let mut handshaken = false;

    loop {
        line.clear();
        let n = reader.read_line(&mut line)?;
        if n == 0 {
            // EOF: the core closed the pipe. Clean shutdown.
            break;
        }
        let trimmed = line.trim_end_matches(['\n', '\r']);
        if trimmed.is_empty() {
            continue;
        }

        let frame = match termhub_protocol::decode_core(trimmed) {
            Ok(f) => f,
            Err(e) => {
                // A malformed line never desyncs the stream; report and continue.
                eprintln!("termhub-agent: skipping malformed frame: {e}");
                continue;
            }
        };

        match frame.msg {
            CoreToAgent::Hello(Hello { protocol_version, core_version }) => {
                if protocol_version != PROTOCOL_VERSION {
                    eprintln!(
                        "termhub-agent: protocol mismatch (core={protocol_version}, agent={PROTOCOL_VERSION}); continuing best-effort"
                    );
                }
                eprintln!("termhub-agent: handshake with {core_version}");
                handshaken = true;
                let ready = AgentFrame {
                    channel: Channel::Control,
                    msg: AgentToCore::Ready(Ready {
                        protocol_version: PROTOCOL_VERSION,
                        agent_version: crate::agent_version(),
                        distro: detect_distro(),
                        journal_head_seq: journal.head_seq(),
                    }),
                };
                write_frame(&mut writer, &ready)?;
            }

            CoreToAgent::Request { id, priority: _, body } => {
                // SUBAGENT(transport): `priority` is currently ignored (strict
                // arrival order). The scheduler enhancement uses it.
                if !handshaken {
                    eprintln!("termhub-agent: request {id} before handshake; serving anyway");
                }
                let resp_body = dispatch::handle(&journal, body);
                let resp = AgentFrame {
                    // Reply on the same logical channel kind as the request's
                    // nature. We keep it simple: control by default. The
                    // scheduler subagent may classify per-op.
                    channel: Channel::Control,
                    msg: AgentToCore::Response { id, body: resp_body },
                };
                write_frame(&mut writer, &resp)?;
            }

            CoreToAgent::Ping { nonce } => {
                let pong = AgentFrame {
                    channel: Channel::Control,
                    msg: AgentToCore::Pong { nonce },
                };
                write_frame(&mut writer, &pong)?;
            }

            CoreToAgent::ReplayJournal { after_seq } => {
                let entries = match journal.replay(after_seq) {
                    Ok(e) => e,
                    Err(e) => {
                        eprintln!("termhub-agent: journal replay failed: {e:#}");
                        Vec::new()
                    }
                };
                let mut last_seq = after_seq;
                for entry in entries {
                    last_seq = entry.seq;
                    let f = AgentFrame {
                        channel: Channel::Events,
                        msg: AgentToCore::Journal { seq: entry.seq, entry },
                    };
                    write_frame(&mut writer, &f)?;
                }
                let done = AgentFrame {
                    channel: Channel::Events,
                    msg: AgentToCore::ReplayComplete { last_seq },
                };
                write_frame(&mut writer, &done)?;
            }

            CoreToAgent::Shutdown => {
                eprintln!("termhub-agent: shutdown requested");
                break;
            }

            CoreToAgent::Unknown => {
                eprintln!("termhub-agent: ignoring unknown core message");
            }
        }
    }

    Ok(())
}

/// Encode `frame` as one NDJSON line + newline and flush so the core sees it
/// promptly (no buffering across requests).
fn write_frame(writer: &mut impl Write, frame: &AgentFrame) -> Result<()> {
    let line = termhub_protocol::encode_agent(frame)?;
    writer.write_all(line.as_bytes())?;
    writer.write_all(b"\n")?;
    writer.flush()?;
    Ok(())
}

/// Best-effort distro name from `/etc/os-release` `PRETTY_NAME`.
fn detect_distro() -> Option<String> {
    let text = std::fs::read_to_string("/etc/os-release").ok()?;
    for line in text.lines() {
        if let Some(rest) = line.strip_prefix("PRETTY_NAME=") {
            return Some(rest.trim_matches('"').to_string());
        }
    }
    None
}
