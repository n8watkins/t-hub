//! Harness adapter seam (Codex Phase-1, D1 of the ratified plan
//! `~/.t-hub/captain/reviews/codex-phase1-plan-2026-07-11.md`).
//!
//! T-Hub launches an agent by wrapping an opaque `startupCommand` string
//! (`commands.rs::pane_command`); the item-3 capability env is injected at the
//! tmux SESSION level, independent of what runs in the pane. So the *harness*
//! choice rides that existing opaque command - Phase 1 adds NO `SpawnOptions`
//! field and touches none of `commands.rs`/`control.rs`/`plane.rs`/`tmux.rs`/
//! `supervision.rs`/`fleet.rs`. This module is a pure, side-effect-free builder
//! of the launch/turn strings (plus the permission-flag map), keyed off the
//! session's `provider` string (model.rs).
//!
//! Scope discipline (Phase 1):
//!   - This is the SEAM only. The `claude` impl is a thin delegation to the
//!     existing literal command strings; it does NOT migrate `claude/hooks.rs`,
//!     `claude/install.rs`, or the Claude recall scan behind the trait (explicit
//!     Phase-2 non-goal).
//!   - `exec_turn_argv` is DEFINED here (PR-A) and CONSUMED by the PR-B
//!     `--codex-tap` lifecycle producer.
//!   - Continuity-catalog reads (`list_resumable`, the full 7-capability shape)
//!     are D6/PR-C; see the doc-stub at the bottom of this module. They are left
//!     out of the live trait so PR-A does not pull the D6 `recent.rs` types in.
//!
//! Forward-compatibility: `Harness` is an ACCESSOR over the `provider` String
//! (which stays a String on the wire and in the DB, exactly like the identity
//! rekey narrowing) - not a serialization change. Unknown/legacy/empty provider
//! strings resolve to `Claude` (today's only behavior), test-locked below.

// PR-A lands the seam; several accessors and the concrete-adapter re-exports are
// consumed by the PR-B producer and PR-C catalog that build on top of it. Allow
// unused until those land - the *shape* is the deliverable.
#![allow(dead_code, unused_imports)]

mod claude;
mod codex;

pub use claude::ClaudeHarness;
pub use codex::CodexHarness;

use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::process::Command;
use std::time::Duration;

/// Which agent harness backs a session. Keyed to `AgentSessionRecord::provider`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Harness {
    Claude,
    Codex,
}

impl Harness {
    /// Parse a `provider` string into a harness. Case-insensitive and
    /// whitespace-trimmed; anything that is not a known non-Claude harness
    /// resolves to [`Harness::Claude`] - preserving today's behavior for
    /// legacy/empty/unknown provider strings.
    pub fn from_provider(provider: &str) -> Self {
        match provider.trim().to_ascii_lowercase().as_str() {
            "codex" => Harness::Codex,
            _ => Harness::Claude,
        }
    }

    /// The canonical provider string (matches `AgentSessionRecord::provider`).
    pub fn as_provider(self) -> &'static str {
        match self {
            Harness::Claude => "claude",
            Harness::Codex => "codex",
        }
    }

    /// The adapter that builds this harness's launch/turn strings.
    pub fn adapter(self) -> &'static dyn HarnessAdapter {
        match self {
            Harness::Claude => &claude::ClaudeHarness,
            Harness::Codex => &codex::CodexHarness,
        }
    }

    /// Harness-native syntax for explicitly loading the Captain skill.
    pub fn captain_invocation(self) -> &'static str {
        match self {
            Harness::Claude => "/captain",
            Harness::Codex => "$captain",
        }
    }
}

impl std::fmt::Display for Harness {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_provider())
    }
}

/// The t-hub permission posture a spawned session runs under, mapped onto each
/// harness's own flags by [`HarnessAdapter::permission_map`]. Named after the
/// Claude `defaultMode` values it mirrors so the crew doctrine reads the same
/// across harnesses.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum PermMode {
    /// Crew default: no approval prompts. Claude `--dangerously-skip-permissions`;
    /// Codex `--dangerously-bypass-approvals-and-sandbox --skip-git-repo-check`.
    BypassPermissions,
    /// Approximate "edits allowed without prompt". Codex `--sandbox
    /// workspace-write` (documented gap: no exact analog, and network is off by
    /// default in workspace-write - so this mode cannot `git push`).
    AcceptEdits,
    /// Read-only / default posture.
    Default,
}

impl PermMode {
    pub fn as_str(self) -> &'static str {
        match self {
            PermMode::BypassPermissions => "bypassPermissions",
            PermMode::AcceptEdits => "acceptEdits",
            PermMode::Default => "default",
        }
    }
}

impl std::fmt::Display for PermMode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A bounded, provider-neutral view of the foreground process that owns a
/// terminal at one instant. The raw argv is used only for in-process provider
/// verification and is never included in a response or error because the
/// initial Harness prompt may contain sensitive task context.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HarnessProcessEvidence {
    pid: u32,
    start_ticks: u64,
    argv: Vec<String>,
}

impl HarnessProcessEvidence {
    #[cfg(test)]
    pub(crate) fn test(pid: u32, start_ticks: u64, argv: &[&str]) -> Self {
        Self {
            pid,
            start_ticks,
            argv: argv.iter().map(|arg| (*arg).to_string()).collect(),
        }
    }

    fn identity(&self) -> (u32, u64) {
        (self.pid, self.start_ticks)
    }
}

/// Credential-safe failure classes for fail-closed launch attestation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LaunchAttestationError {
    UnreadableEvidence,
    StaleEvidence,
    WrongProvider,
    WrapperObscured,
    MissingPermission,
    WrongPermission,
    ConflictingPermission,
}

impl std::fmt::Display for LaunchAttestationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let message = match self {
            Self::UnreadableEvidence => "provider-native process evidence is unreadable",
            Self::StaleEvidence => "provider-native process evidence predates this launch",
            Self::WrongProvider => "process evidence belongs to the wrong Harness provider",
            Self::WrapperObscured => "a wrapper obscures the provider-native foreground process",
            Self::MissingPermission => "the required provider-native permission flag is missing",
            Self::WrongPermission => "provider-native permission evidence has the wrong mode",
            Self::ConflictingPermission => {
                "provider-native permission evidence contains conflicting modes"
            }
        };
        f.write_str(message)
    }
}

impl std::error::Error for LaunchAttestationError {}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HarnessPermissionAttestation {
    pub provider: Harness,
    pub permission: PermMode,
}

/// The Phase-1 slice of the harness capability surface. The audit documents a
/// fuller 7-capability shape (fresh/resume launch, headless turn, permission
/// map, session home, resumable catalog, hook/producer install, migration);
/// Phase 1 carries only what the seam + presets + provisioning + resume wiring
/// need. See the module-level doc-stub for the deferred capabilities.
pub trait HarnessAdapter: Send + Sync {
    /// The harness this adapter drives.
    fn harness(&self) -> Harness;

    /// Interactive launch string for a FRESH session. An empty `prompt` yields
    /// the harness's bare interactive entry (the "new agent" spawn preset); a
    /// non-empty prompt is passed as the initial instruction, safely quoted.
    fn fresh_argv(&self, prompt: &str) -> String;

    /// Interactive launch with an explicit permission posture. Captain creation
    /// uses this instead of inheriting a harness or user default implicitly.
    fn fresh_argv_with_permissions(&self, prompt: &str, perm: PermMode) -> String;

    /// Interactive launch string that RESUMES a specific `session_id` (the
    /// harness's own resume entry: `claude --resume '<id>'` / `codex resume
    /// '<id>'`). The frontend "Resume ..." picker preset (no id) is a separate,
    /// frontend-only string in `SpawnMenu.tsx`.
    fn resume_argv(&self, session_id: &str) -> String;

    /// The HEADLESS crew-turn pipeline string: one `exec`-style turn whose
    /// lifecycle streams into the t-hub journal via the producer tap. `resume =
    /// Some(id)` continues an existing thread/session; `None` starts fresh.
    /// `perm` selects the flags via [`Self::permission_map`]. PR-A defines this;
    /// the PR-B `--codex-tap` producer consumes it.
    fn exec_turn_argv(&self, prompt: &str, resume: Option<&str>, perm: PermMode) -> String;

    /// The harness-native flags for a t-hub [`PermMode`] (D4 permission map).
    fn permission_map(&self, perm: PermMode) -> Vec<String>;

    /// Verify the effective permission posture from authoritative foreground
    /// process argv after launch. Implementations must recognize only their own
    /// provider-native executable shape and must reject wrappers and conflicting
    /// permission modes.
    fn attest_permissions(
        &self,
        evidence: &HarnessProcessEvidence,
        expected: PermMode,
    ) -> Result<HarnessPermissionAttestation, LaunchAttestationError>;

    /// The directory this harness writes its recall/rollout state under
    /// (`~/.claude/projects` / `~/.codex/sessions`). The D6/PR-C continuity
    /// catalog walks this tree.
    fn session_home(&self) -> PathBuf;
}

/// Require evidence from a process created by this launch, then delegate the
/// provider-native permission interpretation to the selected adapter.
pub fn attest_launch_permissions(
    adapter: &dyn HarnessAdapter,
    before: &HarnessProcessEvidence,
    after: &HarnessProcessEvidence,
    expected: PermMode,
) -> Result<HarnessPermissionAttestation, LaunchAttestationError> {
    if before.identity() == after.identity() {
        return Err(LaunchAttestationError::StaleEvidence);
    }
    adapter.attest_permissions(after, expected)
}

/// Read one authoritative foreground process identity and argv from the tmux
/// pane. The shell performs only bounded local reads: exactly one pane, one
/// foreground process, and at most 64 KiB of cmdline data. Any malformed,
/// oversized, missing, or non-UTF-8 result collapses to the same credential-safe
/// unreadable-evidence error.
pub fn observe_harness_process(
    tmux_target: &str,
) -> Result<HarnessProcessEvidence, LaunchAttestationError> {
    const SCRIPT: &str = r#"
set -eu
pane_pid=$(tmux -L "$1" list-panes -t "$2" -F '#{pane_pid}')
set -- $pane_pid
[ "$#" -eq 1 ]
case "$1" in ''|*[!0-9]*) exit 21;; esac
foreground_pid=$(ps -o tpgid= -p "$1" | tr -d ' ')
case "$foreground_pid" in ''|*[!0-9]*|0) exit 22;; esac
stat=$(cat "/proc/$foreground_pid/stat")
rest=${stat##*) }
set -- $rest
[ "$#" -ge 20 ]
start_ticks=${20}
cmdline="/proc/$foreground_pid/cmdline"
size=$(wc -c < "$cmdline" | tr -d ' ')
case "$size" in ''|*[!0-9]*|0) exit 23;; esac
[ "$size" -le 65536 ]
printf 'THPA1\n%s\n%s\n' "$foreground_pid" "$start_ticks"
cat "$cmdline"
"#;

    #[cfg(windows)]
    let command = {
        use std::os::windows::process::CommandExt;
        let mut command = Command::new("wsl.exe");
        command
            .arg("--cd")
            .arg("~")
            .arg("-e")
            .arg("sh")
            .arg("-c")
            .arg(SCRIPT)
            .arg("t-hub-permission-attestation")
            .arg(crate::tmux::socket())
            .arg(tmux_target);
        command.creation_flags(0x0800_0000);
        command
    };
    #[cfg(unix)]
    let command = {
        let mut command = Command::new("sh");
        command
            .arg("-c")
            .arg(SCRIPT)
            .arg("t-hub-permission-attestation")
            .arg(crate::tmux::socket())
            .arg(tmux_target);
        command
    };

    let output = crate::bounded_exec::output_with_timeout(command, Duration::from_secs(2))
        .map_err(|_| LaunchAttestationError::UnreadableEvidence)?;
    if !output.status.success() || !output.stderr.is_empty() {
        return Err(LaunchAttestationError::UnreadableEvidence);
    }
    parse_process_evidence(&output.stdout)
}

fn parse_process_evidence(bytes: &[u8]) -> Result<HarnessProcessEvidence, LaunchAttestationError> {
    let mut fields = bytes.splitn(4, |byte| *byte == b'\n');
    if fields.next() != Some(b"THPA1".as_slice()) {
        return Err(LaunchAttestationError::UnreadableEvidence);
    }
    let pid = parse_ascii_number(fields.next())?;
    let start_ticks = parse_ascii_number(fields.next())?;
    let cmdline = fields
        .next()
        .ok_or(LaunchAttestationError::UnreadableEvidence)?;
    if cmdline.is_empty() || cmdline.len() > 65_536 || !cmdline.ends_with(&[0]) {
        return Err(LaunchAttestationError::UnreadableEvidence);
    }
    let argv = cmdline[..cmdline.len() - 1]
        .split(|byte| *byte == 0)
        .map(|arg| {
            std::str::from_utf8(arg)
                .map(str::to_string)
                .map_err(|_| LaunchAttestationError::UnreadableEvidence)
        })
        .collect::<Result<Vec<_>, _>>()?;
    if argv.is_empty() || argv.len() > 256 || argv.iter().any(|arg| arg.len() > 16_384) {
        return Err(LaunchAttestationError::UnreadableEvidence);
    }
    Ok(HarnessProcessEvidence {
        pid: u32::try_from(pid).map_err(|_| LaunchAttestationError::UnreadableEvidence)?,
        start_ticks,
        argv,
    })
}

fn parse_ascii_number(field: Option<&[u8]>) -> Result<u64, LaunchAttestationError> {
    let field = field.ok_or(LaunchAttestationError::UnreadableEvidence)?;
    std::str::from_utf8(field)
        .ok()
        .and_then(|value| value.parse().ok())
        .ok_or(LaunchAttestationError::UnreadableEvidence)
}

pub(crate) fn provider_arguments<'a>(
    evidence: &'a HarnessProcessEvidence,
    expected: Harness,
) -> Result<&'a [String], LaunchAttestationError> {
    let (provider, provider_index) =
        process_provider(&evidence.argv).ok_or(LaunchAttestationError::WrapperObscured)?;
    if provider != expected {
        return Err(LaunchAttestationError::WrongProvider);
    }
    Ok(&evidence.argv[provider_index + 1..])
}

fn process_provider(argv: &[String]) -> Option<(Harness, usize)> {
    let executable = argv.first().map(|arg| executable_name(arg))?;
    if let Some(provider) = provider_executable(executable) {
        return Some((provider, 0));
    }
    if matches!(executable, "node" | "nodejs" | "bun" | "deno") {
        return argv
            .get(1)
            .and_then(|arg| provider_executable(executable_name(arg)))
            .map(|provider| (provider, 1));
    }
    None
}

fn executable_name(value: &str) -> &str {
    value
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or(value)
        .strip_suffix(".exe")
        .unwrap_or_else(|| value.rsplit(['/', '\\']).next().unwrap_or(value))
}

fn provider_executable(executable: &str) -> Option<Harness> {
    match executable.to_ascii_lowercase().as_str() {
        "codex" => Some(Harness::Codex),
        "claude" => Some(Harness::Claude),
        _ => None,
    }
}

pub(crate) fn leading_permission_options(args: &[String]) -> Vec<(&str, Option<&str>)> {
    let mut options = Vec::new();
    let mut index = 0;
    while let Some(flag) = args.get(index) {
        if flag == "--" || !flag.starts_with('-') {
            break;
        }
        let takes_value = matches!(
            flag.as_str(),
            "--sandbox" | "--ask-for-approval" | "--permission-mode"
        );
        let value = takes_value
            .then(|| args.get(index + 1).map(String::as_str))
            .flatten();
        options.push((flag.as_str(), value));
        index += if takes_value { 2 } else { 1 };
    }
    options
}

/// Safely single-quote `s` for a POSIX shell (the `startupCommand` runs inside
/// an interactive login shell, `commands.rs::pane_command`). Wraps in `'...'`
/// and escapes embedded single quotes as `'\''`, so arbitrary prompt text
/// (backticks, `$()`, quotes) is passed literally and never shell-interpreted.
pub(crate) fn sh_single_quote(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('\'');
    for ch in s.chars() {
        if ch == '\'' {
            out.push_str("'\\''");
        } else {
            out.push(ch);
        }
    }
    out.push('\'');
    out
}

/// The user's home directory, used to root the session homes. Falls back to
/// `~` literally only if `HOME` is unset (never expected on the fleet hosts).
pub(crate) fn home_dir() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("~"))
}

// ---------------------------------------------------------------------------
// Phase-2 / PR-C deferred capability shape (doc-stub, per the audit's full
// 7-capability adapter). These are intentionally NOT live trait methods in
// PR-A:
//   - `list_resumable()` -> the continuity catalog rows. Its return type couples
//     to the D6 `recent.rs::RecentSession` (which gains a `provider` field in
//     PR-C); declaring it here would force PR-A to edit D6-owned files. It lands
//     with D6/PR-C alongside `codex_recent()`.
//   - `install_producer()` -> Claude hooks install / Codex `[hooks]` producer
//     (Phase 2; the Codex `[hooks]` path is additionally gated by Codex's new
//     hook-trust regime, which is why Phase 1 uses the trust-free `exec --json`
//     producer instead).
//   - `migrate_argv(id)` -> crew-migration resume-by-id. Phase 1 encodes this as
//     doctrine (Codex crews migrate with `codex resume '<uuid>'`, mirror of the
//     `claude --resume <uuid>` directive) rather than a code path.
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn provider_string_maps_to_adapter() {
        assert_eq!(Harness::from_provider("claude"), Harness::Claude);
        assert_eq!(Harness::from_provider("codex"), Harness::Codex);
        // Case/whitespace tolerant.
        assert_eq!(Harness::from_provider("  Codex "), Harness::Codex);
        assert_eq!(Harness::from_provider("CLAUDE"), Harness::Claude);
    }

    #[test]
    fn unknown_and_legacy_providers_resolve_to_claude() {
        // No-regression lock: anything unknown/legacy/empty is Claude (today's
        // only behavior). This is the guard that fails if the fallback is ever
        // flipped to Codex.
        for p in [
            "",
            "   ",
            "gpt",
            "anthropic",
            "openai",
            "codexx",
            "claude-2",
        ] {
            assert_eq!(
                Harness::from_provider(p),
                Harness::Claude,
                "provider {p:?} must resolve to Claude"
            );
        }
    }

    #[test]
    fn provider_roundtrips_through_display() {
        assert_eq!(Harness::Claude.as_provider(), "claude");
        assert_eq!(Harness::Codex.as_provider(), "codex");
        assert_eq!(Harness::Claude.to_string(), "claude");
        assert_eq!(Harness::Codex.to_string(), "codex");
        // as_provider round-trips through from_provider.
        for h in [Harness::Claude, Harness::Codex] {
            assert_eq!(Harness::from_provider(h.as_provider()), h);
        }
    }

    #[test]
    fn adapter_matches_harness() {
        assert_eq!(Harness::Claude.adapter().harness(), Harness::Claude);
        assert_eq!(Harness::Codex.adapter().harness(), Harness::Codex);
    }

    #[test]
    fn captain_invocation_is_harness_native() {
        assert_eq!(Harness::Codex.captain_invocation(), "$captain");
        assert_eq!(Harness::Claude.captain_invocation(), "/captain");
    }

    #[test]
    fn single_quote_escapes_embedded_quotes_and_metachars() {
        assert_eq!(sh_single_quote("hello"), "'hello'");
        // Backticks / $() / redirects pass through literally inside the quotes.
        assert_eq!(sh_single_quote("a `b` $(c) > d"), "'a `b` $(c) > d'");
        // Embedded single quote becomes '\'' .
        assert_eq!(sh_single_quote("it's"), "'it'\\''s'");
    }

    #[test]
    fn launch_attestation_rejects_stale_process_identity() {
        let evidence = HarnessProcessEvidence::test(
            42,
            900,
            &[
                "codex",
                "--dangerously-bypass-approvals-and-sandbox",
                "work",
            ],
        );
        assert_eq!(
            attest_launch_permissions(
                Harness::Codex.adapter(),
                &evidence,
                &evidence,
                PermMode::BypassPermissions,
            )
            .unwrap_err(),
            LaunchAttestationError::StaleEvidence
        );
    }

    #[test]
    fn process_evidence_parser_is_bounded_and_strict() {
        let parsed =
            parse_process_evidence(b"THPA1\n42\n900\ncodex\0--sandbox\0read-only\0").unwrap();
        assert_eq!(parsed.identity(), (42, 900));
        assert_eq!(parsed.argv, ["codex", "--sandbox", "read-only"]);

        assert_eq!(
            parse_process_evidence(b"THPA1\n42\n900\ncodex"),
            Err(LaunchAttestationError::UnreadableEvidence)
        );
        let oversized = vec![b'x'; 65_537];
        assert_eq!(
            parse_process_evidence(&oversized),
            Err(LaunchAttestationError::UnreadableEvidence)
        );
    }

    #[test]
    fn permission_modes_serialize_as_separate_harness_axis() {
        assert_eq!(
            serde_json::to_value(PermMode::BypassPermissions).unwrap(),
            serde_json::json!("bypassPermissions")
        );
        assert_eq!(PermMode::BypassPermissions.to_string(), "bypassPermissions");
    }
}
