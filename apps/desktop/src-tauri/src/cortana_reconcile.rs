//! Pure planning for the durable Cortana singleton.
//!
//! Runtime discovery and mutation stay in the control plane.
//! This module decides which observed runtime, if any, is authoritative and
//! refuses to guess when identity, generation, or liveness evidence is unclear.

use serde::{Deserialize, Serialize};
use std::collections::{BTreeSet, HashSet};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum RuntimeEvidence {
    Alive,
    Gone,
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CortanaRuntimeCandidate {
    pub terminal_id: String,
    pub identity_id: Option<String>,
    pub generation: u64,
    pub harness: String,
    pub provider_session_id: Option<String>,
    pub terminal: RuntimeEvidence,
    pub harness_process: RuntimeEvidence,
    pub identity_bound_to_terminal: bool,
    pub canonical_control_file: bool,
    pub rotating_control_env_scrubbed: bool,
    /// An older T-Hub injected one rotating endpoint directly into Cortana.
    /// This is retirement-only evidence: both values must be non-empty and
    /// demonstrably different from the current listener, and the stable
    /// discovery file must be absent. It must never authorize adoption.
    pub stale_legacy_control_env: bool,
    /// The runtime presented a non-empty per-session bearer which no longer
    /// resolves in the current durable identity store.
    pub unresolved_session_bearer: bool,
    /// Exact Linux/tmux process generation authorized for a legacy retirement.
    /// Adoption never relies on this evidence.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effect_identity: Option<CortanaOrphanEffectIdentity>,
    pub current_control_capability: bool,
    pub trusted_cortana_identity: bool,
}

pub const LEGACY_ORPHAN_PROVENANCE_VERSION: u32 = 1;
pub const MANAGED_OWNER_TOKEN_VERSION: u32 = 2;

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

pub const ACTIVE_HARNESS_ATTESTATION_VERSION: u32 = 1;

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

pub const ACTIVE_HARNESS_ATTESTATION_RECOVERY_VERSION: u32 = 1;

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

pub const MANAGED_QUARANTINE_BASIS_VERSION: u32 = 1;

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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CortanaReconcilePlan {
    pub operation_id: String,
    pub action: CortanaReconcileAction,
    pub authoritative: Option<CortanaRuntimeCandidate>,
    pub retire_terminal_ids: Vec<String>,
    #[serde(default)]
    pub quarantine_terminal_ids: Vec<String>,
    pub next_generation: u64,
    pub degraded_reason: Option<String>,
}

impl CortanaReconcilePlan {
    fn degraded(operation_id: &str, generation: u64, reason: impl Into<String>) -> Self {
        Self {
            operation_id: operation_id.to_string(),
            action: CortanaReconcileAction::Degraded,
            authoritative: None,
            retire_terminal_ids: Vec::new(),
            quarantine_terminal_ids: Vec::new(),
            next_generation: generation,
            degraded_reason: Some(reason.into()),
        }
    }

    fn degraded_with_quarantine(
        operation_id: &str,
        generation: u64,
        reason: impl Into<String>,
        quarantine_terminal_ids: Vec<String>,
    ) -> Self {
        Self {
            operation_id: operation_id.to_string(),
            action: CortanaReconcileAction::Degraded,
            authoritative: None,
            retire_terminal_ids: Vec::new(),
            quarantine_terminal_ids,
            next_generation: generation,
            degraded_reason: Some(reason.into()),
        }
    }
}

fn full_capability_runtime(candidate: &CortanaRuntimeCandidate) -> bool {
    candidate.terminal == RuntimeEvidence::Alive
        && candidate.harness_process == RuntimeEvidence::Alive
        && candidate.identity_bound_to_terminal
        && candidate.canonical_control_file
        && candidate.rotating_control_env_scrubbed
        && candidate.current_control_capability
        && candidate.trusted_cortana_identity
        && candidate.identity_id.is_some()
}

fn valid_runtime(candidate: &CortanaRuntimeCandidate) -> bool {
    full_capability_runtime(candidate) && candidate.generation > 0
}

/// Select the single authoritative Cortana runtime from bounded observations.
///
/// The caller supplies every runtime discovered in Cortana's reserved home.
/// A live runtime with untrusted authority, an unknown liveness result, or two
/// candidates at the same highest generation makes the result degraded.
/// Same-generation owned duplicates are returned as exact quarantine targets
/// without choosing an authority.
/// Lower trusted generations can be safely retired only after one strictly
/// higher authoritative generation has been selected.
/// One trusted generation-zero runtime can be migrated to generation one only
/// while the durable singleton has neither an identity nor a generation.
pub fn plan_reconciliation(
    durable: &CortanaDurableIdentity,
    operation_id: &str,
    candidates: &[CortanaRuntimeCandidate],
) -> CortanaReconcilePlan {
    let operation_id = operation_id.trim();
    if operation_id.is_empty() {
        return CortanaReconcilePlan::degraded(
            "invalid-operation",
            durable.generation,
            "Cortana reconciliation requires a stable non-empty operation identity",
        );
    }

    let mut terminal_ids = HashSet::new();
    if candidates
        .iter()
        .any(|candidate| !terminal_ids.insert(candidate.terminal_id.as_str()))
    {
        return CortanaReconcilePlan::degraded(
            operation_id,
            durable.generation,
            "Cortana runtime discovery returned a duplicate terminal identity",
        );
    }

    if let Some(candidate) = candidates.iter().find(|candidate| {
        candidate.terminal == RuntimeEvidence::Unknown
            || candidate.terminal == RuntimeEvidence::Alive
                && candidate.harness_process == RuntimeEvidence::Unknown
    }) {
        return CortanaReconcilePlan::degraded(
            operation_id,
            durable.generation,
            format!(
                "Cortana runtime '{}' has uncertain liveness evidence",
                candidate.terminal_id
            ),
        );
    }

    if let Some(candidate) = candidates.iter().find(|candidate| {
        candidate.terminal == RuntimeEvidence::Alive
            && (!candidate.current_control_capability
                || !candidate.trusted_cortana_identity
                || candidate.identity_id.is_none()
                || !candidate.identity_bound_to_terminal
                || !candidate.canonical_control_file
                || !candidate.rotating_control_env_scrubbed
                || candidate.generation == 0 && candidate.harness_process != RuntimeEvidence::Alive)
    }) {
        return CortanaReconcilePlan::degraded(
            operation_id,
            durable.generation,
            format!(
                "live runtime '{}' in Cortana's reserved scope lacks authoritative identity, generation, or control evidence",
                candidate.terminal_id
            ),
        );
    }

    let trusted_live = candidates
        .iter()
        .filter(|candidate| full_capability_runtime(candidate))
        .cloned()
        .collect::<Vec<_>>();

    if let Some(identity_id) = durable.identity_id.as_deref() {
        if let Some(candidate) = trusted_live
            .iter()
            .find(|candidate| candidate.identity_id.as_deref() != Some(identity_id))
        {
            return CortanaReconcilePlan::degraded(
                operation_id,
                durable.generation,
                format!(
                    "runtime '{}' presents a different Cortana identity than the durable singleton",
                    candidate.terminal_id
                ),
            );
        }
    }

    if let Some(harness) = durable.harness.as_deref() {
        if let Some(candidate) = trusted_live
            .iter()
            .find(|candidate| candidate.harness != harness)
        {
            return CortanaReconcilePlan::degraded(
                operation_id,
                durable.generation,
                format!(
                    "runtime '{}' uses harness '{}' instead of durable harness '{}'",
                    candidate.terminal_id, candidate.harness, harness
                ),
            );
        }
    }

    let generation_zero = trusted_live
        .iter()
        .filter(|candidate| candidate.generation == 0)
        .collect::<Vec<_>>();
    if !generation_zero.is_empty() {
        let durable_is_uninitialized = durable.identity_id.is_none() && durable.generation == 0;
        if durable_is_uninitialized && generation_zero.len() == 1 && trusted_live.len() == 1 {
            let mut authoritative = generation_zero[0].clone();
            authoritative.generation = 1;
            return CortanaReconcilePlan {
                operation_id: operation_id.to_string(),
                action: CortanaReconcileAction::Adopt,
                authoritative: Some(authoritative),
                retire_terminal_ids: Vec::new(),
                quarantine_terminal_ids: Vec::new(),
                next_generation: 2,
                degraded_reason: None,
            };
        }
        return CortanaReconcilePlan::degraded(
            operation_id,
            durable.generation,
            "a generation-zero Cortana runtime may only be migrated when it is the sole trusted live full-capability candidate and no durable identity or generation exists",
        );
    }

    let mut viable = trusted_live
        .into_iter()
        .filter(valid_runtime)
        .collect::<Vec<_>>();

    viable.sort_by(|left, right| {
        right
            .generation
            .cmp(&left.generation)
            .then_with(|| left.terminal_id.cmp(&right.terminal_id))
    });
    let highest_generation = viable
        .first()
        .map(|candidate| candidate.generation)
        .unwrap_or(durable.generation);

    let highest_candidates = viable
        .iter()
        .filter(|candidate| candidate.generation == highest_generation)
        .collect::<Vec<_>>();
    if highest_candidates.len() > 1 {
        let identity_id = highest_candidates[0].identity_id.as_deref();
        if highest_candidates
            .iter()
            .any(|candidate| candidate.identity_id.as_deref() != identity_id)
        {
            return CortanaReconcilePlan::degraded(
                operation_id,
                highest_generation,
                format!(
                    "multiple trusted Cortana identities claim generation {highest_generation}"
                ),
            );
        }
        let harness = highest_candidates[0].harness.as_str();
        if highest_candidates
            .iter()
            .any(|candidate| candidate.harness != harness)
        {
            return CortanaReconcilePlan::degraded(
                operation_id,
                highest_generation,
                format!(
                    "multiple Cortana harnesses claim authoritative generation {highest_generation}"
                ),
            );
        }
        let quarantine_terminal_ids = highest_candidates
            .iter()
            .map(|candidate| candidate.terminal_id.clone())
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect();
        return CortanaReconcilePlan::degraded_with_quarantine(
            operation_id,
            highest_generation,
            format!(
                "multiple Cortana runtimes claim authoritative generation {highest_generation}"
            ),
            quarantine_terminal_ids,
        );
    }

    if highest_generation < durable.generation {
        return CortanaReconcilePlan::degraded(
            operation_id,
            durable.generation,
            "all observed Cortana runtimes are older than the durable generation",
        );
    }

    let Some(authoritative) = viable.first().cloned() else {
        return CortanaReconcilePlan {
            operation_id: operation_id.to_string(),
            action: if durable.identity_id.is_some() {
                CortanaReconcileAction::Recover
            } else {
                CortanaReconcileAction::Create
            },
            authoritative: None,
            retire_terminal_ids: Vec::new(),
            quarantine_terminal_ids: Vec::new(),
            next_generation: durable.generation.saturating_add(1).max(1),
            degraded_reason: None,
        };
    };

    let retire_terminal_ids = viable
        .iter()
        .filter(|candidate| candidate.generation < authoritative.generation)
        .map(|candidate| candidate.terminal_id.clone())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    let same_runtime = durable.terminal_id.as_deref() == Some(authoritative.terminal_id.as_str())
        && durable.generation == authoritative.generation;
    let action = if same_runtime {
        CortanaReconcileAction::Keep
    } else if durable.identity_id.is_some() {
        CortanaReconcileAction::Recover
    } else {
        CortanaReconcileAction::Adopt
    };

    CortanaReconcilePlan {
        operation_id: operation_id.to_string(),
        action,
        next_generation: authoritative.generation.saturating_add(1),
        authoritative: Some(authoritative),
        retire_terminal_ids,
        quarantine_terminal_ids: Vec::new(),
        degraded_reason: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn candidate(id: &str, identity: &str, generation: u64) -> CortanaRuntimeCandidate {
        CortanaRuntimeCandidate {
            terminal_id: id.into(),
            identity_id: Some(identity.into()),
            generation,
            harness: "codex".into(),
            provider_session_id: Some("thread-1".into()),
            terminal: RuntimeEvidence::Alive,
            harness_process: RuntimeEvidence::Alive,
            identity_bound_to_terminal: true,
            canonical_control_file: true,
            rotating_control_env_scrubbed: true,
            stale_legacy_control_env: false,
            unresolved_session_bearer: false,
            effect_identity: None,
            current_control_capability: true,
            trusted_cortana_identity: true,
        }
    }

    fn durable() -> CortanaDurableIdentity {
        CortanaDurableIdentity {
            identity_id: Some("cortana-identity".into()),
            generation: 4,
            terminal_id: Some("term-4".into()),
            harness: Some("codex".into()),
            provider_session_id: Some("thread-1".into()),
            conversation_id: Some("conversation-1".into()),
            checkpoint: Some("checkpoint-1".into()),
            owner: None,
            managed_launch: None,
            active_harness_attestation: None,
            active_harness_attestation_recovery: None,
            quarantine_ledger: Vec::new(),
            legacy_orphan_provenance: None,
            recovery: CortanaRecoveryState::Healthy {
                operation_id: "startup-3".into(),
                verified_at: 10,
            },
        }
    }

    #[test]
    fn keeps_the_exact_healthy_generation() {
        let plan = plan_reconciliation(
            &durable(),
            "startup-4",
            &[candidate("term-4", "cortana-identity", 4)],
        );
        assert_eq!(plan.action, CortanaReconcileAction::Keep);
        assert_eq!(plan.authoritative.unwrap().terminal_id, "term-4");
        assert_eq!(plan.next_generation, 5);
        assert!(plan.retire_terminal_ids.is_empty());
        assert!(plan.quarantine_terminal_ids.is_empty());
    }

    #[test]
    fn selects_the_highest_generation_and_retires_only_older_trusted_candidates() {
        let plan = plan_reconciliation(
            &durable(),
            "startup-5",
            &[
                candidate("term-4", "cortana-identity", 4),
                candidate("term-5", "cortana-identity", 5),
            ],
        );
        assert_eq!(plan.action, CortanaReconcileAction::Recover);
        assert_eq!(plan.authoritative.unwrap().terminal_id, "term-5");
        assert_eq!(plan.retire_terminal_ids, vec!["term-4"]);
        assert!(plan.quarantine_terminal_ids.is_empty());
        assert_eq!(plan.next_generation, 6);
    }

    #[test]
    fn equal_highest_generations_fail_closed_with_exact_quarantine_targets() {
        let plan = plan_reconciliation(
            &durable(),
            "startup-5",
            &[
                candidate("term-b", "cortana-identity", 5),
                candidate("term-old", "cortana-identity", 4),
                candidate("term-a", "cortana-identity", 5),
                candidate("term-c", "cortana-identity", 5),
            ],
        );
        assert_eq!(plan.action, CortanaReconcileAction::Degraded);
        assert!(plan.authoritative.is_none());
        assert!(plan.retire_terminal_ids.is_empty());
        assert_eq!(
            plan.quarantine_terminal_ids,
            vec!["term-a", "term-b", "term-c"]
        );
        assert!(plan
            .degraded_reason
            .unwrap()
            .contains("multiple Cortana runtimes"));
    }

    #[test]
    fn duplicate_quarantine_order_is_independent_of_discovery_order() {
        let first = plan_reconciliation(
            &durable(),
            "startup-5",
            &[
                candidate("term-z", "cortana-identity", 5),
                candidate("term-a", "cortana-identity", 5),
            ],
        );
        let second = plan_reconciliation(
            &durable(),
            "startup-5",
            &[
                candidate("term-a", "cortana-identity", 5),
                candidate("term-z", "cortana-identity", 5),
            ],
        );
        assert_eq!(
            first.quarantine_terminal_ids,
            second.quarantine_terminal_ids
        );
        assert_eq!(first.quarantine_terminal_ids, vec!["term-a", "term-z"]);
    }

    #[test]
    fn migrates_one_legacy_runtime_to_an_adopted_generation_one_candidate() {
        let legacy = candidate("term-legacy", "legacy-cortana", 0);
        let plan = plan_reconciliation(
            &CortanaDurableIdentity::default(),
            "startup-1",
            std::slice::from_ref(&legacy),
        );

        assert_eq!(plan.action, CortanaReconcileAction::Adopt);
        assert_eq!(plan.next_generation, 2);
        assert!(plan.retire_terminal_ids.is_empty());
        assert!(plan.quarantine_terminal_ids.is_empty());
        let authoritative = plan.authoritative.unwrap();
        assert_eq!(authoritative.terminal_id, "term-legacy");
        assert_eq!(authoritative.identity_id.as_deref(), Some("legacy-cortana"));
        assert_eq!(authoritative.generation, 1);
        assert_eq!(legacy.generation, 0);
    }

    #[test]
    fn migration_requires_no_durable_identity_and_no_durable_generation() {
        let durable_identity = CortanaDurableIdentity {
            identity_id: Some("legacy-cortana".into()),
            ..CortanaDurableIdentity::default()
        };
        let durable_generation = CortanaDurableIdentity {
            generation: 1,
            ..CortanaDurableIdentity::default()
        };

        for durable in [durable_identity, durable_generation] {
            let plan = plan_reconciliation(
                &durable,
                "startup-1",
                &[candidate("term-legacy", "legacy-cortana", 0)],
            );
            assert_eq!(plan.action, CortanaReconcileAction::Degraded);
            assert!(plan.authoritative.is_none());
            assert!(plan.quarantine_terminal_ids.is_empty());
            assert!(plan.degraded_reason.unwrap().contains("generation-zero"));
        }
    }

    #[test]
    fn migration_requires_exactly_one_trusted_live_full_capability_candidate() {
        let uninitialized = CortanaDurableIdentity::default();
        let cases = [
            vec![
                candidate("term-a", "legacy-cortana", 0),
                candidate("term-b", "legacy-cortana", 0),
            ],
            vec![
                candidate("term-legacy", "legacy-cortana", 0),
                candidate("term-new", "legacy-cortana", 1),
            ],
        ];

        for candidates in cases {
            let plan = plan_reconciliation(&uninitialized, "startup-1", &candidates);
            assert_eq!(plan.action, CortanaReconcileAction::Degraded);
            assert!(plan.authoritative.is_none());
            assert!(plan.retire_terminal_ids.is_empty());
            assert!(plan.quarantine_terminal_ids.is_empty());
        }
    }

    #[test]
    fn incomplete_generation_zero_candidates_are_never_adopted() {
        let mut terminal_unknown = candidate("term-terminal-unknown", "legacy-cortana", 0);
        terminal_unknown.terminal = RuntimeEvidence::Unknown;
        let mut terminal_gone = candidate("term-terminal-gone", "legacy-cortana", 0);
        terminal_gone.terminal = RuntimeEvidence::Gone;
        let mut harness_unknown = candidate("term-harness-unknown", "legacy-cortana", 0);
        harness_unknown.harness_process = RuntimeEvidence::Unknown;
        let mut harness_gone = candidate("term-harness-gone", "legacy-cortana", 0);
        harness_gone.harness_process = RuntimeEvidence::Gone;
        let mut no_control = candidate("term-no-control", "legacy-cortana", 0);
        no_control.current_control_capability = false;
        let mut untrusted = candidate("term-untrusted", "legacy-cortana", 0);
        untrusted.trusted_cortana_identity = false;
        let mut no_identity = candidate("term-no-identity", "legacy-cortana", 0);
        no_identity.identity_id = None;

        for runtime in [
            terminal_unknown,
            terminal_gone,
            harness_unknown,
            harness_gone,
            no_control,
            untrusted,
            no_identity,
        ] {
            let plan =
                plan_reconciliation(&CortanaDurableIdentity::default(), "startup-1", &[runtime]);
            assert_ne!(plan.action, CortanaReconcileAction::Adopt);
            assert!(plan.authoritative.is_none());
            assert!(plan.quarantine_terminal_ids.is_empty());
        }
    }

    #[test]
    fn uncertain_liveness_fails_closed() {
        let mut runtime = candidate("term-4", "cortana-identity", 4);
        runtime.harness_process = RuntimeEvidence::Unknown;
        let plan = plan_reconciliation(&durable(), "startup-4", &[runtime]);
        assert_eq!(plan.action, CortanaReconcileAction::Degraded);
        assert!(plan.quarantine_terminal_ids.is_empty());
        assert!(plan.degraded_reason.unwrap().contains("uncertain liveness"));
    }

    #[test]
    fn unsafe_observations_never_become_quarantine_targets() {
        let duplicate_a = candidate("term-a", "cortana-identity", 5);
        let duplicate_b = candidate("term-b", "cortana-identity", 5);
        let mut foreign = candidate("term-foreign", "foreign-identity", 5);
        foreign.trusted_cortana_identity = false;
        let mut no_control = candidate("term-no-control", "cortana-identity", 5);
        no_control.current_control_capability = false;
        let mut terminal_unknown = candidate("term-terminal-unknown", "cortana-identity", 5);
        terminal_unknown.terminal = RuntimeEvidence::Unknown;
        let mut harness_unknown = candidate("term-harness-unknown", "cortana-identity", 5);
        harness_unknown.harness_process = RuntimeEvidence::Unknown;
        let mut wrong_harness = candidate("term-wrong-harness", "cortana-identity", 5);
        wrong_harness.harness = "claude".into();
        let wrong_identity = candidate("term-wrong-identity", "other-cortana-identity", 5);

        for unsafe_candidate in [
            foreign,
            no_control,
            terminal_unknown,
            harness_unknown,
            wrong_harness,
            wrong_identity,
        ] {
            let plan = plan_reconciliation(
                &durable(),
                "startup-5",
                &[duplicate_a.clone(), duplicate_b.clone(), unsafe_candidate],
            );
            assert_eq!(plan.action, CortanaReconcileAction::Degraded);
            assert!(plan.authoritative.is_none());
            assert!(plan.retire_terminal_ids.is_empty());
            assert!(plan.quarantine_terminal_ids.is_empty());
        }
    }

    #[test]
    fn non_live_candidates_are_excluded_from_exact_duplicate_quarantine_targets() {
        let mut terminal_gone = candidate("term-terminal-gone", "cortana-identity", 5);
        terminal_gone.terminal = RuntimeEvidence::Gone;
        let mut harness_gone = candidate("term-harness-gone", "cortana-identity", 5);
        harness_gone.harness_process = RuntimeEvidence::Gone;

        let plan = plan_reconciliation(
            &durable(),
            "startup-5",
            &[
                candidate("term-b", "cortana-identity", 5),
                terminal_gone,
                harness_gone,
                candidate("term-a", "cortana-identity", 5),
            ],
        );

        assert_eq!(plan.action, CortanaReconcileAction::Degraded);
        assert_eq!(plan.quarantine_terminal_ids, vec!["term-a", "term-b"]);
    }

    #[test]
    fn conflicting_undurable_identity_or_harness_evidence_is_not_owned_for_quarantine() {
        let different_identities = plan_reconciliation(
            &CortanaDurableIdentity::default(),
            "startup-5",
            &[
                candidate("term-a", "cortana-a", 5),
                candidate("term-b", "cortana-b", 5),
            ],
        );
        assert_eq!(
            different_identities.action,
            CortanaReconcileAction::Degraded
        );
        assert!(different_identities.quarantine_terminal_ids.is_empty());
        assert!(different_identities
            .degraded_reason
            .unwrap()
            .contains("trusted Cortana identities"));

        let mut claude = candidate("term-b", "cortana-a", 5);
        claude.harness = "claude".into();
        let different_harnesses = plan_reconciliation(
            &CortanaDurableIdentity::default(),
            "startup-5",
            &[candidate("term-a", "cortana-a", 5), claude],
        );
        assert_eq!(different_harnesses.action, CortanaReconcileAction::Degraded);
        assert!(different_harnesses.quarantine_terminal_ids.is_empty());
        assert!(different_harnesses
            .degraded_reason
            .unwrap()
            .contains("Cortana harnesses"));
    }

    #[test]
    fn a_foreign_identity_cannot_take_over_with_a_higher_generation() {
        let plan = plan_reconciliation(
            &durable(),
            "startup-9",
            &[candidate("term-9", "foreign-identity", 9)],
        );
        assert_eq!(plan.action, CortanaReconcileAction::Degraded);
        assert!(plan.quarantine_terminal_ids.is_empty());
        assert!(plan
            .degraded_reason
            .unwrap()
            .contains("different Cortana identity"));
    }

    #[test]
    fn an_untrusted_live_runtime_blocks_automatic_recovery() {
        let mut runtime = candidate("term-9", "cortana-identity", 9);
        runtime.current_control_capability = false;
        let plan = plan_reconciliation(&durable(), "startup-9", &[runtime]);
        assert_eq!(plan.action, CortanaReconcileAction::Degraded);
        assert!(plan.quarantine_terminal_ids.is_empty());
        assert!(plan
            .degraded_reason
            .unwrap()
            .contains("lacks authoritative identity"));
    }

    #[test]
    fn no_runtime_recovers_the_durable_identity_at_the_next_generation() {
        let plan = plan_reconciliation(&durable(), "startup-5", &[]);
        assert_eq!(plan.action, CortanaReconcileAction::Recover);
        assert_eq!(plan.next_generation, 5);
        assert!(plan.authoritative.is_none());
    }

    #[test]
    fn no_identity_creates_the_first_generation() {
        let plan = plan_reconciliation(&CortanaDurableIdentity::default(), "startup-1", &[]);
        assert_eq!(plan.action, CortanaReconcileAction::Create);
        assert_eq!(plan.next_generation, 1);
    }

    #[test]
    fn a_stable_operation_identity_is_required() {
        let plan = plan_reconciliation(&durable(), " ", &[]);
        assert_eq!(plan.action, CortanaReconcileAction::Degraded);
        assert_eq!(plan.operation_id, "invalid-operation");
    }

    #[test]
    fn changing_the_harness_requires_an_explicit_durable_update() {
        let mut runtime = candidate("term-5", "cortana-identity", 5);
        runtime.harness = "claude".into();
        let plan = plan_reconciliation(&durable(), "startup-5", &[runtime]);
        assert_eq!(plan.action, CortanaReconcileAction::Degraded);
        assert!(plan.degraded_reason.unwrap().contains("durable harness"));
    }

    #[test]
    fn exact_runtime_binding_and_control_environment_are_mandatory() {
        for mutate in [
            |candidate: &mut CortanaRuntimeCandidate| candidate.identity_bound_to_terminal = false,
            |candidate: &mut CortanaRuntimeCandidate| candidate.canonical_control_file = false,
            |candidate: &mut CortanaRuntimeCandidate| {
                candidate.rotating_control_env_scrubbed = false
            },
        ] {
            let mut observed = candidate("term-4", "cortana-identity", 4);
            mutate(&mut observed);
            let plan = plan_reconciliation(&durable(), "startup-binding", &[observed]);
            assert_eq!(plan.action, CortanaReconcileAction::Degraded);
            assert!(plan.authoritative.is_none());
        }
    }
}
