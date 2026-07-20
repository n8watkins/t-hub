//! Powder-independent durable records for supervised agent sessions.
//!
//! This module intentionally contains no terminal, provider, registry, or
//! network code.  It is the stable data boundary used by the de-Powder control
//! contract while the legacy Crew representation remains readable elsewhere.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

pub const MAX_ASSIGNMENT_BYTES: usize = 16 * 1024;
pub const MAX_CHECKPOINT_BYTES: usize = 4 * 1024;
pub const MAX_EVIDENCE_REFERENCE_BYTES: usize = 16 * 1024;
pub const MAX_EVENT_BATCH: usize = 128;
pub const MAX_CHECKPOINT_HISTORY: usize = 4096;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum RuntimeState {
    #[serde(rename = "starting")]
    Starting,
    #[serde(rename = "running")]
    Running,
    #[serde(rename = "idle")]
    Idle,
    #[serde(rename = "needsPermission")]
    NeedsPermission,
    #[serde(rename = "exited")]
    Exited,
    #[serde(rename = "unavailable")]
    Unavailable,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum WorkStage {
    #[serde(rename = "assigned")]
    Assigned,
    #[serde(rename = "working")]
    Working,
    #[serde(rename = "needsInput")]
    NeedsInput,
    #[serde(rename = "readyForReview")]
    ReadyForReview,
    #[serde(rename = "awaitingIntegration")]
    AwaitingIntegration,
    #[serde(rename = "complete")]
    Complete,
    #[serde(rename = "stopped")]
    Stopped,
}

/// Independently reportable delivery states for one exact implementation.
///
/// `complete` is never stored. It is derived from independent review and
/// acceptance-test evidence for the resulting commit. Later release states do
/// not change or replace that definition.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DeliveryStates {
    pub implemented: bool,
    pub reviewed: bool,
    pub tested: bool,
    pub complete: bool,
    pub integrated: bool,
    pub packaged: bool,
    pub installed: bool,
    pub live_verified: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ReviewEvidence {
    pub commit: String,
    pub reviewer_identity: String,
    pub reference: String,
    pub recorded_at: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase", deny_unknown_fields)]
pub enum AcceptanceEnvironment {
    Source,
    PackagedGuiE2e {
        artifact_id: String,
        source_commit: String,
        installation_target: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AcceptanceTestEvidence {
    pub commit: String,
    pub runner_identity: String,
    pub reference: String,
    pub environment: AcceptanceEnvironment,
    pub recorded_at: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct IntegrationEvidence {
    pub source_commit: String,
    pub canonical_baseline: String,
    pub canonical_commit: String,
    pub reference: String,
    pub recorded_at: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ArtifactEvidence {
    pub artifact_id: String,
    pub source_baseline: String,
    pub reference: String,
    pub recorded_at: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct InstallationEvidence {
    pub artifact_id: String,
    pub target: String,
    pub reference: String,
    pub recorded_at: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum VerifierKind {
    Human,
    AiAgent,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct LiveVerificationEvidence {
    pub artifact_id: String,
    pub target: String,
    pub verifier_identity: String,
    pub verifier_kind: VerifierKind,
    pub reference: String,
    pub recorded_at: u64,
}

/// Provenance from the exact dispatch baseline through verification of the
/// installed application.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DeliveryProvenance {
    pub source_baseline: String,
    #[serde(default)]
    pub requires_packaged_gui_e2e: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resulting_commit: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub independent_review: Option<ReviewEvidence>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub acceptance_test: Option<AcceptanceTestEvidence>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub integration: Option<IntegrationEvidence>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub artifact: Option<ArtifactEvidence>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub installation: Option<InstallationEvidence>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub live_verification: Option<LiveVerificationEvidence>,
}

impl DeliveryProvenance {
    pub fn new(source_baseline: impl Into<String>, requires_packaged_gui_e2e: bool) -> Self {
        Self {
            source_baseline: source_baseline.into(),
            requires_packaged_gui_e2e,
            resulting_commit: None,
            independent_review: None,
            acceptance_test: None,
            integration: None,
            artifact: None,
            installation: None,
            live_verification: None,
        }
    }

    pub fn states(&self) -> DeliveryStates {
        let baseline_is_exact = validate_commit("sourceBaseline", &self.source_baseline).is_ok();
        let implemented = baseline_is_exact
            && self
                .resulting_commit
                .as_deref()
                .is_some_and(|commit| validate_commit("resultingCommit", commit).is_ok());
        let reviewed = matches!(
            (&self.resulting_commit, &self.independent_review),
            (Some(commit), Some(review))
                if implemented
                    && review.commit == *commit
                    && !review.reviewer_identity.trim().is_empty()
                    && validate_reference("independentReview.reference", &review.reference).is_ok()
        );
        let tested = matches!(
            (&self.resulting_commit, &self.acceptance_test),
            (Some(commit), Some(test))
                if implemented
                    && test.commit == *commit
                    && !test.runner_identity.trim().is_empty()
                    && validate_reference("acceptanceTest.reference", &test.reference).is_ok()
                    && self.acceptance_environment_matches(test, commit)
        );
        let complete = reviewed && tested;
        let completed_at = match (&self.independent_review, &self.acceptance_test) {
            (Some(review), Some(test)) => review.recorded_at.max(test.recorded_at),
            _ => 0,
        };
        let integrated = matches!(
            (&self.resulting_commit, &self.integration),
            (Some(commit), Some(integration))
                if complete
                    && integration.source_commit == *commit
                    && !integration.canonical_baseline.trim().is_empty()
                    && validate_commit("integration.canonicalCommit", &integration.canonical_commit).is_ok()
                    && validate_reference("integration.reference", &integration.reference).is_ok()
                    && integration.recorded_at >= completed_at
        );
        let packaged = matches!(
            (&self.integration, &self.artifact),
            (Some(integration), Some(artifact))
                if integrated
                    && !artifact.artifact_id.trim().is_empty()
                    && artifact.source_baseline == integration.canonical_commit
                    && validate_reference("artifact.reference", &artifact.reference).is_ok()
                    && artifact.recorded_at >= integration.recorded_at
        );
        let installed = matches!(
            (&self.artifact, &self.installation),
            (Some(artifact), Some(installation))
                if packaged
                    && installation.artifact_id == artifact.artifact_id
                    && !installation.target.trim().is_empty()
                    && validate_reference("installation.reference", &installation.reference).is_ok()
                    && installation.recorded_at >= artifact.recorded_at
        );
        let live_verified = matches!(
            (&self.installation, &self.live_verification),
            (Some(installation), Some(verification))
                if installed
                    && verification.artifact_id == installation.artifact_id
                    && verification.target == installation.target
                    && !verification.verifier_identity.trim().is_empty()
                    && validate_reference("liveVerification.reference", &verification.reference).is_ok()
                    && verification.recorded_at >= installation.recorded_at
        );
        DeliveryStates {
            implemented,
            reviewed,
            tested,
            complete,
            integrated,
            packaged,
            installed,
            live_verified,
        }
    }

    fn acceptance_environment_matches(
        &self,
        test: &AcceptanceTestEvidence,
        resulting_commit: &str,
    ) -> bool {
        match &test.environment {
            AcceptanceEnvironment::Source => !self.requires_packaged_gui_e2e,
            AcceptanceEnvironment::PackagedGuiE2e {
                artifact_id,
                source_commit,
                installation_target,
            } => {
                !artifact_id.trim().is_empty()
                    && source_commit == resulting_commit
                    && !installation_target.trim().is_empty()
            }
        }
    }

    pub fn record_implementation(&mut self, commit: impl Into<String>) -> Result<(), String> {
        let mut next = self.clone();
        set_once(&mut next.resulting_commit, commit.into(), "implemented")?;
        next.validate()?;
        *self = next;
        Ok(())
    }

    pub fn record_review(&mut self, evidence: ReviewEvidence) -> Result<(), String> {
        let mut next = self.clone();
        set_once(&mut next.independent_review, evidence, "reviewed")?;
        next.validate()?;
        *self = next;
        Ok(())
    }

    pub fn record_acceptance_test(
        &mut self,
        evidence: AcceptanceTestEvidence,
    ) -> Result<(), String> {
        let mut next = self.clone();
        set_once(&mut next.acceptance_test, evidence, "tested")?;
        next.validate()?;
        *self = next;
        Ok(())
    }

    pub fn record_integration(&mut self, evidence: IntegrationEvidence) -> Result<(), String> {
        let mut next = self.clone();
        set_once(&mut next.integration, evidence, "integrated")?;
        next.validate()?;
        *self = next;
        Ok(())
    }

    pub fn record_artifact(&mut self, evidence: ArtifactEvidence) -> Result<(), String> {
        let mut next = self.clone();
        set_once(&mut next.artifact, evidence, "packaged")?;
        next.validate()?;
        *self = next;
        Ok(())
    }

    pub fn record_installation(&mut self, evidence: InstallationEvidence) -> Result<(), String> {
        let mut next = self.clone();
        set_once(&mut next.installation, evidence, "installed")?;
        next.validate()?;
        *self = next;
        Ok(())
    }

    pub fn record_live_verification(
        &mut self,
        evidence: LiveVerificationEvidence,
    ) -> Result<(), String> {
        let mut next = self.clone();
        set_once(&mut next.live_verification, evidence, "liveVerified")?;
        next.validate()?;
        *self = next;
        Ok(())
    }

    pub fn validate(&self) -> Result<(), String> {
        validate_commit("sourceBaseline", &self.source_baseline)?;
        if let Some(commit) = self.resulting_commit.as_deref() {
            validate_commit("resultingCommit", commit)?;
        }

        if let Some(review) = &self.independent_review {
            let resulting_commit = self
                .resulting_commit
                .as_deref()
                .ok_or("delivery provenance cannot be reviewed before it is implemented")?;
            validate_commit("independentReview.commit", &review.commit)?;
            if review.commit != resulting_commit {
                return Err("independentReview.commit must equal the exact resultingCommit".into());
            }
            validate_nonempty(
                "independentReview.reviewerIdentity",
                &review.reviewer_identity,
            )?;
            validate_reference("independentReview.reference", &review.reference)?;
        }

        if let Some(test) = &self.acceptance_test {
            let resulting_commit = self
                .resulting_commit
                .as_deref()
                .ok_or("delivery provenance cannot be tested before it is implemented")?;
            validate_commit("acceptanceTest.commit", &test.commit)?;
            if test.commit != resulting_commit {
                return Err("acceptanceTest.commit must equal the exact resultingCommit".into());
            }
            validate_nonempty("acceptanceTest.runnerIdentity", &test.runner_identity)?;
            validate_reference("acceptanceTest.reference", &test.reference)?;
            match &test.environment {
                AcceptanceEnvironment::Source if self.requires_packaged_gui_e2e => {
                    return Err(
                        "acceptanceTest must contain packaged GUI E2E evidence for this scope"
                            .into(),
                    );
                }
                AcceptanceEnvironment::Source => {}
                AcceptanceEnvironment::PackagedGuiE2e {
                    artifact_id,
                    source_commit,
                    installation_target,
                } => {
                    validate_nonempty("acceptanceTest.environment.artifactId", artifact_id)?;
                    validate_commit("acceptanceTest.environment.sourceCommit", source_commit)?;
                    if source_commit != resulting_commit {
                        return Err(
                            "packaged GUI E2E sourceCommit must equal resultingCommit".into()
                        );
                    }
                    validate_nonempty(
                        "acceptanceTest.environment.installationTarget",
                        installation_target,
                    )?;
                }
            }
        }

        if let Some(integration) = &self.integration {
            let resulting_commit = self
                .resulting_commit
                .as_deref()
                .ok_or("delivery provenance cannot be integrated before it is implemented")?;
            if !self.states().complete {
                return Err(
                    "delivery provenance cannot be integrated before review and testing complete"
                        .into(),
                );
            }
            validate_commit("integration.sourceCommit", &integration.source_commit)?;
            if integration.source_commit != resulting_commit {
                return Err("integration.sourceCommit must equal resultingCommit".into());
            }
            validate_nonempty(
                "integration.canonicalBaseline",
                &integration.canonical_baseline,
            )?;
            validate_commit("integration.canonicalCommit", &integration.canonical_commit)?;
            validate_reference("integration.reference", &integration.reference)?;
            let completed_at = self
                .independent_review
                .as_ref()
                .map(|review| review.recorded_at)
                .unwrap_or_default()
                .max(
                    self.acceptance_test
                        .as_ref()
                        .map(|test| test.recorded_at)
                        .unwrap_or_default(),
                );
            if integration.recorded_at < completed_at {
                return Err(
                    "integration.recordedAt must not precede review or testing evidence".into(),
                );
            }
        }

        if let Some(artifact) = &self.artifact {
            let integration = self
                .integration
                .as_ref()
                .ok_or("delivery provenance cannot be packaged before it is integrated")?;
            validate_nonempty("artifact.artifactId", &artifact.artifact_id)?;
            validate_commit("artifact.sourceBaseline", &artifact.source_baseline)?;
            if artifact.source_baseline != integration.canonical_commit {
                return Err(
                    "artifact.sourceBaseline must equal integration.canonicalCommit".into(),
                );
            }
            validate_reference("artifact.reference", &artifact.reference)?;
            if artifact.recorded_at < integration.recorded_at {
                return Err("artifact.recordedAt must not precede integration evidence".into());
            }
        }

        if let Some(installation) = &self.installation {
            let artifact = self
                .artifact
                .as_ref()
                .ok_or("delivery provenance cannot be installed before it is packaged")?;
            if installation.artifact_id != artifact.artifact_id {
                return Err("installation.artifactId must equal artifact.artifactId".into());
            }
            validate_nonempty("installation.target", &installation.target)?;
            validate_reference("installation.reference", &installation.reference)?;
            if installation.recorded_at < artifact.recorded_at {
                return Err("installation.recordedAt must not precede artifact evidence".into());
            }
        }

        if let Some(verification) = &self.live_verification {
            let installation = self
                .installation
                .as_ref()
                .ok_or("delivery provenance cannot be live-verified before it is installed")?;
            if verification.artifact_id != installation.artifact_id {
                return Err(
                    "liveVerification.artifactId must equal installation.artifactId".into(),
                );
            }
            if verification.target != installation.target {
                return Err("liveVerification.target must equal installation.target".into());
            }
            validate_nonempty(
                "liveVerification.verifierIdentity",
                &verification.verifier_identity,
            )?;
            validate_reference("liveVerification.reference", &verification.reference)?;
            if verification.recorded_at < installation.recorded_at {
                return Err(
                    "liveVerification.recordedAt must not precede installation evidence".into(),
                );
            }
        }
        Ok(())
    }
}

fn set_once<T: PartialEq>(slot: &mut Option<T>, value: T, state: &str) -> Result<(), String> {
    match slot {
        Some(existing) if existing == &value => Ok(()),
        Some(_) => Err(format!(
            "delivery provenance state '{state}' is immutable once recorded"
        )),
        None => {
            *slot = Some(value);
            Ok(())
        }
    }
}

fn validate_nonempty(field: &str, value: &str) -> Result<(), String> {
    if value.trim().is_empty() {
        return Err(format!("delivery provenance {field} must not be empty"));
    }
    Ok(())
}

fn validate_reference(field: &str, value: &str) -> Result<(), String> {
    validate_nonempty(field, value)?;
    if value.len() > MAX_EVIDENCE_REFERENCE_BYTES {
        return Err(format!(
            "delivery provenance {field} must be at most {MAX_EVIDENCE_REFERENCE_BYTES} bytes"
        ));
    }
    Ok(())
}

fn validate_commit(field: &str, commit: &str) -> Result<(), String> {
    if !matches!(commit.len(), 40 | 64) || !commit.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(format!(
            "delivery provenance {field} must be an exact 40- or 64-character Git commit"
        ));
    }
    Ok(())
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AgentSessionRecord {
    pub agent_session_id: String,
    pub captain_session_id: String,
    pub project_id: String,
    pub assignment: String,
    pub directory: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub worktree_path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace_tab_id: Option<String>,
    pub harness: String,
    pub provider: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_conversation_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resume_point: Option<String>,
    pub runtime_state: RuntimeState,
    pub work_stage: WorkStage,
    /// Missing only on records written before delivery provenance was added.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub delivery: Option<DeliveryProvenance>,
    pub created_at: u64,
    pub updated_at: u64,
}

impl AgentSessionRecord {
    pub fn validate(&self) -> Result<(), String> {
        for (field, value) in [
            ("agentSessionId", self.agent_session_id.as_str()),
            ("captainSessionId", self.captain_session_id.as_str()),
            ("projectId", self.project_id.as_str()),
            ("directory", self.directory.as_str()),
            ("harness", self.harness.as_str()),
            ("provider", self.provider.as_str()),
        ] {
            if value.trim().is_empty() {
                return Err(format!("agent session {field} must not be empty"));
            }
        }
        if self.assignment.trim().is_empty() {
            return Err("agent session assignment must not be empty".into());
        }
        if self.assignment.len() > MAX_ASSIGNMENT_BYTES {
            return Err(format!(
                "agent session assignment must be at most {MAX_ASSIGNMENT_BYTES} bytes"
            ));
        }
        if !matches!(self.harness.as_str(), "codex" | "claude") || self.harness != self.provider {
            return Err("agent session harness and provider must both be codex or claude".into());
        }
        if self.updated_at < self.created_at {
            return Err("agent session updatedAt must not precede createdAt".into());
        }
        if let Some(delivery) = &self.delivery {
            delivery.validate()?;
            if delivery
                .independent_review
                .as_ref()
                .is_some_and(|review| review.reviewer_identity == self.agent_session_id)
            {
                return Err(
                    "agent session independent reviewer must differ from the implementing agent"
                        .into(),
                );
            }
            if self.work_stage == WorkStage::Complete && !delivery.states().complete {
                return Err(
                    "agent session workStage complete requires exact-commit review and testing"
                        .into(),
                );
            }
        }
        Ok(())
    }

    /// Enforces the baseline requirement for newly dispatched sessions while
    /// keeping legacy records readable through [`Self::validate`].
    pub fn validate_for_dispatch(&self) -> Result<(), String> {
        self.validate()?;
        if self.delivery.is_none() {
            return Err("agent session dispatch requires an exact source baseline".into());
        }
        Ok(())
    }

    pub fn delivery_states(&self) -> Option<DeliveryStates> {
        self.delivery.as_ref().map(DeliveryProvenance::states)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AgentCheckpoint {
    pub cursor: u64,
    pub agent_session_id: String,
    pub author_session_id: String,
    pub summary: String,
    pub created_at: u64,
}

impl AgentCheckpoint {
    pub fn validate(&self) -> Result<(), String> {
        if self.summary.trim().is_empty() {
            return Err("agent checkpoint summary must not be empty".into());
        }
        if self.summary.len() > MAX_CHECKPOINT_BYTES {
            return Err(format!(
                "agent checkpoint summary must be at most {MAX_CHECKPOINT_BYTES} bytes"
            ));
        }
        if self.agent_session_id.trim().is_empty() || self.author_session_id.trim().is_empty() {
            return Err("agent checkpoint identities must not be empty".into());
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AgentEvent {
    pub cursor: u64,
    pub agent_session_id: String,
    pub kind: String,
    pub created_at: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub runtime_state: Option<RuntimeState>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub work_stage: Option<WorkStage>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub checkpoint: Option<AgentCheckpoint>,
}

pub fn snapshot_digest<T: Serialize>(snapshot: &T) -> Result<String, String> {
    let bytes = serde_json::to_vec(snapshot)
        .map_err(|error| format!("failed to serialize agent snapshot for digest: {error}"))?;
    let digest = Sha256::digest(bytes);
    Ok(format!("sha256:{digest:x}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    const SOURCE_BASELINE: &str = "1111111111111111111111111111111111111111";
    const RESULT_COMMIT: &str = "2222222222222222222222222222222222222222";
    const CANONICAL_COMMIT: &str = "3333333333333333333333333333333333333333";

    fn record() -> AgentSessionRecord {
        AgentSessionRecord {
            agent_session_id: "agent-1".into(),
            captain_session_id: "captain-1".into(),
            project_id: "project-1".into(),
            assignment: "Implement the migration".into(),
            directory: "/repo".into(),
            worktree_path: Some("/worktree".into()),
            branch: Some("feature/agent".into()),
            workspace_tab_id: None,
            harness: "codex".into(),
            provider: "codex".into(),
            provider_conversation_id: Some("conversation-1".into()),
            resume_point: None,
            runtime_state: RuntimeState::Starting,
            work_stage: WorkStage::Assigned,
            delivery: None,
            created_at: 10,
            updated_at: 10,
        }
    }

    fn review() -> ReviewEvidence {
        ReviewEvidence {
            commit: RESULT_COMMIT.into(),
            reviewer_identity: "reviewer-1".into(),
            reference: "review://result-commit/approved".into(),
            recorded_at: 20,
        }
    }

    fn source_test() -> AcceptanceTestEvidence {
        AcceptanceTestEvidence {
            commit: RESULT_COMMIT.into(),
            runner_identity: "tester-1".into(),
            reference: "test://result-commit/acceptance".into(),
            environment: AcceptanceEnvironment::Source,
            recorded_at: 21,
        }
    }

    fn packaged_gui_test() -> AcceptanceTestEvidence {
        AcceptanceTestEvidence {
            commit: RESULT_COMMIT.into(),
            runner_identity: "tester-1".into(),
            reference: "e2e://windows/visible-flow".into(),
            environment: AcceptanceEnvironment::PackagedGuiE2e {
                artifact_id: "candidate:result-commit".into(),
                source_commit: RESULT_COMMIT.into(),
                installation_target: "Windows user installation".into(),
            },
            recorded_at: 21,
        }
    }

    fn integration() -> IntegrationEvidence {
        IntegrationEvidence {
            source_commit: RESULT_COMMIT.into(),
            canonical_baseline: "main".into(),
            canonical_commit: CANONICAL_COMMIT.into(),
            reference: "git://main/integration".into(),
            recorded_at: 22,
        }
    }

    fn artifact() -> ArtifactEvidence {
        ArtifactEvidence {
            artifact_id: "sha256:release-artifact".into(),
            source_baseline: CANONICAL_COMMIT.into(),
            reference: "artifact://windows/release".into(),
            recorded_at: 23,
        }
    }

    fn installation() -> InstallationEvidence {
        InstallationEvidence {
            artifact_id: "sha256:release-artifact".into(),
            target: "C:\\Program Files\\T-Hub".into(),
            reference: "install://windows/user".into(),
            recorded_at: 24,
        }
    }

    fn live_verification() -> LiveVerificationEvidence {
        LiveVerificationEvidence {
            artifact_id: "sha256:release-artifact".into(),
            target: "C:\\Program Files\\T-Hub".into(),
            verifier_identity: "agent-verifier-1".into(),
            verifier_kind: VerifierKind::AiAgent,
            reference: "verification://installed/visible-flow".into(),
            recorded_at: 25,
        }
    }

    #[test]
    fn record_serializes_the_frozen_wire_names() {
        let value = serde_json::to_value(record()).unwrap();
        assert_eq!(value["agentSessionId"], "agent-1");
        assert_eq!(value["runtimeState"], "starting");
        assert_eq!(value["workStage"], "assigned");
        assert_eq!(value["providerConversationId"], "conversation-1");
    }

    #[test]
    fn delivery_states_remain_distinct_through_the_full_flow() {
        let mut delivery = DeliveryProvenance::new(SOURCE_BASELINE, false);
        assert_eq!(
            delivery.states(),
            DeliveryStates {
                implemented: false,
                reviewed: false,
                tested: false,
                complete: false,
                integrated: false,
                packaged: false,
                installed: false,
                live_verified: false,
            }
        );

        delivery.record_implementation(RESULT_COMMIT).unwrap();
        delivery.record_review(review()).unwrap();
        assert!(delivery.states().implemented);
        assert!(delivery.states().reviewed);
        assert!(!delivery.states().tested);
        assert!(!delivery.states().complete);

        delivery.record_acceptance_test(source_test()).unwrap();
        assert!(delivery.states().complete);
        assert!(!delivery.states().integrated);

        delivery.record_integration(integration()).unwrap();
        assert!(delivery.states().integrated);
        assert!(!delivery.states().packaged);

        delivery.record_artifact(artifact()).unwrap();
        assert!(delivery.states().packaged);
        assert!(!delivery.states().installed);

        delivery.record_installation(installation()).unwrap();
        assert!(delivery.states().installed);
        assert!(!delivery.states().live_verified);

        delivery
            .record_live_verification(live_verification())
            .unwrap();
        assert!(delivery.states().live_verified);
        delivery.validate().unwrap();

        let value = serde_json::to_value(delivery.states()).unwrap();
        assert_eq!(value["complete"], true);
        assert_eq!(value["liveVerified"], true);

        let value = serde_json::to_value(&delivery).unwrap();
        assert_eq!(value["sourceBaseline"], SOURCE_BASELINE);
        assert_eq!(value["resultingCommit"], RESULT_COMMIT);
        assert_eq!(value["integration"]["canonicalCommit"], CANONICAL_COMMIT);
        assert_eq!(value["liveVerification"]["verifierKind"], "aiAgent");
    }

    #[test]
    fn complete_requires_review_and_tests_for_the_exact_result_commit() {
        let mut delivery = DeliveryProvenance::new(SOURCE_BASELINE, false);
        delivery.record_implementation(RESULT_COMMIT).unwrap();

        let mut wrong_review = review();
        wrong_review.commit = CANONICAL_COMMIT.into();
        assert!(delivery.record_review(wrong_review).is_err());
        assert!(delivery.independent_review.is_none());

        delivery.record_review(review()).unwrap();
        assert!(!delivery.states().complete);

        let mut wrong_test = source_test();
        wrong_test.commit = CANONICAL_COMMIT.into();
        assert!(delivery.record_acceptance_test(wrong_test).is_err());
        assert!(delivery.acceptance_test.is_none());
        assert!(!delivery.states().complete);

        delivery.record_acceptance_test(source_test()).unwrap();
        assert!(delivery.states().complete);
    }

    #[test]
    fn out_of_order_and_conflated_release_evidence_is_rejected_atomically() {
        let mut delivery = DeliveryProvenance::new(SOURCE_BASELINE, false);
        assert!(delivery.record_implementation("abbreviated").is_err());
        assert!(delivery.resulting_commit.is_none());
        assert!(delivery.record_review(review()).is_err());
        assert!(delivery.independent_review.is_none());

        delivery.record_implementation(RESULT_COMMIT).unwrap();
        assert!(delivery.record_integration(integration()).is_err());
        assert!(delivery.integration.is_none());
        assert!(delivery.record_artifact(artifact()).is_err());
        assert!(delivery.artifact.is_none());
        assert!(delivery.record_installation(installation()).is_err());
        assert!(delivery.installation.is_none());
        assert!(delivery
            .record_live_verification(live_verification())
            .is_err());
        assert!(delivery.live_verification.is_none());

        delivery.record_review(review()).unwrap();
        delivery.record_acceptance_test(source_test()).unwrap();
        delivery.record_integration(integration()).unwrap();
        let mut early_artifact = artifact();
        early_artifact.recorded_at = 21;
        assert!(delivery.record_artifact(early_artifact).is_err());
        assert!(delivery.artifact.is_none());
        delivery.record_artifact(artifact()).unwrap();

        let mut wrong_installation = installation();
        wrong_installation.artifact_id = "sha256:different".into();
        assert!(delivery.record_installation(wrong_installation).is_err());
        assert!(delivery.installation.is_none());
    }

    #[test]
    fn visible_product_scope_requires_packaged_gui_e2e_evidence() {
        let mut delivery = DeliveryProvenance::new(SOURCE_BASELINE, true);
        delivery.record_implementation(RESULT_COMMIT).unwrap();
        delivery.record_review(review()).unwrap();
        assert!(delivery.record_acceptance_test(source_test()).is_err());
        assert!(!delivery.states().complete);

        delivery
            .record_acceptance_test(packaged_gui_test())
            .unwrap();
        assert!(delivery.states().complete);
    }

    #[test]
    fn dispatch_requires_an_exact_source_commit_without_breaking_legacy_loads() {
        let legacy = serde_json::json!({
            "agentSessionId": "agent-1",
            "captainSessionId": "captain-1",
            "projectId": "project-1",
            "assignment": "Legacy assignment",
            "directory": "/repo",
            "harness": "codex",
            "provider": "codex",
            "runtimeState": "exited",
            "workStage": "complete",
            "createdAt": 10,
            "updatedAt": 11
        });
        let legacy: AgentSessionRecord = serde_json::from_value(legacy).unwrap();
        legacy.validate().unwrap();
        assert_eq!(legacy.delivery_states(), None);
        assert!(legacy.validate_for_dispatch().is_err());

        let mut dispatched = record();
        dispatched.delivery = Some(DeliveryProvenance::new(SOURCE_BASELINE, false));
        dispatched.validate_for_dispatch().unwrap();

        dispatched.delivery = Some(DeliveryProvenance::new("short", false));
        assert!(dispatched.validate_for_dispatch().is_err());
    }

    #[test]
    fn complete_work_stage_requires_independent_review_and_acceptance_tests() {
        let mut value = record();
        value.work_stage = WorkStage::Complete;
        value.delivery = Some(DeliveryProvenance::new(SOURCE_BASELINE, false));
        assert!(value.validate().is_err());

        let delivery = value.delivery.as_mut().unwrap();
        delivery.record_implementation(RESULT_COMMIT).unwrap();
        let mut self_review = review();
        self_review.reviewer_identity = value.agent_session_id.clone();
        delivery.record_review(self_review).unwrap();
        delivery.record_acceptance_test(source_test()).unwrap();
        assert!(value.validate().is_err());

        value.delivery.as_mut().unwrap().independent_review = Some(review());
        value.validate().unwrap();
    }

    #[test]
    fn recorded_delivery_evidence_is_immutable_but_idempotent() {
        let mut delivery = DeliveryProvenance::new(SOURCE_BASELINE, false);
        delivery.record_implementation(RESULT_COMMIT).unwrap();
        delivery.record_review(review()).unwrap();
        delivery.record_review(review()).unwrap();

        let mut replacement = review();
        replacement.reference = "review://replacement".into();
        assert!(delivery.record_review(replacement).is_err());
        assert_eq!(delivery.independent_review, Some(review()));
    }

    #[test]
    fn explicit_work_stage_survives_runtime_updates() {
        let mut value = record();
        value.runtime_state = RuntimeState::Idle;
        assert_eq!(value.work_stage, WorkStage::Assigned);
        value.runtime_state = RuntimeState::Exited;
        assert_eq!(value.work_stage, WorkStage::Assigned);
    }

    #[test]
    fn validation_bounds_assignment_and_checkpoint_text() {
        let mut value = record();
        value.assignment = "x".repeat(MAX_ASSIGNMENT_BYTES + 1);
        assert!(value.validate().is_err());

        let checkpoint = AgentCheckpoint {
            cursor: 1,
            agent_session_id: "agent-1".into(),
            author_session_id: "captain-1".into(),
            summary: "x".repeat(MAX_CHECKPOINT_BYTES + 1),
            created_at: 11,
        };
        assert!(checkpoint.validate().is_err());
    }

    #[test]
    fn snapshot_digest_is_stable() {
        let value = record();
        assert_eq!(
            snapshot_digest(&value).unwrap(),
            snapshot_digest(&value).unwrap()
        );
    }
}
