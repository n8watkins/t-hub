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
    pub current_control_capability: bool,
    pub trusted_cortana_identity: bool,
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
            next_generation: generation,
            degraded_reason: Some(reason.into()),
        }
    }
}

fn valid_runtime(candidate: &CortanaRuntimeCandidate) -> bool {
    candidate.terminal == RuntimeEvidence::Alive
        && candidate.harness_process == RuntimeEvidence::Alive
        && candidate.current_control_capability
        && candidate.trusted_cortana_identity
        && candidate.identity_id.is_some()
        && candidate.generation > 0
}

/// Select the single authoritative Cortana runtime from bounded observations.
///
/// The caller supplies every runtime discovered in Cortana's reserved home.
/// A live runtime with untrusted authority, an unknown liveness result, or two
/// candidates at the same highest generation makes the result degraded.
/// Lower trusted generations can be safely retired only after one strictly
/// higher authoritative generation has been selected.
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
                || candidate.generation == 0)
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

    let mut viable = candidates
        .iter()
        .filter(|candidate| valid_runtime(candidate))
        .cloned()
        .collect::<Vec<_>>();

    if let Some(identity_id) = durable.identity_id.as_deref() {
        if let Some(candidate) = viable
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
        if let Some(candidate) = viable.iter().find(|candidate| candidate.harness != harness) {
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

    if viable
        .iter()
        .filter(|candidate| candidate.generation == highest_generation)
        .count()
        > 1
    {
        return CortanaReconcilePlan::degraded(
            operation_id,
            highest_generation,
            format!(
                "multiple Cortana runtimes claim authoritative generation {highest_generation}"
            ),
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
        assert_eq!(plan.next_generation, 6);
    }

    #[test]
    fn equal_highest_generations_fail_closed() {
        let plan = plan_reconciliation(
            &durable(),
            "startup-5",
            &[
                candidate("term-a", "cortana-identity", 5),
                candidate("term-b", "cortana-identity", 5),
            ],
        );
        assert_eq!(plan.action, CortanaReconcileAction::Degraded);
        assert!(plan
            .degraded_reason
            .unwrap()
            .contains("multiple Cortana runtimes"));
    }

    #[test]
    fn uncertain_liveness_fails_closed() {
        let mut runtime = candidate("term-4", "cortana-identity", 4);
        runtime.harness_process = RuntimeEvidence::Unknown;
        let plan = plan_reconciliation(&durable(), "startup-4", &[runtime]);
        assert_eq!(plan.action, CortanaReconcileAction::Degraded);
        assert!(plan.degraded_reason.unwrap().contains("uncertain liveness"));
    }

    #[test]
    fn a_foreign_identity_cannot_take_over_with_a_higher_generation() {
        let plan = plan_reconciliation(
            &durable(),
            "startup-9",
            &[candidate("term-9", "foreign-identity", 9)],
        );
        assert_eq!(plan.action, CortanaReconcileAction::Degraded);
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
}
