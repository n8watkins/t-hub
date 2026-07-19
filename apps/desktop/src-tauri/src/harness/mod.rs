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
/// provider Harness, but does not expand Crew scope, T-Hub capability, Powder
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
#[derive(Debug, Clone, PartialEq, Eq)]
struct TerminalGeneration {
    session_name: String,
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HarnessProcessEvidence {
    terminal: TerminalGeneration,
    ancestry: Vec<ProcessIdentity>,
    pid: u32,
    start_ticks: u64,
    executable_device: u64,
    executable_inode: u64,
    resolved_process_executable: String,
    argv: Vec<String>,
}

/// Non-secret tmux generation proven by one bounded foreground-process read.
/// Recovery journals this before it treats a reserved terminal as spawned.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct HarnessTerminalGenerationReceipt {
    pub tmux_target: String,
    pub session_id: u64,
    pub session_created: u64,
    pub window_id: u64,
    pub pane_id: u64,
    pub pane_pid: u32,
}

/// Exact provider file selected before launch. For a node-style provider this
/// is the script inode, distinct from the node process executable inode.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct HarnessProviderLaunchReceipt {
    pub resolved_provider_executable: String,
    pub provider_executable_device: u64,
    pub provider_executable_inode: u64,
}

/// Adapter-produced receipt for the effective provider runtime. Callers cannot
/// obtain this from desired policy alone: construction runs provider-native
/// permission, model, reasoning, executable, and process-generation checks.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct HarnessRuntimePolicyReceipt {
    pub terminal: HarnessTerminalGenerationReceipt,
    pub process_pid: u32,
    pub process_start_ticks: u64,
    pub process_executable_device: u64,
    pub process_executable_inode: u64,
    pub provider_executable_device: u64,
    pub provider_executable_inode: u64,
    pub resolved_provider_executable: String,
    pub executable_version: String,
    pub observed_provider: String,
    pub observed_harness: String,
    pub observed_model: String,
    pub observed_reasoning_effort: String,
    pub observed_permission: PermMode,
}

pub(crate) struct VerifiedHarnessTerminalGeneration(HarnessTerminalGenerationReceipt);

impl VerifiedHarnessTerminalGeneration {
    pub(crate) fn into_persisted(self) -> HarnessTerminalGenerationReceipt {
        self.0
    }
}

pub(crate) struct VerifiedHarnessProviderLaunch(HarnessProviderLaunchReceipt);

impl VerifiedHarnessProviderLaunch {
    pub(crate) fn into_persisted(self) -> HarnessProviderLaunchReceipt {
        self.0
    }
}

/// Provider selection that retains the exact opened executable until the launch
/// handoff has completed. The launch callback must not return until the child has
/// opened this `/proc` handle, so credentials can never be handed to a later path
/// replacement. Only a successful handoff can produce a journalable receipt.
pub(crate) struct OpenedHarnessProviderLaunch {
    receipt: HarnessProviderLaunchReceipt,
    executable: std::fs::File,
    provenance: Vec<ProviderPathBinding>,
    opened_path: PathBuf,
}

struct OneShotCredentialHandoff {
    directory: PathBuf,
    wrapper: PathBuf,
    credentials: PathBuf,
}

impl OneShotCredentialHandoff {
    #[cfg(unix)]
    fn create(
        provider_alias: &std::path::Path,
        command_tail: &str,
        env: &[(String, String)],
    ) -> Result<Self, LaunchAttestationError> {
        use std::io::Write;
        use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};

        let directory = std::env::temp_dir().join(format!(
            "t-hub-provider-credentials-{}",
            uuid::Uuid::new_v4().simple()
        ));
        std::fs::create_dir(&directory).map_err(|_| LaunchAttestationError::UnreadableEvidence)?;
        let wrapper = directory.join("launch");
        let credentials = directory.join("credentials");
        let handoff = Self {
            directory,
            wrapper,
            credentials,
        };
        std::fs::set_permissions(&handoff.directory, std::fs::Permissions::from_mode(0o700))
            .map_err(|_| LaunchAttestationError::UnreadableEvidence)?;
        let mut credential_body = String::new();
        let mut keys = std::collections::HashSet::new();
        for (key, value) in env {
            if key.is_empty()
                || key.contains('=')
                || key.chars().any(char::is_whitespace)
                || key.chars().any(char::is_control)
                || !keys.insert(key.as_str())
            {
                return Err(LaunchAttestationError::CredentialHandoffFailed);
            }
            credential_body.push_str("export ");
            credential_body.push_str(key);
            credential_body.push('=');
            credential_body.push_str(&sh_single_quote(value));
            credential_body.push('\n');
        }
        let mut credential_file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&handoff.credentials)
            .map_err(|_| LaunchAttestationError::CredentialHandoffFailed)?;
        credential_file
            .write_all(credential_body.as_bytes())
            .and_then(|_| credential_file.sync_all())
            .map_err(|_| LaunchAttestationError::CredentialHandoffFailed)?;
        let wrapper_body = format!(
            "#!/bin/sh\nexec 3<{} || exit 125\nrm -f -- {} {} || exit 126\n. /proc/self/fd/3 || exit 127\nexec {}{}\n",
            sh_single_quote(&handoff.credentials.to_string_lossy()),
            sh_single_quote(&handoff.credentials.to_string_lossy()),
            sh_single_quote(&handoff.wrapper.to_string_lossy()),
            sh_single_quote(&provider_alias.to_string_lossy()),
            command_tail,
        );
        let mut wrapper_file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o700)
            .open(&handoff.wrapper)
            .map_err(|_| LaunchAttestationError::CredentialHandoffFailed)?;
        wrapper_file
            .write_all(wrapper_body.as_bytes())
            .and_then(|_| wrapper_file.sync_all())
            .map_err(|_| LaunchAttestationError::CredentialHandoffFailed)?;
        Ok(handoff)
    }

    fn consumed(&self) -> bool {
        !self.wrapper.exists() && !self.credentials.exists()
    }
}

impl Drop for OneShotCredentialHandoff {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.wrapper);
        let _ = std::fs::remove_file(&self.credentials);
        let _ = std::fs::remove_dir(&self.directory);
    }
}

impl OpenedHarnessProviderLaunch {
    #[cfg(test)]
    pub(crate) fn test_receipt(&self) -> VerifiedHarnessProviderLaunch {
        VerifiedHarnessProviderLaunch(self.receipt.clone())
    }

    #[cfg(test)]
    pub(crate) fn test_opened_path(&self) -> &std::path::Path {
        &self.opened_path
    }

    #[cfg(unix)]
    pub(crate) fn launch_in_exact_tmux_pane(
        self,
        expected: Harness,
        tmux_target: &str,
        cwd: &str,
        trusted_provider_command: &str,
        env: &[(String, String)],
        failure_generation: &mut HarnessTerminalGenerationReceipt,
    ) -> Result<VerifiedHarnessProviderProcessLaunch, LaunchAttestationError> {
        use std::os::unix::fs::{symlink, PermissionsExt};

        let provider = expected.as_provider();
        let initial_generation = crate::tmux::pane_generation_exact(tmux_target)
            .map_err(|_| LaunchAttestationError::UnreadableEvidence)?;
        if failure_generation.tmux_target != tmux_target
            || incumbent_terminal_generation(failure_generation) != initial_generation
        {
            return Err(LaunchAttestationError::TerminalChanged);
        }
        let command_tail = trusted_provider_command
            .strip_prefix(provider)
            .filter(|tail| tail.is_empty() || tail.starts_with(char::is_whitespace))
            .ok_or(LaunchAttestationError::WrongProvider)?;
        let alias_directory = std::env::temp_dir().join(format!(
            "t-hub-provider-handoff-{}",
            uuid::Uuid::new_v4().simple()
        ));
        std::fs::create_dir(&alias_directory)
            .map_err(|_| LaunchAttestationError::UnreadableEvidence)?;
        std::fs::set_permissions(&alias_directory, std::fs::Permissions::from_mode(0o700))
            .map_err(|_| LaunchAttestationError::UnreadableEvidence)?;
        let alias = alias_directory.join(provider);
        symlink(&self.opened_path, &alias)
            .map_err(|_| LaunchAttestationError::UnreadableEvidence)?;
        let alias_guard = ProviderLaunchAlias {
            directory: alias_directory,
            alias: alias.clone(),
        };
        if provider_file_identity(&alias)?
            != provider_file_identity_from_open_file(&self.executable)?
        {
            return Err(LaunchAttestationError::ProcessChanged);
        }
        for binding in &self.provenance {
            binding.revalidate()?;
        }
        let credential_handoff = OneShotCredentialHandoff::create(&alias, command_tail, env)?;
        let launch_command = format!(
            "exec {}",
            sh_single_quote(&credential_handoff.wrapper.to_string_lossy())
        );
        let respawn = match crate::tmux::respawn_pane_exact(tmux_target, cwd, &launch_command) {
            Ok(respawn) => respawn,
            Err(_) => return Err(LaunchAttestationError::UnreadableEvidence),
        };
        *failure_generation = terminal_generation_receipt_from_pane(tmux_target, respawn.after);
        let consumed_deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        while !credential_handoff.consumed() {
            if crate::tmux::pane_generation_exact(tmux_target)
                .is_ok_and(|generation| generation != respawn.after)
            {
                return Err(LaunchAttestationError::TerminalChanged);
            }
            if std::time::Instant::now() >= consumed_deadline {
                return Err(LaunchAttestationError::CredentialHandoffFailed);
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        let mut observed = None;
        for _ in 0..20 {
            match observe_harness_process(tmux_target) {
                Ok(evidence) if provider_arguments(&evidence, expected).is_ok() => {
                    observed = Some(evidence);
                    break;
                }
                Ok(_) | Err(LaunchAttestationError::UnreadableEvidence) => {
                    std::thread::sleep(std::time::Duration::from_millis(50));
                }
                Err(error) => return Err(error),
            }
        }
        let first = observed.ok_or(LaunchAttestationError::UnreadableEvidence)?;
        let second = observe_harness_process(tmux_target)?;
        let stable = confirm_stable_launch_baseline(&first, &second)?;
        let terminal = &stable.terminal;
        if terminal.session_name != tmux_target
            || terminal.session_id != respawn.after.session_id
            || terminal.session_created != respawn.after.session_created
            || terminal.window_id != respawn.after.window_id
            || terminal.pane_id != respawn.after.pane_id
            || terminal.pane_pid != respawn.after.pane_pid
            || provider_file_identity(&alias)?
                != provider_file_identity_from_open_file(&self.executable)?
        {
            return Err(LaunchAttestationError::TerminalChanged);
        }
        for binding in &self.provenance {
            binding.revalidate()?;
        }
        Ok(VerifiedHarnessProviderProcessLaunch {
            provider: self.receipt,
            terminal: terminal_generation_receipt(tmux_target, &stable),
            evidence: stable,
            _executable: self.executable,
            _provenance: self.provenance,
            _alias: alias_guard,
        })
    }
}

#[cfg(unix)]
fn incumbent_terminal_generation(
    receipt: &HarnessTerminalGenerationReceipt,
) -> crate::tmux::PaneGeneration {
    crate::tmux::PaneGeneration {
        session_id: receipt.session_id,
        session_created: receipt.session_created,
        window_id: receipt.window_id,
        pane_id: receipt.pane_id,
        pane_pid: receipt.pane_pid,
    }
}

#[cfg(unix)]
pub(crate) fn terminal_generation_receipt_from_pane(
    tmux_target: &str,
    pane: crate::tmux::PaneGeneration,
) -> HarnessTerminalGenerationReceipt {
    HarnessTerminalGenerationReceipt {
        tmux_target: tmux_target.to_string(),
        session_id: pane.session_id,
        session_created: pane.session_created,
        window_id: pane.window_id,
        pane_id: pane.pane_id,
        pane_pid: pane.pane_pid,
    }
}

#[cfg(unix)]
struct ProviderLaunchAlias {
    directory: PathBuf,
    alias: PathBuf,
}

#[cfg(unix)]
impl Drop for ProviderLaunchAlias {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.alias);
        let _ = std::fs::remove_dir(&self.directory);
    }
}

/// Non-forgeable result of the concrete provider child handoff.
///
/// The exact executable, provenance chain, and private argv alias remain open
/// until the trusted runner has persisted spawn evidence and completed runtime
/// attestation.
#[cfg(unix)]
pub(crate) struct VerifiedHarnessProviderProcessLaunch {
    provider: HarnessProviderLaunchReceipt,
    terminal: HarnessTerminalGenerationReceipt,
    evidence: HarnessProcessEvidence,
    _executable: std::fs::File,
    _provenance: Vec<ProviderPathBinding>,
    _alias: ProviderLaunchAlias,
}

#[cfg(unix)]
impl VerifiedHarnessProviderProcessLaunch {
    pub(crate) fn spawn_receipts(
        &self,
    ) -> (
        VerifiedHarnessProviderLaunch,
        VerifiedHarnessTerminalGeneration,
    ) {
        (
            VerifiedHarnessProviderLaunch(self.provider.clone()),
            VerifiedHarnessTerminalGeneration(self.terminal.clone()),
        )
    }

    pub(crate) fn evidence(&self) -> &HarnessProcessEvidence {
        &self.evidence
    }
}

pub(crate) struct VerifiedHarnessRuntimeReceipt(HarnessRuntimePolicyReceipt);

impl VerifiedHarnessRuntimeReceipt {
    pub(crate) fn into_persisted(self) -> HarnessRuntimePolicyReceipt {
        self.0
    }

    pub(crate) fn persisted(&self) -> &HarnessRuntimePolicyReceipt {
        &self.0
    }
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
                session_name: "th_cor-candidate-2".to_string(),
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
            resolved_process_executable: if PathBuf::from(argv[0]).is_absolute() {
                argv[0].to_string()
            } else {
                format!("/usr/bin/{}", executable_name(argv[0]))
            },
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

pub fn terminal_generation_receipt(
    _requested_target: &str,
    evidence: &HarnessProcessEvidence,
) -> HarnessTerminalGenerationReceipt {
    HarnessTerminalGenerationReceipt {
        tmux_target: evidence.terminal.session_name.clone(),
        session_id: evidence.terminal.session_id,
        session_created: evidence.terminal.session_created,
        window_id: evidence.terminal.window_id,
        pane_id: evidence.terminal.pane_id,
        pane_pid: evidence.terminal.pane_pid,
    }
}

pub(crate) fn observe_terminal_generation_receipt(
    tmux_target: &str,
) -> Result<VerifiedHarnessTerminalGeneration, LaunchAttestationError> {
    observe_harness_process(tmux_target).map(|evidence| {
        VerifiedHarnessTerminalGeneration(terminal_generation_receipt(tmux_target, &evidence))
    })
}

#[cfg(test)]
pub(crate) fn test_verified_terminal_generation(
    tmux_target: &str,
    evidence: &HarnessProcessEvidence,
) -> VerifiedHarnessTerminalGeneration {
    let mut evidence = evidence.clone();
    evidence.terminal.session_name = tmux_target.to_string();
    VerifiedHarnessTerminalGeneration(terminal_generation_receipt(tmux_target, &evidence))
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
    WrongModel,
    WrongReasoningEffort,
    ConflictingPermission,
    MalformedPermission,
    TerminalChanged,
    ProcessChanged,
    AncestryChanged,
    UntrustedUserInstall,
    CredentialHandoffFailed,
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
            Self::WrongModel => "provider-native process evidence has the wrong model",
            Self::WrongReasoningEffort => {
                "provider-native process evidence has the wrong reasoning effort"
            }
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
            Self::UntrustedUserInstall => {
                "user-managed provider install is not launch authority; provision a protected package snapshot and root-owned /etc/t-hub/provider-catalog.json mapping"
            }
            Self::CredentialHandoffFailed => {
                "provider launch credentials were not consumed by the exact one-shot child handoff"
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

    /// Interactive fresh or resume launch with every runtime-policy axis stated
    /// in provider-native argv. Recovery uses this surface so post-launch
    /// attestation verifies effective policy instead of comparing desired data.
    fn interactive_argv_with_runtime_policy(
        &self,
        prompt: &str,
        resume: Option<&str>,
        model: &str,
        reasoning_effort: &str,
        perm: PermMode,
    ) -> String;

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
    const SCRIPT: &str = r#"
set -eu
tmux_socket=$1
tmux_target=$2
pane=$(tmux -L "$tmux_socket" list-panes -t "$tmux_target" -F '#{session_name} #{session_id} #{session_created} #{window_id} #{pane_id} #{pane_pid}')
set -- $pane
 [ "$#" -eq 6 ]
session_name=$1
[ "$session_name" = "$tmux_target" ]
session_id=$2
session_created=$3
window_id=$4
pane_id=$5
pane_pid=$6
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
resolved_process_executable=$(readlink -f "/proc/$foreground_pid/exe")
case "$resolved_process_executable" in /*) ;; *) exit 26;; esac
[ "${#resolved_process_executable}" -le 16384 ]
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
pane_after=$(tmux -L "$tmux_socket" list-panes -t "$tmux_target" -F '#{session_name} #{session_id} #{session_created} #{window_id} #{pane_id} #{pane_pid}')
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
printf 'THPA5\n%s\n%s\n%s\n%s\n%s\n%s\n%s\n%s\n%s\n%s\n%s\n%s\n' \
    "$session_name" "$session_id" "$session_created" "$window_id" "$pane_id" "$pane_pid" \
    "$foreground_pid" "$start_ticks" "$executable_device" "$executable_inode" \
    "$resolved_process_executable" "$ancestry"
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
    let mut fields = bytes.splitn(14, |byte| *byte == b'\n');
    if fields.next() != Some(b"THPA5".as_slice()) {
        return Err(LaunchAttestationError::UnreadableEvidence);
    }
    let terminal = TerminalGeneration {
        session_name: std::str::from_utf8(
            fields
                .next()
                .ok_or(LaunchAttestationError::UnreadableEvidence)?,
        )
        .ok()
        .filter(|name| {
            !name.is_empty()
                && name.len() <= 128
                && name.bytes().all(|byte| {
                    byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b':')
                })
        })
        .ok_or(LaunchAttestationError::UnreadableEvidence)?
        .to_string(),
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
    let resolved_process_executable = std::str::from_utf8(
        fields
            .next()
            .ok_or(LaunchAttestationError::UnreadableEvidence)?,
    )
    .ok()
    .filter(|path| {
        !path.is_empty()
            && path.len() <= 16_384
            && PathBuf::from(path).is_absolute()
            && !path.chars().any(char::is_control)
    })
    .ok_or(LaunchAttestationError::UnreadableEvidence)?
    .to_string();
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
        resolved_process_executable,
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

/// Return the exact provider executable token attested in the foreground argv.
/// This is non-secret and is used to resolve installed-runtime evidence without
/// trusting a separate PATH lookup to identify what the pane is running.
pub(crate) fn provider_executable_path(
    evidence: &HarnessProcessEvidence,
    expected: Harness,
) -> Result<&str, LaunchAttestationError> {
    let (provider, provider_index) =
        process_provider(&evidence.argv).ok_or(LaunchAttestationError::WrapperObscured)?;
    if provider != expected {
        return Err(LaunchAttestationError::WrongProvider);
    }
    evidence
        .argv
        .get(provider_index)
        .map(String::as_str)
        .ok_or(LaunchAttestationError::UnreadableEvidence)
}

/// Resolve the exact provider executable represented by foreground evidence.
/// A native provider launched through PATH is bound to `/proc/<pid>/exe`; a
/// script provider behind an accepted node-style runtime must name an absolute
/// script path. Relative wrapper arguments fail closed.
pub(crate) fn resolved_provider_executable_path(
    evidence: &HarnessProcessEvidence,
    expected: Harness,
) -> Result<PathBuf, LaunchAttestationError> {
    let (provider, provider_index) =
        process_provider(&evidence.argv).ok_or(LaunchAttestationError::WrapperObscured)?;
    if provider != expected {
        return Err(LaunchAttestationError::WrongProvider);
    }
    if provider_index == 0 {
        return Ok(PathBuf::from(&evidence.resolved_process_executable));
    }
    let provider_path = evidence
        .argv
        .get(provider_index)
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
        .ok_or(LaunchAttestationError::WrapperObscured)?;
    std::fs::canonicalize(provider_path).map_err(|_| LaunchAttestationError::UnreadableEvidence)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ProviderFileIdentity {
    device: u64,
    inode: u64,
}

struct ProviderPathBinding {
    path: PathBuf,
    file: std::fs::File,
    identity: ProviderFileIdentity,
}

impl ProviderPathBinding {
    fn revalidate(&self) -> Result<(), LaunchAttestationError> {
        if provider_path_identity(&self.path)? != self.identity
            || provider_path_identity_from_open_file(&self.file)? != self.identity
        {
            return Err(LaunchAttestationError::ProcessChanged);
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ProviderInstallLayout {
    launcher_directory: PathBuf,
    package_root: PathBuf,
    require_protected_ownership: bool,
}

const PROTECTED_PROVIDER_CATALOG_PATH: &str = "/etc/t-hub/provider-catalog.json";
const MAX_PROTECTED_PROVIDER_CATALOG_BYTES: u64 = 64 * 1024;
const MAX_PROTECTED_PROVIDER_CATALOG_ENTRIES: usize = 8;

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ProtectedProviderCatalog {
    schema_version: u32,
    providers: Vec<ProtectedProviderCatalogEntry>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ProtectedProviderCatalogEntry {
    provider: String,
    source_launcher: PathBuf,
    protected_launcher: PathBuf,
    protected_package_root: PathBuf,
}

#[cfg(unix)]
fn provider_file_identity(
    path: &std::path::Path,
) -> Result<ProviderFileIdentity, LaunchAttestationError> {
    use std::os::unix::fs::MetadataExt;

    let metadata =
        std::fs::metadata(path).map_err(|_| LaunchAttestationError::UnreadableEvidence)?;
    if !metadata.is_file() || metadata.ino() == 0 {
        return Err(LaunchAttestationError::UnreadableEvidence);
    }
    Ok(ProviderFileIdentity {
        device: metadata.dev(),
        inode: metadata.ino(),
    })
}

#[cfg(unix)]
fn provider_path_identity(
    path: &std::path::Path,
) -> Result<ProviderFileIdentity, LaunchAttestationError> {
    use std::os::unix::fs::MetadataExt;

    let metadata =
        std::fs::metadata(path).map_err(|_| LaunchAttestationError::UnreadableEvidence)?;
    if metadata.ino() == 0 {
        return Err(LaunchAttestationError::UnreadableEvidence);
    }
    Ok(ProviderFileIdentity {
        device: metadata.dev(),
        inode: metadata.ino(),
    })
}

pub(crate) fn observe_provider_launch_receipt(
    provider_executable: &std::path::Path,
    expected: Harness,
) -> Result<OpenedHarnessProviderLaunch, LaunchAttestationError> {
    match observe_provider_launch_receipt_with_layouts(
        provider_executable,
        expected,
        &trusted_provider_install_layouts(),
        || {},
    ) {
        Err(LaunchAttestationError::UntrustedUserInstall) => {
            observe_provisioned_provider_launch_receipt(
                provider_executable,
                expected,
                std::path::Path::new(PROTECTED_PROVIDER_CATALOG_PATH),
                &std::env::var_os("HOME")
                    .map(PathBuf::from)
                    .ok_or(LaunchAttestationError::UntrustedUserInstall)?,
                true,
            )
        }
        result => result,
    }
}

fn observe_provisioned_provider_launch_receipt(
    source_launcher: &std::path::Path,
    expected: Harness,
    catalog_path: &std::path::Path,
    user_home: &std::path::Path,
    require_protected_ownership: bool,
) -> Result<OpenedHarnessProviderLaunch, LaunchAttestationError> {
    let catalog_bindings = bind_provider_path_chain(catalog_path, require_protected_ownership)
        .map_err(|_| LaunchAttestationError::UntrustedUserInstall)?;
    for binding in &catalog_bindings {
        binding
            .revalidate()
            .map_err(|_| LaunchAttestationError::UntrustedUserInstall)?;
    }
    let catalog_file = catalog_bindings
        .last()
        .ok_or(LaunchAttestationError::UntrustedUserInstall)?
        .file
        .try_clone()
        .map_err(|_| LaunchAttestationError::UntrustedUserInstall)?;
    let catalog = read_protected_provider_catalog(&catalog_file)?;
    let mut matching = catalog.providers.iter().filter(|entry| {
        entry.provider == expected.as_provider() && entry.source_launcher == source_launcher
    });
    let entry = matching
        .next()
        .filter(|_| matching.next().is_none())
        .ok_or(LaunchAttestationError::UntrustedUserInstall)?;
    if !entry.source_launcher.is_absolute()
        || entry
            .source_launcher
            .parent()
            .is_none_or(|parent| !user_managed_provider_launcher_under(parent, user_home))
        || entry
            .source_launcher
            .file_name()
            .and_then(|name| name.to_str())
            .map(executable_name)
            != Some(expected.as_provider())
        || !entry.protected_launcher.is_absolute()
        || !entry.protected_package_root.is_absolute()
    {
        return Err(LaunchAttestationError::UntrustedUserInstall);
    }
    let launcher_directory = entry
        .protected_launcher
        .parent()
        .and_then(|parent| std::fs::canonicalize(parent).ok())
        .ok_or(LaunchAttestationError::UntrustedUserInstall)?;
    let package_root = std::fs::canonicalize(&entry.protected_package_root)
        .map_err(|_| LaunchAttestationError::UntrustedUserInstall)?;
    let layout = ProviderInstallLayout {
        launcher_directory,
        package_root,
        require_protected_ownership,
    };
    let mut opened = observe_provider_launch_receipt_with_layouts(
        &entry.protected_launcher,
        expected,
        &[layout],
        || {},
    )
    .map_err(|_| LaunchAttestationError::UntrustedUserInstall)?;
    opened.provenance.extend(catalog_bindings);
    for binding in &opened.provenance {
        binding.revalidate()?;
    }
    Ok(opened)
}

fn read_protected_provider_catalog(
    file: &std::fs::File,
) -> Result<ProtectedProviderCatalog, LaunchAttestationError> {
    use std::io::Read;

    let mut bytes = Vec::new();
    file.try_clone()
        .and_then(|mut file| {
            file.by_ref()
                .take(MAX_PROTECTED_PROVIDER_CATALOG_BYTES + 1)
                .read_to_end(&mut bytes)
        })
        .map_err(|_| LaunchAttestationError::UntrustedUserInstall)?;
    if bytes.is_empty() || bytes.len() as u64 > MAX_PROTECTED_PROVIDER_CATALOG_BYTES {
        return Err(LaunchAttestationError::UntrustedUserInstall);
    }
    let catalog: ProtectedProviderCatalog =
        serde_json::from_slice(&bytes).map_err(|_| LaunchAttestationError::UntrustedUserInstall)?;
    if catalog.schema_version != 1
        || catalog.providers.is_empty()
        || catalog.providers.len() > MAX_PROTECTED_PROVIDER_CATALOG_ENTRIES
    {
        return Err(LaunchAttestationError::UntrustedUserInstall);
    }
    let mut providers = std::collections::HashSet::new();
    let mut sources = std::collections::HashSet::new();
    if catalog.providers.iter().any(|entry| {
        !matches!(entry.provider.as_str(), "codex" | "claude")
            || !providers.insert(entry.provider.as_str())
            || !sources.insert(entry.source_launcher.as_path())
    }) {
        return Err(LaunchAttestationError::UntrustedUserInstall);
    }
    Ok(catalog)
}

fn observe_provider_launch_receipt_with_layouts<F>(
    provider_executable: &std::path::Path,
    expected: Harness,
    layouts: &[ProviderInstallLayout],
    after_open: F,
) -> Result<OpenedHarnessProviderLaunch, LaunchAttestationError>
where
    F: FnOnce(),
{
    if provider_executable
        .file_name()
        .and_then(|name| name.to_str())
        .map(executable_name)
        != Some(expected.as_provider())
    {
        return Err(LaunchAttestationError::WrongProvider);
    }
    let launcher_directory = provider_executable
        .parent()
        .and_then(|parent| std::fs::canonicalize(parent).ok())
        .ok_or(LaunchAttestationError::UnreadableEvidence)?;
    let layout = layouts
        .iter()
        .find(|layout| layout.launcher_directory == launcher_directory)
        .ok_or_else(|| {
            if user_managed_provider_launcher(&launcher_directory) {
                LaunchAttestationError::UntrustedUserInstall
            } else {
                LaunchAttestationError::WrongProvider
            }
        })?;
    let launcher_is_symlink = std::fs::symlink_metadata(provider_executable)
        .map_err(|_| LaunchAttestationError::UnreadableEvidence)?
        .file_type()
        .is_symlink();
    let opened = std::fs::File::open(provider_executable)
        .map_err(|_| LaunchAttestationError::UnreadableEvidence)?;
    let identity = provider_file_identity_from_open_file(&opened)?;
    let executable = canonical_open_file_path(&opened)?;
    after_open();
    let current_executable = std::fs::canonicalize(provider_executable)
        .map_err(|_| LaunchAttestationError::UnreadableEvidence)?;
    if current_executable != executable || provider_file_identity(provider_executable)? != identity
    {
        return Err(LaunchAttestationError::ProcessChanged);
    }
    if !launcher_is_symlink {
        return Err(LaunchAttestationError::WrongProvider);
    }
    let provenance = validate_provider_package_provenance(layout, &executable, expected)?;
    for binding in &provenance {
        binding.revalidate()?;
    }
    let opened_path = open_file_descriptor_path(&opened);
    Ok(OpenedHarnessProviderLaunch {
        receipt: HarnessProviderLaunchReceipt {
            resolved_provider_executable: executable.to_string_lossy().into_owned(),
            provider_executable_device: identity.device,
            provider_executable_inode: identity.inode,
        },
        executable: opened,
        provenance,
        opened_path,
    })
}

fn user_managed_provider_launcher(launcher_directory: &std::path::Path) -> bool {
    let Some(home) = std::env::var_os("HOME").map(PathBuf::from) else {
        return false;
    };
    user_managed_provider_launcher_under(launcher_directory, &home)
}

fn user_managed_provider_launcher_under(
    launcher_directory: &std::path::Path,
    home: &std::path::Path,
) -> bool {
    let candidates = [
        home.join(".bun/bin"),
        home.join(".npm-global/bin"),
        home.join(".local/bin"),
    ];
    candidates
        .iter()
        .any(|candidate| candidate == launcher_directory)
        || launcher_directory.starts_with(home.join(".nvm/versions/node"))
}

fn provider_package_relative_root(expected: Harness) -> &'static std::path::Path {
    match expected {
        Harness::Codex => std::path::Path::new("@openai/codex"),
        Harness::Claude => std::path::Path::new("@anthropic-ai/claude-code"),
    }
}

fn provider_package_bin_relative_path(expected: Harness) -> &'static str {
    match expected {
        Harness::Codex => "bin/codex.js",
        Harness::Claude => "bin/claude.exe",
    }
}

fn validate_provider_package_provenance(
    layout: &ProviderInstallLayout,
    executable: &std::path::Path,
    expected: Harness,
) -> Result<Vec<ProviderPathBinding>, LaunchAttestationError> {
    let package_root = layout
        .package_root
        .join(provider_package_relative_root(expected));
    if executable != package_root.join(provider_package_bin_relative_path(expected)) {
        return Err(LaunchAttestationError::WrongProvider);
    }
    let manifest_path = package_root.join("package.json");
    let manifest_file = std::fs::File::open(&manifest_path)
        .map_err(|_| LaunchAttestationError::UnreadableEvidence)?;
    let manifest_bytes = {
        use std::io::Read;

        let mut bytes = Vec::new();
        manifest_file
            .try_clone()
            .and_then(|mut file| file.by_ref().take(64 * 1024 + 1).read_to_end(&mut bytes))
            .map_err(|_| LaunchAttestationError::UnreadableEvidence)?;
        bytes
    };
    if manifest_bytes.is_empty() || manifest_bytes.len() > 64 * 1024 {
        return Err(LaunchAttestationError::UnreadableEvidence);
    }
    let manifest: serde_json::Value = serde_json::from_slice(&manifest_bytes)
        .map_err(|_| LaunchAttestationError::UnreadableEvidence)?;
    let expected_package = match expected {
        Harness::Codex => "@openai/codex",
        Harness::Claude => "@anthropic-ai/claude-code",
    };
    if manifest.get("name").and_then(serde_json::Value::as_str) != Some(expected_package)
        || manifest
            .get("bin")
            .and_then(serde_json::Value::as_object)
            .and_then(|bin| bin.get(expected.as_provider()))
            .and_then(serde_json::Value::as_str)
            != Some(provider_package_bin_relative_path(expected))
    {
        return Err(LaunchAttestationError::WrongProvider);
    }
    let mut bindings = bind_provider_path_chain(
        &layout.launcher_directory,
        layout.require_protected_ownership,
    )?;
    bindings.extend(bind_provider_path_chain(
        &manifest_path,
        layout.require_protected_ownership,
    )?);
    bindings.extend(bind_provider_path_chain(
        executable,
        layout.require_protected_ownership,
    )?);
    bindings.push(ProviderPathBinding {
        path: manifest_path,
        identity: provider_path_identity_from_open_file(&manifest_file)?,
        file: manifest_file,
    });
    Ok(bindings)
}

#[cfg(unix)]
fn validate_protected_provider_open_file(
    file: &std::fs::File,
) -> Result<(), LaunchAttestationError> {
    use std::os::unix::fs::{MetadataExt, PermissionsExt};

    let metadata = file
        .metadata()
        .map_err(|_| LaunchAttestationError::UnreadableEvidence)?;
    if metadata.uid() != 0 || metadata.permissions().mode() & 0o022 != 0 {
        return Err(LaunchAttestationError::WrongProvider);
    }
    Ok(())
}

#[cfg(not(unix))]
fn validate_protected_provider_open_file(
    _file: &std::fs::File,
) -> Result<(), LaunchAttestationError> {
    Err(LaunchAttestationError::UnreadableEvidence)
}

fn bind_provider_path_chain(
    path: &std::path::Path,
    require_protected_ownership: bool,
) -> Result<Vec<ProviderPathBinding>, LaunchAttestationError> {
    use std::path::Component;

    if !path.is_absolute() {
        return Err(LaunchAttestationError::WrongProvider);
    }
    let mut current = PathBuf::from("/");
    let mut bindings = Vec::new();
    let root =
        std::fs::File::open(&current).map_err(|_| LaunchAttestationError::UnreadableEvidence)?;
    if require_protected_ownership {
        validate_protected_provider_open_file(&root)?;
    }
    bindings.push(ProviderPathBinding {
        path: current.clone(),
        identity: provider_path_identity_from_open_file(&root)?,
        file: root,
    });
    for component in path.components() {
        let Component::Normal(component) = component else {
            continue;
        };
        current.push(component);
        if std::fs::symlink_metadata(&current)
            .map_err(|_| LaunchAttestationError::UnreadableEvidence)?
            .file_type()
            .is_symlink()
        {
            return Err(LaunchAttestationError::WrongProvider);
        }
        let file = std::fs::File::open(&current)
            .map_err(|_| LaunchAttestationError::UnreadableEvidence)?;
        if require_protected_ownership {
            validate_protected_provider_open_file(&file)?;
        }
        bindings.push(ProviderPathBinding {
            path: current.clone(),
            identity: provider_path_identity_from_open_file(&file)?,
            file,
        });
    }
    Ok(bindings)
}

fn trusted_provider_install_layouts() -> Vec<ProviderInstallLayout> {
    let layouts = [
        ("/usr/bin", "/usr/lib/node_modules"),
        ("/usr/local/bin", "/usr/local/lib/node_modules"),
        ("/opt/homebrew/bin", "/opt/homebrew/lib/node_modules"),
    ]
    .into_iter()
    .map(|(launcher, packages)| ProviderInstallLayout {
        launcher_directory: PathBuf::from(launcher),
        package_root: PathBuf::from(packages),
        require_protected_ownership: true,
    })
    .collect::<Vec<_>>();
    layouts
        .into_iter()
        .filter_map(|layout| {
            Some(ProviderInstallLayout {
                launcher_directory: std::fs::canonicalize(layout.launcher_directory).ok()?,
                package_root: std::fs::canonicalize(layout.package_root).ok()?,
                require_protected_ownership: layout.require_protected_ownership,
            })
        })
        .collect()
}

#[cfg(unix)]
fn provider_file_identity_from_open_file(
    file: &std::fs::File,
) -> Result<ProviderFileIdentity, LaunchAttestationError> {
    use std::os::unix::fs::MetadataExt;

    let metadata = file
        .metadata()
        .map_err(|_| LaunchAttestationError::UnreadableEvidence)?;
    if !metadata.is_file() || metadata.ino() == 0 {
        return Err(LaunchAttestationError::UnreadableEvidence);
    }
    Ok(ProviderFileIdentity {
        device: metadata.dev(),
        inode: metadata.ino(),
    })
}

#[cfg(unix)]
fn provider_path_identity_from_open_file(
    file: &std::fs::File,
) -> Result<ProviderFileIdentity, LaunchAttestationError> {
    use std::os::unix::fs::MetadataExt;

    let metadata = file
        .metadata()
        .map_err(|_| LaunchAttestationError::UnreadableEvidence)?;
    if metadata.ino() == 0 {
        return Err(LaunchAttestationError::UnreadableEvidence);
    }
    Ok(ProviderFileIdentity {
        device: metadata.dev(),
        inode: metadata.ino(),
    })
}

#[cfg(unix)]
fn canonical_open_file_path(file: &std::fs::File) -> Result<PathBuf, LaunchAttestationError> {
    use std::os::fd::AsRawFd;

    std::fs::canonicalize(format!(
        "/proc/{}/fd/{}",
        std::process::id(),
        file.as_raw_fd()
    ))
    .map_err(|_| LaunchAttestationError::UnreadableEvidence)
}

#[cfg(unix)]
fn open_file_descriptor_path(file: &std::fs::File) -> PathBuf {
    use std::os::fd::AsRawFd;

    PathBuf::from(format!(
        "/proc/{}/fd/{}",
        std::process::id(),
        file.as_raw_fd()
    ))
}

#[cfg(not(unix))]
fn provider_file_identity_from_open_file(
    _file: &std::fs::File,
) -> Result<ProviderFileIdentity, LaunchAttestationError> {
    Err(LaunchAttestationError::UnreadableEvidence)
}

#[cfg(not(unix))]
fn provider_path_identity_from_open_file(
    _file: &std::fs::File,
) -> Result<ProviderFileIdentity, LaunchAttestationError> {
    Err(LaunchAttestationError::UnreadableEvidence)
}

#[cfg(not(unix))]
fn provider_path_identity(
    _path: &std::path::Path,
) -> Result<ProviderFileIdentity, LaunchAttestationError> {
    Err(LaunchAttestationError::UnreadableEvidence)
}

#[cfg(not(unix))]
fn canonical_open_file_path(_file: &std::fs::File) -> Result<PathBuf, LaunchAttestationError> {
    Err(LaunchAttestationError::UnreadableEvidence)
}

#[cfg(not(unix))]
fn open_file_descriptor_path(_file: &std::fs::File) -> PathBuf {
    PathBuf::new()
}

#[cfg(test)]
fn test_provider_install_layout(root: &std::path::Path) -> ProviderInstallLayout {
    ProviderInstallLayout {
        launcher_directory: root.join("bin"),
        package_root: root.join("lib/node_modules"),
        require_protected_ownership: false,
    }
}

#[cfg(test)]
pub(crate) fn test_observe_provider_launch_receipt(
    provider_executable: &std::path::Path,
    expected: Harness,
    installation_root: &std::path::Path,
) -> Result<OpenedHarnessProviderLaunch, LaunchAttestationError> {
    observe_provider_launch_receipt_with_layouts(
        provider_executable,
        expected,
        &[test_provider_install_layout(installation_root)],
        || {},
    )
}

#[cfg(test)]
pub(crate) fn test_verified_provider_launch(
    resolved_provider_executable: &str,
    provider_executable_device: u64,
    provider_executable_inode: u64,
) -> VerifiedHarnessProviderLaunch {
    VerifiedHarnessProviderLaunch(HarnessProviderLaunchReceipt {
        resolved_provider_executable: resolved_provider_executable.to_string(),
        provider_executable_device,
        provider_executable_inode,
    })
}

#[cfg(not(unix))]
fn provider_file_identity(
    _path: &std::path::Path,
) -> Result<ProviderFileIdentity, LaunchAttestationError> {
    Err(LaunchAttestationError::UnreadableEvidence)
}

#[cfg(unix)]
fn probe_provider_version(
    executable: &std::path::Path,
) -> Result<(String, ProviderFileIdentity), LaunchAttestationError> {
    use std::os::fd::AsRawFd;
    use std::os::unix::fs::MetadataExt;

    let executable_file =
        std::fs::File::open(executable).map_err(|_| LaunchAttestationError::UnreadableEvidence)?;
    let metadata = executable_file
        .metadata()
        .map_err(|_| LaunchAttestationError::UnreadableEvidence)?;
    if !metadata.is_file() || metadata.ino() == 0 {
        return Err(LaunchAttestationError::UnreadableEvidence);
    }
    let file_identity = ProviderFileIdentity {
        device: metadata.dev(),
        inode: metadata.ino(),
    };
    let opened_executable = format!(
        "/proc/{}/fd/{}",
        std::process::id(),
        executable_file.as_raw_fd()
    );
    let mut command = Command::new(opened_executable);
    command.arg("--version");
    let output = crate::bounded_exec::output_with_timeout(command, Duration::from_secs(3))
        .map_err(|_| LaunchAttestationError::UnreadableEvidence)?;
    if !output.status.success()
        || output.stdout.is_empty()
        || !output.stderr.is_empty()
        || output.stdout.len() > 4096
    {
        return Err(LaunchAttestationError::UnreadableEvidence);
    }
    let executable_version = std::str::from_utf8(&output.stdout)
        .map_err(|_| LaunchAttestationError::UnreadableEvidence)?
        .trim();
    if executable_version.is_empty()
        || executable_version.len() > 4096
        || executable_version.chars().any(char::is_control)
    {
        return Err(LaunchAttestationError::UnreadableEvidence);
    }
    Ok((executable_version.to_string(), file_identity))
}

#[cfg(not(unix))]
fn probe_provider_version(
    _executable: &std::path::Path,
) -> Result<(String, ProviderFileIdentity), LaunchAttestationError> {
    Err(LaunchAttestationError::UnreadableEvidence)
}

/// Attest the exact model and reasoning selection from provider-native argv.
/// Missing, repeated, malformed, or substituted options fail closed.
pub(crate) fn attest_runtime_policy(
    evidence: &HarnessProcessEvidence,
    expected: Harness,
    expected_model: &str,
    expected_reasoning_effort: &str,
) -> Result<(), LaunchAttestationError> {
    let args = provider_arguments(evidence, expected)?;
    let model = unique_option_value(args, &["--model", "-m"])
        .map_err(|_| LaunchAttestationError::WrongModel)?
        .ok_or(LaunchAttestationError::WrongModel)?;
    if model != expected_model {
        return Err(LaunchAttestationError::WrongModel);
    }
    let reasoning = match expected {
        Harness::Codex => unique_config_value(args, "model_reasoning_effort")?,
        Harness::Claude => unique_option_value(args, &["--effort"])
            .map_err(|_| LaunchAttestationError::WrongReasoningEffort)?
            .map(str::to_string),
    }
    .ok_or(LaunchAttestationError::WrongReasoningEffort)?;
    if reasoning != expected_reasoning_effort {
        return Err(LaunchAttestationError::WrongReasoningEffort);
    }
    Ok(())
}

/// Produce one typed receipt only after every effective runtime-policy axis and
/// the exact foreground process generation have been verified.
fn seal_runtime_policy_receipt(
    tmux_target: &str,
    evidence: &HarnessProcessEvidence,
    expected: Harness,
    expected_model: &str,
    expected_reasoning_effort: &str,
    expected_permission: PermMode,
    provider_file: ProviderFileIdentity,
    executable_version: &str,
    proven_provider_path: Option<&std::path::Path>,
) -> Result<VerifiedHarnessRuntimeReceipt, LaunchAttestationError> {
    if evidence.terminal.session_name != tmux_target {
        return Err(LaunchAttestationError::TerminalChanged);
    }
    expected
        .adapter()
        .attest_permissions(evidence, expected_permission)?;
    attest_runtime_policy(
        evidence,
        expected,
        expected_model,
        expected_reasoning_effort,
    )?;
    let executable = match proven_provider_path {
        Some(path) => path.to_path_buf(),
        None => resolved_provider_executable_path(evidence, expected)?,
    };
    let (_, provider_index) = process_provider(&evidence.argv)
        .filter(|(provider, _)| *provider == expected)
        .ok_or(LaunchAttestationError::WrongProvider)?;
    if provider_index == 0
        && (provider_file.device, provider_file.inode) != evidence.executable_identity()
    {
        return Err(LaunchAttestationError::ProcessChanged);
    }
    let executable = executable
        .to_str()
        .filter(|path| !path.is_empty() && !path.chars().any(char::is_control))
        .ok_or(LaunchAttestationError::UnreadableEvidence)?;
    let executable_version = executable_version.trim();
    if executable_version.is_empty()
        || executable_version.len() > 4096
        || executable_version.chars().any(char::is_control)
    {
        return Err(LaunchAttestationError::UnreadableEvidence);
    }
    Ok(VerifiedHarnessRuntimeReceipt(HarnessRuntimePolicyReceipt {
        terminal: terminal_generation_receipt(tmux_target, evidence),
        process_pid: evidence.pid,
        process_start_ticks: evidence.start_ticks,
        process_executable_device: evidence.executable_device,
        process_executable_inode: evidence.executable_inode,
        provider_executable_device: provider_file.device,
        provider_executable_inode: provider_file.inode,
        resolved_provider_executable: executable.to_string(),
        executable_version: executable_version.to_string(),
        observed_provider: expected.as_provider().to_string(),
        observed_harness: expected.as_provider().to_string(),
        observed_model: expected_model.to_string(),
        observed_reasoning_effort: expected_reasoning_effort.to_string(),
        observed_permission: expected_permission,
    }))
}

#[allow(clippy::too_many_arguments)]
fn verify_probed_runtime_policy_receipt(
    tmux_target: &str,
    accepted: &HarnessProcessEvidence,
    post_probe: Result<HarnessProcessEvidence, LaunchAttestationError>,
    expected: Harness,
    expected_model: &str,
    expected_reasoning_effort: &str,
    expected_permission: PermMode,
    probed_executable: &std::path::Path,
    provider_file_before: ProviderFileIdentity,
    provider_file_after: ProviderFileIdentity,
    executable_version: &str,
) -> Result<VerifiedHarnessRuntimeReceipt, LaunchAttestationError> {
    if provider_file_before != provider_file_after
        || resolved_provider_executable_path(accepted, expected)? != probed_executable
    {
        return Err(LaunchAttestationError::ProcessChanged);
    }
    let (_, stable_evidence) = attest_final_launch_permissions(
        expected.adapter(),
        accepted,
        post_probe,
        expected_permission,
    )?;
    if resolved_provider_executable_path(&stable_evidence, expected)? != probed_executable {
        return Err(LaunchAttestationError::ProcessChanged);
    }
    seal_runtime_policy_receipt(
        tmux_target,
        &stable_evidence,
        expected,
        expected_model,
        expected_reasoning_effort,
        expected_permission,
        provider_file_after,
        executable_version,
        None,
    )
}

/// Observe one stable foreground provider generation and probe that exact
/// executable's bounded version output before issuing a non-forgeable receipt.
pub(crate) fn observe_runtime_policy_receipt(
    tmux_target: &str,
    expected: Harness,
    expected_model: &str,
    expected_reasoning_effort: &str,
    expected_permission: PermMode,
) -> Result<VerifiedHarnessRuntimeReceipt, LaunchAttestationError> {
    let accepted = observe_harness_process(tmux_target)?;
    let (_, final_evidence) = attest_final_launch_permissions(
        expected.adapter(),
        &accepted,
        observe_harness_process(tmux_target),
        expected_permission,
    )?;
    let executable = resolved_provider_executable_path(&final_evidence, expected)?;
    let (executable_version, provider_file) = probe_provider_version(&executable)?;
    let provider_file_after = provider_file_identity(&executable)?;
    verify_probed_runtime_policy_receipt(
        tmux_target,
        &final_evidence,
        observe_harness_process(tmux_target),
        expected,
        expected_model,
        expected_reasoning_effort,
        expected_permission,
        &executable,
        provider_file,
        provider_file_after,
        &executable_version,
    )
}

/// Re-observe a provider whose exact script inode and pane generation were
/// already proven by the durable spawn journal.
///
/// This restart path never resolves the now-expired private launch alias. It
/// requires the same pane generation, reopens the canonical installed path from
/// the spawn receipt, verifies its exact inode, probes version through that
/// opened identity, and brackets the probe with stable process observations.
pub(crate) fn observe_runtime_policy_receipt_from_spawn(
    tmux_target: &str,
    spawn_terminal: &HarnessTerminalGenerationReceipt,
    spawn_provider: &HarnessProviderLaunchReceipt,
    expected: Harness,
    expected_model: &str,
    expected_reasoning_effort: &str,
    expected_permission: PermMode,
) -> Result<VerifiedHarnessRuntimeReceipt, LaunchAttestationError> {
    let accepted = observe_harness_process(tmux_target)?;
    if spawn_terminal.tmux_target != tmux_target
        || accepted.terminal.session_name != tmux_target
        || accepted.terminal.session_id != spawn_terminal.session_id
        || accepted.terminal.session_created != spawn_terminal.session_created
        || accepted.terminal.window_id != spawn_terminal.window_id
        || accepted.terminal.pane_id != spawn_terminal.pane_id
        || accepted.terminal.pane_pid != spawn_terminal.pane_pid
    {
        return Err(LaunchAttestationError::TerminalChanged);
    }
    expected
        .adapter()
        .attest_permissions(&accepted, expected_permission)?;
    attest_runtime_policy(
        &accepted,
        expected,
        expected_model,
        expected_reasoning_effort,
    )?;
    let provider_path = PathBuf::from(&spawn_provider.resolved_provider_executable);
    let expected_file = ProviderFileIdentity {
        device: spawn_provider.provider_executable_device,
        inode: spawn_provider.provider_executable_inode,
    };
    if provider_file_identity(&provider_path)? != expected_file {
        return Err(LaunchAttestationError::ProcessChanged);
    }
    let (version, probed_file) = probe_provider_version(&provider_path)?;
    if probed_file != expected_file || provider_file_identity(&provider_path)? != expected_file {
        return Err(LaunchAttestationError::ProcessChanged);
    }
    let (_, stable) = attest_final_launch_permissions(
        expected.adapter(),
        &accepted,
        observe_harness_process(tmux_target),
        expected_permission,
    )?;
    seal_runtime_policy_receipt(
        tmux_target,
        &stable,
        expected,
        expected_model,
        expected_reasoning_effort,
        expected_permission,
        expected_file,
        &version,
        Some(&provider_path),
    )
}

#[cfg(test)]
pub(crate) fn test_verified_runtime_receipt(
    tmux_target: &str,
    evidence: &HarnessProcessEvidence,
    expected: Harness,
    expected_model: &str,
    expected_reasoning_effort: &str,
    expected_permission: PermMode,
    executable_version: &str,
) -> Result<VerifiedHarnessRuntimeReceipt, LaunchAttestationError> {
    let mut evidence = evidence.clone();
    evidence.terminal.session_name = tmux_target.to_string();
    seal_runtime_policy_receipt(
        tmux_target,
        &evidence,
        expected,
        expected_model,
        expected_reasoning_effort,
        expected_permission,
        ProviderFileIdentity {
            device: evidence.executable_device,
            inode: evidence.executable_inode,
        },
        executable_version,
        None,
    )
}

#[cfg(test)]
pub(crate) fn test_verified_runtime_receipt_from_observed_file(
    tmux_target: &str,
    evidence: &HarnessProcessEvidence,
    expected: Harness,
    expected_model: &str,
    expected_reasoning_effort: &str,
    expected_permission: PermMode,
    executable_version: &str,
) -> Result<VerifiedHarnessRuntimeReceipt, LaunchAttestationError> {
    let mut evidence = evidence.clone();
    evidence.terminal.session_name = tmux_target.to_string();
    let executable = resolved_provider_executable_path(&evidence, expected)?;
    let provider_file = provider_file_identity(&executable)?;
    seal_runtime_policy_receipt(
        tmux_target,
        &evidence,
        expected,
        expected_model,
        expected_reasoning_effort,
        expected_permission,
        provider_file,
        executable_version,
        None,
    )
}

fn unique_option_value<'a>(args: &'a [String], names: &[&str]) -> Result<Option<&'a str>, ()> {
    let mut found = None;
    let mut index = 0;
    while let Some(argument) = args.get(index) {
        if argument == "--" || !argument.starts_with('-') {
            break;
        }
        let mut matched = None;
        for name in names {
            if argument == name {
                matched = Some(
                    args.get(index + 1)
                        .filter(|value| !value.starts_with('-'))
                        .map(String::as_str)
                        .ok_or(())?,
                );
                index += 1;
                break;
            }
            if let Some(value) = argument.strip_prefix(&format!("{name}=")) {
                if value.is_empty() {
                    return Err(());
                }
                matched = Some(value);
                break;
            }
        }
        if let Some(value) = matched {
            if found.replace(value).is_some() {
                return Err(());
            }
        } else if matches!(
            argument.as_str(),
            "--model" | "-m" | "--effort" | "--config" | "-c" | "--sandbox" | "-s"
        ) {
            if args.get(index + 1).is_none() {
                return Err(());
            }
            index += 1;
        }
        index += 1;
    }
    Ok(found)
}

fn unique_config_value(
    args: &[String],
    key: &str,
) -> Result<Option<String>, LaunchAttestationError> {
    let mut found = None;
    let mut index = 0;
    while let Some(argument) = args.get(index) {
        if argument == "--" || !argument.starts_with('-') {
            break;
        }
        let config = if matches!(argument.as_str(), "--config" | "-c") {
            index += 1;
            Some(
                args.get(index)
                    .map(String::as_str)
                    .ok_or(LaunchAttestationError::WrongReasoningEffort)?,
            )
        } else {
            argument
                .strip_prefix("--config=")
                .or_else(|| argument.strip_prefix("-c="))
                .or_else(|| {
                    argument
                        .strip_prefix("-c")
                        .filter(|value| !value.is_empty())
                })
        };
        if let Some(config) = config {
            if let Some(value) = protected_toml_override(config, key)? {
                if found.replace(value).is_some() {
                    return Err(LaunchAttestationError::WrongReasoningEffort);
                }
            }
        } else if matches!(
            argument.as_str(),
            "--model" | "-m" | "--effort" | "--sandbox" | "-s"
        ) {
            if args.get(index + 1).is_none() {
                return Err(LaunchAttestationError::WrongReasoningEffort);
            }
            index += 1;
        }
        index += 1;
    }
    Ok(found)
}

/// Parse the provider-equivalent TOML assignment accepted by Codex `-c`.
/// The protected key may be bare or simply quoted and whitespace around `=` is
/// insignificant. Escapes, dotted/compound keys, malformed quoting, and any
/// other mention of the protected key fail closed rather than being ignored.
fn protected_toml_override(
    config: &str,
    protected_key: &str,
) -> Result<Option<String>, LaunchAttestationError> {
    let config = config.trim();
    let Some((raw_key, raw_value)) = config.split_once('=') else {
        return if config.contains(protected_key) {
            Err(LaunchAttestationError::WrongReasoningEffort)
        } else {
            Ok(None)
        };
    };
    let raw_key = raw_key.trim();
    let parsed_key = parse_simple_toml_token(raw_key)?;
    if parsed_key != protected_key {
        return if raw_key.contains(protected_key) {
            Err(LaunchAttestationError::WrongReasoningEffort)
        } else {
            Ok(None)
        };
    }
    let value = parse_simple_toml_token(raw_value.trim())?;
    if value.is_empty()
        || value.chars().any(char::is_control)
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
    {
        return Err(LaunchAttestationError::WrongReasoningEffort);
    }
    Ok(Some(value.to_string()))
}

fn parse_simple_toml_token(value: &str) -> Result<&str, LaunchAttestationError> {
    if value.is_empty() {
        return Err(LaunchAttestationError::WrongReasoningEffort);
    }
    let first = value.as_bytes()[0];
    if matches!(first, b'\'' | b'"') {
        if value.len() < 2
            || value.as_bytes()[value.len() - 1] != first
            || value[1..value.len() - 1].contains(['\\', '\'', '"'])
        {
            return Err(LaunchAttestationError::WrongReasoningEffort);
        }
        return Ok(&value[1..value.len() - 1]);
    }
    if value.contains(['\'', '"']) {
        return Err(LaunchAttestationError::WrongReasoningEffort);
    }
    Ok(value)
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

    fn empty_candidate_session_credentials() -> Vec<(String, String)> {
        vec![
            ("T_HUB_CONTROL_ADDR".into(), String::new()),
            ("T_HUB_CONTROL_TOKEN".into(), String::new()),
            ("T_HUB_SESSION_TOKEN".into(), String::new()),
        ]
    }

    #[cfg(unix)]
    fn observed_terminal_generation(target: &str) -> HarnessTerminalGenerationReceipt {
        observe_terminal_generation_receipt(target)
            .unwrap()
            .into_persisted()
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
            b"THPA5\nth_cor-candidate-2\n$17\n123456\n@9\n%42\n42\n42\n900\n8\n1234\n/usr/bin/codex\n42:900\ncodex\0--sandbox\0read-only\0",
        )
        .unwrap();
        assert_eq!(parsed.identity(), (42, 900));
        assert_eq!(parsed.executable_identity(), (8, 1234));
        assert_eq!(parsed.argv, ["codex", "--sandbox", "read-only"]);

        assert_eq!(
            parse_process_evidence(
                b"THPA5\nth_cor-candidate-2\n$17\n123456\n@9\n%42\n42\n42\n900\n8\n1234\n/usr/bin/codex\n42:900\ncodex",
            ),
            Err(LaunchAttestationError::UnreadableEvidence)
        );
        assert_eq!(
            parse_process_evidence(
                b"THPA5\nth_cor-candidate-2\n$17\n123456\n@9\n%42\n42\n42\n900\n8\n1234\n/usr/bin/codex\n42:900,7:100\ncodex\0",
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

    #[test]
    fn runtime_policy_attestation_accepts_exact_codex_and_claude_options() {
        let codex = HarnessProcessEvidence::test(
            42,
            900,
            &[
                "/opt/t-hub/bin/codex",
                "--dangerously-bypass-approvals-and-sandbox",
                "--model",
                "gpt-5.6-terra",
                "-c",
                "model_reasoning_effort=medium",
                "coordinate",
            ],
        );
        attest_runtime_policy(&codex, Harness::Codex, "gpt-5.6-terra", "medium").unwrap();
        assert_eq!(
            provider_executable_path(&codex, Harness::Codex).unwrap(),
            "/opt/t-hub/bin/codex"
        );

        let claude = HarnessProcessEvidence::test(
            43,
            901,
            &[
                "node",
                "/opt/t-hub/bin/claude",
                "--dangerously-skip-permissions",
                "--model=claude-opus-4-1",
                "--effort",
                "high",
                "coordinate",
            ],
        );
        attest_runtime_policy(&claude, Harness::Claude, "claude-opus-4-1", "high").unwrap();
        assert_eq!(
            provider_executable_path(&claude, Harness::Claude).unwrap(),
            "/opt/t-hub/bin/claude"
        );
    }

    #[test]
    fn runtime_policy_attestation_fails_closed_on_missing_duplicate_or_substituted_axes() {
        let cases = [
            (
                vec![
                    "codex",
                    "--model",
                    "other",
                    "-c",
                    "model_reasoning_effort=medium",
                ],
                LaunchAttestationError::WrongModel,
            ),
            (
                vec![
                    "codex",
                    "--model",
                    "gpt-5.6-terra",
                    "--model",
                    "gpt-5.6-terra",
                    "-c",
                    "model_reasoning_effort=medium",
                ],
                LaunchAttestationError::WrongModel,
            ),
            (
                vec!["codex", "--model", "gpt-5.6-terra"],
                LaunchAttestationError::WrongReasoningEffort,
            ),
            (
                vec![
                    "codex",
                    "--model",
                    "gpt-5.6-terra",
                    "-c",
                    "model_reasoning_effort=high",
                ],
                LaunchAttestationError::WrongReasoningEffort,
            ),
        ];
        for (argv, expected) in cases {
            let evidence = HarnessProcessEvidence::test(42, 900, &argv);
            assert_eq!(
                attest_runtime_policy(&evidence, Harness::Codex, "gpt-5.6-terra", "medium"),
                Err(expected)
            );
        }
    }

    #[test]
    fn runtime_policy_receipt_binds_exact_process_terminal_and_policy_or_fails_closed() {
        let argv = [
            "/opt/t-hub/bin/codex",
            "--dangerously-bypass-approvals-and-sandbox",
            "--model",
            "gpt-5.6-terra",
            "-c",
            "model_reasoning_effort=medium",
        ];
        let evidence = HarnessProcessEvidence::test(42, 900, &argv);
        let receipt = seal_runtime_policy_receipt(
            "th_cor-candidate-2",
            &evidence,
            Harness::Codex,
            "gpt-5.6-terra",
            "medium",
            PermMode::BypassPermissions,
            ProviderFileIdentity {
                device: evidence.executable_device,
                inode: evidence.executable_inode,
            },
            "codex-cli 1.2.3",
            None,
        )
        .unwrap()
        .into_persisted();
        assert_eq!(
            receipt.terminal,
            terminal_generation_receipt("th_cor-candidate-2", &evidence)
        );
        assert_eq!(receipt.process_pid, 42);
        assert_eq!(receipt.process_start_ticks, 900);
        assert_eq!(receipt.resolved_provider_executable, "/opt/t-hub/bin/codex");
        assert_eq!(receipt.observed_model, "gpt-5.6-terra");
        assert_eq!(receipt.observed_reasoning_effort, "medium");
        assert_eq!(receipt.observed_permission, PermMode::BypassPermissions);

        for (model, reasoning, permission, version, expected) in [
            (
                "other-model",
                "medium",
                PermMode::BypassPermissions,
                "codex-cli 1.2.3",
                LaunchAttestationError::WrongModel,
            ),
            (
                "gpt-5.6-terra",
                "high",
                PermMode::BypassPermissions,
                "codex-cli 1.2.3",
                LaunchAttestationError::WrongReasoningEffort,
            ),
            (
                "gpt-5.6-terra",
                "medium",
                PermMode::Default,
                "codex-cli 1.2.3",
                LaunchAttestationError::ConflictingPermission,
            ),
            (
                "gpt-5.6-terra",
                "medium",
                PermMode::BypassPermissions,
                "\n",
                LaunchAttestationError::UnreadableEvidence,
            ),
        ] {
            assert_eq!(
                seal_runtime_policy_receipt(
                    "th_cor-candidate-2",
                    &evidence,
                    Harness::Codex,
                    model,
                    reasoning,
                    permission,
                    ProviderFileIdentity {
                        device: evidence.executable_device,
                        inode: evidence.executable_inode,
                    },
                    version,
                    None,
                )
                .map(VerifiedHarnessRuntimeReceipt::into_persisted),
                Err(expected)
            );
        }
    }

    #[test]
    fn probed_runtime_receipt_rejects_unreadable_foreign_and_drifting_evidence() {
        let argv = [
            "/opt/t-hub/bin/codex",
            "--dangerously-bypass-approvals-and-sandbox",
            "--model",
            "gpt-5.6-terra",
            "-c",
            "model_reasoning_effort=medium",
        ];
        let accepted = HarnessProcessEvidence::test(42, 900, &argv);
        let provider_file = ProviderFileIdentity {
            device: accepted.executable_device,
            inode: accepted.executable_inode,
        };
        let executable = PathBuf::from("/opt/t-hub/bin/codex");
        let verify = |post_probe, after, version| {
            verify_probed_runtime_policy_receipt(
                "th_cor-candidate-2",
                &accepted,
                post_probe,
                Harness::Codex,
                "gpt-5.6-terra",
                "medium",
                PermMode::BypassPermissions,
                &executable,
                provider_file,
                after,
                version,
            )
            .map(VerifiedHarnessRuntimeReceipt::into_persisted)
        };
        assert!(verify(Ok(accepted.clone()), provider_file, "codex-cli 1.2.3").is_ok());
        assert_eq!(
            verify(
                Err(LaunchAttestationError::UnreadableEvidence),
                provider_file,
                "codex-cli 1.2.3"
            ),
            Err(LaunchAttestationError::UnreadableEvidence)
        );

        let mut foreign_pane = accepted.clone();
        foreign_pane.terminal.pane_id += 1;
        assert_eq!(
            verify(Ok(foreign_pane), provider_file, "codex-cli 1.2.3"),
            Err(LaunchAttestationError::TerminalChanged)
        );
        let mut process_drift = accepted.clone();
        process_drift.start_ticks += 1;
        assert_eq!(
            verify(Ok(process_drift), provider_file, "codex-cli 1.2.3"),
            Err(LaunchAttestationError::ProcessChanged)
        );
        let mut executable_drift = accepted.clone();
        executable_drift.executable_inode += 1;
        assert_eq!(
            verify(Ok(executable_drift), provider_file, "codex-cli 1.2.3"),
            Err(LaunchAttestationError::ProcessChanged)
        );
        let mut path_drift = accepted.clone();
        path_drift.resolved_process_executable = "/tmp/replaced-codex".into();
        assert_eq!(
            verify(Ok(path_drift), provider_file, "codex-cli 1.2.3"),
            Err(LaunchAttestationError::ProcessChanged)
        );
        assert_eq!(
            verify(
                Ok(accepted.clone()),
                ProviderFileIdentity {
                    device: provider_file.device,
                    inode: provider_file.inode + 1,
                },
                "codex-cli 9.9.9"
            ),
            Err(LaunchAttestationError::ProcessChanged)
        );
        assert_eq!(
            verify(Ok(accepted.clone()), provider_file, "\n"),
            Err(LaunchAttestationError::UnreadableEvidence)
        );
    }

    #[cfg(unix)]
    #[test]
    fn node_provider_receipt_binds_script_inode_separately_from_node_process() {
        let temp = tempfile::tempdir().unwrap();
        let provider = temp.path().join("codex");
        std::fs::write(&provider, "#!/usr/bin/env node\n").unwrap();
        let provider_path = provider.to_str().unwrap();
        let evidence = HarnessProcessEvidence::test(
            42,
            900,
            &[
                "/usr/bin/node",
                provider_path,
                "--dangerously-bypass-approvals-and-sandbox",
                "--model",
                "gpt-5.6-terra",
                "-c",
                "model_reasoning_effort=medium",
            ],
        );
        let provider_file = provider_file_identity(&provider).unwrap();
        let receipt = seal_runtime_policy_receipt(
            "th_cor-candidate-2",
            &evidence,
            Harness::Codex,
            "gpt-5.6-terra",
            "medium",
            PermMode::BypassPermissions,
            provider_file,
            "codex-cli 1.2.3",
            None,
        )
        .unwrap()
        .into_persisted();
        assert_eq!(
            (
                receipt.provider_executable_device,
                receipt.provider_executable_inode,
            ),
            (provider_file.device, provider_file.inode)
        );
        assert_ne!(
            (
                receipt.process_executable_device,
                receipt.process_executable_inode,
            ),
            (provider_file.device, provider_file.inode)
        );
    }

    #[cfg(unix)]
    #[test]
    fn installed_symlinked_node_cli_launch_binds_the_canonical_script_inode() {
        use std::os::unix::fs::symlink;

        let temp = tempfile::tempdir().unwrap();
        let package_bin = temp.path().join("lib/node_modules/@openai/codex/bin");
        let launcher_bin = temp.path().join("bin");
        std::fs::create_dir_all(&package_bin).unwrap();
        std::fs::create_dir_all(&launcher_bin).unwrap();
        let installed_script = package_bin.join("codex.js");
        let launch_token = launcher_bin.join("codex");
        std::fs::write(&installed_script, "#!/usr/bin/env node\n").unwrap();
        std::fs::write(
            package_bin.parent().unwrap().join("package.json"),
            br#"{"name":"@openai/codex","bin":{"codex":"bin/codex.js"}}"#,
        )
        .unwrap();
        symlink(&installed_script, &launch_token).unwrap();
        let expected_identity = provider_file_identity(&installed_script).unwrap();

        let opened =
            test_observe_provider_launch_receipt(&launch_token, Harness::Codex, temp.path())
                .unwrap();
        assert_eq!(
            std::fs::read(opened.test_opened_path()).unwrap(),
            b"#!/usr/bin/env node\n"
        );
        let receipt = opened.test_receipt().into_persisted();
        assert_eq!(
            receipt.resolved_provider_executable,
            std::fs::canonicalize(&installed_script)
                .unwrap()
                .to_string_lossy()
        );
        assert_eq!(
            (
                receipt.provider_executable_device,
                receipt.provider_executable_inode,
            ),
            (expected_identity.device, expected_identity.inode)
        );
        assert!(matches!(
            test_observe_provider_launch_receipt(&launch_token, Harness::Claude, temp.path()),
            Err(LaunchAttestationError::WrongProvider)
        ));
    }

    #[cfg(unix)]
    #[test]
    fn codex_named_symlink_rejects_claude_and_foreign_canonical_targets() {
        use std::os::unix::fs::symlink;

        let temp = tempfile::tempdir().unwrap();
        let launcher_bin = temp.path().join("bin");
        std::fs::create_dir_all(&launcher_bin).unwrap();
        let claude_bin = temp
            .path()
            .join("lib/node_modules/@anthropic-ai/claude-code/bin/claude.exe");
        std::fs::create_dir_all(claude_bin.parent().unwrap()).unwrap();
        std::fs::write(&claude_bin, "#!/usr/bin/env node\n").unwrap();
        let foreign = temp.path().join("foreign-provider.js");
        std::fs::write(&foreign, "#!/usr/bin/env node\n").unwrap();

        for (label, target) in [("claude", &claude_bin), ("foreign", &foreign)] {
            let launch_token = launcher_bin.join(format!("codex-{label}"));
            symlink(target, &launch_token).unwrap();
            let codex_token = launcher_bin.join("codex");
            std::fs::rename(&launch_token, &codex_token).unwrap();
            assert!(matches!(
                test_observe_provider_launch_receipt(&codex_token, Harness::Codex, temp.path()),
                Err(LaunchAttestationError::WrongProvider)
            ));
            std::fs::remove_file(codex_token).unwrap();
        }
    }

    #[cfg(unix)]
    #[test]
    fn forged_provider_suffix_outside_the_trusted_package_root_is_rejected() {
        use std::os::unix::fs::symlink;

        let temp = tempfile::tempdir().unwrap();
        let launcher_bin = temp.path().join("bin");
        let forged = temp.path().join("attacker/@openai/codex/bin/codex.js");
        std::fs::create_dir_all(&launcher_bin).unwrap();
        std::fs::create_dir_all(forged.parent().unwrap()).unwrap();
        std::fs::write(&forged, "#!/usr/bin/env node\n").unwrap();
        let launch_token = launcher_bin.join("codex");
        symlink(&forged, &launch_token).unwrap();

        assert!(matches!(
            test_observe_provider_launch_receipt(&launch_token, Harness::Codex, temp.path()),
            Err(LaunchAttestationError::WrongProvider)
        ));
    }

    #[cfg(unix)]
    #[test]
    fn provider_provenance_binds_every_intermediate_directory_against_rename() {
        let temp = tempfile::tempdir().unwrap();
        let package = temp.path().join("lib/node_modules/@openai/codex/bin");
        std::fs::create_dir_all(&package).unwrap();
        let executable = package.join("codex.js");
        std::fs::write(&executable, "trusted").unwrap();
        let bindings = bind_provider_path_chain(&executable, false).unwrap();

        let openai = temp.path().join("lib/node_modules/@openai");
        let displaced = temp.path().join("lib/node_modules/@openai-old");
        std::fs::rename(&openai, &displaced).unwrap();
        let forged = openai.join("codex/bin");
        std::fs::create_dir_all(&forged).unwrap();
        std::fs::write(forged.join("codex.js"), "attacker").unwrap();

        assert!(bindings.iter().any(|binding| {
            matches!(
                binding.revalidate(),
                Err(LaunchAttestationError::ProcessChanged)
            )
        }));
    }

    #[test]
    fn user_managed_provider_layouts_require_protected_catalog_provenance() {
        let home = PathBuf::from("/home/operator");
        for launcher in [
            home.join(".bun/bin"),
            home.join(".npm-global/bin"),
            home.join(".local/bin"),
            home.join(".nvm/versions/node/v24.0.0/bin"),
        ] {
            assert!(user_managed_provider_launcher_under(&launcher, &home));
        }
        assert!(!user_managed_provider_launcher_under(
            std::path::Path::new("/usr/local/bin"),
            &home
        ));
        assert_eq!(
            LaunchAttestationError::UntrustedUserInstall.to_string(),
            "user-managed provider install is not launch authority; provision a protected package snapshot and root-owned /etc/t-hub/provider-catalog.json mapping"
        );
    }

    #[cfg(unix)]
    #[test]
    fn protected_catalog_maps_authoritative_user_launchers_to_protected_snapshots() {
        use std::os::unix::fs::symlink;

        for (harness, source_directory) in [
            (Harness::Codex, ".bun/bin"),
            (Harness::Claude, ".npm-global/bin"),
        ] {
            let temp = tempfile::tempdir().unwrap();
            let home = temp.path().join("home/operator");
            let source_bin = home.join(source_directory);
            std::fs::create_dir_all(&source_bin).unwrap();
            let untrusted_package = temp.path().join("untrusted/provider.js");
            std::fs::create_dir_all(untrusted_package.parent().unwrap()).unwrap();
            std::fs::write(&untrusted_package, "untrusted").unwrap();
            let source_launcher = source_bin.join(harness.as_provider());
            symlink(&untrusted_package, &source_launcher).unwrap();

            let protected_root = temp.path().join("protected");
            let protected_bin = protected_root.join("bin");
            let package_root = protected_root.join("lib/node_modules");
            let package = package_root.join(provider_package_relative_root(harness));
            let executable = package.join(provider_package_bin_relative_path(harness));
            std::fs::create_dir_all(executable.parent().unwrap()).unwrap();
            std::fs::create_dir_all(&protected_bin).unwrap();
            std::fs::write(&executable, "protected").unwrap();
            std::fs::write(
                package.join("package.json"),
                serde_json::to_vec(&serde_json::json!({
                    "name": provider_package_relative_root(harness).to_string_lossy(),
                    "bin": {
                        harness.as_provider(): provider_package_bin_relative_path(harness),
                    },
                }))
                .unwrap(),
            )
            .unwrap();
            let protected_launcher = protected_bin.join(harness.as_provider());
            symlink(&executable, &protected_launcher).unwrap();
            let catalog_path = temp.path().join("etc/t-hub/provider-catalog.json");
            std::fs::create_dir_all(catalog_path.parent().unwrap()).unwrap();
            std::fs::write(
                &catalog_path,
                serde_json::to_vec(&serde_json::json!({
                    "schemaVersion": 1,
                    "providers": [{
                        "provider": harness.as_provider(),
                        "sourceLauncher": source_launcher,
                        "protectedLauncher": protected_launcher,
                        "protectedPackageRoot": package_root,
                    }],
                }))
                .unwrap(),
            )
            .unwrap();

            let opened = observe_provisioned_provider_launch_receipt(
                &source_launcher,
                harness,
                &catalog_path,
                &home,
                false,
            )
            .unwrap();
            assert_eq!(
                opened.receipt.resolved_provider_executable,
                std::fs::canonicalize(&executable)
                    .unwrap()
                    .to_string_lossy()
            );
            assert_ne!(
                opened.receipt.resolved_provider_executable,
                std::fs::canonicalize(&source_launcher)
                    .unwrap()
                    .to_string_lossy()
            );
            assert!(opened
                .provenance
                .iter()
                .all(|binding| binding.revalidate().is_ok()));
        }
    }

    #[cfg(unix)]
    #[test]
    fn protected_catalog_is_bounded_exact_and_swap_detecting() {
        use std::os::unix::fs::symlink;

        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join("home/operator");
        let source_bin = home.join(".bun/bin");
        std::fs::create_dir_all(&source_bin).unwrap();
        let source_launcher = source_bin.join("codex");
        symlink("missing-user-provider", &source_launcher).unwrap();
        let protected_root = temp.path().join("protected");
        let package_root = protected_root.join("lib/node_modules");
        let package = package_root.join("@openai/codex");
        let executable = package.join("bin/codex.js");
        let protected_bin = protected_root.join("bin");
        std::fs::create_dir_all(executable.parent().unwrap()).unwrap();
        std::fs::create_dir_all(&protected_bin).unwrap();
        std::fs::write(&executable, "protected").unwrap();
        std::fs::write(
            package.join("package.json"),
            br#"{"name":"@openai/codex","bin":{"codex":"bin/codex.js"}}"#,
        )
        .unwrap();
        let protected_launcher = protected_bin.join("codex");
        symlink(&executable, &protected_launcher).unwrap();
        let catalog_path = temp.path().join("etc/t-hub/provider-catalog.json");
        std::fs::create_dir_all(catalog_path.parent().unwrap()).unwrap();
        let catalog = serde_json::json!({
            "schemaVersion": 1,
            "providers": [{
                "provider": "codex",
                "sourceLauncher": source_launcher,
                "protectedLauncher": protected_launcher,
                "protectedPackageRoot": package_root,
            }],
        });
        std::fs::write(&catalog_path, serde_json::to_vec(&catalog).unwrap()).unwrap();
        let opened = observe_provisioned_provider_launch_receipt(
            &source_launcher,
            Harness::Codex,
            &catalog_path,
            &home,
            false,
        )
        .unwrap();

        let replacement = catalog_path.with_extension("replacement");
        std::fs::write(&replacement, serde_json::to_vec(&catalog).unwrap()).unwrap();
        std::fs::rename(&replacement, &catalog_path).unwrap();
        assert!(opened.provenance.iter().any(|binding| {
            matches!(
                binding.revalidate(),
                Err(LaunchAttestationError::ProcessChanged)
            )
        }));
        if Command::new("tmux")
            .arg("-V")
            .output()
            .is_ok_and(|output| output.status.success())
        {
            struct SessionGuard(String);
            impl Drop for SessionGuard {
                fn drop(&mut self) {
                    let _ = crate::tmux::kill_session(&self.0);
                }
            }
            let suffix = uuid::Uuid::new_v4().simple().to_string();
            let target = format!("th_catalog_swap_{}", &suffix[..8]);
            crate::tmux::new_session_with_env(
                &target,
                "/tmp",
                Some("sleep 30"),
                &empty_candidate_session_credentials(),
            )
            .unwrap();
            let _guard = SessionGuard(target.clone());
            let mut failure_generation = observed_terminal_generation(&target);
            assert!(matches!(
                opened.launch_in_exact_tmux_pane(
                    Harness::Codex,
                    &target,
                    "/tmp",
                    "codex",
                    &[("T_HUB_SESSION_TOKEN".into(), "candidate-secret".into())],
                    &mut failure_generation,
                ),
                Err(LaunchAttestationError::ProcessChanged)
            ));
            assert!(
                crate::tmux::session_environment_exact(&target, "T_HUB_SESSION_TOKEN")
                    .unwrap()
                    .is_none_or(|value| value.is_empty())
            );
        }

        for malformed in [
            serde_json::json!({"schemaVersion": 2, "providers": []}),
            serde_json::json!({"schemaVersion": 1, "providers": []}),
            serde_json::json!({
                "schemaVersion": 1,
                "providers": [{
                    "provider": "codex",
                    "sourceLauncher": home.join("other/bin/codex"),
                    "protectedLauncher": protected_bin.join("codex"),
                    "protectedPackageRoot": protected_root.join("lib/node_modules"),
                }],
            }),
        ] {
            std::fs::write(&catalog_path, serde_json::to_vec(&malformed).unwrap()).unwrap();
            assert!(matches!(
                observe_provisioned_provider_launch_receipt(
                    &source_launcher,
                    Harness::Codex,
                    &catalog_path,
                    &home,
                    false,
                ),
                Err(LaunchAttestationError::UntrustedUserInstall)
            ));
        }
    }

    #[cfg(unix)]
    #[test]
    fn direct_provider_binaries_in_launcher_directories_are_never_provenance() {
        let temp = tempfile::tempdir().unwrap();
        let launcher_bin = temp.path().join("bin");
        std::fs::create_dir_all(&launcher_bin).unwrap();
        for harness in [Harness::Codex, Harness::Claude] {
            let direct = launcher_bin.join(harness.as_provider());
            std::fs::write(&direct, "#!/bin/sh\nexit 0\n").unwrap();
            assert!(matches!(
                test_observe_provider_launch_receipt(&direct, harness, temp.path()),
                Err(LaunchAttestationError::WrongProvider)
            ));
            std::fs::remove_file(direct).unwrap();
        }
    }

    #[cfg(unix)]
    #[test]
    fn missing_or_substituted_package_manifest_cannot_prove_a_provider() {
        use std::os::unix::fs::symlink;

        let temp = tempfile::tempdir().unwrap();
        let launcher_bin = temp.path().join("bin");
        let package_bin = temp.path().join("lib/node_modules/@openai/codex/bin");
        std::fs::create_dir_all(&launcher_bin).unwrap();
        std::fs::create_dir_all(&package_bin).unwrap();
        let script = package_bin.join("codex.js");
        let launcher = launcher_bin.join("codex");
        std::fs::write(&script, "#!/usr/bin/env node\n").unwrap();
        symlink(&script, &launcher).unwrap();
        assert!(matches!(
            test_observe_provider_launch_receipt(&launcher, Harness::Codex, temp.path()),
            Err(LaunchAttestationError::UnreadableEvidence)
        ));
        std::fs::write(
            package_bin.parent().unwrap().join("package.json"),
            br#"{"name":"attacker/codex","bin":{"codex":"bin/codex.js"}}"#,
        )
        .unwrap();
        assert!(matches!(
            test_observe_provider_launch_receipt(&launcher, Harness::Codex, temp.path()),
            Err(LaunchAttestationError::WrongProvider)
        ));
    }

    #[cfg(unix)]
    #[test]
    fn provider_launcher_swap_after_open_cannot_issue_a_receipt() {
        use std::os::unix::fs::symlink;

        let temp = tempfile::tempdir().unwrap();
        let launcher_bin = temp.path().join("bin");
        let package_bin = temp.path().join("lib/node_modules/@openai/codex/bin");
        std::fs::create_dir_all(&launcher_bin).unwrap();
        std::fs::create_dir_all(&package_bin).unwrap();
        let accepted = package_bin.join("codex.js");
        let replacement = temp.path().join("replacement.js");
        std::fs::write(&accepted, "#!/usr/bin/env node\n").unwrap();
        std::fs::write(&replacement, "#!/usr/bin/env node\n").unwrap();
        std::fs::write(
            package_bin.parent().unwrap().join("package.json"),
            br#"{"name":"@openai/codex","bin":{"codex":"bin/codex.js"}}"#,
        )
        .unwrap();
        let launch_token = launcher_bin.join("codex");
        symlink(&accepted, &launch_token).unwrap();

        let result = observe_provider_launch_receipt_with_layouts(
            &launch_token,
            Harness::Codex,
            &[test_provider_install_layout(temp.path())],
            || {
                std::fs::remove_file(&launch_token).unwrap();
                symlink(&replacement, &launch_token).unwrap();
            },
        );
        assert!(matches!(
            result,
            Err(LaunchAttestationError::ProcessChanged)
        ));
    }

    #[cfg(unix)]
    #[test]
    fn provider_swap_after_observation_keeps_the_opened_inode_exact() {
        use std::os::unix::fs::symlink;

        let temp = tempfile::tempdir().unwrap();
        let launcher_bin = temp.path().join("bin");
        let package_bin = temp.path().join("lib/node_modules/@openai/codex/bin");
        std::fs::create_dir_all(&launcher_bin).unwrap();
        std::fs::create_dir_all(&package_bin).unwrap();
        let accepted = package_bin.join("codex.js");
        let replacement = temp.path().join("attacker.js");
        std::fs::write(&accepted, "trusted-provider-v1").unwrap();
        std::fs::write(&replacement, "attacker-provider-v2").unwrap();
        std::fs::write(
            package_bin.parent().unwrap().join("package.json"),
            br#"{"name":"@openai/codex","bin":{"codex":"bin/codex.js"}}"#,
        )
        .unwrap();
        let launcher = launcher_bin.join("codex");
        symlink(&accepted, &launcher).unwrap();
        let observed =
            test_observe_provider_launch_receipt(&launcher, Harness::Codex, temp.path()).unwrap();

        std::fs::remove_file(&launcher).unwrap();
        symlink(&replacement, &launcher).unwrap();
        assert_eq!(std::fs::read(&launcher).unwrap(), b"attacker-provider-v2");
        assert_eq!(
            std::fs::read(observed.test_opened_path()).unwrap(),
            b"trusted-provider-v1"
        );
        assert!(observed
            .test_opened_path()
            .to_string_lossy()
            .contains("/proc/"));
        assert_eq!(
            observed
                .test_receipt()
                .into_persisted()
                .provider_executable_inode,
            provider_file_identity(&accepted).unwrap().inode
        );
    }

    #[cfg(unix)]
    #[test]
    fn exact_tmux_handoff_executes_opened_inode_and_delivers_credentials_to_that_child() {
        use std::os::unix::fs::{symlink, PermissionsExt};

        if !Command::new("tmux")
            .arg("-V")
            .output()
            .is_ok_and(|output| output.status.success())
            || !Command::new("node")
                .arg("--version")
                .output()
                .is_ok_and(|output| output.status.success())
        {
            eprintln!("tmux or node is unavailable - skipping exact provider handoff fixture");
            return;
        }
        struct SessionGuard(String);
        impl Drop for SessionGuard {
            fn drop(&mut self) {
                let _ = crate::tmux::kill_session(&self.0);
            }
        }

        let temp = tempfile::tempdir().unwrap();
        let package_bin = temp.path().join("lib/node_modules/@openai/codex/bin");
        let launcher_bin = temp.path().join("bin");
        std::fs::create_dir_all(&package_bin).unwrap();
        std::fs::create_dir_all(&launcher_bin).unwrap();
        let provider = package_bin.join("codex.js");
        let attacker = temp.path().join("attacker.js");
        let proof = temp.path().join("proof.txt");
        std::fs::write(
            &provider,
            "#!/usr/bin/env node\nconst fs=require('fs'); const p=process.argv[process.argv.indexOf('--proof')+1]; fs.writeFileSync(p, process.env.T_HUB_SESSION_TOKEN === 'candidate-secret' ? 'trusted-received' : 'trusted-missing'); setInterval(()=>{}, 1000);\n",
        )
        .unwrap();
        std::fs::set_permissions(&provider, std::fs::Permissions::from_mode(0o700)).unwrap();
        std::fs::write(
            &attacker,
            "#!/usr/bin/env node\nrequire('fs').writeFileSync(process.argv[2], 'attacker'); setInterval(()=>{}, 1000);\n",
        )
        .unwrap();
        std::fs::set_permissions(&attacker, std::fs::Permissions::from_mode(0o700)).unwrap();
        std::fs::write(
            package_bin.parent().unwrap().join("package.json"),
            br#"{"name":"@openai/codex","bin":{"codex":"bin/codex.js"}}"#,
        )
        .unwrap();
        let launcher = launcher_bin.join("codex");
        symlink(&provider, &launcher).unwrap();
        let opened =
            test_observe_provider_launch_receipt(&launcher, Harness::Codex, temp.path()).unwrap();
        std::fs::remove_file(&launcher).unwrap();
        symlink(&attacker, &launcher).unwrap();

        let suffix = uuid::Uuid::new_v4().simple().to_string();
        let target = format!("th_exact_handoff_{}", &suffix[..8]);
        crate::tmux::new_session_with_env(
            &target,
            "/tmp",
            Some("sleep 30"),
            &empty_candidate_session_credentials(),
        )
        .unwrap();
        let guard = SessionGuard(target.clone());
        let command = format!(
            "codex --proof {}",
            sh_single_quote(&proof.to_string_lossy())
        );
        let mut failure_generation = observed_terminal_generation(&target);
        let launch = opened
            .launch_in_exact_tmux_pane(
                Harness::Codex,
                &target,
                "/tmp",
                &command,
                &[("T_HUB_SESSION_TOKEN".into(), "candidate-secret".into())],
                &mut failure_generation,
            )
            .unwrap();
        let (provider_receipt, terminal_receipt) = launch.spawn_receipts();
        assert_eq!(std::fs::read_to_string(&proof).unwrap(), "trusted-received");
        assert_eq!(
            provider_receipt.into_persisted().provider_executable_inode,
            provider_file_identity(&provider).unwrap().inode
        );
        assert_eq!(terminal_receipt.into_persisted().tmux_target, target);
        assert_eq!(
            provider_arguments(launch.evidence(), Harness::Codex).unwrap()[0],
            "--proof"
        );
        assert!(
            crate::tmux::session_environment_exact(&target, "T_HUB_SESSION_TOKEN")
                .unwrap()
                .is_none_or(|value| value.is_empty())
        );
        drop(launch);
        drop(guard);
    }

    #[cfg(unix)]
    #[test]
    fn exact_tmux_handoff_preserves_failure_generation_and_never_persists_credentials() {
        use std::os::unix::fs::{symlink, PermissionsExt};

        if !Command::new("tmux")
            .arg("-V")
            .output()
            .is_ok_and(|output| output.status.success())
        {
            eprintln!("tmux is unavailable - skipping credential scrub fixture");
            return;
        }
        struct SessionGuard(String);
        impl Drop for SessionGuard {
            fn drop(&mut self) {
                let _ = crate::tmux::kill_session(&self.0);
            }
        }
        let temp = tempfile::tempdir().unwrap();
        let package_bin = temp.path().join("lib/node_modules/@openai/codex/bin");
        let launcher_bin = temp.path().join("bin");
        std::fs::create_dir_all(&package_bin).unwrap();
        std::fs::create_dir_all(&launcher_bin).unwrap();
        let provider = package_bin.join("codex.js");
        std::fs::write(&provider, "#!/bin/sh\nexec sleep 30\n").unwrap();
        std::fs::set_permissions(&provider, std::fs::Permissions::from_mode(0o700)).unwrap();
        std::fs::write(
            package_bin.parent().unwrap().join("package.json"),
            br#"{"name":"@openai/codex","bin":{"codex":"bin/codex.js"}}"#,
        )
        .unwrap();
        let launcher = launcher_bin.join("codex");
        symlink(&provider, &launcher).unwrap();
        let mut guards = Vec::new();

        let suffix = uuid::Uuid::new_v4().simple().to_string();
        let partial_target = format!("th_scrub_partial_{}", &suffix[..8]);
        crate::tmux::new_session_with_env(
            &partial_target,
            "/tmp",
            Some("sleep 30"),
            &empty_candidate_session_credentials(),
        )
        .unwrap();
        guards.push(SessionGuard(partial_target.clone()));
        assert!(crate::tmux::unset_session_environment_exact(
            &format!("{partial_target}_missing"),
            &["T_HUB_SESSION_TOKEN".to_string()],
        )
        .is_err());
        let opened =
            test_observe_provider_launch_receipt(&launcher, Harness::Codex, temp.path()).unwrap();
        let mut failure_generation = observed_terminal_generation(&partial_target);
        let dormant_generation = failure_generation.clone();
        assert!(matches!(
            opened.launch_in_exact_tmux_pane(
                Harness::Codex,
                &partial_target,
                "/tmp",
                "codex",
                &[
                    ("T_HUB_SESSION_TOKEN".into(), "candidate-secret".into()),
                    ("MALFORMED KEY".into(), "never-authoritative".into()),
                ],
                &mut failure_generation,
            ),
            Err(LaunchAttestationError::CredentialHandoffFailed)
        ));
        assert_eq!(failure_generation, dormant_generation);
        assert!(
            crate::tmux::session_environment_exact(&partial_target, "T_HUB_SESSION_TOKEN")
                .unwrap()
                .is_none_or(|value| value.is_empty())
        );

        let suffix = uuid::Uuid::new_v4().simple().to_string();
        let rejected_target = format!("th_scrub_reject_{}", &suffix[..8]);
        crate::tmux::new_session_with_env(
            &rejected_target,
            "/tmp",
            Some("sleep 30"),
            &empty_candidate_session_credentials(),
        )
        .unwrap();
        guards.push(SessionGuard(rejected_target.clone()));
        let opened =
            test_observe_provider_launch_receipt(&launcher, Harness::Codex, temp.path()).unwrap();
        let mut failure_generation = observed_terminal_generation(&rejected_target);
        let dormant_generation = failure_generation.clone();
        assert!(opened
            .launch_in_exact_tmux_pane(
                Harness::Codex,
                &rejected_target,
                "/tmp",
                "codex",
                &[("T_HUB_SESSION_TOKEN".into(), "candidate-secret".into())],
                &mut failure_generation,
            )
            .is_err());
        assert_ne!(failure_generation, dormant_generation);
        assert_eq!(
            incumbent_terminal_generation(&failure_generation),
            crate::tmux::pane_generation_exact(&rejected_target).unwrap()
        );
        assert!(
            crate::tmux::session_environment_exact(&rejected_target, "T_HUB_SESSION_TOKEN")
                .unwrap()
                .is_none_or(|value| value.is_empty())
        );
        let inheritance_proof = temp.path().join("later-respawn-environment");
        let proof_command = format!(
            "if [ -z \"${{T_HUB_SESSION_TOKEN}}\" ]; then printf none > {}; else printf inherited > {}; fi; sleep 30",
            sh_single_quote(&inheritance_proof.to_string_lossy()),
            sh_single_quote(&inheritance_proof.to_string_lossy()),
        );
        crate::tmux::respawn_pane_exact(
            &rejected_target,
            "/tmp",
            &format!("sh -c {}", sh_single_quote(&proof_command)),
        )
        .unwrap();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        while !inheritance_proof.exists() && std::time::Instant::now() < deadline {
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert_eq!(std::fs::read_to_string(&inheritance_proof).unwrap(), "none");
        drop(guards);
    }

    #[cfg(unix)]
    #[test]
    fn version_probe_executes_the_opened_provider_inode() {
        use std::os::unix::fs::PermissionsExt;

        let temp = tempfile::tempdir().unwrap();
        let provider = temp.path().join("codex");
        std::fs::write(&provider, "#!/bin/sh\nprintf 'codex-cli 1.2.3\\n'\n").unwrap();
        std::fs::set_permissions(&provider, std::fs::Permissions::from_mode(0o700)).unwrap();
        let expected_identity = provider_file_identity(&provider).unwrap();
        let (version, observed_identity) = probe_provider_version(&provider).unwrap();
        assert_eq!(version, "codex-cli 1.2.3");
        assert_eq!(observed_identity, expected_identity);
    }

    #[test]
    fn process_observation_rejects_real_tmux_unique_prefix_resolution() {
        if !Command::new("tmux")
            .arg("-V")
            .output()
            .is_ok_and(|output| output.status.success())
        {
            eprintln!("tmux is unavailable - skipping prefix-collision fixture");
            return;
        }
        struct SessionGuard(String);
        impl Drop for SessionGuard {
            fn drop(&mut self) {
                let _ = crate::tmux::kill_session(&self.0);
            }
        }

        let suffix = uuid::Uuid::new_v4().simple().to_string();
        let requested = format!("th_prefix_{}", &suffix[..8]);
        let actual = format!("{requested}_foreign");
        crate::tmux::new_session_with_env(&actual, "", Some("sleep 30"), &[]).unwrap();
        let guard = SessionGuard(actual.clone());
        assert_eq!(
            observe_harness_process(&requested),
            Err(LaunchAttestationError::UnreadableEvidence)
        );
        assert!(crate::tmux::has_session(&actual));
        drop(guard);
        assert!(!crate::tmux::has_session(&actual));
    }

    #[test]
    fn codex_reasoning_attestation_matches_toml_whitespace_quotes_and_rejects_overrides() {
        for config_args in [
            vec!["-c", "model_reasoning_effort=medium"],
            vec!["-c=model_reasoning_effort = medium"],
            vec!["-cmodel_reasoning_effort = \"medium\""],
            vec!["--config", "'model_reasoning_effort' = 'medium'"],
            vec!["--config=model_reasoning_effort=medium"],
        ] {
            let mut argv = vec!["codex", "--model", "gpt-5.6-terra"];
            argv.extend(config_args);
            let evidence = HarnessProcessEvidence::test(42, 900, &argv);
            attest_runtime_policy(&evidence, Harness::Codex, "gpt-5.6-terra", "medium").unwrap();
        }

        let rejected = [
            vec![
                "codex",
                "--model",
                "gpt-5.6-terra",
                "-c",
                "model_reasoning_effort=medium",
                "-c",
                "model_reasoning_effort = \"high\"",
            ],
            vec![
                "codex",
                "--model",
                "gpt-5.6-terra",
                "-c",
                "model_reasoning_effort=high",
                "-c",
                "model_reasoning_effort = medium",
            ],
            vec![
                "codex",
                "--model",
                "gpt-5.6-terra",
                "-c",
                "model_reasoning_effort=medium",
                "-cmodel_reasoning_effort=high",
            ],
            vec![
                "codex",
                "--model",
                "gpt-5.6-terra",
                "-cmodel_reasoning_effort=high",
                "-c",
                "model_reasoning_effort=medium",
            ],
            vec![
                "codex",
                "--model",
                "gpt-5.6-terra",
                "-c",
                "model_reasoning_effort=medium",
                "-c",
                "model_reasoning_effort=medium",
            ],
            vec![
                "codex",
                "--model",
                "gpt-5.6-terra",
                "-c",
                "model_reasoning_effort = \"medium",
            ],
            vec![
                "codex",
                "--model",
                "gpt-5.6-terra",
                "-c",
                "model_reasoning_effort.extra = medium",
            ],
            vec![
                "codex",
                "--model",
                "gpt-5.6-terra",
                "-c",
                "model_reasoning_effort medium",
            ],
            vec![
                "codex",
                "--model",
                "gpt-5.6-terra",
                "-c",
                "\"model_reasoning_effort\" = \"med\\ium\"",
            ],
        ];
        for argv in rejected {
            let evidence = HarnessProcessEvidence::test(42, 900, &argv);
            assert_eq!(
                attest_runtime_policy(&evidence, Harness::Codex, "gpt-5.6-terra", "medium"),
                Err(LaunchAttestationError::WrongReasoningEffort),
                "argv was falsely approved: {argv:?}"
            );
        }
    }
}
