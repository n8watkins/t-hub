//! Claude plan usage via the `/usage` slash command.
//!
//! The statusline `rate_limits` block (claude/status.rs) only exists on Pro/Max
//! AND only after the first API response, so it often reads blank. Claude Code's
//! `/usage` command, run headlessly as `claude -p /usage`, always prints the
//! plan usage directly — exactly what the user wants ("how much weekly do I have
//! left"):
//!
//! ```text
//! Current session: 23% used · resets Jun 14, 11:10pm (America/Los_Angeles)
//! Current week (all models): 42% used · resets Jun 20, 9pm (America/Los_Angeles)
//! Current week (Sonnet only): 0% used
//! ```
//!
//! We run it on demand (the sidebar polls), parse the percentages + reset text,
//! and return them. On Windows we shell into WSL through the user's INTERACTIVE
//! login shell (`$SHELL -ilc`) so `claude` resolves on the PATH set in ~/.zshrc
//! (same reason resolve_pane_command uses `-ilc`).

use serde::Serialize;

/// Parsed `/usage` output. Every field is optional so a missing/!changed line
/// degrades gracefully. Percentages are the USED amount (0..=100); the UI shows
/// "left" = 100 - used. Resets are kept as Claude's human text (e.g. "Jun 20,
/// 9pm") — good enough for a sidebar hint without timezone parsing.
#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ClaudeUsage {
    pub session_used_pct: Option<f32>,
    pub session_resets: Option<String>,
    pub week_used_pct: Option<f32>,
    pub week_resets: Option<String>,
    pub week_sonnet_used_pct: Option<f32>,
    /// True when we got a recognizable usage readout at all (vs. an error / not
    /// logged in / `/usage` unavailable).
    pub ok: bool,
}

/// Tauri command: run `claude -p /usage` and parse it. Best-effort: returns a
/// `ClaudeUsage { ok: false }` rather than erroring so the sidebar can show a
/// gentle hint.
#[tauri::command]
pub async fn claude_usage() -> Result<ClaudeUsage, String> {
    Ok(tauri::async_runtime::spawn_blocking(claude_usage_blocking)
        .await
        .unwrap_or_default())
}

/// SYNC `/usage` read — the core of [`claude_usage`] minus the async/`spawn_blocking`
/// wrapper. The control channel calls this (server-split M3) to serve the daemon's
/// Claude plan usage over the socket, so a thin client gets the sidebar Usage strip
/// remotely. Runs the same `claude -p /usage` flow on a (blocking) control connection
/// thread — the blocking process IO is fine there. No cache: `/usage` is itself a
/// fresh per-call network read (and the sidebar polls it at a long interval).
pub fn claude_usage_blocking() -> ClaudeUsage {
    run_usage()
}

/// How many times to (re)run `/usage` before giving up. `claude -p /usage` is
/// flaky: the session/week percentages arrive after a network round-trip and the
/// process frequently exits having printed only the intro line, so a single run
/// succeeds well under half the time. Re-running until a parse yields numbers
/// makes the sidebar reliably populate. Bounded to 2 so a persistently-failing
/// state (logged out / Claude down) settles after a couple tries instead of
/// looping — note this means the failure path DOES run all 2 attempts (it only
/// short-circuits on a successful parse). Each attempt spawns a heavy nested
/// process tree (wsl.exe -> bash -> script -> $SHELL -ilc -> claude), and this
/// fires on window focus alongside other pollers, so the worst-case spawn count
/// is kept low; usage is also polled at a long interval (UsageStrip POLL_MS).
const USAGE_ATTEMPTS: usize = 2;

fn run_usage() -> ClaudeUsage {
    let mut last = ClaudeUsage::default();
    for attempt in 1..=USAGE_ATTEMPTS {
        let out = match usage_command().output() {
            Ok(o) => o,
            Err(e) => {
                crate::diag::diag_log(format!(
                    "{{\"t\":\"usage\",\"m\":\"claude -p /usage spawn FAILED: {e}\"}}"
                ));
                return ClaudeUsage::default();
            }
        };
        let text = String::from_utf8_lossy(&out.stdout);
        let parsed = parse_usage(&text);
        crate::diag::diag_log(format!(
            "{{\"t\":\"usage\",\"m\":\"claude -p /usage attempt={}/{} ok={} week={:?} session={:?}\"}}",
            attempt, USAGE_ATTEMPTS, parsed.ok, parsed.week_used_pct, parsed.session_used_pct
        ));
        if parsed.ok {
            return parsed;
        }
        last = parsed;
    }
    last
}

/// The shell command that runs `/usage` and prints the full readout.
///
/// TWO things are needed or `/usage` prints nothing useful:
///   1. claude must be on PATH — it lives in `~/.npm-global/bin` exported in
///      ~/.zshrc, which only an INTERACTIVE login shell sources -> `$SHELL -ilc`.
///   2. A PSEUDO-TTY — `claude -p /usage` only prints the session/week numbers
///      when attached to a terminal; piped (our captured stdout) it prints just
///      the intro line. `script -qec '<cmd>' /dev/null` runs `<cmd>` under a pty,
///      so the numbers appear. (Verified: piped = intro only; under `script` =
///      full output.)
const USAGE_SHELL_CMD: &str =
    "script -qec 'exec \"${SHELL:-/bin/sh}\" -ilc \"claude -p /usage\"' /dev/null";

/// Build the invocation. Windows: through WSL. unix (dev): a login shell.
#[cfg(windows)]
fn usage_command() -> std::process::Command {
    use std::os::windows::process::CommandExt;
    let mut c = std::process::Command::new("wsl.exe");
    let distro = std::env::var("T_HUB_DISTRO").unwrap_or_else(|_| "Ubuntu-24.04".to_string());
    // `-e` (exec) runs bash DIRECTLY. With a bare `--`, wsl.exe routes the command
    // through the user's default login shell (e.g. zsh), not bash — see the detailed
    // note on tmux.rs `pane_info_command`.
    c.arg("-d")
        .arg(distro)
        .arg("--cd")
        .arg("~")
        .arg("-e")
        .arg("bash")
        .arg("-lc")
        .arg(USAGE_SHELL_CMD);
    c.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    c
}

#[cfg(not(windows))]
fn usage_command() -> std::process::Command {
    let mut c = std::process::Command::new("sh");
    c.arg("-lc").arg(USAGE_SHELL_CMD);
    c
}

/// Extract the first integer/decimal percentage that appears before a `%` in `s`.
fn pct_in(s: &str) -> Option<f32> {
    let idx = s.find('%')?;
    // Walk back over the number (digits + one dot) just before the '%'.
    let bytes = s.as_bytes();
    let mut start = idx;
    while start > 0 {
        let c = bytes[start - 1];
        if c.is_ascii_digit() || c == b'.' {
            start -= 1;
        } else {
            break;
        }
    }
    if start == idx {
        return None;
    }
    s[start..idx].trim().parse::<f32>().ok()
}

/// The reset clause after "resets " (up to the timezone paren), trimmed.
fn resets_in(s: &str) -> Option<String> {
    let after = s.split("resets ").nth(1)?;
    let clause = after.split('(').next().unwrap_or(after).trim();
    if clause.is_empty() {
        None
    } else {
        Some(clause.to_string())
    }
}

/// Parse the `/usage` text. Matches the "Current session" + "Current week (all
/// models)" + "Current week (Sonnet only)" lines case-insensitively.
fn parse_usage(text: &str) -> ClaudeUsage {
    let mut u = ClaudeUsage::default();
    for raw in text.lines() {
        let line = raw.trim();
        let low = line.to_lowercase();
        if !low.contains('%') {
            continue;
        }
        if low.contains("session") {
            u.session_used_pct = pct_in(line);
            u.session_resets = resets_in(line);
            u.ok = true;
        } else if low.contains("week") && low.contains("sonnet") {
            u.week_sonnet_used_pct = pct_in(line);
            u.ok = true;
        } else if low.contains("week") {
            u.week_used_pct = pct_in(line);
            u.week_resets = resets_in(line);
            u.ok = true;
        }
    }
    u
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_real_usage_output() {
        let text = "You are currently using your subscription to power your Claude Code usage\n\n\
            Current session: 23% used \u{b7} resets Jun 14, 11:10pm (America/Los_Angeles)\n\
            Current week (all models): 42% used \u{b7} resets Jun 20, 9pm (America/Los_Angeles)\n\
            Current week (Sonnet only): 0% used\n";
        let u = parse_usage(text);
        assert!(u.ok);
        assert_eq!(u.session_used_pct, Some(23.0));
        assert_eq!(u.session_resets.as_deref(), Some("Jun 14, 11:10pm"));
        assert_eq!(u.week_used_pct, Some(42.0));
        assert_eq!(u.week_resets.as_deref(), Some("Jun 20, 9pm"));
        assert_eq!(u.week_sonnet_used_pct, Some(0.0));
    }

    #[test]
    fn empty_or_garbage_is_not_ok() {
        assert!(!parse_usage("").ok);
        assert!(!parse_usage("not logged in\nno percentages here").ok);
    }

    #[test]
    fn pct_in_grabs_the_number() {
        assert_eq!(pct_in("foo 42% used"), Some(42.0));
        assert_eq!(pct_in("12.5% left"), Some(12.5));
        assert_eq!(pct_in("no percent"), None);
    }
}
