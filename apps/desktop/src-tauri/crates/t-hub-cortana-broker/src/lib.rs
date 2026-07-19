use t_hub_cortana_broker_protocol::{
    BrokerLaunchReceipt, BrokerRefusal, BrokerResponse, CortanaBrokerRequest, FakeBrokerJournal,
    ObservedPeer, ProtectedBrokerPolicy, ProtectedPolicy, ValidatedBrokerRequest,
};

pub mod store;

/// Kernel placement authority for one Cortana candidate.
///
/// Production implementations run as root and retain ownership of the exact
/// non-delegated cgroup from placement through credential release. The method
/// `attest_and_release` is one authority boundary: it must revalidate the pinned
/// candidate and provider handles, prove the cgroup is root-owned and excludes
/// the application UID, and only then write the fixed launch byte while that
/// ownership remains unchanged.
pub trait KernelLaunchAuthority {
    type Containment;

    fn contain(
        &mut self,
        request: &ValidatedBrokerRequest,
    ) -> Result<Self::Containment, BrokerRefusal>;

    fn attest_and_release(
        &mut self,
        request: &ValidatedBrokerRequest,
        containment: &mut Self::Containment,
    ) -> Result<KernelReleaseReceipt, BrokerRefusal>;

    fn rollback(
        &mut self,
        request: &ValidatedBrokerRequest,
        containment: Self::Containment,
    ) -> Result<(), BrokerRefusal>;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KernelReleaseReceipt {
    pub cgroup_device: u64,
    pub cgroup_inode: u64,
}

pub struct CortanaBrokerEngine<A> {
    policy: ProtectedBrokerPolicy,
    journal: FakeBrokerJournal,
    authority: A,
}

impl<A: KernelLaunchAuthority> CortanaBrokerEngine<A> {
    pub fn new(policy: ProtectedBrokerPolicy, authority: A) -> Self {
        Self {
            policy,
            journal: FakeBrokerJournal::default(),
            authority,
        }
    }

    pub fn ensure(&mut self, peer: &ObservedPeer, request: CortanaBrokerRequest) -> BrokerResponse {
        match self.ensure_inner(peer, request) {
            Ok(receipt) => BrokerResponse::Committed(receipt),
            Err(refusal) => BrokerResponse::Refused(refusal),
        }
    }

    fn ensure_inner(
        &mut self,
        peer: &ObservedPeer,
        request: CortanaBrokerRequest,
    ) -> Result<BrokerLaunchReceipt, BrokerRefusal> {
        let validated = request.validate(peer, &self.policy)?;
        let operation = self.journal.prepare(validated.clone())?;
        if let Some(receipt) = operation.receipt {
            return Ok(receipt);
        }
        let mut containment = self.authority.contain(&validated)?;
        self.journal.mark_contained(
            &validated.request.operation_id,
            &validated.request.request_digest,
        )?;
        let released = match self
            .authority
            .attest_and_release(&validated, &mut containment)
        {
            Ok(released) => released,
            Err(error) => {
                if self.authority.rollback(&validated, containment).is_err() {
                    let _ = self
                        .journal
                        .mark_cleanup_pending(&validated.request.operation_id);
                    return Err(BrokerRefusal::CleanupPending);
                }
                return Err(error);
            }
        };
        self.journal.commit(
            &validated.request.operation_id,
            &validated.request.request_digest,
            released.cgroup_device,
            released.cgroup_inode,
            &validated.policy,
        )
    }

    pub fn journal(&self) -> &FakeBrokerJournal {
        &self.journal
    }

    pub fn authority(&self) -> &A {
        &self.authority
    }
}

/// Deterministic unprivileged authority used only by broker contract tests.
///
/// It models the security property supplied by a root-owned cgroup rather than
/// pretending a same-UID temporary directory is protected. Mutation attempts
/// pass through `attempt_same_uid_mutation`, which is denied once containment
/// is sealed and recorded before the exact release barrier runs.
pub struct FakeKernelAuthority<F = fn(&mut FakeKernelContainment)> {
    next_device: u64,
    next_inode: u64,
    release_barrier: Option<F>,
    release_count: usize,
    fail_contain: bool,
    fail_release: bool,
    fail_rollback: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FakeKernelContainment {
    candidate_pid: u32,
    candidate_start_ticks: u64,
    root_owned: bool,
    app_uid_can_mutate: bool,
    foreign_tasks: usize,
    delegated_children: usize,
    credentials_released: bool,
    mutation_denials: usize,
}

impl FakeKernelContainment {
    pub fn attempt_same_uid_mutation(&mut self, add_child: bool) -> bool {
        if self.root_owned && !self.app_uid_can_mutate {
            self.mutation_denials += 1;
            return false;
        }
        if add_child {
            self.delegated_children += 1;
        } else {
            self.foreign_tasks += 1;
        }
        true
    }

    pub fn credentials_released(&self) -> bool {
        self.credentials_released
    }

    pub fn mutation_denials(&self) -> usize {
        self.mutation_denials
    }
}

impl<F> FakeKernelAuthority<F> {
    pub fn with_release_barrier(barrier: F) -> Self {
        Self {
            next_device: 101,
            next_inode: 202,
            release_barrier: Some(barrier),
            release_count: 0,
            fail_contain: false,
            fail_release: false,
            fail_rollback: false,
        }
    }

    pub fn release_count(&self) -> usize {
        self.release_count
    }

    pub fn fail_release(&mut self, fail_rollback: bool) {
        self.fail_release = true;
        self.fail_rollback = fail_rollback;
    }
}

impl Default for FakeKernelAuthority<fn(&mut FakeKernelContainment)> {
    fn default() -> Self {
        Self::with_release_barrier(|_| {})
    }
}

impl<F: FnMut(&mut FakeKernelContainment)> KernelLaunchAuthority for FakeKernelAuthority<F> {
    type Containment = FakeKernelContainment;

    fn contain(
        &mut self,
        request: &ValidatedBrokerRequest,
    ) -> Result<Self::Containment, BrokerRefusal> {
        if self.fail_contain {
            return Err(BrokerRefusal::AuthorityUnavailable);
        }
        Ok(FakeKernelContainment {
            candidate_pid: request.request.candidate.pid,
            candidate_start_ticks: request.request.candidate.start_ticks,
            root_owned: true,
            app_uid_can_mutate: false,
            foreign_tasks: 0,
            delegated_children: 0,
            credentials_released: false,
            mutation_denials: 0,
        })
    }

    fn attest_and_release(
        &mut self,
        request: &ValidatedBrokerRequest,
        containment: &mut Self::Containment,
    ) -> Result<KernelReleaseReceipt, BrokerRefusal> {
        if containment.candidate_pid != request.request.candidate.pid
            || containment.candidate_start_ticks != request.request.candidate.start_ticks
            || !containment.root_owned
            || containment.app_uid_can_mutate
            || containment.foreign_tasks != 0
            || containment.delegated_children != 0
        {
            return Err(BrokerRefusal::CandidateMismatch);
        }
        if let Some(barrier) = &mut self.release_barrier {
            barrier(containment);
        }
        if !containment.root_owned
            || containment.app_uid_can_mutate
            || containment.foreign_tasks != 0
            || containment.delegated_children != 0
            || self.fail_release
        {
            return Err(BrokerRefusal::CandidateMismatch);
        }
        containment.credentials_released = true;
        self.release_count += 1;
        Ok(KernelReleaseReceipt {
            cgroup_device: self.next_device,
            cgroup_inode: self.next_inode,
        })
    }

    fn rollback(
        &mut self,
        _request: &ValidatedBrokerRequest,
        _containment: Self::Containment,
    ) -> Result<(), BrokerRefusal> {
        if self.fail_rollback {
            Err(BrokerRefusal::CleanupPending)
        } else {
            Ok(())
        }
    }
}

pub fn protected_policy_for_test(
    peer: &ObservedPeer,
    runtime: ProtectedPolicy,
) -> ProtectedBrokerPolicy {
    ProtectedBrokerPolicy {
        client: t_hub_cortana_broker_protocol::ProtectedClient {
            uid: peer.uid,
            executable_device: peer.executable_device,
            executable_inode: peer.executable_inode,
        },
        runtimes: vec![runtime],
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use t_hub_cortana_broker_protocol::{
        BrokerHandleLayout, CandidateGeneration, CortanaScopeBinding, RuntimePolicyKey,
        BROKER_SCHEMA_VERSION,
    };

    fn fixture() -> (CortanaBrokerRequest, ObservedPeer, ProtectedBrokerPolicy) {
        let peer = ObservedPeer {
            uid: 1000,
            pid: 77,
            process_start_ticks: 88,
            executable_device: 11,
            executable_inode: 22,
        };
        let runtime_key = RuntimePolicyKey {
            catalog_version: 1,
            provider: "codex".into(),
            harness: "codex".into(),
            model: "gpt-5.4".into(),
            reasoning_effort: "medium".into(),
            permission: "bypassPermissions".into(),
            capability: "control".into(),
        };
        let runtime = ProtectedPolicy {
            key: runtime_key.clone(),
            executable_device: 33,
            executable_inode: 44,
            executable_version: "codex 1.2.3".into(),
        };
        let mut request = CortanaBrokerRequest {
            schema_version: BROKER_SCHEMA_VERSION,
            operation_id: "0123456789abcdef0123456789abcdef".into(),
            request_digest: String::new(),
            scope: CortanaScopeBinding {
                project_id: "project-1".into(),
                ship_id: "ship-1".into(),
                captain_id: "captain-1".into(),
                cortana_identity_id: "cortana".into(),
                cortana_authority_generation: 5,
                session_identity_id: "identity-1".into(),
                session_identity_generation: 7,
                terminal_id: "terminal-1".into(),
            },
            candidate: CandidateGeneration {
                pid: 99,
                start_ticks: 100,
                tmux_session_id: 1,
                tmux_session_created: 2,
                tmux_window_id: 3,
                tmux_pane_id: 4,
            },
            policy: runtime_key,
            handles: BrokerHandleLayout {
                candidate_pidfd: 0,
                provider_executable: 1,
                credential_pipe: 2,
                launch_gate: 3,
            },
        };
        request.request_digest = request.canonical_digest().unwrap();
        let policy = protected_policy_for_test(&peer, runtime);
        (request, peer, policy)
    }

    #[test]
    fn post_validation_pre_write_barrier_denies_child_and_task_mutation() {
        let (request, peer, policy) = fixture();
        let authority =
            FakeKernelAuthority::with_release_barrier(|scope: &mut FakeKernelContainment| {
                assert!(!scope.attempt_same_uid_mutation(true));
                assert!(!scope.attempt_same_uid_mutation(false));
                assert_eq!(scope.mutation_denials(), 2);
                assert!(!scope.credentials_released());
            });
        let mut broker = CortanaBrokerEngine::new(policy, authority);
        let response = broker.ensure(&peer, request);
        assert!(matches!(response, BrokerResponse::Committed(_)));
        assert_eq!(broker.authority().release_count(), 1);
    }

    #[test]
    fn confused_deputy_stale_generation_and_changed_replay_fail_closed() {
        let (request, peer, policy) = fixture();
        let mut broker = CortanaBrokerEngine::new(policy, FakeKernelAuthority::default());
        let mut foreign = peer.clone();
        foreign.uid += 1;
        assert_eq!(
            broker.ensure(&foreign, request.clone()),
            BrokerResponse::Refused(BrokerRefusal::UnauthorizedPeer)
        );
        let committed = broker.ensure(&peer, request.clone());
        assert!(matches!(committed, BrokerResponse::Committed(_)));
        let mut stale = request;
        stale.scope.session_identity_generation -= 1;
        stale.request_digest = stale.canonical_digest().unwrap();
        assert_eq!(
            broker.ensure(&peer, stale),
            BrokerResponse::Refused(BrokerRefusal::DigestConflict)
        );
    }

    #[test]
    fn response_loss_replay_returns_receipt_without_second_release() {
        let (request, peer, policy) = fixture();
        let mut broker = CortanaBrokerEngine::new(policy, FakeKernelAuthority::default());
        let first = broker.ensure(&peer, request.clone());
        let replay = broker.ensure(&peer, request);
        assert_eq!(replay, first);
        assert_eq!(broker.authority().release_count(), 1);
    }

    #[test]
    fn release_and_rollback_failure_persists_cleanup_pending() {
        let (request, peer, policy) = fixture();
        let mut authority = FakeKernelAuthority::default();
        authority.fail_release(true);
        let mut broker = CortanaBrokerEngine::new(policy, authority);
        assert_eq!(
            broker.ensure(&peer, request.clone()),
            BrokerResponse::Refused(BrokerRefusal::CleanupPending)
        );
        assert_eq!(
            broker.ensure(&peer, request),
            BrokerResponse::Refused(BrokerRefusal::CleanupPending)
        );
    }
}
