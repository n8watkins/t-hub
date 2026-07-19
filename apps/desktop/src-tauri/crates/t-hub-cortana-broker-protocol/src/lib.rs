use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;

pub const BROKER_SCHEMA_VERSION: u32 = 1;
pub const MAX_ID: usize = 128;
pub const MAX_POLICY_VALUE: usize = 128;
pub const MAX_VERSION: usize = 256;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CortanaBrokerRequest {
    pub schema_version: u32,
    pub operation_id: String,
    pub request_digest: String,
    pub scope: CortanaScopeBinding,
    pub candidate: CandidateGeneration,
    pub policy: RuntimePolicyKey,
    pub handles: BrokerHandleLayout,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CortanaScopeBinding {
    pub project_id: String,
    pub ship_id: String,
    pub captain_id: String,
    pub cortana_identity_id: String,
    pub cortana_authority_generation: u64,
    pub session_identity_id: String,
    pub session_identity_generation: u64,
    pub terminal_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CandidateGeneration {
    pub pid: u32,
    pub start_ticks: u64,
    pub tmux_session_id: u64,
    pub tmux_session_created: u64,
    pub tmux_window_id: u64,
    pub tmux_pane_id: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RuntimePolicyKey {
    pub catalog_version: u32,
    pub provider: String,
    pub harness: String,
    pub model: String,
    pub reasoning_effort: String,
    pub permission: String,
    pub capability: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BrokerHandleLayout {
    pub candidate_pidfd: u8,
    pub provider_executable: u8,
    pub credential_pipe: u8,
    pub launch_gate: u8,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ObservedPeer {
    pub uid: u32,
    pub pid: u32,
    pub process_start_ticks: u64,
    pub executable_device: u64,
    pub executable_inode: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProtectedClient {
    pub uid: u32,
    pub executable_device: u64,
    pub executable_inode: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProtectedPolicy {
    pub key: RuntimePolicyKey,
    pub executable_device: u64,
    pub executable_inode: u64,
    pub executable_version: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProtectedBrokerPolicy {
    pub client: ProtectedClient,
    pub runtimes: Vec<ProtectedPolicy>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BrokerLaunchReceipt {
    pub schema_version: u32,
    pub operation_id: String,
    pub request_digest: String,
    pub broker_generation: u64,
    pub cgroup_device: u64,
    pub cgroup_inode: u64,
    pub candidate_pid: u32,
    pub candidate_start_ticks: u64,
    pub executable_device: u64,
    pub executable_inode: u64,
    pub executable_version: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", tag = "status", content = "value")]
pub enum BrokerResponse {
    Committed(BrokerLaunchReceipt),
    Refused(BrokerRefusal),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum BrokerRefusal {
    Malformed,
    UnauthorizedPeer,
    ScopeConflict,
    DigestConflict,
    StaleGeneration,
    UnsupportedPolicy,
    InvalidHandles,
    CandidateMismatch,
    AuthorityUnavailable,
    CleanupPending,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ValidatedBrokerRequest {
    pub request: CortanaBrokerRequest,
    pub policy: ProtectedPolicy,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DurableBrokerOperation {
    pub request_digest: String,
    pub scope: CortanaScopeBinding,
    pub candidate: CandidateGeneration,
    pub phase: DurableBrokerPhase,
    pub receipt: Option<BrokerLaunchReceipt>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DurableBrokerPhase {
    Prepared,
    Contained,
    Committed,
    CleanupPending,
}

#[derive(Debug, Default)]
pub struct FakeBrokerJournal {
    operations: BTreeMap<String, DurableBrokerOperation>,
    authority_generation: u64,
}

impl CortanaBrokerRequest {
    pub fn canonical_digest(&self) -> Result<String, BrokerRefusal> {
        let mut canonical = self.clone();
        canonical.request_digest.clear();
        let encoded = serde_json::to_vec(&canonical).map_err(|_| BrokerRefusal::Malformed)?;
        Ok(lower_hex(&Sha256::digest(encoded)))
    }

    pub fn validate(
        &self,
        peer: &ObservedPeer,
        protected: &ProtectedBrokerPolicy,
    ) -> Result<ValidatedBrokerRequest, BrokerRefusal> {
        if self.schema_version != BROKER_SCHEMA_VERSION
            || !valid_operation_id(&self.operation_id)
            || !valid_digest(&self.request_digest)
            || self.canonical_digest()? != self.request_digest
            || !valid_scope(&self.scope)
            || !valid_candidate(&self.candidate)
            || !valid_policy_key(&self.policy)
            || !valid_handles(&self.handles)
        {
            return Err(BrokerRefusal::Malformed);
        }
        if peer.uid != protected.client.uid
            || peer.pid == 0
            || peer.process_start_ticks == 0
            || peer.executable_device != protected.client.executable_device
            || peer.executable_inode != protected.client.executable_inode
        {
            return Err(BrokerRefusal::UnauthorizedPeer);
        }
        let policy = protected
            .runtimes
            .iter()
            .find(|allowed| allowed.key == self.policy)
            .cloned()
            .ok_or(BrokerRefusal::UnsupportedPolicy)?;
        if policy.executable_device == 0
            || policy.executable_inode == 0
            || policy.executable_version.is_empty()
            || policy.executable_version.len() > MAX_VERSION
            || policy.executable_version.trim() != policy.executable_version
        {
            return Err(BrokerRefusal::UnsupportedPolicy);
        }
        Ok(ValidatedBrokerRequest {
            request: self.clone(),
            policy,
        })
    }
}

impl FakeBrokerJournal {
    pub fn prepare(
        &mut self,
        validated: ValidatedBrokerRequest,
    ) -> Result<DurableBrokerOperation, BrokerRefusal> {
        let request = validated.request;
        if let Some(existing) = self.operations.get(&request.operation_id) {
            if existing.request_digest != request.request_digest {
                return Err(BrokerRefusal::DigestConflict);
            }
            return Ok(existing.clone());
        }
        if let Some(existing) = self.operations.values().find(|existing| {
            existing.scope.cortana_identity_id == request.scope.cortana_identity_id
                && existing.scope.project_id == request.scope.project_id
                && existing.phase != DurableBrokerPhase::CleanupPending
        }) {
            if existing.scope != request.scope {
                return Err(BrokerRefusal::ScopeConflict);
            }
            if existing.candidate != request.candidate {
                return Err(BrokerRefusal::CandidateMismatch);
            }
        }
        let operation = DurableBrokerOperation {
            request_digest: request.request_digest.clone(),
            scope: request.scope,
            candidate: request.candidate,
            phase: DurableBrokerPhase::Prepared,
            receipt: None,
        };
        self.operations
            .insert(request.operation_id, operation.clone());
        Ok(operation)
    }

    pub fn commit(
        &mut self,
        operation_id: &str,
        request_digest: &str,
        cgroup_device: u64,
        cgroup_inode: u64,
        policy: &ProtectedPolicy,
    ) -> Result<BrokerLaunchReceipt, BrokerRefusal> {
        let operation = self
            .operations
            .get_mut(operation_id)
            .ok_or(BrokerRefusal::Malformed)?;
        if operation.request_digest != request_digest {
            return Err(BrokerRefusal::DigestConflict);
        }
        if let Some(receipt) = &operation.receipt {
            return Ok(receipt.clone());
        }
        if cgroup_device == 0 || cgroup_inode == 0 {
            return Err(BrokerRefusal::CandidateMismatch);
        }
        self.authority_generation = self
            .authority_generation
            .checked_add(1)
            .ok_or(BrokerRefusal::AuthorityUnavailable)?;
        let receipt = BrokerLaunchReceipt {
            schema_version: BROKER_SCHEMA_VERSION,
            operation_id: operation_id.to_string(),
            request_digest: request_digest.to_string(),
            broker_generation: self.authority_generation,
            cgroup_device,
            cgroup_inode,
            candidate_pid: operation.candidate.pid,
            candidate_start_ticks: operation.candidate.start_ticks,
            executable_device: policy.executable_device,
            executable_inode: policy.executable_inode,
            executable_version: policy.executable_version.clone(),
        };
        operation.phase = DurableBrokerPhase::Committed;
        operation.receipt = Some(receipt.clone());
        Ok(receipt)
    }

    pub fn mark_contained(
        &mut self,
        operation_id: &str,
        request_digest: &str,
    ) -> Result<(), BrokerRefusal> {
        let operation = self
            .operations
            .get_mut(operation_id)
            .ok_or(BrokerRefusal::Malformed)?;
        if operation.request_digest != request_digest {
            return Err(BrokerRefusal::DigestConflict);
        }
        match operation.phase {
            DurableBrokerPhase::Prepared | DurableBrokerPhase::Contained => {
                operation.phase = DurableBrokerPhase::Contained;
                Ok(())
            }
            DurableBrokerPhase::Committed => Ok(()),
            DurableBrokerPhase::CleanupPending => Err(BrokerRefusal::CleanupPending),
        }
    }

    pub fn mark_cleanup_pending(&mut self, operation_id: &str) -> Result<(), BrokerRefusal> {
        let operation = self
            .operations
            .get_mut(operation_id)
            .ok_or(BrokerRefusal::Malformed)?;
        operation.phase = DurableBrokerPhase::CleanupPending;
        Ok(())
    }

    pub fn operation(&self, operation_id: &str) -> Option<&DurableBrokerOperation> {
        self.operations.get(operation_id)
    }
}

fn valid_scope(scope: &CortanaScopeBinding) -> bool {
    [
        &scope.project_id,
        &scope.ship_id,
        &scope.captain_id,
        &scope.cortana_identity_id,
        &scope.session_identity_id,
        &scope.terminal_id,
    ]
    .into_iter()
    .all(|value| valid_bounded(value, MAX_ID))
        && scope.cortana_authority_generation > 0
        && scope.session_identity_generation > 0
}

fn valid_candidate(candidate: &CandidateGeneration) -> bool {
    candidate.pid > 0 && candidate.start_ticks > 0 && candidate.tmux_session_created > 0
}

fn valid_policy_key(policy: &RuntimePolicyKey) -> bool {
    policy.catalog_version > 0
        && [
            &policy.provider,
            &policy.harness,
            &policy.model,
            &policy.reasoning_effort,
            &policy.permission,
            &policy.capability,
        ]
        .into_iter()
        .all(|value| valid_bounded(value, MAX_POLICY_VALUE))
        && matches!(policy.provider.as_str(), "codex" | "claude")
        && matches!(policy.harness.as_str(), "codex" | "claude")
}

fn valid_handles(handles: &BrokerHandleLayout) -> bool {
    let mut values = [
        handles.candidate_pidfd,
        handles.provider_executable,
        handles.credential_pipe,
        handles.launch_gate,
    ];
    values.sort_unstable();
    values == [0, 1, 2, 3]
}

fn valid_operation_id(value: &str) -> bool {
    value.len() == 32
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
}

fn valid_digest(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
}

fn valid_bounded(value: &str, max: usize) -> bool {
    !value.is_empty()
        && value.len() <= max
        && value.trim() == value
        && !value.chars().any(char::is_control)
}

fn lower_hex(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        output.push(HEX[(byte >> 4) as usize] as char);
        output.push(HEX[(byte & 0x0f) as usize] as char);
    }
    output
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture() -> (CortanaBrokerRequest, ObservedPeer, ProtectedBrokerPolicy) {
        let policy = RuntimePolicyKey {
            catalog_version: 1,
            provider: "codex".into(),
            harness: "codex".into(),
            model: "gpt-5.4".into(),
            reasoning_effort: "medium".into(),
            permission: "bypassPermissions".into(),
            capability: "control".into(),
        };
        let mut request = CortanaBrokerRequest {
            schema_version: BROKER_SCHEMA_VERSION,
            operation_id: "0123456789abcdef0123456789abcdef".into(),
            request_digest: String::new(),
            scope: CortanaScopeBinding {
                project_id: "project-1".into(),
                ship_id: "ship-1".into(),
                captain_id: "captain-1".into(),
                cortana_identity_id: "cortana-permanent".into(),
                cortana_authority_generation: 7,
                session_identity_id: "session-identity-1".into(),
                session_identity_generation: 11,
                terminal_id: "terminal-1".into(),
            },
            candidate: CandidateGeneration {
                pid: 4242,
                start_ticks: 9001,
                tmux_session_id: 1,
                tmux_session_created: 2,
                tmux_window_id: 3,
                tmux_pane_id: 4,
            },
            policy: policy.clone(),
            handles: BrokerHandleLayout {
                candidate_pidfd: 0,
                provider_executable: 1,
                credential_pipe: 2,
                launch_gate: 3,
            },
        };
        request.request_digest = request.canonical_digest().unwrap();
        (
            request,
            ObservedPeer {
                uid: 1000,
                pid: 55,
                process_start_ticks: 88,
                executable_device: 10,
                executable_inode: 20,
            },
            ProtectedBrokerPolicy {
                client: ProtectedClient {
                    uid: 1000,
                    executable_device: 10,
                    executable_inode: 20,
                },
                runtimes: vec![ProtectedPolicy {
                    key: policy,
                    executable_device: 30,
                    executable_inode: 40,
                    executable_version: "codex 1.2.3".into(),
                }],
            },
        )
    }

    #[test]
    fn canonical_request_rejects_changed_digest_unknown_fields_and_handle_escape() {
        let (request, peer, policy) = fixture();
        assert!(request.validate(&peer, &policy).is_ok());
        let mut changed = request.clone();
        changed.scope.project_id = "other".into();
        assert_eq!(
            changed.validate(&peer, &policy),
            Err(BrokerRefusal::Malformed)
        );
        let mut handles = request.clone();
        handles.handles.launch_gate = 9;
        handles.request_digest = handles.canonical_digest().unwrap();
        assert_eq!(
            handles.validate(&peer, &policy),
            Err(BrokerRefusal::Malformed)
        );
        let mut encoded = serde_json::to_value(&request).unwrap();
        encoded
            .as_object_mut()
            .unwrap()
            .insert("command".into(), serde_json::json!("sh"));
        assert!(serde_json::from_value::<CortanaBrokerRequest>(encoded).is_err());
    }

    #[test]
    fn peer_policy_and_scope_are_exact_and_fail_closed() {
        let (request, peer, policy) = fixture();
        let mut foreign = peer.clone();
        foreign.executable_inode += 1;
        assert_eq!(
            request.validate(&foreign, &policy),
            Err(BrokerRefusal::UnauthorizedPeer)
        );
        let mut unsupported = request.clone();
        unsupported.policy.model = "other".into();
        unsupported.request_digest = unsupported.canonical_digest().unwrap();
        assert_eq!(
            unsupported.validate(&peer, &policy),
            Err(BrokerRefusal::UnsupportedPolicy)
        );
    }

    #[test]
    fn replay_response_loss_and_changed_digest_converge() {
        let (request, peer, policy) = fixture();
        let validated = request.validate(&peer, &policy).unwrap();
        let protected_runtime = validated.policy.clone();
        let mut journal = FakeBrokerJournal::default();
        let first = journal.prepare(validated.clone()).unwrap();
        assert_eq!(first.phase, DurableBrokerPhase::Prepared);
        assert_eq!(journal.prepare(validated.clone()).unwrap(), first);
        journal
            .mark_contained(&request.operation_id, &request.request_digest)
            .unwrap();
        let committed = journal
            .commit(
                &request.operation_id,
                &request.request_digest,
                50,
                60,
                &protected_runtime,
            )
            .unwrap();
        assert_eq!(
            journal
                .commit(
                    &request.operation_id,
                    &request.request_digest,
                    50,
                    60,
                    &protected_runtime,
                )
                .unwrap(),
            committed
        );
        let mut changed = validated;
        changed.request.request_digest = "a".repeat(64);
        assert_eq!(journal.prepare(changed), Err(BrokerRefusal::DigestConflict));
    }

    #[test]
    fn stale_scope_candidate_and_cleanup_pending_never_rebind() {
        let (request, peer, policy) = fixture();
        let validated = request.validate(&peer, &policy).unwrap();
        let mut journal = FakeBrokerJournal::default();
        journal.prepare(validated).unwrap();
        let mut contender = request.clone();
        contender.operation_id = "fedcba9876543210fedcba9876543210".into();
        contender.scope.session_identity_generation += 1;
        contender.request_digest = contender.canonical_digest().unwrap();
        let contender = contender.validate(&peer, &policy).unwrap();
        assert_eq!(
            journal.prepare(contender),
            Err(BrokerRefusal::ScopeConflict)
        );
        journal.mark_cleanup_pending(&request.operation_id).unwrap();
        assert_eq!(
            journal.mark_contained(&request.operation_id, &request.request_digest),
            Err(BrokerRefusal::CleanupPending)
        );
    }
}
