//! The durable Cortana record as it is stored in `captains.json`.
//!
//! This module used to plan reconciliation: rank discovered runtimes on a
//! generation ladder, decide which one was authoritative, and quarantine the
//! rest. That machinery was removed with the discovery it served (see
//! `control/cortana.rs`), and what remains is the persisted data model.
//!
//! MOST OF IT IS DORMANT. `owner`, `managed_launch`, the two attestation records,
//! `quarantine_ledger` and `legacy_orphan_provenance` are never written and never
//! read. They stay declared so an existing `captains.json` still parses without a
//! schema migration, and the first successful reconcile clears them. The live
//! fields are `identity_id`, `terminal_id`, `harness`, `provider_session_id`,
//! `conversation_id`, `checkpoint` and `recovery`.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CortanaOrphanEffectIdentity {
    pub tmux_session_id: u64,
    pub tmux_session_created: u64,
    pub tmux_window_id: u64,
    pub tmux_pane_id: u64,
    pub pane_pid: u32,
    pub pane_start_ticks: u64,
    pub pane_process_group_id: u32,
    pub pane_process_session_id: u32,
    pub foreground_pid: u32,
    pub foreground_start_ticks: u64,
    pub foreground_process_group_id: u32,
    pub foreground_process_session_id: u32,
}

/// One-use evidence recovered while upgrading to captains schema v22.
///
/// The record can only be derived from an exact healthy schema-v18 durable
/// Cortana binding. It authorizes preparing a transaction for that one terminal;
/// it does not authorize adopting the old runtime or any replacement.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CortanaLegacyOrphanProvenance {
    pub version: u32,
    pub source_schema_version: u32,
    pub identity_id: String,
    pub terminal_id: String,
    pub generation: u64,
    pub harness: String,
    pub healthy_operation_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CortanaExecutableIdentity {
    pub path: String,
    pub device: u64,
    pub inode: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CortanaManagedSystemTools {
    pub python: CortanaExecutableIdentity,
    pub systemctl: CortanaExecutableIdentity,
    pub systemd_run: CortanaExecutableIdentity,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum CortanaManagedLaunchPhase {
    Prepared,
    OwnerObserved,
    Observed,
    Claimed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CortanaManagedLaunchIntent {
    pub version: u32,
    pub operation_id: String,
    pub terminal_id: String,
    pub tmux_target: String,
    pub identity_id: String,
    pub generation: u64,
    pub harness: String,
    pub unit_name: String,
    pub launch_nonce: String,
    pub tools: CortanaManagedSystemTools,
    pub phase: CortanaManagedLaunchPhase,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expected_harness_launch_provenance: Option<crate::harness::ExpectedHarnessLaunchProvenance>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub harness_process: Option<crate::harness::HarnessProcessIdentity>,
}

/// Sanitized evidence retained after the launch WAL commits Healthy.
///
/// This contains no bearer, raw argv, or session token. It binds the expected
/// executable provenance to the exact accepted process generation so every
/// later Cortana authorization can revalidate the live runtime.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CortanaActiveHarnessAttestation {
    pub version: u32,
    pub expected_launch_provenance: crate::harness::ExpectedHarnessLaunchProvenance,
    pub process: crate::harness::HarnessProcessIdentity,
}

/// Write-ahead evidence for upgrading a live pre-attestation Cortana runtime.
///
/// The configured launch provenance and exact process generation are resolved
/// independently from the legacy durable owner. A restart must revalidate this
/// evidence before it can become active authority.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CortanaActiveHarnessAttestationRecovery {
    pub version: u32,
    pub operation_id: String,
    pub identity_id: String,
    pub generation: u64,
    pub terminal_id: String,
    pub harness: String,
    pub expected_launch_provenance: crate::harness::ExpectedHarnessLaunchProvenance,
    pub process: crate::harness::HarnessProcessIdentity,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CortanaManagedOwnerToken {
    pub version: u32,
    pub unit_name: String,
    pub invocation_id: String,
    pub cgroup_path: String,
    pub cgroup_inode: u64,
    pub launcher_pid: u32,
    pub launcher_start_ticks: u64,
    pub launch_nonce: String,
    pub tools: CortanaManagedSystemTools,
    pub tmux: CortanaOrphanEffectIdentity,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CortanaLegacyQuarantine {
    pub terminal_id: String,
    pub identity_id: String,
    pub generation: u64,
    pub harness: String,
    pub tmux: CortanaOrphanEffectIdentity,
    pub authority_revoked: bool,
    pub quarantined_at: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CortanaManagedQuarantineBasis {
    pub version: u32,
    pub claim_ship_slug: String,
    pub claim_assignment_id: String,
    pub claim_terminal_id: String,
    pub claim_harness: String,
    pub owner: CortanaManagedOwnerToken,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active_harness_attestation: Option<CortanaActiveHarnessAttestation>,
    pub replacement_generation: u64,
    pub prior_ledger_count: usize,
    pub prior_ledger_sha256: String,
    pub workspace_ids: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CortanaDurableIdentity {
    pub identity_id: Option<String>,
    pub generation: u64,
    pub terminal_id: Option<String>,
    pub harness: Option<String>,
    pub provider_session_id: Option<String>,
    pub conversation_id: Option<String>,
    pub checkpoint: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owner: Option<CortanaManagedOwnerToken>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub managed_launch: Option<CortanaManagedLaunchIntent>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active_harness_attestation: Option<CortanaActiveHarnessAttestation>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active_harness_attestation_recovery: Option<CortanaActiveHarnessAttestationRecovery>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub quarantine_ledger: Vec<CortanaLegacyQuarantine>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub legacy_orphan_provenance: Option<CortanaLegacyOrphanProvenance>,
    #[serde(default)]
    pub recovery: CortanaRecoveryState,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum CortanaRecoveryState {
    #[default]
    Uninitialized,
    Recovering {
        operation_id: String,
        started_at: u64,
    },
    /// Durable authorization to retire one exact reserved-scope runtime whose
    /// bearer no longer resolves in the identity store, then replace it at the
    /// next generation. The record is written before the external tmux effect
    /// and retained until the replacement Fleet claim and durable identity are
    /// committed together.
    ReplacingOrphan {
        operation_id: String,
        started_at: u64,
        orphan_terminal_id: String,
        orphan_identity_id: String,
        orphan_generation: u64,
        harness: String,
        effect_identity: CortanaOrphanEffectIdentity,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        managed_basis: Option<Box<CortanaManagedQuarantineBasis>>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        replacement_identity_id: Option<String>,
    },
    LegacyUnownedQuarantined {
        operation_id: String,
        quarantined_at: u64,
        legacy_terminal_id: String,
        legacy_generation: u64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        replacement_identity_id: Option<String>,
    },
    Healthy {
        operation_id: String,
        verified_at: u64,
    },
    Degraded {
        operation_id: String,
        reason: String,
        detected_at: u64,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum CortanaReconcileAction {
    Keep,
    Adopt,
    Recover,
    Create,
    Degraded,
}
