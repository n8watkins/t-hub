//! Powder-independent durable records for supervised agent sessions.
//!
//! This module intentionally contains no terminal, provider, registry, or
//! network code.  It is the stable data boundary used by the de-Powder control
//! contract while the legacy Crew representation remains readable elsewhere.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;

pub const MAX_ASSIGNMENT_BYTES: usize = 16 * 1024;
pub const MAX_CHECKPOINT_BYTES: usize = 4 * 1024;
pub const MAX_EVIDENCE_REFERENCE_BYTES: usize = 16 * 1024;
pub const MAX_INTEGRATION_INPUTS: usize = 256;
pub const MAX_INTEGRATION_ID_BYTES: usize = 1024;
pub const MAX_EVENT_BATCH: usize = 128;
pub const MAX_CHECKPOINT_HISTORY: usize = 4096;
pub const MAX_FOLLOWUP_BYTES: usize = 16 * 1024;

/// Provider-neutral durable follow-up intent. Control transports parse into this
/// type, while validation stays independent of MCP, sockets, terminal delivery,
/// and JSON response formatting.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentFollowup {
    pub request_id: String,
    pub captain_session_id: String,
    pub ship_slug: String,
    pub project_id: String,
    pub agent_session_id: String,
    pub message: String,
    /// Replaces durable Assignment metadata only when explicitly supplied.
    pub replacement_assignment: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentFollowupOutcome {
    pub request_id: String,
    pub captain_session_id: String,
    pub ship_slug: String,
    pub project_id: String,
    pub agent_session_id: String,
    pub message_seq: u64,
    pub idempotent_replay: bool,
    pub assignment_changed: bool,
}

impl AgentFollowup {
    pub fn validate(&self) -> Result<(), String> {
        for (field, value) in [
            ("requestId", self.request_id.as_str()),
            ("captainSessionId", self.captain_session_id.as_str()),
            ("shipSlug", self.ship_slug.as_str()),
            ("projectId", self.project_id.as_str()),
            ("agentSessionId", self.agent_session_id.as_str()),
            ("message", self.message.as_str()),
        ] {
            if value.trim().is_empty() {
                return Err(format!("agent_followup requires a non-empty '{field}'"));
            }
        }
        if self.request_id.len() > 128
            || !self.request_id.bytes().all(|byte| {
                byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b':' | b'.')
            })
        {
            return Err(
                "agent_followup requestId must be at most 128 URL-safe identifier characters"
                    .into(),
            );
        }
        if self.message.len() > MAX_FOLLOWUP_BYTES {
            return Err(format!(
                "agent_followup message must be at most {MAX_FOLLOWUP_BYTES} bytes"
            ));
        }
        if let Some(assignment) = &self.replacement_assignment {
            if assignment.trim().is_empty() {
                return Err(
                    "agent_followup replacementAssignment must be non-empty when supplied".into(),
                );
            }
            if assignment.len() > MAX_ASSIGNMENT_BYTES {
                return Err(format!(
                    "agent_followup replacementAssignment must be at most {MAX_ASSIGNMENT_BYTES} bytes"
                ));
            }
        }
        Ok(())
    }

    /// Stable digest of the complete immutable operation meaning. The durable
    /// inbox binds requestId to this value rather than only to terminal-delivery
    /// fields, so scope changes cannot masquerade as transport retries.
    pub fn semantic_digest(&self) -> String {
        let value = serde_json::json!({
            "requestId": self.request_id,
            "captainSessionId": self.captain_session_id,
            "shipSlug": self.ship_slug,
            "projectId": self.project_id,
            "agentSessionId": self.agent_session_id,
            "message": self.message,
            "replacementAssignment": self.replacement_assignment,
        });
        format!("{:x}", Sha256::digest(value.to_string()))
    }
}

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
        #[serde(alias = "artifactId")]
        artifact_id: String,
        #[serde(alias = "sourceCommit")]
        source_commit: String,
        #[serde(alias = "installationTarget")]
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

/// One ordered lane input incorporated by an integration owner.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct IntegrationInput {
    pub lane_id: String,
    pub agent_session_id: String,
    pub source_baseline: String,
    pub resulting_commit: String,
}

/// The complete ordered set of lane commits used to produce an integration.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct IntegrationManifest {
    pub integration_owner_identity: String,
    pub inputs: Vec<IntegrationInput>,
}

impl IntegrationManifest {
    pub(crate) fn validate_for_source_commit(&self, source_commit: &str) -> Result<(), String> {
        validate_bounded_identifier(
            "integration.manifest.integrationOwnerIdentity",
            &self.integration_owner_identity,
        )?;
        if self.inputs.is_empty() {
            return Err("delivery provenance integration.manifest.inputs must not be empty".into());
        }
        if self.inputs.len() > MAX_INTEGRATION_INPUTS {
            return Err(format!(
                "delivery provenance integration.manifest.inputs must contain at most {MAX_INTEGRATION_INPUTS} entries"
            ));
        }

        let mut lane_ids = BTreeSet::new();
        let mut agent_session_ids = BTreeSet::new();
        let mut contains_source_commit = false;
        for (index, input) in self.inputs.iter().enumerate() {
            let prefix = format!("integration.manifest.inputs[{index}]");
            validate_bounded_identifier(&format!("{prefix}.laneId"), &input.lane_id)?;
            validate_bounded_identifier(
                &format!("{prefix}.agentSessionId"),
                &input.agent_session_id,
            )?;
            validate_commit(&format!("{prefix}.sourceBaseline"), &input.source_baseline)?;
            validate_commit(
                &format!("{prefix}.resultingCommit"),
                &input.resulting_commit,
            )?;
            if !lane_ids.insert(input.lane_id.as_str()) {
                return Err(format!(
                    "delivery provenance integration.manifest laneId '{}' must be unique",
                    input.lane_id
                ));
            }
            if !agent_session_ids.insert(input.agent_session_id.as_str()) {
                return Err(format!(
                    "delivery provenance integration.manifest agentSessionId '{}' must be unique",
                    input.agent_session_id
                ));
            }
            contains_source_commit |= input.resulting_commit == source_commit;
        }
        if !contains_source_commit {
            return Err(
                "delivery provenance integration.manifest must contain an input whose resultingCommit equals integration.sourceCommit"
                    .into(),
            );
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct IntegrationEvidence {
    pub source_commit: String,
    pub canonical_baseline: String,
    pub canonical_commit: String,
    pub reference: String,
    pub recorded_at: u64,
    /// Missing only on integration evidence written before manifests were required.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub manifest: Option<IntegrationManifest>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ArtifactSignatureStatus {
    Unsigned,
    SignedUnverified,
    Verified,
}

/// Immutable build facts that bind an installer to its exact source and output.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ArtifactManifest {
    pub branch: String,
    pub source_commit: String,
    pub git_tree: String,
    pub version: String,
    pub installer_sha256: String,
    /// Unix epoch milliseconds at build completion.
    pub built_at: u64,
    pub signature_status: ArtifactSignatureStatus,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ArtifactEvidence {
    pub artifact_id: String,
    pub source_baseline: String,
    pub reference: String,
    pub recorded_at: u64,
    /// Missing only on artifact evidence written before build manifests were required.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub manifest: Option<ArtifactManifest>,
}

impl ArtifactEvidence {
    fn validate_complete_manifest(&self, integration: &IntegrationEvidence) -> Result<(), String> {
        let manifest = self
            .manifest
            .as_ref()
            .ok_or("new artifact evidence requires artifact.manifest")?;
        validate_commit("artifact.sourceBaseline", &self.source_baseline)?;
        validate_bounded_identifier("artifact.manifest.branch", &manifest.branch)?;
        if manifest.branch != integration.canonical_baseline {
            return Err(
                "delivery provenance artifact.manifest.branch must equal integration.canonicalBaseline"
                    .into(),
            );
        }
        validate_commit("artifact.manifest.sourceCommit", &manifest.source_commit)?;
        if self.source_baseline != integration.canonical_commit
            || manifest.source_commit != integration.canonical_commit
        {
            return Err(
                "delivery provenance artifact sourceBaseline and manifest.sourceCommit must equal integration.canonicalCommit"
                    .into(),
            );
        }
        validate_git_object("artifact.manifest.gitTree", &manifest.git_tree)?;
        validate_bounded_identifier("artifact.manifest.version", &manifest.version)?;
        validate_sha256(
            "artifact.manifest.installerSha256",
            &manifest.installer_sha256,
        )?;
        if manifest.built_at == 0 {
            return Err("delivery provenance artifact.manifest.builtAt must be positive".into());
        }
        if manifest.built_at < integration.recorded_at {
            return Err(
                "delivery provenance artifact.manifest.builtAt must not precede integration.recordedAt"
                    .into(),
            );
        }
        if manifest.built_at > self.recorded_at {
            return Err(
                "delivery provenance artifact.manifest.builtAt must not follow artifact.recordedAt"
                    .into(),
            );
        }
        Ok(())
    }
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
                    && integration.manifest.as_ref().is_some_and(|manifest|
                        manifest.validate_for_source_commit(&integration.source_commit).is_ok()
                    )
                    && integration.recorded_at >= completed_at
        );
        let packaged = matches!(
            (&self.integration, &self.artifact),
            (Some(integration), Some(artifact))
                if integrated
                    && !artifact.artifact_id.trim().is_empty()
                    && artifact.source_baseline == integration.canonical_commit
                    && validate_reference("artifact.reference", &artifact.reference).is_ok()
                    && artifact.validate_complete_manifest(integration).is_ok()
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
        evidence
            .manifest
            .as_ref()
            .ok_or("new integration evidence requires integration.manifest")?
            .validate_for_source_commit(&evidence.source_commit)?;
        let mut next = self.clone();
        set_once(&mut next.integration, evidence, "integrated")?;
        next.validate()?;
        *self = next;
        Ok(())
    }

    pub fn record_artifact(&mut self, evidence: ArtifactEvidence) -> Result<(), String> {
        if !self.states().integrated {
            return Err(
                "delivery provenance cannot be packaged before it has valid integration evidence"
                    .into(),
            );
        }
        let integration = self
            .integration
            .as_ref()
            .ok_or("delivery provenance cannot be packaged before it is integrated")?;
        evidence.validate_complete_manifest(integration)?;
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
            if let Some(manifest) = &integration.manifest {
                manifest.validate_for_source_commit(&integration.source_commit)?;
            }
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
            if artifact.manifest.is_some() {
                artifact.validate_complete_manifest(integration)?;
            }
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

fn validate_bounded_identifier(field: &str, value: &str) -> Result<(), String> {
    validate_nonempty(field, value)?;
    if value.len() > MAX_INTEGRATION_ID_BYTES {
        return Err(format!(
            "delivery provenance {field} must be at most {MAX_INTEGRATION_ID_BYTES} bytes"
        ));
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

fn validate_git_object(field: &str, value: &str) -> Result<(), String> {
    if !matches!(value.len(), 40 | 64) || !value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(format!(
            "delivery provenance {field} must be an exact 40- or 64-character hexadecimal Git object ID"
        ));
    }
    Ok(())
}

fn validate_sha256(field: &str, value: &str) -> Result<(), String> {
    if value.len() != 64 || !value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(format!(
            "delivery provenance {field} must be an exact 64-character hexadecimal SHA-256"
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
    /// Missing only on records written before adaptive dispatch preflight.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lane_claim: Option<crate::governor::LaneClaim>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub integration_contracts: Vec<crate::governor::IntegrationContract>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dispatch_capacity: Option<crate::governor::CapacityReport>,
    /// Capacity class admitted for this durable runtime. Administrative values
    /// are intent only and do not grant a role without a separate appointment.
    #[serde(default)]
    pub admission_purpose: crate::governor::AdmissionPurpose,
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
        match (&self.lane_claim, &self.dispatch_capacity) {
            (Some(lane), Some(capacity)) => {
                if lane.lane_id.trim().is_empty()
                    || lane.owner_id != self.agent_session_id
                    || lane.dependencies.is_none()
                {
                    return Err(
                        "agent session lane evidence must name this agent as the explicit owner"
                            .into(),
                    );
                }
                if capacity.requested_lanes != 1 {
                    return Err(
                        "agent session dispatch capacity must describe exactly one admitted lane"
                            .into(),
                    );
                }
            }
            (None, None) if self.integration_contracts.is_empty() => {}
            (None, None) => {
                return Err(
                    "agent session integration contracts require durable lane evidence".into(),
                );
            }
            _ => {
                return Err(
                    "agent session lane and dispatch capacity evidence must be recorded together"
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
        if self.lane_claim.is_none() || self.dispatch_capacity.is_none() {
            return Err(
                "agent session dispatch requires explicit lane ownership and adaptive capacity evidence"
                    .into(),
            );
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub delivery_states: Option<DeliveryStates>,
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
    const OTHER_RESULT_COMMIT: &str = "4444444444444444444444444444444444444444";

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
            lane_claim: None,
            integration_contracts: Vec::new(),
            dispatch_capacity: None,
            admission_purpose: crate::governor::AdmissionPurpose::Ordinary,
            created_at: 10,
            updated_at: 10,
        }
    }

    fn dispatch_evidence() -> (crate::governor::LaneClaim, crate::governor::CapacityReport) {
        let lane = crate::governor::LaneClaim {
            lane_id: "lane-1".into(),
            owner_id: "agent-1".into(),
            dependencies: Some(std::collections::BTreeSet::new()),
            mutable_files: std::collections::BTreeSet::new(),
            mutable_schemas: std::collections::BTreeSet::new(),
            mutable_interfaces: std::collections::BTreeSet::new(),
        };
        let request = crate::governor::DispatchPreflight {
            requested_lanes: vec![lane.clone()],
            requested_provider_lanes: 1,
            admission_purpose: crate::governor::AdmissionPurpose::Ordinary,
            ship_admin_scope: None,
            active_lanes: Vec::new(),
            satisfied_dependencies: std::collections::BTreeSet::new(),
            integration_contracts: Vec::new(),
            capacity: crate::governor::RuntimeCapacity {
                live_sessions: 3,
                machine_healthy: true,
                machine_session_capacity: 64,
                provider_session_capacity: 64,
                provider_live_sessions: 3,
                provider_capacity_status: crate::governor::ProviderCapacityStatus {
                    source: "test-telemetry".into(),
                    degraded: false,
                    detail: None,
                },
                available_worktrees: 8,
                active_captains: 0,
                active_captain_ships: std::collections::BTreeSet::new(),
                live_cortana: 1,
                live_fleet_admins: 1,
                live_ship_admins: 0,
                live_ship_admin_scopes: std::collections::BTreeMap::new(),
                live_recovery_sessions: 1,
            },
        };
        let capacity = crate::governor::SpawnGovernor::default()
            .preflight_dispatch(&request)
            .unwrap();
        (lane, capacity)
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
                artifact_id: "sha256:candidate-artifact".into(),
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
            manifest: Some(IntegrationManifest {
                integration_owner_identity: "integration-owner-1".into(),
                inputs: vec![
                    IntegrationInput {
                        lane_id: "shared-interface".into(),
                        agent_session_id: "agent-interface".into(),
                        source_baseline: SOURCE_BASELINE.into(),
                        resulting_commit: OTHER_RESULT_COMMIT.into(),
                    },
                    IntegrationInput {
                        lane_id: "implementation".into(),
                        agent_session_id: "agent-implementation".into(),
                        source_baseline: SOURCE_BASELINE.into(),
                        resulting_commit: RESULT_COMMIT.into(),
                    },
                ],
            }),
        }
    }

    fn artifact() -> ArtifactEvidence {
        ArtifactEvidence {
            artifact_id: "sha256:release-artifact".into(),
            source_baseline: CANONICAL_COMMIT.into(),
            reference: "artifact://windows/release".into(),
            recorded_at: 23,
            manifest: Some(ArtifactManifest {
                branch: "main".into(),
                source_commit: CANONICAL_COMMIT.into(),
                git_tree: "5555555555555555555555555555555555555555".into(),
                version: "0.3.107".into(),
                installer_sha256:
                    "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".into(),
                built_at: 22,
                signature_status: ArtifactSignatureStatus::Verified,
            }),
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
    fn packaged_gui_environment_preserves_legacy_wire_shape_and_accepts_camel_case() {
        let legacy = serde_json::json!({
            "kind": "packagedGuiE2e",
            "artifact_id": "candidate-legacy",
            "source_commit": RESULT_COMMIT,
            "installation_target": "legacy target"
        });
        let environment: AcceptanceEnvironment = serde_json::from_value(legacy.clone()).unwrap();
        assert_eq!(serde_json::to_value(&environment).unwrap(), legacy);

        let camel: AcceptanceEnvironment = serde_json::from_value(serde_json::json!({
            "kind": "packagedGuiE2e",
            "artifactId": "candidate-legacy",
            "sourceCommit": RESULT_COMMIT,
            "installationTarget": "legacy target"
        }))
        .unwrap();
        assert_eq!(camel, environment);
        let rolled_back = serde_json::to_value(camel).unwrap();
        assert_eq!(rolled_back, legacy);
        let round_trip: AcceptanceEnvironment = serde_json::from_value(rolled_back).unwrap();
        assert_eq!(round_trip, environment);
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
        assert_eq!(
            value["integration"]["manifest"]["inputs"][0]["laneId"],
            "shared-interface"
        );
        assert_eq!(
            value["integration"]["manifest"]["inputs"][1]["laneId"],
            "implementation"
        );
        assert_eq!(value["artifact"]["manifest"]["branch"], "main");
        assert_eq!(value["artifact"]["manifest"]["signatureStatus"], "verified");
        assert_eq!(value["liveVerification"]["verifierKind"], "aiAgent");
    }

    #[test]
    fn visible_gui_lifecycle_keeps_candidate_and_canonical_artifacts_distinct() {
        let mut delivery = DeliveryProvenance::new(SOURCE_BASELINE, true);
        delivery.record_implementation(RESULT_COMMIT).unwrap();
        delivery.record_review(review()).unwrap();
        delivery
            .record_acceptance_test(packaged_gui_test())
            .unwrap();
        assert!(delivery.states().complete);

        delivery.record_integration(integration()).unwrap();
        delivery.record_artifact(artifact()).unwrap();
        delivery.record_installation(installation()).unwrap();
        delivery
            .record_live_verification(live_verification())
            .unwrap();

        let value = serde_json::to_value(&delivery).unwrap();
        assert_eq!(
            value["acceptanceTest"]["environment"]["artifact_id"],
            "sha256:candidate-artifact"
        );
        assert_eq!(value["artifact"]["artifactId"], "sha256:release-artifact");
        assert_eq!(
            value["acceptanceTest"]["environment"]["source_commit"],
            RESULT_COMMIT
        );
        assert_eq!(value["artifact"]["sourceBaseline"], CANONICAL_COMMIT);
        assert!(delivery.states().live_verified);
    }

    #[test]
    fn legacy_integration_without_a_manifest_loads_but_is_not_integrated() {
        let mut delivery = DeliveryProvenance::new(SOURCE_BASELINE, false);
        delivery.record_implementation(RESULT_COMMIT).unwrap();
        delivery.record_review(review()).unwrap();
        delivery.record_acceptance_test(source_test()).unwrap();
        delivery.record_integration(integration()).unwrap();
        let mut value = serde_json::to_value(delivery).unwrap();
        value["integration"]
            .as_object_mut()
            .unwrap()
            .remove("manifest");

        let legacy: DeliveryProvenance = serde_json::from_value(value).unwrap();
        legacy.validate().unwrap();
        assert!(legacy.states().complete);
        assert!(!legacy.states().integrated);
        assert!(legacy.integration.unwrap().manifest.is_none());
    }

    #[test]
    fn new_integrations_require_a_valid_complete_ordered_manifest() {
        let mut delivery = DeliveryProvenance::new(SOURCE_BASELINE, false);
        delivery.record_implementation(RESULT_COMMIT).unwrap();
        delivery.record_review(review()).unwrap();
        delivery.record_acceptance_test(source_test()).unwrap();

        let mut missing = integration();
        missing.manifest = None;
        assert!(delivery.record_integration(missing).is_err());
        assert!(delivery.integration.is_none());

        let mut empty = integration();
        empty.manifest.as_mut().unwrap().inputs.clear();
        assert!(delivery.record_integration(empty).is_err());
        assert!(delivery.integration.is_none());

        let mut duplicate_lane = integration();
        duplicate_lane.manifest.as_mut().unwrap().inputs[1].lane_id = "shared-interface".into();
        assert!(delivery.record_integration(duplicate_lane).is_err());
        assert!(delivery.integration.is_none());

        let mut duplicate_agent = integration();
        duplicate_agent.manifest.as_mut().unwrap().inputs[1].agent_session_id =
            "agent-interface".into();
        assert!(delivery.record_integration(duplicate_agent).is_err());
        assert!(delivery.integration.is_none());

        let mut malformed_commit = integration();
        malformed_commit.manifest.as_mut().unwrap().inputs[0].source_baseline = "main".into();
        assert!(delivery.record_integration(malformed_commit).is_err());
        assert!(delivery.integration.is_none());

        let mut missing_source = integration();
        missing_source.manifest.as_mut().unwrap().inputs[1].resulting_commit =
            CANONICAL_COMMIT.into();
        assert!(delivery.record_integration(missing_source).is_err());
        assert!(delivery.integration.is_none());

        let mut oversized = integration();
        let input = oversized.manifest.as_ref().unwrap().inputs[0].clone();
        oversized.manifest.as_mut().unwrap().inputs = vec![input; MAX_INTEGRATION_INPUTS + 1];
        assert!(delivery.record_integration(oversized).is_err());
        assert!(delivery.integration.is_none());

        delivery.record_integration(integration()).unwrap();
        assert!(delivery.states().integrated);
    }

    #[test]
    fn legacy_artifact_without_a_manifest_loads_but_is_not_packaged() {
        let mut delivery = DeliveryProvenance::new(SOURCE_BASELINE, false);
        delivery.record_implementation(RESULT_COMMIT).unwrap();
        delivery.record_review(review()).unwrap();
        delivery.record_acceptance_test(source_test()).unwrap();
        delivery.record_integration(integration()).unwrap();
        delivery.record_artifact(artifact()).unwrap();
        let mut value = serde_json::to_value(delivery).unwrap();
        value["artifact"]
            .as_object_mut()
            .unwrap()
            .remove("manifest");

        let legacy: DeliveryProvenance = serde_json::from_value(value).unwrap();
        legacy.validate().unwrap();
        assert!(legacy.states().integrated);
        assert!(!legacy.states().packaged);
        assert!(legacy.artifact.unwrap().manifest.is_none());
    }

    #[test]
    fn new_artifacts_require_complete_exact_build_provenance() {
        let sha256_source = "b".repeat(64);
        let mut sha256_integration = integration();
        sha256_integration.canonical_commit = sha256_source.clone();
        let mut sha256_artifact = artifact();
        sha256_artifact.source_baseline = sha256_source.clone();
        sha256_artifact.manifest.as_mut().unwrap().source_commit = sha256_source;
        sha256_artifact.manifest.as_mut().unwrap().git_tree = "c".repeat(64);
        sha256_artifact
            .validate_complete_manifest(&sha256_integration)
            .unwrap();

        let mut delivery = DeliveryProvenance::new(SOURCE_BASELINE, false);
        delivery.record_implementation(RESULT_COMMIT).unwrap();
        delivery.record_review(review()).unwrap();
        delivery.record_acceptance_test(source_test()).unwrap();
        delivery.record_integration(integration()).unwrap();

        let mut missing = artifact();
        missing.manifest = None;
        assert!(delivery.record_artifact(missing).is_err());
        assert!(delivery.artifact.is_none());

        let mut wrong_branch = artifact();
        wrong_branch.manifest.as_mut().unwrap().branch = "release".into();
        assert!(delivery.record_artifact(wrong_branch).is_err());

        let mut wrong_source = artifact();
        wrong_source.manifest.as_mut().unwrap().source_commit = RESULT_COMMIT.into();
        assert!(delivery.record_artifact(wrong_source).is_err());

        let mut short_tree = artifact();
        short_tree.manifest.as_mut().unwrap().git_tree = "5".repeat(39);
        assert!(delivery.record_artifact(short_tree).is_err());

        let mut short_installer_hash = artifact();
        short_installer_hash
            .manifest
            .as_mut()
            .unwrap()
            .installer_sha256 = "a".repeat(63);
        assert!(delivery.record_artifact(short_installer_hash).is_err());

        let mut non_hex_installer_hash = artifact();
        non_hex_installer_hash
            .manifest
            .as_mut()
            .unwrap()
            .installer_sha256 = "z".repeat(64);
        assert!(delivery.record_artifact(non_hex_installer_hash).is_err());

        let mut future_build = artifact();
        future_build.manifest.as_mut().unwrap().built_at = future_build.recorded_at + 1;
        assert!(delivery.record_artifact(future_build).is_err());

        let mut pre_integration_build = artifact();
        pre_integration_build.manifest.as_mut().unwrap().built_at = integration().recorded_at - 1;
        assert!(delivery.record_artifact(pre_integration_build).is_err());

        delivery.record_artifact(artifact()).unwrap();
        assert!(delivery.states().packaged);
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
        let (lane_claim, dispatch_capacity) = dispatch_evidence();
        dispatched.lane_claim = Some(lane_claim);
        dispatched.dispatch_capacity = Some(dispatch_capacity);
        dispatched.validate_for_dispatch().unwrap();

        dispatched.dispatch_capacity = None;
        assert!(dispatched.validate_for_dispatch().is_err());
        let (_, dispatch_capacity) = dispatch_evidence();
        dispatched.dispatch_capacity = Some(dispatch_capacity);
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
    fn followup_validation_requires_stable_identity_and_explicit_valid_scope() {
        let mut followup = AgentFollowup {
            request_id: "followup:one".into(),
            captain_session_id: "captain-1".into(),
            ship_slug: "ship-1".into(),
            project_id: "project-1".into(),
            agent_session_id: "agent-1".into(),
            message: "Continue with the reviewed fix.".into(),
            replacement_assignment: None,
        };
        followup.validate().unwrap();
        followup.request_id = "bad request".into();
        assert!(followup.validate().is_err());
        followup.request_id = "followup:one".into();
        followup.replacement_assignment = Some(String::new());
        assert!(followup.validate().is_err());

        followup.replacement_assignment = None;
        let original = followup.semantic_digest();
        followup.replacement_assignment = Some("Changed Assignment".into());
        assert_ne!(followup.semantic_digest(), original);
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
