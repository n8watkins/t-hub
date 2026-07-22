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
use sha2::{Digest, Sha256};
use std::path::PathBuf;
use std::process::Command;
use std::time::{Duration, Instant};

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
    /// Codex `--dangerously-bypass-approvals-and-sandbox`.
    BypassPermissions,
    /// Approximate "edits allowed without prompt". Codex `--sandbox
    /// workspace-write` (documented gap: no exact analog, and network is off by
    /// default in workspace-write - so this mode cannot `git push`).
    AcceptEdits,
    /// Read-only / default posture.
    Default,
}

/// The General-authorized local execution posture for dispatched Crew in this
/// Captain fleet. This grants full local worktree execution through the
/// provider Harness, but does not expand Crew scope, T-Hub capability, legacy
/// authority, or authority over destructive and outward-facing actions.
pub const CREW_DEFAULT_PERMISSION: PermMode = PermMode::BypassPermissions;

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
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct TerminalGeneration {
    session_id: u64,
    session_created: u64,
    window_id: u64,
    pane_id: u64,
    pane_pid: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ProcessIdentity {
    pid: u32,
    start_ticks: u64,
}

pub const HARNESS_PROCESS_IDENTITY_VERSION: u32 = 1;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct HarnessProcessAncestor {
    pub pid: u32,
    pub start_ticks: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct HarnessExecutableIdentity {
    pub path: String,
    pub device: u64,
    pub inode: u64,
}

pub const EXPECTED_HARNESS_LAUNCH_PROVENANCE_VERSION: u32 = 2;

/// Sanitized identity of the configured provider entry point resolved before
/// any managed launch effect. Script launches bind both the runtime and the
/// exact entry script while retaining only a value-free argv layout digest.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ExpectedHarnessLaunchProvenance {
    pub version: u32,
    pub provider: String,
    pub kind: String,
    pub executable: HarnessExecutableIdentity,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub entry_script: Option<HarnessExecutableIdentity>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trusted_child_executable: Option<HarnessExecutableIdentity>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub argv_layout_sha256: Option<String>,
}

/// Credential-safe, durable identity for the exact provider process accepted
/// inside one managed terminal generation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct HarnessProcessIdentity {
    pub version: u32,
    pub provider: String,
    pub pid: u32,
    pub start_ticks: u64,
    pub executable: HarnessExecutableIdentity,
    pub argv_sha256: String,
    pub process_group_id: u32,
    pub process_session_id: u32,
    pub tmux_session_id: u64,
    pub tmux_session_created: u64,
    pub tmux_window_id: u64,
    pub tmux_pane_id: u64,
    pub pane_pid: u32,
    pub pane_start_ticks: u64,
    pub ancestry: Vec<HarnessProcessAncestor>,
    pub cgroup_path: String,
    pub session_token_sha256: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HarnessProcessEvidence {
    terminal: TerminalGeneration,
    ancestry: Vec<ProcessIdentity>,
    pid: u32,
    start_ticks: u64,
    executable_device: u64,
    executable_inode: u64,
    argv: Vec<String>,
}

impl HarnessProcessEvidence {
    #[cfg(test)]
    pub(crate) fn test(pid: u32, start_ticks: u64, argv: &[&str]) -> Self {
        Self::test_with_context(
            pid,
            start_ticks,
            u64::from(pid),
            start_ticks,
            argv,
            123_456,
            pid,
            &[(pid, start_ticks)],
        )
    }

    #[cfg(test)]
    pub(crate) fn test_after_exec(
        pid: u32,
        start_ticks: u64,
        executable_inode: u64,
        argv: &[&str],
    ) -> Self {
        Self::test_with_context(
            pid,
            start_ticks,
            u64::from(pid),
            executable_inode,
            argv,
            123_456,
            pid,
            &[(pid, start_ticks)],
        )
    }

    #[cfg(test)]
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn test_with_context(
        pid: u32,
        start_ticks: u64,
        executable_device: u64,
        executable_inode: u64,
        argv: &[&str],
        session_created: u64,
        pane_pid: u32,
        ancestry: &[(u32, u64)],
    ) -> Self {
        Self {
            terminal: TerminalGeneration {
                session_id: 17,
                session_created,
                window_id: 9,
                pane_id: 42,
                pane_pid,
            },
            ancestry: ancestry
                .iter()
                .map(|(pid, start_ticks)| ProcessIdentity {
                    pid: *pid,
                    start_ticks: *start_ticks,
                })
                .collect(),
            pid,
            start_ticks,
            executable_device,
            executable_inode,
            argv: argv.iter().map(|arg| (*arg).to_string()).collect(),
        }
    }

    fn identity(&self) -> (u32, u64) {
        (self.pid, self.start_ticks)
    }

    fn executable_identity(&self) -> (u64, u64) {
        (self.executable_device, self.executable_inode)
    }

    fn matches_pane_generation(&self, pane: &crate::tmux::PaneGeneration) -> bool {
        self.terminal.session_id == pane.session_id
            && self.terminal.session_created == pane.session_created
            && self.terminal.window_id == pane.window_id
            && self.terminal.pane_id == pane.pane_id
            && self.terminal.pane_pid == pane.pane_pid
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
    MalformedPermission,
    TerminalChanged,
    ProcessChanged,
    AncestryChanged,
    HarnessMissing,
    ExpectedProvenanceMismatch,
    UntrustedLaunchCommand,
    CgroupChanged,
    SessionTokenMissing,
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
            Self::MalformedPermission => {
                "provider-native permission evidence contains a malformed option"
            }
            Self::TerminalChanged => "the terminal pane generation changed during launch",
            Self::ProcessChanged => "the provider-native process changed during launch acceptance",
            Self::AncestryChanged => {
                "the provider-native process ancestry changed during launch acceptance"
            }
            Self::HarnessMissing => "the provider-native Harness process is not an ancestor",
            Self::ExpectedProvenanceMismatch => {
                "the provider-native process does not match its prepared launch provenance"
            }
            Self::UntrustedLaunchCommand => {
                "the configured provider launch command has an untrusted shape"
            }
            Self::CgroupChanged => "the provider-native process left its managed cgroup",
            Self::SessionTokenMissing => {
                "the provider-native process lacks its scoped session identity"
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

    /// Interactive resume with an explicit permission posture. Recovery paths
    /// use this instead of inheriting a potentially broader user default.
    fn resume_argv_with_permissions(&self, session_id: &str, perm: PermMode) -> String;

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
    if before.terminal != after.terminal {
        return Err(LaunchAttestationError::TerminalChanged);
    }
    if before.identity() == after.identity()
        && before.executable_identity() == after.executable_identity()
    {
        return Err(LaunchAttestationError::StaleEvidence);
    }
    adapter.attest_permissions(after, expected)
}

/// Authorize a private dormant-pane to provider-pane respawn.
///
/// A respawn intentionally replaces `pane_pid`, unlike the normal shell-to-exec
/// launch path.  This verifier therefore requires the exact pre/post tmux
/// transition, preserves the session/window/pane tuple, rejects an unchanged
/// process generation, and delegates provider-native posture validation only
/// after that provenance check succeeds.
pub fn attest_respawn_launch_permissions(
    adapter: &dyn HarnessAdapter,
    before: &HarnessProcessEvidence,
    transition: &crate::tmux::RespawnPaneTransition,
    after: &HarnessProcessEvidence,
    expected: PermMode,
) -> Result<HarnessPermissionAttestation, LaunchAttestationError> {
    if !before.matches_pane_generation(&transition.before)
        || !after.matches_pane_generation(&transition.after)
        || transition.before.session_id != transition.after.session_id
        || transition.before.session_created != transition.after.session_created
        || transition.before.window_id != transition.after.window_id
        || transition.before.pane_id != transition.after.pane_id
    {
        return Err(LaunchAttestationError::TerminalChanged);
    }
    if transition.before.pane_pid == transition.after.pane_pid
        || before.identity() == after.identity()
        || before.executable_identity() == after.executable_identity()
    {
        return Err(LaunchAttestationError::StaleEvidence);
    }
    adapter.attest_permissions(after, expected)
}

/// Confirm that two pre-launch observations refer to the exact same foreground
/// process in the same pane generation.  This is stricter than accepting a
/// readable shell once: startup may replace a just-observed login shell before
/// the provider command can be sent.
pub fn confirm_stable_launch_baseline(
    first: &HarnessProcessEvidence,
    second: &HarnessProcessEvidence,
) -> Result<HarnessProcessEvidence, LaunchAttestationError> {
    if first.terminal != second.terminal {
        return Err(LaunchAttestationError::TerminalChanged);
    }
    if first.identity() != second.identity()
        || first.executable_identity() != second.executable_identity()
    {
        return Err(LaunchAttestationError::ProcessChanged);
    }
    if first.ancestry != second.ancestry {
        return Err(LaunchAttestationError::AncestryChanged);
    }
    Ok(second.clone())
}

/// Re-verify provider posture and exact process provenance at a durable launch
/// acceptance boundary. The final observation must belong to the same tmux
/// pane generation, provider process lifetime, executable, and ancestry as the
/// already-attested process.
pub fn attest_final_launch_permissions(
    adapter: &dyn HarnessAdapter,
    accepted: &HarnessProcessEvidence,
    final_observation: Result<HarnessProcessEvidence, LaunchAttestationError>,
    expected: PermMode,
) -> Result<(HarnessPermissionAttestation, HarnessProcessEvidence), LaunchAttestationError> {
    let final_evidence = final_observation?;
    if accepted.terminal != final_evidence.terminal {
        return Err(LaunchAttestationError::TerminalChanged);
    }
    let attestation = adapter.attest_permissions(&final_evidence, expected)?;
    if accepted.identity() != final_evidence.identity()
        || accepted.executable_identity() != final_evidence.executable_identity()
    {
        return Err(LaunchAttestationError::ProcessChanged);
    }
    if accepted.ancestry != final_evidence.ancestry {
        return Err(LaunchAttestationError::AncestryChanged);
    }
    Ok((attestation, final_evidence))
}

/// Read one authoritative foreground process identity and argv from the tmux
/// pane. The shell performs only bounded local reads: exactly one pane, one
/// foreground process, and at most 64 KiB of cmdline data. Any malformed,
/// oversized, missing, or non-UTF-8 result collapses to the same credential-safe
/// unreadable-evidence error.
pub fn observe_harness_process(
    tmux_target: &str,
) -> Result<HarnessProcessEvidence, LaunchAttestationError> {
    observe_harness_process_until(tmux_target, Instant::now() + Duration::from_secs(2))
}

fn observation_time_remaining(deadline: Instant) -> Result<Duration, LaunchAttestationError> {
    let remaining = deadline.saturating_duration_since(Instant::now());
    if remaining.is_zero() {
        Err(LaunchAttestationError::UnreadableEvidence)
    } else {
        Ok(remaining)
    }
}

fn observe_harness_process_until(
    tmux_target: &str,
    deadline: Instant,
) -> Result<HarnessProcessEvidence, LaunchAttestationError> {
    const SCRIPT: &str = r#"
set -eu
tmux_socket=$1
tmux_target=$2
pane=$(tmux -L "$tmux_socket" list-panes -t "$tmux_target" -F '#{session_id} #{session_created} #{window_id} #{pane_id} #{pane_pid}')
set -- $pane
[ "$#" -eq 5 ]
session_id=$1
session_created=$2
window_id=$3
pane_id=$4
pane_pid=$5
case "$session_created:$pane_pid" in *[!0-9:]*) exit 21;; esac
foreground_pid=$(ps -o tpgid= -p "$pane_pid" | tr -d ' ')
case "$foreground_pid" in ''|*[!0-9]*|0) exit 22;; esac
process_stat=$(cat "/proc/$foreground_pid/stat")
rest=${process_stat##*) }
set -- $rest
[ "$#" -ge 20 ]
start_ticks=${20}
set -- $(stat -Lc '%d %i' "/proc/$foreground_pid/exe")
[ "$#" -eq 2 ]
case "$1:$2" in *[!0-9:]*) exit 24;; esac
executable_device=$1
executable_inode=$2
ancestry=
ancestry_count=0
current_pid=$foreground_pid
while :; do
    process_stat=$(cat "/proc/$current_pid/stat")
    rest=${process_stat##*) }
    set -- $rest
    [ "$#" -ge 20 ]
    parent_pid=$2
    current_start_ticks=${20}
    case "$parent_pid:$current_start_ticks" in *[!0-9:]*) exit 25;; esac
    if [ -z "$ancestry" ]; then
        ancestry="$current_pid:$current_start_ticks"
    else
        ancestry="$ancestry,$current_pid:$current_start_ticks"
    fi
    ancestry_count=$((ancestry_count + 1))
    [ "$ancestry_count" -le 64 ]
    [ "$current_pid" = "$pane_pid" ] && break
    [ "$parent_pid" -gt 0 ]
    [ "$parent_pid" != "$current_pid" ]
    current_pid=$parent_pid
done
cmdline="/proc/$foreground_pid/cmdline"
size=$(wc -c < "$cmdline" | tr -d ' ')
case "$size" in ''|*[!0-9]*|0) exit 23;; esac
[ "$size" -le 65536 ]
pane_after=$(tmux -L "$tmux_socket" list-panes -t "$tmux_target" -F '#{session_id} #{session_created} #{window_id} #{pane_id} #{pane_pid}')
[ "$pane_after" = "$pane" ]
foreground_after=$(ps -o tpgid= -p "$pane_pid" | tr -d ' ')
[ "$foreground_after" = "$foreground_pid" ]
process_stat=$(cat "/proc/$foreground_pid/stat")
rest=${process_stat##*) }
set -- $rest
[ "$#" -ge 20 ]
[ "${20}" = "$start_ticks" ]
set -- $(stat -Lc '%d %i' "/proc/$foreground_pid/exe")
[ "$#" -eq 2 ]
[ "$1" = "$executable_device" ]
[ "$2" = "$executable_inode" ]
printf 'THPA3\n%s\n%s\n%s\n%s\n%s\n%s\n%s\n%s\n%s\n%s\n' \
    "$session_id" "$session_created" "$window_id" "$pane_id" "$pane_pid" \
    "$foreground_pid" "$start_ticks" "$executable_device" "$executable_inode" "$ancestry"
cat "$cmdline"
"#;

    let tmux_socket = crate::tmux::validated_socket_name()
        .map_err(|_| LaunchAttestationError::UnreadableEvidence)?;
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
            .arg(tmux_socket)
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
            .arg(tmux_socket)
            .arg(tmux_target);
        command
    };

    let output = crate::bounded_exec::output_with_timeout(
        command,
        observation_time_remaining(deadline)?.min(Duration::from_secs(2)),
    )
    .map_err(|_| LaunchAttestationError::UnreadableEvidence)?;
    if !output.status.success() || !output.stderr.is_empty() {
        return Err(LaunchAttestationError::UnreadableEvidence);
    }
    parse_process_evidence(&output.stdout)
}

struct ScopedProcessDetails {
    pid: u32,
    parent_pid: u32,
    start_ticks: u64,
    process_group_id: u32,
    process_session_id: u32,
    executable_path: String,
    executable_device: u64,
    executable_inode: u64,
    cgroup_path: String,
    session_token: Option<String>,
    argv: Vec<String>,
}

fn observe_scoped_process(pid: u32) -> Result<ScopedProcessDetails, LaunchAttestationError> {
    const SCRIPT: &str = r#"
set -eu
pid=$1
case "$pid" in ''|*[!0-9]*|0) exit 31;; esac
stat_before=$(cat "/proc/$pid/stat")
rest=${stat_before##*) }
set -- $rest
[ "$#" -ge 20 ]
parent_pid=$2
process_group_id=$3
process_session_id=$4
start_ticks=${20}
case "$parent_pid:$process_group_id:$process_session_id:$start_ticks" in *[!0-9:]*) exit 32;; esac
executable_path=$(readlink -f "/proc/$pid/exe")
[ -n "$executable_path" ]
case "$executable_path" in *' (deleted)') exit 33;; esac
[ "$(printf '%s\n' "$executable_path" | wc -l | tr -d ' ')" -eq 1 ]
set -- $(stat -Lc '%d %i' "/proc/$pid/exe")
[ "$#" -eq 2 ]
executable_device=$1
executable_inode=$2
case "$executable_device:$executable_inode" in *[!0-9:]*) exit 34;; esac
cgroup=$(cat "/proc/$pid/cgroup")
case "$cgroup" in 0::/*) ;; *) exit 35;; esac
[ "$(printf '%s\n' "$cgroup" | wc -l | tr -d ' ')" -eq 1 ]
cgroup_path=${cgroup#0::}
token_line=$(tr '\0' '\n' < "/proc/$pid/environ" | awk 'BEGIN { found=0 } /^T_HUB_SESSION_TOKEN=/ { if (found) exit 42; found=1; value=substr($0, length("T_HUB_SESSION_TOKEN=") + 1) } END { if (found) printf "%s", value }')
[ "${#token_line}" -le 4096 ]
cmdline="/proc/$pid/cmdline"
size=$(wc -c < "$cmdline" | tr -d ' ')
case "$size" in ''|*[!0-9]*|0) exit 36;; esac
[ "$size" -le 65536 ]
stat_after=$(cat "/proc/$pid/stat")
[ "$stat_after" = "$stat_before" ]
set -- $(stat -Lc '%d %i' "/proc/$pid/exe")
[ "$#" -eq 2 ]
[ "$1" = "$executable_device" ]
[ "$2" = "$executable_inode" ]
printf 'THPI1\n%s\n%s\n%s\n%s\n%s\n%s\n%s\n%s\n%s\n%s\n' \
    "$pid" "$parent_pid" "$start_ticks" "$process_group_id" "$process_session_id" \
    "$executable_device" "$executable_inode" "$executable_path" "$cgroup_path" "$token_line"
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
            .arg("t-hub-scoped-harness-attestation")
            .arg(pid.to_string());
        command.creation_flags(0x0800_0000);
        command
    };
    #[cfg(unix)]
    let command = {
        let mut command = Command::new("sh");
        command
            .arg("-c")
            .arg(SCRIPT)
            .arg("t-hub-scoped-harness-attestation")
            .arg(pid.to_string());
        command
    };
    let output = crate::bounded_exec::output_with_timeout(command, Duration::from_secs(2))
        .map_err(|_| LaunchAttestationError::UnreadableEvidence)?;
    if !output.status.success() || !output.stderr.is_empty() {
        return Err(LaunchAttestationError::UnreadableEvidence);
    }
    parse_scoped_process_details(&output.stdout)
}

fn parse_scoped_process_details(
    bytes: &[u8],
) -> Result<ScopedProcessDetails, LaunchAttestationError> {
    let mut fields = bytes.splitn(12, |byte| *byte == b'\n');
    if fields.next() != Some(b"THPI1".as_slice()) {
        return Err(LaunchAttestationError::UnreadableEvidence);
    }
    let number = |field| {
        u32::try_from(parse_ascii_number(field)?)
            .map_err(|_| LaunchAttestationError::UnreadableEvidence)
    };
    let pid = number(fields.next())?;
    let parent_pid = number(fields.next())?;
    let start_ticks = parse_ascii_number(fields.next())?;
    let process_group_id = number(fields.next())?;
    let process_session_id = number(fields.next())?;
    let executable_device = parse_ascii_number(fields.next())?;
    let executable_inode = parse_ascii_number(fields.next())?;
    let text = |field: Option<&[u8]>| {
        std::str::from_utf8(field.ok_or(LaunchAttestationError::UnreadableEvidence)?)
            .map(str::to_string)
            .map_err(|_| LaunchAttestationError::UnreadableEvidence)
    };
    let executable_path = text(fields.next())?;
    let cgroup_path = text(fields.next())?;
    let token = text(fields.next())?;
    let cmdline = fields
        .next()
        .ok_or(LaunchAttestationError::UnreadableEvidence)?;
    if pid == 0
        || start_ticks == 0
        || process_group_id == 0
        || process_session_id == 0
        || executable_device == 0
        || executable_inode == 0
        || !executable_path.starts_with('/')
        || executable_path.contains('\0')
        || !cgroup_path.starts_with('/')
        || cgroup_path.contains('\0')
        || token.len() > 4096
        || cmdline.is_empty()
        || cmdline.len() > 65_536
        || !cmdline.ends_with(&[0])
    {
        return Err(LaunchAttestationError::UnreadableEvidence);
    }
    let argv = cmdline[..cmdline.len() - 1]
        .split(|byte| *byte == 0)
        .map(|argument| {
            std::str::from_utf8(argument)
                .map(str::to_string)
                .map_err(|_| LaunchAttestationError::UnreadableEvidence)
        })
        .collect::<Result<Vec<_>, _>>()?;
    if argv.is_empty() || argv.len() > 256 || argv.iter().any(|arg| arg.len() > 16_384) {
        return Err(LaunchAttestationError::UnreadableEvidence);
    }
    Ok(ScopedProcessDetails {
        pid,
        parent_pid,
        start_ticks,
        process_group_id,
        process_session_id,
        executable_path,
        executable_device,
        executable_inode,
        cgroup_path,
        session_token: (!token.is_empty()).then_some(token),
        argv,
    })
}

fn observe_scoped_processes_until(
    identities: &[ProcessIdentity],
    deadline: Instant,
) -> Result<Vec<ScopedProcessDetails>, LaunchAttestationError> {
    if identities.is_empty() || identities.len() > 64 {
        return Err(LaunchAttestationError::UnreadableEvidence);
    }
    #[cfg(test)]
    pause_scoped_ancestry_batch_until_deadline(deadline);
    const SCRIPT: &str = r#"
set -eu
count=$1
shift
case "$count" in ''|*[!0-9]*|0) exit 51;; esac
[ "$count" -le 64 ]
[ "$#" -eq "$count" ]
printf 'THPB1\n%s\n' "$count"
for expected in "$@"; do
    pid=${expected%%:*}
    expected_start=${expected#*:}
    case "$pid:$expected_start" in ''|*[!0-9:]*) exit 52;; esac
    stat_before=$(cat "/proc/$pid/stat")
    rest=${stat_before##*) }
    set -- $rest
    [ "$#" -ge 20 ]
    parent_pid=$2
    process_group_id=$3
    process_session_id=$4
    start_ticks=${20}
    [ "$start_ticks" = "$expected_start" ]
    case "$parent_pid:$process_group_id:$process_session_id:$start_ticks" in *[!0-9:]*) exit 53;; esac
    executable_path=$(readlink -f "/proc/$pid/exe")
    [ -n "$executable_path" ]
    case "$executable_path" in *' (deleted)') exit 54;; esac
    [ "$(printf '%s\n' "$executable_path" | wc -l | tr -d ' ')" -eq 1 ]
    set -- $(stat -Lc '%d %i' "/proc/$pid/exe")
    [ "$#" -eq 2 ]
    executable_device=$1
    executable_inode=$2
    case "$executable_device:$executable_inode" in *[!0-9:]*) exit 55;; esac
    cgroup=$(cat "/proc/$pid/cgroup")
    case "$cgroup" in 0::/*) ;; *) exit 56;; esac
    [ "$(printf '%s\n' "$cgroup" | wc -l | tr -d ' ')" -eq 1 ]
    cgroup_path=${cgroup#0::}
    token_line=$(tr '\0' '\n' < "/proc/$pid/environ" | awk 'BEGIN { found=0 } /^T_HUB_SESSION_TOKEN=/ { if (found) exit 62; found=1; value=substr($0, length("T_HUB_SESSION_TOKEN=") + 1) } END { if (found) printf "%s", value }')
    [ "${#token_line}" -le 4096 ]
    [ "$(printf '%s\n' "$token_line" | wc -l | tr -d ' ')" -eq 1 ]
    cmdline="/proc/$pid/cmdline"
    size=$(wc -c < "$cmdline" | tr -d ' ')
    case "$size" in ''|*[!0-9]*|0) exit 57;; esac
    [ "$size" -le 65536 ]
    stat_after=$(cat "/proc/$pid/stat")
    [ "$stat_after" = "$stat_before" ]
    set -- $(stat -Lc '%d %i' "/proc/$pid/exe")
    [ "$#" -eq 2 ]
    [ "$1" = "$executable_device" ]
    [ "$2" = "$executable_inode" ]
    printf 'THPI1\n%s\n%s\n%s\n%s\n%s\n%s\n%s\n%s\n%s\n%s\n%s\n' \
        "$pid" "$parent_pid" "$start_ticks" "$process_group_id" "$process_session_id" \
        "$executable_device" "$executable_inode" "$executable_path" "$cgroup_path" \
        "$token_line" "$size"
    cat "$cmdline"
    printf '\n'
done
"#;
    let mut command = Command::new("sh");
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command = Command::new("wsl.exe");
        command.arg("--cd").arg("~").arg("-e").arg("sh");
        command.creation_flags(0x0800_0000);
    }
    command
        .arg("-c")
        .arg(SCRIPT)
        .arg("t-hub-scoped-harness-attestation-batch")
        .arg(identities.len().to_string());
    for identity in identities {
        command.arg(format!("{}:{}", identity.pid, identity.start_ticks));
    }
    let output = crate::bounded_exec::output_with_timeout_and_limit(
        command,
        observation_time_remaining(deadline)?.min(Duration::from_secs(2)),
        identities.len().saturating_mul(70_000).saturating_add(1024),
    )
    .map_err(|_| LaunchAttestationError::UnreadableEvidence)?;
    if !output.status.success() || !output.stderr.is_empty() {
        return Err(LaunchAttestationError::UnreadableEvidence);
    }
    let bytes = output.stdout;
    let mut cursor = 0usize;
    let take_line = |cursor: &mut usize| -> Result<&[u8], LaunchAttestationError> {
        let rest = bytes
            .get(*cursor..)
            .ok_or(LaunchAttestationError::UnreadableEvidence)?;
        let end = rest
            .iter()
            .position(|byte| *byte == b'\n')
            .ok_or(LaunchAttestationError::UnreadableEvidence)?;
        *cursor = cursor.saturating_add(end + 1);
        Ok(&rest[..end])
    };
    if take_line(&mut cursor)? != b"THPB1" {
        return Err(LaunchAttestationError::UnreadableEvidence);
    }
    let count = usize::try_from(parse_ascii_number(Some(take_line(&mut cursor)?))?)
        .map_err(|_| LaunchAttestationError::UnreadableEvidence)?;
    if count != identities.len() {
        return Err(LaunchAttestationError::UnreadableEvidence);
    }
    let mut observed = Vec::with_capacity(count);
    for identity in identities {
        let mut record = Vec::new();
        for _ in 0..11 {
            record.extend_from_slice(take_line(&mut cursor)?);
            record.push(b'\n');
        }
        let cmdline_size = usize::try_from(parse_ascii_number(Some(take_line(&mut cursor)?))?)
            .map_err(|_| LaunchAttestationError::UnreadableEvidence)?;
        if cmdline_size == 0 || cmdline_size > 65_536 {
            return Err(LaunchAttestationError::UnreadableEvidence);
        }
        let end = cursor
            .checked_add(cmdline_size)
            .ok_or(LaunchAttestationError::UnreadableEvidence)?;
        record.extend_from_slice(
            bytes
                .get(cursor..end)
                .ok_or(LaunchAttestationError::UnreadableEvidence)?,
        );
        cursor = end;
        if bytes.get(cursor) != Some(&b'\n') {
            return Err(LaunchAttestationError::UnreadableEvidence);
        }
        cursor += 1;
        let process = parse_scoped_process_details(&record)?;
        if process.pid != identity.pid || process.start_ticks != identity.start_ticks {
            return Err(LaunchAttestationError::ProcessChanged);
        }
        observed.push(process);
    }
    if cursor != bytes.len() {
        return Err(LaunchAttestationError::UnreadableEvidence);
    }
    Ok(observed)
}

#[cfg(test)]
thread_local! {
    static SCOPED_ANCESTRY_DEADLINE_STALL: std::cell::RefCell<Option<std::sync::mpsc::Sender<()>>> =
        const { std::cell::RefCell::new(None) };
}

#[cfg(test)]
pub(crate) fn stall_next_scoped_ancestry_batch_for_current_thread(
    reached: std::sync::mpsc::Sender<()>,
) {
    SCOPED_ANCESTRY_DEADLINE_STALL.with(|slot| {
        let previous = slot.borrow_mut().replace(reached);
        assert!(
            previous.is_none(),
            "scoped ancestry stall already installed"
        );
    });
}

#[cfg(test)]
fn pause_scoped_ancestry_batch_until_deadline(deadline: Instant) {
    let reached = SCOPED_ANCESTRY_DEADLINE_STALL.with(|slot| slot.borrow_mut().take());
    if let Some(reached) = reached {
        reached
            .send(())
            .expect("scoped ancestry stall receiver dropped");
        let (hold_open, wait) = std::sync::mpsc::channel::<()>();
        let _hold_open = hold_open;
        let _ = wait.recv_timeout(deadline.saturating_duration_since(Instant::now()));
    }
}

fn framed_sha256(label: &[u8], values: impl IntoIterator<Item = impl AsRef<[u8]>>) -> String {
    let mut digest = Sha256::new();
    digest.update(label);
    for value in values {
        let value = value.as_ref();
        digest.update((value.len() as u64).to_be_bytes());
        digest.update(value);
    }
    format!("sha256:{:x}", digest.finalize())
}

fn constant_time_eq(left: &[u8], right: &[u8]) -> bool {
    let mut difference = left.len() ^ right.len();
    let compared_len = left.len().max(right.len());
    for index in 0..compared_len {
        difference |= usize::from(
            left.get(index).copied().unwrap_or_default()
                ^ right.get(index).copied().unwrap_or_default(),
        );
    }
    difference == 0
}

struct ResolvedLaunchExecutable {
    identity: HarnessExecutableIdentity,
    shebang: Option<String>,
}

fn resolve_launch_executable(
    candidate: &str,
) -> Result<ResolvedLaunchExecutable, LaunchAttestationError> {
    resolve_launch_executable_with_shell(candidate, None)
}

/// Interactive bash and dash report job-control setup failures when their stdin
/// is not a terminal, including on GitHub-hosted runners. Accept only those two
/// complete, content-free diagnostics. Startup-file errors and arbitrary shell
/// output still fail closed instead of being ignored or surfaced to callers.
fn valid_launch_resolution_stderr(shell_kind: &str, stderr: &[u8]) -> bool {
    if !matches!(shell_kind, "bash" | "dash" | "fish" | "sh" | "zsh") {
        return false;
    }
    if stderr.is_empty() {
        return true;
    }
    let Ok(stderr) = std::str::from_utf8(stderr) else {
        return false;
    };
    match shell_kind {
        "bash" => {
            let Some(stderr) = stderr.strip_suffix('\n') else {
                return false;
            };
            let mut lines = stderr.lines();
            let Some(first) = lines.next() else {
                return false;
            };
            if lines.next() != Some("bash: no job control in this shell") || lines.next().is_some()
            {
                return false;
            }
            let Some(first) = first.strip_prefix("bash: cannot set terminal process group (")
            else {
                return false;
            };
            let Some((process_group, reason)) = first.split_once("): ") else {
                return false;
            };
            process_group.parse::<i64>().is_ok()
                && matches!(
                    reason,
                    "Inappropriate ioctl for device" | "No such device or address"
                )
        }
        "dash" | "sh" => {
            stderr == "t-hub-shell-bridge: 0: can't access tty; job control turned off\n"
        }
        "fish" | "zsh" => false,
        _ => false,
    }
}

fn resolve_launch_executable_with_shell(
    candidate: &str,
    login_shell_override: Option<&str>,
) -> Result<ResolvedLaunchExecutable, LaunchAttestationError> {
    // The pane launches through `${SHELL} -ilc`, so a non-interactive `-lc`
    // probe can observe the wrong PATH (notably when zsh configures tools in
    // `.zshrc`). The bridge below uses that same shell mode only to classify a
    // command and resolve its absolute external path. It rejects functions,
    // aliases, relative paths, and unsupported shell languages, then hands the
    // path to an absolute POSIX shell that never executes the provider. Resetting
    // PATH inside the attestation probe also prevents user PATH shims from
    // intercepting readlink/stat/head. The optional override is a race-free test
    // seam; production passes an empty second script argument on Unix and WSL.
    const SCRIPT: &str = r#"
set -eu
candidate=$1
[ "$#" -eq 2 ]
[ -n "$candidate" ]
[ "${#candidate}" -le 4096 ]
login_shell=${2:-${SHELL:-/bin/sh}}
case "$login_shell" in /*) ;; *) exit 50;; esac
[ -f "$login_shell" ]
[ -x "$login_shell" ]
shell_kind=${login_shell##*/}
probe='set -eu
resolved=$1
shell_kind=$2
[ "$#" -eq 2 ]
[ -n "$resolved" ]
[ "${#resolved}" -le 4096 ]
case "$resolved" in /*) ;; *) exit 51;; esac
case "$shell_kind" in bash|dash|fish|sh|zsh) ;; *) exit 52;; esac
PATH=/usr/bin:/bin
export PATH
canonical=$(readlink -f "$resolved")
case "$canonical" in /*) ;; *) exit 53;; esac
[ -f "$canonical" ]
[ -x "$canonical" ]
[ "$(printf "%s\n" "$canonical" | wc -l | tr -d " ")" -eq 1 ]
set -- $(stat -Lc "%d %i" "$canonical")
[ "$#" -eq 2 ]
case "$1:$2" in *[!0-9:]*) exit 54;; esac
device=$1
inode=$2
shebang=
if LC_ALL=C head -c 2 "$canonical" | grep -q "^#!"; then
    shebang=$(LC_ALL=C head -n 1 "$canonical")
    shebang=${shebang#\#!}
    [ "${#shebang}" -le 1024 ]
    [ "$(printf "%s\n" "$shebang" | wc -l | tr -d " ")" -eq 1 ]
fi
printf "THLE2\n%s\n%s\n%s\n%s\n%s\n" "$shell_kind" "$canonical" "$device" "$inode" "$shebang"'
case "$shell_kind" in
  bash|dash|sh|zsh)
    bridge='candidate=$3
case "$candidate" in
  /*) resolved=$candidate;;
  */*) exit 70;;
  *)
    resolved=$(command -v -- "$candidate") || exit 70
    case "$resolved" in /*) ;; *) exit 70;; esac;;
esac
exec /bin/sh -c "$1" "$2" "$resolved" "$4"'
    "$login_shell" -ilc "$bridge" t-hub-shell-bridge "$probe" t-hub-provider-resolution "$candidate" "$shell_kind";;
  fish)
    bridge='set candidate $argv[3]
set resolved $candidate
if not string match -q "/*" -- $candidate
    if string match -q "*/*" -- $candidate
        exit 70
    end
    test (type -t -- $candidate) = file
    or exit 70
    set resolved (command -v -- $candidate)
    or exit 70
    string match -q "/*" -- $resolved
    or exit 70
end
exec /bin/sh -c $argv[1] $argv[2] $resolved $argv[4]'
    "$login_shell" -ilc "$bridge" "$probe" t-hub-provider-resolution "$candidate" "$shell_kind";;
  *) exit 55;;
esac
"#;
    if candidate.is_empty() || candidate.len() > 4096 || candidate.contains(['\0', '\n', '\r']) {
        return Err(LaunchAttestationError::UntrustedLaunchCommand);
    }
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
            .arg("t-hub-provider-provenance")
            .arg(candidate)
            .arg(login_shell_override.unwrap_or_default());
        command.creation_flags(0x0800_0000);
        command
    };
    #[cfg(unix)]
    let command = {
        let mut command = Command::new("sh");
        command
            .arg("-c")
            .arg(SCRIPT)
            .arg("t-hub-provider-provenance")
            .arg(candidate)
            .arg(login_shell_override.unwrap_or_default());
        command
    };
    let output =
        crate::bounded_exec::output_with_timeout_and_limit(command, Duration::from_secs(2), 8192)
            .map_err(|_| LaunchAttestationError::UntrustedLaunchCommand)?;
    if !output.status.success() {
        return Err(LaunchAttestationError::UntrustedLaunchCommand);
    }
    let mut fields = output.stdout.splitn(7, |byte| *byte == b'\n');
    if fields.next() != Some(b"THLE2".as_slice()) {
        return Err(LaunchAttestationError::UntrustedLaunchCommand);
    }
    let text = |field: Option<&[u8]>| {
        std::str::from_utf8(field.ok_or(LaunchAttestationError::UntrustedLaunchCommand)?)
            .map(str::to_string)
            .map_err(|_| LaunchAttestationError::UntrustedLaunchCommand)
    };
    let shell_kind = text(fields.next())?;
    let path = text(fields.next())?;
    let device = parse_ascii_number(fields.next())
        .map_err(|_| LaunchAttestationError::UntrustedLaunchCommand)?;
    let inode = parse_ascii_number(fields.next())
        .map_err(|_| LaunchAttestationError::UntrustedLaunchCommand)?;
    let shebang = text(fields.next())?;
    if !valid_launch_resolution_stderr(&shell_kind, &output.stderr)
        || fields.next() != Some(b"".as_slice())
        || !matches!(shell_kind.as_str(), "bash" | "dash" | "fish" | "sh" | "zsh")
        || !path.starts_with('/')
        || path.contains(['\0', '\n', '\r'])
        || device == 0
        || inode == 0
    {
        return Err(LaunchAttestationError::UntrustedLaunchCommand);
    }
    Ok(ResolvedLaunchExecutable {
        identity: HarnessExecutableIdentity {
            path,
            device,
            inode,
        },
        shebang: (!shebang.is_empty()).then_some(shebang),
    })
}

fn argv_layout_sha256(arguments: &[String]) -> String {
    let fields = arguments.iter().map(|argument| {
        if let Some(flag) = argument.strip_prefix('-') {
            let flag = flag.split_once('=').map_or(flag, |(name, _)| name);
            format!("flag:{flag}")
        } else {
            "value".to_string()
        }
    });
    framed_sha256(b"t-hub:harness-argv-layout:v1\0", fields)
}

fn codex_native_layout(os: &str, arch: &str) -> Option<(&'static str, &'static str, &'static str)> {
    match (os, arch) {
        ("Linux" | "Android", "x86_64") => {
            Some(("codex-linux-x64", "x86_64-unknown-linux-musl", "codex"))
        }
        ("Linux" | "Android", "aarch64" | "arm64") => {
            Some(("codex-linux-arm64", "aarch64-unknown-linux-musl", "codex"))
        }
        ("Darwin", "x86_64") => Some(("codex-darwin-x64", "x86_64-apple-darwin", "codex")),
        ("Darwin", "aarch64" | "arm64") => {
            Some(("codex-darwin-arm64", "aarch64-apple-darwin", "codex"))
        }
        _ => None,
    }
}

fn codex_native_candidates(
    package_root: &str,
    platform_package: &str,
    target_triple: &str,
    executable_name: &str,
) -> Option<[String; 2]> {
    let namespace_root = package_root.strip_suffix("/codex")?;
    Some([
        format!("{namespace_root}/{platform_package}/vendor/{target_triple}/bin/{executable_name}"),
        format!("{package_root}/vendor/{target_triple}/bin/{executable_name}"),
    ])
}

fn resolve_trusted_script_child(
    entry: &HarnessExecutableIdentity,
    expected_provider: Harness,
) -> Result<Option<HarnessExecutableIdentity>, LaunchAttestationError> {
    if expected_provider != Harness::Codex {
        return Ok(None);
    }
    let Some(package_root) = entry.path.strip_suffix("/bin/codex.js") else {
        return Ok(None);
    };
    if !package_root.ends_with("/@openai/codex")
        || package_root.contains(['\0', '\n', '\r'])
        || package_root.len() > 4096
    {
        return Ok(None);
    }
    const SCRIPT: &str = r#"
set -eu
package_root=$1
os=$(uname -s)
arch=$(uname -m)
case "$os:$arch" in
  Linux:x86_64|Android:x86_64)
    platform_package=codex-linux-x64
    target_triple=x86_64-unknown-linux-musl
    executable_name=codex;;
  Linux:aarch64|Linux:arm64|Android:aarch64|Android:arm64)
    platform_package=codex-linux-arm64
    target_triple=aarch64-unknown-linux-musl
    executable_name=codex;;
  Darwin:x86_64)
    platform_package=codex-darwin-x64
    target_triple=x86_64-apple-darwin
    executable_name=codex;;
  Darwin:aarch64|Darwin:arm64)
    platform_package=codex-darwin-arm64
    target_triple=aarch64-apple-darwin
    executable_name=codex;;
  *) exit 60;;
esac
namespace_root=${package_root%/codex}
optional="$namespace_root/$platform_package/vendor/$target_triple/bin/$executable_name"
bundled="$package_root/vendor/$target_triple/bin/$executable_name"
candidate=
if [ -f "$optional" ] && [ -x "$optional" ]; then
  candidate=$optional
elif [ -f "$bundled" ] && [ -x "$bundled" ]; then
  candidate=$bundled
else
  exit 61
fi
canonical=$(readlink -f "$candidate")
case "$canonical" in /*) ;; *) exit 62;; esac
[ -f "$canonical" ]
[ -x "$canonical" ]
if LC_ALL=C head -c 2 "$canonical" | grep -q '^#!'; then exit 63; fi
set -- $(stat -Lc '%d %i' "$canonical")
[ "$#" -eq 2 ]
case "$1:$2" in *[!0-9:]*) exit 64;; esac
printf 'THLC1\n%s\n%s\n%s\n%s\n%s\n%s\n' "$os" "$arch" "$candidate" "$canonical" "$1" "$2"
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
            .arg("t-hub-codex-native-child")
            .arg(package_root);
        command.creation_flags(0x0800_0000);
        command
    };
    #[cfg(unix)]
    let command = {
        let mut command = Command::new("sh");
        command
            .arg("-c")
            .arg(SCRIPT)
            .arg("t-hub-codex-native-child")
            .arg(package_root);
        command
    };
    let output = crate::bounded_exec::output_with_timeout(command, Duration::from_secs(2))
        .map_err(|_| LaunchAttestationError::UntrustedLaunchCommand)?;
    if !output.status.success() || !output.stderr.is_empty() || output.stdout.len() > 4352 {
        return Err(LaunchAttestationError::UntrustedLaunchCommand);
    }
    let mut fields = output.stdout.splitn(8, |byte| *byte == b'\n');
    if fields.next() != Some(b"THLC1".as_slice()) {
        return Err(LaunchAttestationError::UntrustedLaunchCommand);
    }
    fn text(field: Option<&[u8]>) -> Result<&str, LaunchAttestationError> {
        std::str::from_utf8(field.ok_or(LaunchAttestationError::UntrustedLaunchCommand)?)
            .map_err(|_| LaunchAttestationError::UntrustedLaunchCommand)
    }
    let os = text(fields.next())?;
    let arch = text(fields.next())?;
    let selected = text(fields.next())?;
    let path = text(fields.next())?.to_string();
    let device = text(fields.next())?
        .parse::<u64>()
        .map_err(|_| LaunchAttestationError::UntrustedLaunchCommand)?;
    let inode = text(fields.next())?
        .parse::<u64>()
        .map_err(|_| LaunchAttestationError::UntrustedLaunchCommand)?;
    let (platform_package, target_triple, executable_name) =
        codex_native_layout(os, arch).ok_or(LaunchAttestationError::UntrustedLaunchCommand)?;
    let candidates = codex_native_candidates(
        package_root,
        platform_package,
        target_triple,
        executable_name,
    )
    .ok_or(LaunchAttestationError::UntrustedLaunchCommand)?;
    if fields.next() != Some(b"".as_slice())
        || !candidates.iter().any(|candidate| candidate == selected)
        || !path.starts_with('/')
        || path.len() > 4096
        || path.contains(['\0', '\n', '\r'])
        || device == 0
        || inode == 0
    {
        return Err(LaunchAttestationError::UntrustedLaunchCommand);
    }
    Ok(Some(HarnessExecutableIdentity {
        path,
        device,
        inode,
    }))
}

pub fn resolve_expected_harness_launch_provenance(
    command: &str,
    expected_provider: Harness,
) -> Result<ExpectedHarnessLaunchProvenance, LaunchAttestationError> {
    if command.is_empty() || command.len() > 65_536 || command.contains('\0') {
        return Err(LaunchAttestationError::UntrustedLaunchCommand);
    }
    let mut argv =
        shell_words::split(command).map_err(|_| LaunchAttestationError::UntrustedLaunchCommand)?;
    if argv.first().is_some_and(|argument| argument == "exec") {
        argv.remove(0);
    }
    let executable_argument = argv
        .first()
        .ok_or(LaunchAttestationError::UntrustedLaunchCommand)?;
    if provider_executable(executable_name(executable_argument)) != Some(expected_provider)
        || argv
            .iter()
            .any(|argument| matches!(argument.as_str(), "&&" | "||" | ";" | "|" | "&"))
    {
        return Err(LaunchAttestationError::UntrustedLaunchCommand);
    }
    let entry = resolve_launch_executable(executable_argument)?;
    let Some(shebang) = entry.shebang.as_deref() else {
        return Ok(ExpectedHarnessLaunchProvenance {
            version: EXPECTED_HARNESS_LAUNCH_PROVENANCE_VERSION,
            provider: expected_provider.as_provider().into(),
            kind: "direct".into(),
            executable: entry.identity,
            entry_script: None,
            trusted_child_executable: None,
            argv_layout_sha256: None,
        });
    };
    let shebang_argv =
        shell_words::split(shebang).map_err(|_| LaunchAttestationError::UntrustedLaunchCommand)?;
    let runtime_argument = match shebang_argv.as_slice() {
        [environment, runtime]
            if executable_name(environment) == "env"
                && matches!(
                    executable_name(runtime).to_ascii_lowercase().as_str(),
                    "node" | "nodejs" | "bun" | "deno"
                ) =>
        {
            runtime
        }
        [runtime]
            if matches!(
                executable_name(runtime).to_ascii_lowercase().as_str(),
                "node" | "nodejs" | "bun" | "deno"
            ) =>
        {
            runtime
        }
        _ => return Err(LaunchAttestationError::UntrustedLaunchCommand),
    };
    let runtime = resolve_launch_executable(runtime_argument)?;
    if runtime.shebang.is_some() {
        return Err(LaunchAttestationError::UntrustedLaunchCommand);
    }
    let trusted_child_executable =
        resolve_trusted_script_child(&entry.identity, expected_provider)?;
    Ok(ExpectedHarnessLaunchProvenance {
        version: EXPECTED_HARNESS_LAUNCH_PROVENANCE_VERSION,
        provider: expected_provider.as_provider().into(),
        kind: "script".into(),
        executable: runtime.identity,
        entry_script: Some(entry.identity),
        trusted_child_executable,
        argv_layout_sha256: Some(argv_layout_sha256(&argv[1..])),
    })
}

pub fn valid_expected_harness_launch_provenance(
    expected: &ExpectedHarnessLaunchProvenance,
) -> bool {
    let valid_executable = |executable: &HarnessExecutableIdentity| {
        executable.path.starts_with('/')
            && executable.path.len() <= 4096
            && !executable.path.contains(['\0', '\n', '\r'])
            && executable.device > 0
            && executable.inode > 0
    };
    let valid_digest = |value: &str| {
        value.len() == 71
            && value.starts_with("sha256:")
            && value[7..]
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    };
    matches!(
        expected.version,
        1 | EXPECTED_HARNESS_LAUNCH_PROVENANCE_VERSION
    ) && (expected.version == EXPECTED_HARNESS_LAUNCH_PROVENANCE_VERSION
        || expected.trusted_child_executable.is_none())
        && matches!(expected.provider.as_str(), "codex" | "claude")
        && valid_executable(&expected.executable)
        && match expected.kind.as_str() {
            "direct" => {
                expected.entry_script.is_none()
                    && expected.trusted_child_executable.is_none()
                    && expected.argv_layout_sha256.is_none()
            }
            "script" => {
                expected.entry_script.as_ref().is_some_and(valid_executable)
                    && expected
                        .trusted_child_executable
                        .as_ref()
                        .is_none_or(valid_executable)
                    && expected
                        .argv_layout_sha256
                        .as_deref()
                        .is_some_and(valid_digest)
            }
            _ => false,
        }
}

fn scoped_process_matches_expected_launch(
    process: &ScopedProcessDetails,
    expected: &ExpectedHarnessLaunchProvenance,
) -> Result<bool, LaunchAttestationError> {
    if process.executable_path != expected.executable.path
        || process.executable_device != expected.executable.device
        || process.executable_inode != expected.executable.inode
    {
        return Ok(false);
    }
    match expected.kind.as_str() {
        "direct" => Ok(expected.entry_script.is_none() && expected.argv_layout_sha256.is_none()),
        "script" => {
            let observed_entry = process
                .argv
                .get(1)
                .ok_or(LaunchAttestationError::ExpectedProvenanceMismatch)
                .and_then(|entry| resolve_launch_executable(entry))?;
            let observed_layout = argv_layout_sha256(&process.argv[2..]);
            Ok(
                expected.entry_script.as_ref() == Some(&observed_entry.identity)
                    && expected.argv_layout_sha256.as_deref() == Some(observed_layout.as_str()),
            )
        }
        _ => Err(LaunchAttestationError::ExpectedProvenanceMismatch),
    }
}

fn scoped_process_matches_executable(
    process: &ScopedProcessDetails,
    expected: &HarnessExecutableIdentity,
) -> bool {
    process.executable_path == expected.path
        && process.executable_device == expected.device
        && process.executable_inode == expected.inode
}

/// Observe the exact provider process in the foreground ancestry and return
/// only bounded, credential-safe evidence suitable for durable recovery.
pub fn observe_scoped_harness_process(
    tmux_target: &str,
    expected_provider: Harness,
    expected_launch: &ExpectedHarnessLaunchProvenance,
    identity_id: &str,
    expected_session_token: &str,
    expected_cgroup_path: &str,
    pane_start_ticks: u64,
    deadline: Instant,
) -> Result<HarnessProcessIdentity, LaunchAttestationError> {
    if identity_id.is_empty()
        || expected_session_token.is_empty()
        || !expected_cgroup_path.starts_with('/')
        || pane_start_ticks == 0
        || !valid_expected_harness_launch_provenance(expected_launch)
        || expected_launch.provider != expected_provider.as_provider()
    {
        return Err(LaunchAttestationError::UnreadableEvidence);
    }
    let foreground = observe_harness_process_until(tmux_target, deadline)?;
    let details = observe_scoped_processes_until(&foreground.ancestry, deadline)?;
    for (index, process) in details.iter().enumerate() {
        if index + 1 < details.len() && process.parent_pid != details[index + 1].pid {
            return Err(LaunchAttestationError::AncestryChanged);
        }
        if process.cgroup_path != expected_cgroup_path {
            return Err(LaunchAttestationError::CgroupChanged);
        }
    }
    let mut harness_index = None;
    for (index, process) in details.iter().enumerate() {
        if scoped_process_matches_expected_launch(process, expected_launch)? {
            harness_index = Some(index);
            break;
        }
    }
    let wrapper_index = harness_index.ok_or(LaunchAttestationError::ExpectedProvenanceMismatch)?;
    let mut harness_index = wrapper_index;
    if let Some((index, process)) =
        details[..wrapper_index]
            .iter()
            .enumerate()
            .find(|(_, process)| {
                process_provider(&process.argv)
                    .is_some_and(|(provider, _)| provider == expected_provider)
            })
    {
        if expected_launch.kind != "script"
            || !expected_launch
                .trusted_child_executable
                .as_ref()
                .is_some_and(|expected| scoped_process_matches_executable(process, expected))
        {
            return Err(LaunchAttestationError::ExpectedProvenanceMismatch);
        }
        harness_index = index;
    }
    let process = &details[harness_index];
    let token = process
        .session_token
        .as_deref()
        .filter(|token| constant_time_eq(token.as_bytes(), expected_session_token.as_bytes()))
        .ok_or(LaunchAttestationError::SessionTokenMissing)?;
    let ancestry = details[harness_index..]
        .iter()
        .map(|process| HarnessProcessAncestor {
            pid: process.pid,
            start_ticks: process.start_ticks,
        })
        .collect::<Vec<_>>();
    Ok(HarnessProcessIdentity {
        version: HARNESS_PROCESS_IDENTITY_VERSION,
        provider: expected_provider.as_provider().into(),
        pid: process.pid,
        start_ticks: process.start_ticks,
        executable: HarnessExecutableIdentity {
            path: process.executable_path.clone(),
            device: process.executable_device,
            inode: process.executable_inode,
        },
        argv_sha256: framed_sha256(
            b"t-hub:harness-argv:v1\0",
            process.argv.iter().map(String::as_bytes),
        ),
        process_group_id: process.process_group_id,
        process_session_id: process.process_session_id,
        tmux_session_id: foreground.terminal.session_id,
        tmux_session_created: foreground.terminal.session_created,
        tmux_window_id: foreground.terminal.window_id,
        tmux_pane_id: foreground.terminal.pane_id,
        pane_pid: foreground.terminal.pane_pid,
        pane_start_ticks,
        ancestry,
        cgroup_path: expected_cgroup_path.into(),
        session_token_sha256: framed_sha256(
            b"t-hub:harness-session-token:v1\0",
            [identity_id.as_bytes(), token.as_bytes()],
        ),
    })
}

pub fn valid_harness_process_identity(identity: &HarnessProcessIdentity) -> bool {
    let valid_digest = |value: &str| {
        value.len() == 71
            && value.starts_with("sha256:")
            && value[7..]
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    };
    identity.version == HARNESS_PROCESS_IDENTITY_VERSION
        && matches!(identity.provider.as_str(), "codex" | "claude")
        && identity.pid > 0
        && identity.start_ticks > 0
        && identity.executable.path.starts_with('/')
        && !identity.executable.path.contains('\0')
        && !identity.executable.path.contains(['\n', '\r'])
        && identity.executable.device > 0
        && identity.executable.inode > 0
        && valid_digest(&identity.argv_sha256)
        && identity.process_group_id > 0
        && identity.process_session_id > 0
        && identity.tmux_session_created > 0
        && identity.pane_pid > 0
        && identity.pane_start_ticks > 0
        && !identity.ancestry.is_empty()
        && identity.ancestry.len() <= 64
        && identity
            .ancestry
            .iter()
            .enumerate()
            .all(|(index, ancestor)| {
                ancestor.pid > 0
                    && ancestor.start_ticks > 0
                    && identity.ancestry[..index]
                        .iter()
                        .all(|seen| seen.pid != ancestor.pid)
            })
        && identity.ancestry.first().is_some_and(|ancestor| {
            ancestor.pid == identity.pid && ancestor.start_ticks == identity.start_ticks
        })
        && identity.ancestry.last().is_some_and(|ancestor| {
            ancestor.pid == identity.pane_pid && ancestor.start_ticks == identity.pane_start_ticks
        })
        && identity.cgroup_path.starts_with('/')
        && !identity.cgroup_path.contains('\0')
        && !identity.cgroup_path.contains(['\n', '\r'])
        && valid_digest(&identity.session_token_sha256)
}

fn parse_process_evidence(bytes: &[u8]) -> Result<HarnessProcessEvidence, LaunchAttestationError> {
    let mut fields = bytes.splitn(12, |byte| *byte == b'\n');
    if fields.next() != Some(b"THPA3".as_slice()) {
        return Err(LaunchAttestationError::UnreadableEvidence);
    }
    let terminal = TerminalGeneration {
        session_id: parse_prefixed_ascii_number(fields.next(), b'$')?,
        session_created: parse_ascii_number(fields.next())?,
        window_id: parse_prefixed_ascii_number(fields.next(), b'@')?,
        pane_id: parse_prefixed_ascii_number(fields.next(), b'%')?,
        pane_pid: u32::try_from(parse_ascii_number(fields.next())?)
            .map_err(|_| LaunchAttestationError::UnreadableEvidence)?,
    };
    if terminal.session_created == 0 || terminal.pane_pid == 0 {
        return Err(LaunchAttestationError::UnreadableEvidence);
    }
    let pid = parse_ascii_number(fields.next())?;
    let start_ticks = parse_ascii_number(fields.next())?;
    let executable_device = parse_ascii_number(fields.next())?;
    let executable_inode = parse_ascii_number(fields.next())?;
    let ancestry = parse_process_ancestry(fields.next())?;
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
    let pid = u32::try_from(pid).map_err(|_| LaunchAttestationError::UnreadableEvidence)?;
    if ancestry.first() != Some(&ProcessIdentity { pid, start_ticks })
        || ancestry.last().map(|identity| identity.pid) != Some(terminal.pane_pid)
    {
        return Err(LaunchAttestationError::UnreadableEvidence);
    }
    Ok(HarnessProcessEvidence {
        terminal,
        ancestry,
        pid,
        start_ticks,
        executable_device,
        executable_inode,
        argv,
    })
}

fn parse_prefixed_ascii_number(
    field: Option<&[u8]>,
    prefix: u8,
) -> Result<u64, LaunchAttestationError> {
    let field = field.ok_or(LaunchAttestationError::UnreadableEvidence)?;
    if field.first() != Some(&prefix) {
        return Err(LaunchAttestationError::UnreadableEvidence);
    }
    parse_ascii_number(Some(&field[1..]))
}

fn parse_process_ancestry(
    field: Option<&[u8]>,
) -> Result<Vec<ProcessIdentity>, LaunchAttestationError> {
    let field = std::str::from_utf8(field.ok_or(LaunchAttestationError::UnreadableEvidence)?)
        .map_err(|_| LaunchAttestationError::UnreadableEvidence)?;
    let ancestry = field
        .split(',')
        .map(|identity| {
            let (pid, start_ticks) = identity
                .split_once(':')
                .ok_or(LaunchAttestationError::UnreadableEvidence)?;
            Ok(ProcessIdentity {
                pid: pid
                    .parse()
                    .ok()
                    .filter(|pid| *pid > 0)
                    .ok_or(LaunchAttestationError::UnreadableEvidence)?,
                start_ticks: start_ticks
                    .parse()
                    .ok()
                    .filter(|start_ticks| *start_ticks > 0)
                    .ok_or(LaunchAttestationError::UnreadableEvidence)?,
            })
        })
        .collect::<Result<Vec<_>, _>>()?;
    if ancestry.is_empty()
        || ancestry.len() > 64
        || ancestry.iter().enumerate().any(|(index, identity)| {
            ancestry[..index]
                .iter()
                .any(|seen| seen.pid == identity.pid)
        })
    {
        return Err(LaunchAttestationError::UnreadableEvidence);
    }
    Ok(ancestry)
}

fn parse_ascii_number(field: Option<&[u8]>) -> Result<u64, LaunchAttestationError> {
    let field = field.ok_or(LaunchAttestationError::UnreadableEvidence)?;
    std::str::from_utf8(field)
        .ok()
        .and_then(|value| value.parse().ok())
        .ok_or(LaunchAttestationError::UnreadableEvidence)
}

pub(crate) fn provider_arguments(
    evidence: &HarnessProcessEvidence,
    expected: Harness,
) -> Result<&[String], LaunchAttestationError> {
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PermissionOptions<'a> {
    options: Vec<(&'a str, Option<&'a str>)>,
}

impl PermissionOptions<'_> {
    pub(crate) fn contains(&self, flag: &str) -> bool {
        self.options.iter().any(|(candidate, _)| *candidate == flag)
    }

    pub(crate) fn value(&self, flag: &str) -> Option<&str> {
        self.options
            .iter()
            .find(|(candidate, _)| *candidate == flag)
            .and_then(|(_, value)| *value)
    }

    pub(crate) fn ensure_unique(
        &self,
        permission_flags: &[&str],
    ) -> Result<(), LaunchAttestationError> {
        if permission_flags.iter().any(|flag| {
            self.options
                .iter()
                .filter(|(candidate, _)| candidate == flag)
                .count()
                > 1
        }) {
            return Err(LaunchAttestationError::ConflictingPermission);
        }
        Ok(())
    }
}

pub(crate) fn leading_permission_options(
    args: &[String],
) -> Result<PermissionOptions<'_>, LaunchAttestationError> {
    let mut options = Vec::new();
    let mut index = 0;
    while let Some(argument) = args.get(index) {
        if argument == "--" || !argument.starts_with('-') {
            break;
        }
        let Some((flag, takes_value, inline_value)) = native_permission_option(argument) else {
            options.push((argument.as_str(), None));
            index += 1;
            continue;
        };
        if !takes_value {
            if inline_value.is_some() {
                return Err(LaunchAttestationError::MalformedPermission);
            }
            options.push((flag, None));
            index += 1;
            continue;
        }
        let (value, consumed) = match inline_value {
            Some("") => return Err(LaunchAttestationError::MissingPermission),
            Some(value) => (value, 1),
            None => {
                let value = args
                    .get(index + 1)
                    .map(String::as_str)
                    .filter(|value| !value.starts_with('-'))
                    .ok_or(LaunchAttestationError::MissingPermission)?;
                (value, 2)
            }
        };
        options.push((flag, Some(value)));
        index += consumed;
    }
    Ok(PermissionOptions { options })
}

fn native_permission_option(argument: &str) -> Option<(&'static str, bool, Option<&str>)> {
    let (long_flag, inline_value) = argument
        .split_once('=')
        .map_or((argument, None), |(flag, value)| (flag, Some(value)));
    let long = match long_flag {
        "--dangerously-bypass-approvals-and-sandbox" => Some((
            "--dangerously-bypass-approvals-and-sandbox",
            false,
            inline_value,
        )),
        "--sandbox" => Some(("--sandbox", true, inline_value)),
        "--ask-for-approval" => Some(("--ask-for-approval", true, inline_value)),
        "--yolo" => Some(("--yolo", false, inline_value)),
        "--full-auto" => Some(("--full-auto", false, inline_value)),
        "--dangerously-skip-permissions" => {
            Some(("--dangerously-skip-permissions", false, inline_value))
        }
        "--permission-mode" => Some(("--permission-mode", true, inline_value)),
        _ => None,
    };
    if long.is_some() {
        return long;
    }
    for (short, canonical) in [("-s", "--sandbox"), ("-a", "--ask-for-approval")] {
        if argument == short {
            return Some((canonical, true, None));
        }
        if let Some(value) = argument.strip_prefix(short) {
            let value = value.strip_prefix('=').unwrap_or(value);
            return Some((canonical, true, Some(value)));
        }
    }
    None
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
    fn wsl_linux_native_layout_prefers_optional_package_then_bundled_fallback() {
        let (package, triple, executable) = codex_native_layout("Linux", "x86_64").unwrap();
        assert_eq!(package, "codex-linux-x64");
        assert_eq!(triple, "x86_64-unknown-linux-musl");
        let candidates = codex_native_candidates(
            "/home/test/node_modules/@openai/codex",
            package,
            triple,
            executable,
        )
        .unwrap();
        assert_eq!(
            candidates[0],
            "/home/test/node_modules/@openai/codex-linux-x64/vendor/x86_64-unknown-linux-musl/bin/codex"
        );
        assert_eq!(
            candidates[1],
            "/home/test/node_modules/@openai/codex/vendor/x86_64-unknown-linux-musl/bin/codex"
        );

        assert_eq!(
            codex_native_layout("Linux", "aarch64").unwrap().0,
            "codex-linux-arm64"
        );
        assert!(codex_native_layout("Windows_NT", "AMD64").is_none());
        assert!(codex_native_candidates(
            "C:\\node_modules\\@openai\\codex",
            package,
            triple,
            executable
        )
        .is_none());
    }

    #[cfg(unix)]
    fn make_executable(path: &std::path::Path) {
        use std::os::unix::fs::PermissionsExt;
        let mut permissions = std::fs::metadata(path).unwrap().permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(path, permissions).unwrap();
    }

    #[test]
    fn launch_resolution_accepts_only_exact_non_tty_shell_diagnostics() {
        assert!(valid_launch_resolution_stderr("bash", b""));
        assert!(valid_launch_resolution_stderr(
            "bash",
            b"bash: cannot set terminal process group (42): Inappropriate ioctl for device\nbash: no job control in this shell\n"
        ));
        assert!(valid_launch_resolution_stderr(
            "bash",
            b"bash: cannot set terminal process group (-1): No such device or address\nbash: no job control in this shell\n"
        ));
        assert!(valid_launch_resolution_stderr(
            "dash",
            b"t-hub-shell-bridge: 0: can't access tty; job control turned off\n"
        ));
        assert!(!valid_launch_resolution_stderr(
            "bash",
            b"bash: cannot set terminal process group (42): Inappropriate ioctl for device\nbash: no job control in this shell\nstartup leaked a secret\n"
        ));
        assert!(!valid_launch_resolution_stderr(
            "bash",
            b"bash: cannot set terminal process group (not-a-pid): Inappropriate ioctl for device\nbash: no job control in this shell\n"
        ));
        assert!(!valid_launch_resolution_stderr(
            "zsh",
            b"unexpected startup output\n"
        ));
        assert!(!valid_launch_resolution_stderr("nu", b""));
    }

    #[cfg(unix)]
    #[test]
    fn launch_resolution_handles_ci_bash_and_available_login_shells() {
        let root = std::env::temp_dir().join(format!(
            "t-hub-login-shell-resolution-{}-{}",
            std::process::id(),
            uuid::Uuid::new_v4().simple()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let provider = root.join("codex");
        std::fs::copy("/bin/sleep", &provider).unwrap();
        let provider = provider.to_str().unwrap();

        let mut tested = Vec::new();
        for shell in [
            "/bin/bash",
            "/usr/bin/bash",
            "/bin/zsh",
            "/usr/bin/zsh",
            "/bin/dash",
            "/usr/bin/dash",
            "/bin/sh",
            "/usr/bin/fish",
            "/bin/fish",
        ] {
            if !std::path::Path::new(shell).is_file()
                || tested.iter().any(|seen: &&str| {
                    std::fs::canonicalize(seen).ok() == std::fs::canonicalize(shell).ok()
                })
            {
                continue;
            }
            resolve_launch_executable_with_shell(provider, Some(shell)).unwrap();
            tested.push(shell);
        }
        assert!(tested.iter().any(|shell| shell.ends_with("/bash")));
        assert!(tested
            .iter()
            .any(|shell| shell.ends_with("/sh") || shell.ends_with("/dash")));

        std::fs::remove_dir_all(root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn login_shell_resolution_rejects_aliases_and_functions_without_executing_provider() {
        let root = std::env::temp_dir().join(format!(
            "t-hub-login-shell-shadow-{}-{}",
            std::process::id(),
            uuid::Uuid::new_v4().simple()
        ));
        let provider_dir = root.join("provider");
        std::fs::create_dir_all(&provider_dir).unwrap();
        let runtime = root.join("node");
        std::fs::copy("/bin/sleep", &runtime).unwrap();
        let marker = root.join("provider-executed");
        let provider = provider_dir.join("codex");
        std::fs::write(
            &provider,
            format!(
                "#!{}\ntouch {}\n",
                runtime.display(),
                sh_single_quote(marker.to_str().unwrap())
            ),
        )
        .unwrap();
        make_executable(&provider);

        let rc = root.join("bashrc");
        let shell = root.join("bash");
        std::fs::write(
            &shell,
            format!(
                "#!/bin/sh\n[ \"$1\" = -ilc ] || exit 90\nshift\nbridge=$1\nshift\nexec /bin/bash --noprofile --rcfile {} -ic \"$bridge\" \"$@\"\n",
                sh_single_quote(rc.to_str().unwrap())
            ),
        )
        .unwrap();
        make_executable(&shell);
        let path_setup = format!(
            "export PATH={}:\"$PATH\"\n",
            sh_single_quote(provider_dir.to_str().unwrap())
        );

        std::fs::write(&rc, format!("{path_setup}codex() {{ :; }}\n")).unwrap();
        assert_eq!(
            resolve_launch_executable_with_shell("codex", Some(shell.to_str().unwrap()))
                .map(|_| ()),
            Err(LaunchAttestationError::UntrustedLaunchCommand)
        );

        std::fs::write(&rc, format!("{path_setup}alias codex=/bin/true\n")).unwrap();
        assert_eq!(
            resolve_launch_executable_with_shell("codex", Some(shell.to_str().unwrap()))
                .map(|_| ()),
            Err(LaunchAttestationError::UntrustedLaunchCommand)
        );

        std::fs::write(&rc, path_setup).unwrap();
        let resolved =
            resolve_launch_executable_with_shell("codex", Some(shell.to_str().unwrap())).unwrap();
        assert_eq!(resolved.identity.path, provider.to_str().unwrap());
        assert!(!marker.exists(), "the provider command must never execute");

        std::fs::remove_dir_all(root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn expected_launch_provenance_binds_direct_symlink_runtime_entry_and_layout() {
        use std::os::unix::fs::symlink;

        let root = std::env::temp_dir().join(format!(
            "t-hub-launch-provenance-{}-{}",
            std::process::id(),
            uuid::Uuid::new_v4().simple()
        ));
        let direct_dir = root.join("direct");
        let link_dir = root.join("link");
        let script_dir = root.join("script");
        let runtime_dir = root.join("runtime");
        for directory in [&direct_dir, &link_dir, &script_dir, &runtime_dir] {
            std::fs::create_dir_all(directory).unwrap();
        }

        let direct = direct_dir.join("codex");
        std::fs::copy("/bin/sleep", &direct).unwrap();
        let direct_expected = resolve_expected_harness_launch_provenance(
            &format!("{} 60", direct.display()),
            Harness::Codex,
        )
        .unwrap();
        assert_eq!(direct_expected.kind, "direct");
        assert!(direct_expected.entry_script.is_none());

        let linked = link_dir.join("codex");
        symlink(&direct, &linked).unwrap();
        let symlink_expected = resolve_expected_harness_launch_provenance(
            &format!("{} 60", linked.display()),
            Harness::Codex,
        )
        .unwrap();
        assert_eq!(symlink_expected, direct_expected);

        let replacement = direct_dir.join("replacement");
        std::fs::copy("/bin/true", &replacement).unwrap();
        std::fs::rename(&replacement, &direct).unwrap();
        let replaced_direct = resolve_expected_harness_launch_provenance(
            &format!("{} 60", direct.display()),
            Harness::Codex,
        )
        .unwrap();
        assert_eq!(
            replaced_direct.executable.path,
            direct_expected.executable.path
        );
        assert_ne!(
            replaced_direct.executable.inode,
            direct_expected.executable.inode
        );

        let runtime = runtime_dir.join("node");
        std::fs::copy("/bin/sleep", &runtime).unwrap();
        let script = script_dir.join("codex");
        std::fs::write(&script, format!("#!{}\nprovider body\n", runtime.display())).unwrap();
        make_executable(&script);
        let script_expected = resolve_expected_harness_launch_provenance(
            &format!("{} --mode alpha prompt", script.display()),
            Harness::Codex,
        )
        .unwrap();
        let same_layout = resolve_expected_harness_launch_provenance(
            &format!("{} --mode beta secret", script.display()),
            Harness::Codex,
        )
        .unwrap();
        assert_eq!(script_expected.kind, "script");
        assert_eq!(
            script_expected.argv_layout_sha256,
            same_layout.argv_layout_sha256
        );
        assert_ne!(
            script_expected.argv_layout_sha256,
            resolve_expected_harness_launch_provenance(
                &format!("{} --other beta secret", script.display()),
                Harness::Codex,
            )
            .unwrap()
            .argv_layout_sha256
        );

        let script_replacement = script_dir.join("replacement");
        std::fs::write(
            &script_replacement,
            format!("#!{}\nreplacement body\n", runtime.display()),
        )
        .unwrap();
        make_executable(&script_replacement);
        std::fs::rename(&script_replacement, &script).unwrap();
        let replaced_script = resolve_expected_harness_launch_provenance(
            &format!("{} --mode alpha prompt", script.display()),
            Harness::Codex,
        )
        .unwrap();
        assert_eq!(
            replaced_script.entry_script.as_ref().unwrap().path,
            script_expected.entry_script.as_ref().unwrap().path
        );
        assert_ne!(
            replaced_script.entry_script.as_ref().unwrap().inode,
            script_expected.entry_script.as_ref().unwrap().inode
        );

        let runtime_replacement = runtime_dir.join("replacement");
        std::fs::copy("/bin/true", &runtime_replacement).unwrap();
        std::fs::rename(&runtime_replacement, &runtime).unwrap();
        let replaced_runtime = resolve_expected_harness_launch_provenance(
            &format!("{} --mode alpha prompt", script.display()),
            Harness::Codex,
        )
        .unwrap();
        assert_eq!(
            replaced_runtime.executable.path,
            script_expected.executable.path
        );
        assert_ne!(
            replaced_runtime.executable.inode,
            script_expected.executable.inode
        );

        std::fs::remove_dir_all(root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn expected_launch_provenance_rejects_wrong_provider_and_arbitrary_shebangs() {
        let root = std::env::temp_dir().join(format!(
            "t-hub-untrusted-launch-provenance-{}-{}",
            std::process::id(),
            uuid::Uuid::new_v4().simple()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let script = root.join("codex");
        std::fs::write(&script, "#!/bin/sh\nsleep 60\n").unwrap();
        make_executable(&script);
        assert_eq!(
            resolve_expected_harness_launch_provenance(script.to_str().unwrap(), Harness::Codex),
            Err(LaunchAttestationError::UntrustedLaunchCommand)
        );
        assert_eq!(
            resolve_expected_harness_launch_provenance("claude", Harness::Codex),
            Err(LaunchAttestationError::UntrustedLaunchCommand)
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    fn shell_evidence() -> HarnessProcessEvidence {
        HarnessProcessEvidence::test_after_exec(42, 900, 100, &["zsh"])
    }

    fn codex_bypass_evidence(pid: u32, start_ticks: u64) -> HarnessProcessEvidence {
        HarnessProcessEvidence::test_after_exec(
            pid,
            start_ticks,
            200,
            &[
                "codex",
                "--dangerously-bypass-approvals-and-sandbox",
                "work",
            ],
        )
    }

    fn respawn_transition(
        before_pane_pid: u32,
        after_pane_pid: u32,
    ) -> crate::tmux::RespawnPaneTransition {
        crate::tmux::RespawnPaneTransition {
            before: crate::tmux::PaneGeneration {
                session_id: 17,
                session_created: 123_456,
                window_id: 9,
                pane_id: 42,
                pane_pid: before_pane_pid,
            },
            after: crate::tmux::PaneGeneration {
                session_id: 17,
                session_created: 123_456,
                window_id: 9,
                pane_id: 42,
                pane_pid: after_pane_pid,
            },
        }
    }

    fn respawn_provider_evidence() -> HarnessProcessEvidence {
        HarnessProcessEvidence::test_with_context(
            77,
            901,
            77,
            200,
            &[
                "codex",
                "--dangerously-bypass-approvals-and-sandbox",
                "work",
            ],
            123_456,
            77,
            &[(77, 901)],
        )
    }

    #[test]
    fn respawn_attestation_requires_exact_new_pane_generation_and_provider_posture() {
        let before = shell_evidence();
        let after = respawn_provider_evidence();
        let transition = respawn_transition(42, 77);
        assert_eq!(
            attest_respawn_launch_permissions(
                Harness::Codex.adapter(),
                &before,
                &transition,
                &after,
                PermMode::BypassPermissions,
            )
            .unwrap()
            .permission,
            PermMode::BypassPermissions
        );

        let mut substituted = transition;
        substituted.after.window_id = 10;
        assert_eq!(
            attest_respawn_launch_permissions(
                Harness::Codex.adapter(),
                &before,
                &substituted,
                &after,
                PermMode::BypassPermissions,
            )
            .unwrap_err(),
            LaunchAttestationError::TerminalChanged
        );

        assert_eq!(
            attest_respawn_launch_permissions(
                Harness::Codex.adapter(),
                &before,
                &respawn_transition(42, 42),
                &after,
                PermMode::BypassPermissions,
            )
            .unwrap_err(),
            LaunchAttestationError::TerminalChanged
        );

        let stale_after = HarnessProcessEvidence::test_with_context(
            42,
            900,
            42,
            100,
            &["zsh"],
            123_456,
            77,
            &[(42, 900)],
        );
        assert_eq!(
            attest_respawn_launch_permissions(
                Harness::Codex.adapter(),
                &before,
                &transition,
                &stale_after,
                PermMode::BypassPermissions,
            )
            .unwrap_err(),
            LaunchAttestationError::StaleEvidence
        );

        let wrapper = HarnessProcessEvidence::test_with_context(
            77,
            901,
            77,
            200,
            &["sh", "/tmp/codex-wrapper"],
            123_456,
            77,
            &[(77, 901)],
        );
        assert_eq!(
            attest_respawn_launch_permissions(
                Harness::Codex.adapter(),
                &before,
                &transition,
                &wrapper,
                PermMode::BypassPermissions,
            )
            .unwrap_err(),
            LaunchAttestationError::WrapperObscured
        );
    }

    #[test]
    fn launch_acceptance_rejects_provider_exit_between_observations() {
        let before = shell_evidence();
        let first = codex_bypass_evidence(42, 900);
        attest_launch_permissions(
            Harness::Codex.adapter(),
            &before,
            &first,
            PermMode::BypassPermissions,
        )
        .unwrap();
        assert_eq!(
            attest_final_launch_permissions(
                Harness::Codex.adapter(),
                &first,
                Err(LaunchAttestationError::UnreadableEvidence),
                PermMode::BypassPermissions,
            )
            .unwrap_err(),
            LaunchAttestationError::UnreadableEvidence
        );
    }

    #[test]
    fn launch_acceptance_rejects_posture_change_between_observations() {
        let first = codex_bypass_evidence(42, 900);
        let changed = HarnessProcessEvidence::test_after_exec(
            42,
            900,
            200,
            &["codex", "--sandbox", "workspace-write", "work"],
        );
        assert_eq!(
            attest_final_launch_permissions(
                Harness::Codex.adapter(),
                &first,
                Ok(changed),
                PermMode::BypassPermissions,
            )
            .unwrap_err(),
            LaunchAttestationError::WrongPermission
        );
    }

    #[test]
    fn launch_acceptance_rejects_wrapper_or_ancestry_change_between_observations() {
        let first = codex_bypass_evidence(42, 900);
        let wrapper =
            HarnessProcessEvidence::test_after_exec(42, 900, 200, &["sh", "/tmp/codex-wrapper"]);
        assert_eq!(
            attest_final_launch_permissions(
                Harness::Codex.adapter(),
                &first,
                Ok(wrapper),
                PermMode::BypassPermissions,
            )
            .unwrap_err(),
            LaunchAttestationError::WrapperObscured
        );

        let first = HarnessProcessEvidence::test_with_context(
            42,
            900,
            42,
            200,
            &[
                "codex",
                "--dangerously-bypass-approvals-and-sandbox",
                "work",
            ],
            123_456,
            7,
            &[(42, 900), (7, 100)],
        );
        let changed_ancestry = HarnessProcessEvidence::test_with_context(
            42,
            900,
            42,
            200,
            &[
                "codex",
                "--dangerously-bypass-approvals-and-sandbox",
                "work",
            ],
            123_456,
            7,
            &[(42, 900), (8, 101), (7, 100)],
        );
        assert_eq!(
            attest_final_launch_permissions(
                Harness::Codex.adapter(),
                &first,
                Ok(changed_ancestry),
                PermMode::BypassPermissions,
            )
            .unwrap_err(),
            LaunchAttestationError::AncestryChanged
        );
    }

    #[test]
    fn launch_acceptance_rejects_pane_generation_change_between_observations() {
        let first = codex_bypass_evidence(42, 900);
        let replacement = HarnessProcessEvidence::test_with_context(
            42,
            900,
            42,
            200,
            &[
                "codex",
                "--dangerously-bypass-approvals-and-sandbox",
                "work",
            ],
            123_457,
            42,
            &[(42, 900)],
        );
        assert_eq!(
            attest_final_launch_permissions(
                Harness::Codex.adapter(),
                &first,
                Ok(replacement),
                PermMode::BypassPermissions,
            )
            .unwrap_err(),
            LaunchAttestationError::TerminalChanged
        );
    }

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
    fn launch_attestation_accepts_proven_exec_transition() {
        let before = HarnessProcessEvidence::test_after_exec(42, 900, 100, &["zsh"]);
        let after = HarnessProcessEvidence::test_after_exec(
            42,
            900,
            200,
            &[
                "codex",
                "--dangerously-bypass-approvals-and-sandbox",
                "work",
            ],
        );
        assert_eq!(
            attest_launch_permissions(
                Harness::Codex.adapter(),
                &before,
                &after,
                PermMode::BypassPermissions,
            )
            .unwrap()
            .permission,
            PermMode::BypassPermissions
        );
    }

    #[test]
    fn process_evidence_parser_is_bounded_and_strict() {
        let parsed = parse_process_evidence(
            b"THPA3\n$17\n123456\n@9\n%42\n42\n42\n900\n8\n1234\n42:900\ncodex\0--sandbox\0read-only\0",
        )
        .unwrap();
        assert_eq!(parsed.identity(), (42, 900));
        assert_eq!(parsed.executable_identity(), (8, 1234));
        assert_eq!(parsed.argv, ["codex", "--sandbox", "read-only"]);

        assert_eq!(
            parse_process_evidence(
                b"THPA3\n$17\n123456\n@9\n%42\n42\n42\n900\n8\n1234\n42:900\ncodex",
            ),
            Err(LaunchAttestationError::UnreadableEvidence)
        );
        assert_eq!(
            parse_process_evidence(
                b"THPA3\n$17\n123456\n@9\n%42\n42\n42\n900\n8\n1234\n42:900,7:100\ncodex\0",
            ),
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

    #[test]
    fn crew_default_permission_uses_exact_provider_native_bypass_flags() {
        assert_eq!(CREW_DEFAULT_PERMISSION, PermMode::BypassPermissions);
        assert_eq!(
            Harness::Codex
                .adapter()
                .permission_map(CREW_DEFAULT_PERMISSION),
            ["--dangerously-bypass-approvals-and-sandbox"]
        );
        assert_eq!(
            Harness::Claude
                .adapter()
                .permission_map(CREW_DEFAULT_PERMISSION),
            ["--dangerously-skip-permissions"]
        );
    }
}
