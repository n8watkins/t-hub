//! Protected Powder connection profiles and the narrow API client T-Hub uses.
//!
//! Captain state stores only a profile name and Powder repository. Endpoint and
//! credential material lives in `~/.t-hub/powder-profiles.json` or process env.

use std::collections::HashMap;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use hmac::{Hmac, Mac};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use crate::bounded_exec;

const HTTP_TIMEOUT: Duration = Duration::from_secs(12);
const KEY_COMMAND_TIMEOUT: Duration = Duration::from_secs(10);
const MAX_EVIDENCE_RESPONSE_BYTES: usize = 512 * 1024;
const MAX_MUTATION_RESPONSE_BYTES: usize = 1024 * 1024;
const MAX_CLAIM_RESPONSE_BYTES: usize = 64 * 1024;
const MAX_CLAIM_IDENTITY_BYTES: usize = 512;
const MAX_CAPABILITY_RESPONSE_BYTES: usize = 128 * 1024;
const MAX_EVIDENCE_ITEMS: usize = 20;
const MAX_CRITERIA: usize = 100;
const MAX_CRITERION_PROOFS: usize = 20;
const MAX_EVIDENCE_TOTAL: usize = 1_000_000;
const MAX_ID_BYTES: usize = 256;
const MAX_SHORT_TEXT_BYTES: usize = 512;
const MAX_EVIDENCE_TEXT_BYTES: usize = 4096;
const MAX_OPERATION_ID_BYTES: usize = 128;
const MAX_OPERATION_AUTHORITY_BYTES: usize = 256;
const MAX_OPERATION_REQUEST_BYTES: usize = 64 * 1024;
const MAX_OPERATION_FAILURE_BYTES: usize = 512;
const MAX_WORK_LOG_ATTRIBUTION_BYTES: usize = 256;
const MAX_CRITERION_REVIEWER_BYTES: usize = 256;
pub const MAX_WORK_LOG_BODY_BYTES: usize = 16 * 1024;
pub const MAX_COMPLETION_PROOF_BYTES: usize = 4096;
pub const MAX_COMPLETION_CRITERION_PROOFS: usize = 128;
pub const MAX_PROOF_URL_BYTES: usize = 4096;

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ProfileFile {
    #[serde(default)]
    schema_version: u32,
    #[serde(default)]
    profiles: HashMap<String, ProfileConfig>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ProfileConfig {
    base_url: String,
    agent_name: String,
    #[serde(default)]
    operation_identity: Option<String>,
    #[serde(default)]
    api_key: Option<String>,
    #[serde(default)]
    api_key_env: Option<String>,
    #[serde(default)]
    api_key_command: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Claim {
    pub card_id: String,
    pub run_id: String,
    pub agent: String,
    pub expires_at: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EvidenceClaim {
    pub run_id: String,
    pub agent: String,
    pub expires_at: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkLogAttribution {
    pub agent: String,
    pub model: Option<String>,
    pub reasoning: Option<String>,
    pub harness: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunBoundWorkLog {
    pub expected_run_id: String,
    pub operation_id: String,
    pub attribution: WorkLogAttribution,
    pub body: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunBoundCriterionReview {
    pub expected_run_id: String,
    pub operation_id: String,
    pub criterion_index: usize,
    pub criterion_id: String,
    pub criterion_text: String,
    pub decision: CriterionReviewDecision,
    pub proof: Option<String>,
    pub expected_reviewer_identity: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkLogEntry {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub schema_version: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[serde(rename = "id")]
    pub entry_id: Option<String>,
    pub card_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub actor: Option<String>,
    pub agent: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reasoning: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub harness: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub run_id: Option<String>,
    pub body: String,
    pub created_at: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub updated_at: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunBoundCompletion {
    pub expected_run_id: String,
    pub operation_id: String,
    pub proof: String,
    pub criterion_proofs: Vec<CriterionProof>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CriterionProof {
    pub criterion: usize,
    pub url: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EvidenceProofLink {
    pub url: String,
    pub actor: String,
    pub created_at: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CriterionEvidence {
    pub criterion: usize,
    pub text: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub checked_by: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub checked_at: Option<i64>,
    pub proof_links: Vec<EvidenceProofLink>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CriterionReviewDecision {
    Approved,
    Rejected,
    Cleared,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RunCriterionReview {
    pub review_id: String,
    pub operation_id: String,
    pub card_id: String,
    pub run_id: String,
    pub criterion_index: usize,
    pub criterion_id: String,
    pub criterion_text: String,
    pub decision: CriterionReviewDecision,
    pub reviewer: String,
    pub reviewer_identity: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub proof: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub supersedes_review_id: Option<String>,
    pub created_at: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RunCriterionEvidence {
    pub criterion_index: usize,
    pub criterion_id: String,
    pub criterion_text: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub review: Option<RunCriterionReview>,
}

impl RunCriterionEvidence {
    pub fn is_approved(&self) -> bool {
        self.review
            .as_ref()
            .is_some_and(|review| review.decision == CriterionReviewDecision::Approved)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RunSummary {
    pub run_id: String,
    pub card_id: String,
    pub state: String,
    pub agent: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub proof: Option<String>,
    pub created_at: i64,
    pub updated_at: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EvidenceActivity {
    pub activity_type: String,
    pub payload: String,
    pub created_at: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EvidenceLink {
    pub label: String,
    pub url: String,
    pub created_at: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CardEvidence {
    pub card_id: String,
    pub title: String,
    pub status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub repository: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub claim: Option<EvidenceClaim>,
    pub criteria: Vec<CriterionEvidence>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub current_run_criteria: Option<Vec<RunCriterionEvidence>>,
    pub runs: Vec<RunSummary>,
    pub runs_total: usize,
    pub work_log: Vec<WorkLogEntry>,
    pub work_log_total: usize,
    pub truncated: bool,
}

impl CardEvidence {
    /// Return authoritative criteria only when the card still has the exact
    /// expected run as its current claim. Legacy checked fields are ignored.
    pub fn completion_criteria_for_run(
        &self,
        expected_run_id: &str,
    ) -> Result<&[RunCriterionEvidence], PowderError> {
        let claim = self.claim.as_ref().ok_or_else(|| {
            invalid_request("card has no current run-scoped criterion projection")
        })?;
        if claim.run_id != expected_run_id {
            return Err(invalid_request(
                "card current claim does not match the expected completion run",
            ));
        }
        self.current_run_criteria
            .as_deref()
            .ok_or_else(|| invalid_request("card has no current run-scoped criterion projection"))
    }

    /// Fail closed unless every current criterion has a latest authoritative
    /// review whose decision is exactly `approved` for the expected run.
    pub fn require_completion_criteria_approved(
        &self,
        expected_run_id: &str,
    ) -> Result<(), PowderError> {
        let criteria = self.completion_criteria_for_run(expected_run_id)?;
        if let Some(criterion) = criteria.iter().find(|criterion| !criterion.is_approved()) {
            return Err(invalid_request(format!(
                "criterion {} is not approved for the expected run",
                criterion.criterion_index
            )));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RunEvidence {
    pub run: RunSummary,
    pub card_title: String,
    pub card_status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub repository: Option<String>,
    pub card_criteria: Vec<CriterionEvidence>,
    pub criteria: Vec<RunCriterionEvidence>,
    pub activities: Vec<EvidenceActivity>,
    pub activities_total: usize,
    pub links: Vec<EvidenceLink>,
    pub links_total: usize,
    pub truncated: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CompletionReceipt {
    pub schema_version: String,
    pub card_id: String,
    pub run_id: String,
    pub operation_id: String,
    pub status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub proof: Option<String>,
    pub criterion_proofs: Vec<CriterionProof>,
    pub updated_at: i64,
    pub audit_event_id: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum OperationState {
    Unknown,
    Pending,
    Succeeded,
    Rejected,
    Failed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OperationFailure {
    pub code: String,
    pub message: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OperationOutcome<T> {
    pub operation_id: String,
    pub state: OperationState,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub request_digest: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub target_card_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expected_run_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<T>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub failure: Option<OperationFailure>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub audit_event_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub created_at: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub updated_at: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RunBoundMutationCapabilities {
    pub schema_version: i64,
    pub work_log: bool,
    pub criterion_review: bool,
    pub completion: bool,
    pub operation_recovery: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct CardEvent {
    pub sequence: i64,
    pub event_id: String,
    pub event_type: String,
    pub occurred_at: i64,
    pub card_id: String,
    pub card_title: String,
    pub card_status: String,
    pub repository: Option<String>,
    pub change: Value,
}

#[derive(Debug, Clone, PartialEq)]
pub struct DetailedCard {
    envelope: Value,
    repository: Option<String>,
}

impl DetailedCard {
    pub fn envelope(&self) -> &Value {
        &self.envelope
    }

    pub fn card_value(&self) -> &Value {
        &self.envelope["card"]
    }

    pub fn repository(&self) -> Option<&str> {
        self.repository.as_deref()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PowderBoard {
    pub name: String,
    pub aliases: Vec<String>,
    pub tier: String,
    pub card_count: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PowderErrorKind {
    Unauthorized,
    NotFound,
    Conflict,
    Unreachable,
    Upstream,
    InvalidResponse,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PowderError {
    pub kind: PowderErrorKind,
    pub message: String,
}

impl std::fmt::Display for PowderError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.message)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PowderBoardCard {
    pub id: String,
    pub title: String,
    pub status: String,
    pub priority: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub estimate: Option<String>,
    pub labels: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub claim: Option<PowderBoardClaim>,
    pub updated_at: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PowderBoardClaim {
    pub agent: String,
    pub expires_at: i64,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PowderBoardPage {
    pub cards: Vec<PowderBoardCard>,
    pub total_count: usize,
    pub has_more: bool,
}

#[derive(Debug, Deserialize)]
struct PowderRepositoryList {
    repositories: Vec<PowderRepositorySummary>,
}

#[derive(Debug, Deserialize)]
struct PowderRepositorySummary {
    name: String,
    #[serde(default)]
    aliases: Vec<String>,
    tier: String,
    card_count: usize,
}

pub struct Client {
    base_url: String,
    agent_name: String,
    operation_identity: Option<String>,
    api_key: Option<String>,
    agent: ureq::Agent,
}

pub fn profiles_path() -> PathBuf {
    if let Some(path) = std::env::var_os("T_HUB_POWDER_PROFILES_FILE") {
        return PathBuf::from(path);
    }
    let home = std::env::var_os("USERPROFILE")
        .or_else(|| std::env::var_os("HOME"))
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."));
    home.join(".t-hub").join("powder-profiles.json")
}

/// Return configured profile names without exposing endpoints or credentials.
pub fn configured_profile_names() -> Result<Vec<String>, String> {
    configured_profile_names_from_path(&profiles_path())
}

fn configured_profile_names_from_path(path: &Path) -> Result<Vec<String>, String> {
    match std::fs::read_to_string(path) {
        Ok(body) => {
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let mode = std::fs::metadata(path)
                    .map_err(|error| {
                        format!(
                            "Powder profile file '{}' metadata is unavailable: {error}",
                            path.display()
                        )
                    })?
                    .permissions()
                    .mode();
                if mode & 0o077 != 0 {
                    return Err(format!(
                        "Powder profile file '{}' must be private (chmod 600)",
                        path.display()
                    ));
                }
            }
            let file: ProfileFile = serde_json::from_str(&body).map_err(|error| {
                format!(
                    "Powder profile file '{}' is invalid: {error}",
                    path.display()
                )
            })?;
            if file.schema_version > 1 {
                return Err(format!(
                    "Powder profile file '{}' has unsupported schemaVersion {}",
                    path.display(),
                    file.schema_version
                ));
            }
            let mut names: Vec<String> = file.profiles.into_keys().collect();
            names.sort();
            Ok(names)
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            let env_configured = std::env::var_os("POWDER_API_BASE_URL").is_some()
                && std::env::var_os("POWDER_AGENT_NAME").is_some();
            Ok(if env_configured {
                vec!["default".to_string()]
            } else {
                Vec::new()
            })
        }
        Err(error) => Err(format!(
            "Powder profile file '{}' is unavailable: {error}",
            path.display()
        )),
    }
}

impl Client {
    pub fn from_profile(name: &str) -> Result<Self, String> {
        Self::from_profile_path(name, &profiles_path())
    }

    fn from_profile_path(name: &str, path: &Path) -> Result<Self, String> {
        let config = match std::fs::read_to_string(path) {
            Ok(body) => {
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    let mode = std::fs::metadata(path)
                        .map_err(|error| {
                            format!(
                                "Powder profile file '{}' metadata is unavailable: {error}",
                                path.display()
                            )
                        })?
                        .permissions()
                        .mode();
                    if mode & 0o077 != 0 {
                        return Err(format!(
                            "Powder profile file '{}' must be private (chmod 600)",
                            path.display()
                        ));
                    }
                }
                let file: ProfileFile = serde_json::from_str(&body).map_err(|error| {
                    format!(
                        "Powder profile file '{}' is invalid: {error}",
                        path.display()
                    )
                })?;
                if file.schema_version > 1 {
                    return Err(format!(
                        "Powder profile file '{}' has unsupported schemaVersion {}",
                        path.display(),
                        file.schema_version
                    ));
                }
                file.profiles.get(name).cloned().ok_or_else(|| {
                    format!(
                        "Powder connection profile '{name}' is not configured in '{}'",
                        path.display()
                    )
                })?
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound && name == "default" => {
                ProfileConfig {
                    base_url: std::env::var("POWDER_API_BASE_URL").map_err(|_| {
                        format!(
                            "Powder profile 'default' is not configured: create '{}' or set POWDER_API_BASE_URL",
                            path.display()
                        )
                    })?,
                    agent_name: std::env::var("POWDER_AGENT_NAME").map_err(|_| {
                        "Powder default profile requires POWDER_AGENT_NAME to match its agent-scoped API key"
                            .to_string()
                    })?,
                    operation_identity: std::env::var("POWDER_OPERATION_IDENTITY").ok(),
                    api_key: std::env::var("POWDER_API_KEY").ok(),
                    api_key_env: None,
                    api_key_command: std::env::var("POWDER_API_KEY_CMD").ok(),
                }
            }
            Err(error) => {
                return Err(format!(
                    "Powder profile file '{}' could not be read: {error}",
                    path.display()
                ));
            }
        };
        Self::new(config)
    }

    fn new(config: ProfileConfig) -> Result<Self, String> {
        let base_url = config.base_url.trim().trim_end_matches('/').to_string();
        validate_base_url(&base_url)?;
        let agent_name = config.agent_name.trim().to_string();
        if agent_name.is_empty() {
            return Err("Powder agentName must not be empty".into());
        }
        let operation_identity = config
            .operation_identity
            .map(|identity| identity.trim().to_string())
            .filter(|identity| !identity.is_empty());
        if operation_identity
            .as_ref()
            .is_some_and(|identity| identity.len() > MAX_OPERATION_AUTHORITY_BYTES)
        {
            return Err(format!(
                "Powder operationIdentity exceeds the {MAX_OPERATION_AUTHORITY_BYTES}-byte limit"
            ));
        }
        let api_key = if let Some(command) = config.api_key_command.as_deref() {
            Some(resolve_key_command(command)?)
        } else if let Some(env_name) = config.api_key_env.as_deref() {
            Some(std::env::var(env_name).map_err(|_| {
                format!("Powder API key environment variable '{env_name}' is not set")
            })?)
        } else {
            config.api_key.filter(|key| !key.trim().is_empty())
        };
        let agent = ureq::AgentBuilder::new()
            .timeout_connect(Duration::from_secs(5))
            .timeout_read(HTTP_TIMEOUT)
            .timeout_write(HTTP_TIMEOUT)
            .build();
        Ok(Self {
            base_url,
            agent_name,
            operation_identity,
            api_key,
            agent,
        })
    }

    pub fn health(&self) -> Result<Value, String> {
        self.request("GET", "/healthz", None)
    }

    /// Prove that the configured credential can reach an agent-protected API.
    /// Health endpoints are commonly public and are not an authorization check.
    pub fn authorization_probe(&self) -> Result<(), String> {
        self.tail_events(0, 1).map(|_| ())
    }

    pub fn get_card(&self, card_id: &str) -> Result<DetailedCard, String> {
        parse_detailed_card(
            card_id,
            self.request(
                "GET",
                &format!("/api/v1/cards/{}?detail=detailed", encode_path(card_id)),
                None,
            )?,
        )
    }

    /// Resolve the canonical repository through Powder's agent-authorized API.
    /// A successful response proves both that the mapping exists and that this
    /// profile may access it; health and the global event stream cannot do that.
    pub fn get_repository(&self, repository: &str) -> Result<Value, String> {
        self.request(
            "GET",
            &format!("/api/v1/repositories/{}", encode_path(repository)),
            None,
        )
    }

    /// List visible Powder repository entities for T-Hub's board picker.
    /// Creation remains a separate admin-authorized Powder operation.
    pub fn list_boards(&self) -> Result<Vec<PowderBoard>, String> {
        parse_board_list(self.request("GET", "/api/v1/repositories", None)?)
    }

    /// Return a bounded, repository-scoped card page for T-Hub's native Board.
    /// The protected credential stays inside this client and the parsed response
    /// intentionally omits card bodies and other unnecessary upstream fields.
    pub fn board_page(
        &self,
        repository: &str,
        limit: usize,
    ) -> Result<PowderBoardPage, PowderError> {
        let limit = limit.clamp(1, 1000);
        let value = self.request_typed(
            "GET",
            &format!(
                "/api/v1/cards?repo={}&limit={limit}",
                encode_path(repository)
            ),
            None,
        )?;
        parse_board_page(value)
    }

    pub fn repository_for_board(&self, repository: &str) -> Result<PowderBoard, PowderError> {
        let value = self.request_typed(
            "GET",
            &format!("/api/v1/repositories/{}", encode_path(repository)),
            None,
        )?;
        parse_board_repository(value)
    }

    /// A credential-free URL for opening Powder's complete board externally.
    /// Powder does not currently support repository-filtered board URLs.
    pub fn external_board_url(&self) -> String {
        format!("{}/board", public_endpoint_origin(&self.base_url))
    }

    pub fn claim(&self, card_id: &str, ttl_seconds: u64) -> Result<Claim, String> {
        let value = self
            .request_typed_with_limit(
                "POST",
                &format!("/api/v1/cards/{}/claim", encode_path(card_id)),
                Some(json!({ "agent": self.agent_name, "ttl_seconds": ttl_seconds })),
                MAX_CLAIM_RESPONSE_BYTES,
            )
            .map_err(|error| error.to_string())?;
        validate_initial_claim_receipt(card_id, &self.agent_name, parse_claim(value)?)
    }

    pub fn configured_agent(&self) -> &str {
        &self.agent_name
    }

    /// Non-persisted keyed identity for a validated protected endpoint.
    ///
    /// This is HMAC-SHA-256 over the normalized protected URL, keyed by the
    /// protected API credential.  The key and URL remain client-local, and only
    /// the resulting identity is safe to persist for exact remap detection.
    pub fn endpoint_identity(&self) -> Result<String, String> {
        let credential = self
            .api_key
            .as_deref()
            .filter(|key| !key.is_empty())
            .ok_or(
                "Powder protected endpoint identity cannot be derived without an API credential",
            )?;
        let mut mac = Hmac::<Sha256>::new_from_slice(credential.as_bytes())
            .map_err(|_| "Powder protected endpoint identity could not be initialized")?;
        mac.update(self.base_url.as_bytes());
        Ok(format!("hmac-sha256:{:x}", mac.finalize().into_bytes()))
    }

    pub fn initial_claim_operation_id(&self, card_id: &str) -> Result<String, PowderError> {
        Ok(format!(
            "initial-claim:{}:{}",
            self.require_operation_identity()?,
            card_id
        ))
    }

    pub fn heartbeat(&self, claim: &Claim) -> Result<Claim, String> {
        let value = self.request(
            "POST",
            &format!("/api/v1/cards/{}/heartbeat", encode_path(&claim.card_id)),
            Some(json!({ "run_id": claim.run_id })),
        )?;
        validate_claim_receipt("heartbeat", claim, parse_claim(value)?)
    }

    pub fn renew(&self, claim: &Claim, ttl_seconds: u64) -> Result<Claim, String> {
        let value = self.request(
            "POST",
            &format!("/api/v1/cards/{}/renew", encode_path(&claim.card_id)),
            Some(json!({
                "run_id": claim.run_id,
                "ttl_seconds": ttl_seconds,
            })),
        )?;
        validate_claim_receipt("renewal", claim, parse_claim(value)?)
    }

    pub fn release(&self, claim: &Claim) -> Result<Claim, String> {
        let value = self.request(
            "POST",
            &format!("/api/v1/cards/{}/release", encode_path(&claim.card_id)),
            Some(json!({ "run_id": claim.run_id })),
        )?;
        validate_claim_receipt("release", claim, parse_claim(value)?)
    }

    /// Fail closed unless the connected Powder deployment advertises the full
    /// schema-18 run-bound mutation and recovery contract.
    pub fn require_run_bound_mutation_capabilities(
        &self,
    ) -> Result<RunBoundMutationCapabilities, PowderError> {
        self.require_operation_identity()?;
        let ready =
            self.request_typed_with_limit("GET", "/readyz", None, MAX_CAPABILITY_RESPONSE_BYTES)?;
        if ready.get("ok").and_then(Value::as_bool) != Some(true) {
            return Err(invalid_response("Powder is not ready"));
        }
        let schema_version = ready
            .get("schema_version")
            .and_then(Value::as_i64)
            .ok_or_else(|| invalid_response("Powder readiness omitted schema_version"))?;
        if schema_version < 18 {
            return Err(invalid_request(format!(
                "Powder schema {schema_version} does not support run-bound mutations"
            )));
        }
        let routes = self.request_typed_with_limit(
            "GET",
            "/api/v1/routes",
            None,
            MAX_CAPABILITY_RESPONSE_BYTES,
        )?;
        let routes = routes
            .as_array()
            .filter(|routes| routes.len() <= 256)
            .ok_or_else(|| invalid_response("Powder route catalog is invalid or oversized"))?;
        let supports = |method: &str, path: &str, required_fields: &[&str]| {
            routes.iter().any(|route| {
                route.get("method").and_then(Value::as_str) == Some(method)
                    && route.get("path").and_then(Value::as_str) == Some(path)
                    && required_fields.iter().all(|field| {
                        route
                            .get("body_shape")
                            .and_then(Value::as_str)
                            .is_some_and(|shape| shape.contains(&format!("\"{field}\"")))
                    })
            })
        };
        let capabilities = RunBoundMutationCapabilities {
            schema_version,
            work_log: supports(
                "POST",
                "/api/v1/cards/{id}/runs/{run_id}/work-log",
                &["operation_id", "agent", "body"],
            ),
            criterion_review: supports(
                "POST",
                "/api/v1/cards/{id}/runs/{run_id}/criteria/review",
                &["operation_id", "criterion", "criterion_id", "decision"],
            ),
            completion: supports(
                "POST",
                "/api/v1/cards/{id}/runs/{run_id}/complete",
                &["operation_id", "proof", "criterion_proofs"],
            ),
            operation_recovery: supports("GET", "/api/v1/operations/{id}", &[]),
        };
        if !capabilities.work_log
            || !capabilities.criterion_review
            || !capabilities.completion
            || !capabilities.operation_recovery
        {
            return Err(invalid_request(
                "Powder does not advertise the complete run-bound mutation contract",
            ));
        }
        Ok(capabilities)
    }

    /// Atomically append one Crew-attributed entry to an exact current run.
    ///
    /// `card_id`, `expected_run_id`, and `operation_id` come from T-Hub's
    /// durable Crew binding. Powder derives the actor from the protected API
    /// key, so no caller-controlled actor is sent. The returned operation
    /// outcome preserves pending, rejected, failed, and replayed states.
    pub fn append_run_work_log(
        &self,
        card_id: &str,
        append: &RunBoundWorkLog,
    ) -> Result<OperationOutcome<WorkLogEntry>, PowderError> {
        validate_id("card id", card_id)?;
        validate_id("expected run id", &append.expected_run_id)?;
        validate_operation_id(&append.operation_id)?;
        validate_required_bounded_text(
            "work-log agent",
            &append.attribution.agent,
            MAX_WORK_LOG_ATTRIBUTION_BYTES,
        )?;
        validate_optional_bounded_text(
            "work-log model",
            append.attribution.model.as_deref(),
            MAX_WORK_LOG_ATTRIBUTION_BYTES,
        )?;
        validate_optional_bounded_text(
            "work-log reasoning",
            append.attribution.reasoning.as_deref(),
            MAX_WORK_LOG_ATTRIBUTION_BYTES,
        )?;
        validate_optional_bounded_text(
            "work-log harness",
            append.attribution.harness.as_deref(),
            MAX_WORK_LOG_ATTRIBUTION_BYTES,
        )?;
        validate_required_bounded_text("work-log body", &append.body, MAX_WORK_LOG_BODY_BYTES)?;

        let request_digest = self.work_log_request_digest(card_id, append)?;
        let value = self.request_typed_with_limit(
            "POST",
            &format!(
                "/api/v1/cards/{}/runs/{}/work-log",
                encode_path(card_id),
                encode_path(&append.expected_run_id)
            ),
            Some(json!({
                "operation_id": append.operation_id,
                "agent": append.attribution.agent,
                "model": append.attribution.model,
                "reasoning": append.attribution.reasoning,
                "harness": append.attribution.harness,
                "body": append.body,
            })),
            MAX_MUTATION_RESPONSE_BYTES,
        )?;
        parse_operation_outcome(
            value,
            "work_log_append",
            card_id,
            &append.expected_run_id,
            &append.operation_id,
            Some(&request_digest),
            false,
            |result| {
                parse_run_bound_work_log_entry(
                    result,
                    card_id,
                    &append.expected_run_id,
                    Some(&append.attribution),
                )
            },
        )
    }

    /// Atomically review one criterion on an exact current run.
    pub fn review_run_criterion(
        &self,
        card_id: &str,
        review: &RunBoundCriterionReview,
    ) -> Result<OperationOutcome<RunCriterionReview>, PowderError> {
        validate_id("card id", card_id)?;
        validate_id("expected run id", &review.expected_run_id)?;
        validate_operation_id(&review.operation_id)?;
        validate_id("criterion id", &review.criterion_id)?;
        validate_required_bounded_text(
            "criterion text",
            &review.criterion_text,
            MAX_EVIDENCE_TEXT_BYTES,
        )?;
        validate_required_bounded_text(
            "expected reviewer identity",
            &review.expected_reviewer_identity,
            MAX_CRITERION_REVIEWER_BYTES,
        )?;
        validate_optional_bounded_text(
            "criterion review proof",
            review.proof.as_deref(),
            MAX_COMPLETION_PROOF_BYTES,
        )?;
        let request_digest = self.criterion_review_request_digest(card_id, review)?;
        let value = self.request_typed_with_limit(
            "POST",
            &format!(
                "/api/v1/cards/{}/runs/{}/criteria/review",
                encode_path(card_id),
                encode_path(&review.expected_run_id)
            ),
            Some(json!({
                "operation_id": review.operation_id,
                "criterion": review.criterion_index,
                "criterion_id": review.criterion_id,
                "decision": review.decision,
                "proof": review.proof,
            })),
            MAX_MUTATION_RESPONSE_BYTES,
        )?;
        parse_operation_outcome(
            value,
            "criterion_review",
            card_id,
            &review.expected_run_id,
            &review.operation_id,
            Some(&request_digest),
            false,
            |result| parse_exact_run_criterion_review(result, card_id, review),
        )
    }

    /// Read only the concise, server-bounded evidence needed to supervise one card.
    pub fn card_evidence(&self, card_id: &str) -> Result<CardEvidence, PowderError> {
        validate_id("card id", card_id)?;
        let value = self.request_typed_with_limit(
            "GET",
            &format!("/api/v1/cards/{}", encode_path(card_id)),
            None,
            MAX_EVIDENCE_RESPONSE_BYTES,
        )?;
        parse_card_evidence(value, card_id)
    }

    /// Read only the concise, server-bounded evidence for one authoritative run.
    pub fn run_evidence(&self, run_id: &str) -> Result<RunEvidence, PowderError> {
        validate_id("run id", run_id)?;
        let value = self.request_typed_with_limit(
            "GET",
            &format!("/api/v1/runs/{}", encode_path(run_id)),
            None,
            MAX_EVIDENCE_RESPONSE_BYTES,
        )?;
        parse_run_evidence(value, run_id)
    }

    /// Atomically complete a card only through its exact current run.
    pub fn complete_run_with_proof(
        &self,
        card_id: &str,
        completion: &RunBoundCompletion,
    ) -> Result<OperationOutcome<CompletionReceipt>, PowderError> {
        validate_id("card id", card_id)?;
        validate_id("expected run id", &completion.expected_run_id)?;
        validate_operation_id(&completion.operation_id)?;
        validate_required_bounded_text(
            "completion proof",
            &completion.proof,
            MAX_COMPLETION_PROOF_BYTES,
        )?;
        if completion.criterion_proofs.len() > MAX_COMPLETION_CRITERION_PROOFS {
            return Err(invalid_request(format!(
                "completion criterion proof count exceeds {MAX_COMPLETION_CRITERION_PROOFS}"
            )));
        }
        for proof in &completion.criterion_proofs {
            validate_required_bounded_text("criterion proof URL", &proof.url, MAX_PROOF_URL_BYTES)?;
        }
        let request_digest = self.completion_request_digest(card_id, completion)?;
        let value = self.request_typed_with_limit(
            "POST",
            &format!(
                "/api/v1/cards/{}/runs/{}/complete",
                encode_path(card_id),
                encode_path(&completion.expected_run_id)
            ),
            Some(json!({
                "operation_id": completion.operation_id,
                "proof": completion.proof,
                "criterion_proofs": completion.criterion_proofs,
            })),
            MAX_MUTATION_RESPONSE_BYTES,
        )?;
        parse_operation_outcome(
            value,
            "completion",
            card_id,
            &completion.expected_run_id,
            &completion.operation_id,
            Some(&request_digest),
            false,
            |result| {
                parse_completion_receipt(
                    result,
                    card_id,
                    &completion.expected_run_id,
                    &completion.operation_id,
                )
            },
        )
    }

    /// Recover one bounded work-log operation after an ambiguous response.
    pub fn recover_work_log_operation(
        &self,
        card_id: &str,
        append: &RunBoundWorkLog,
    ) -> Result<OperationOutcome<WorkLogEntry>, PowderError> {
        let request_digest = self.work_log_request_digest(card_id, append)?;
        self.recover_operation(
            "work_log_append",
            card_id,
            &append.expected_run_id,
            &append.operation_id,
            &request_digest,
            |result| {
                parse_run_bound_work_log_entry(
                    result,
                    card_id,
                    &append.expected_run_id,
                    Some(&append.attribution),
                )
            },
        )
    }

    pub fn recover_criterion_review_operation(
        &self,
        card_id: &str,
        review: &RunBoundCriterionReview,
    ) -> Result<OperationOutcome<RunCriterionReview>, PowderError> {
        let request_digest = self.criterion_review_request_digest(card_id, review)?;
        self.recover_operation(
            "criterion_review",
            card_id,
            &review.expected_run_id,
            &review.operation_id,
            &request_digest,
            |result| parse_exact_run_criterion_review(result, card_id, review),
        )
    }

    /// Recover one bounded completion operation after an ambiguous response.
    pub fn recover_completion_operation(
        &self,
        card_id: &str,
        completion: &RunBoundCompletion,
    ) -> Result<OperationOutcome<CompletionReceipt>, PowderError> {
        let request_digest = self.completion_request_digest(card_id, completion)?;
        self.recover_operation(
            "completion",
            card_id,
            &completion.expected_run_id,
            &completion.operation_id,
            &request_digest,
            |result| {
                parse_completion_receipt(
                    result,
                    card_id,
                    &completion.expected_run_id,
                    &completion.operation_id,
                )
            },
        )
    }

    fn recover_operation<T: Serialize>(
        &self,
        kind: &str,
        card_id: &str,
        expected_run_id: &str,
        operation_id: &str,
        expected_request_digest: &str,
        parse_result: impl FnOnce(Value) -> Result<T, PowderError>,
    ) -> Result<OperationOutcome<T>, PowderError> {
        validate_id("card id", card_id)?;
        validate_id("expected run id", expected_run_id)?;
        validate_operation_id(operation_id)?;
        let value = self.request_typed_with_limit(
            "GET",
            &format!("/api/v1/operations/{}", encode_path(operation_id)),
            None,
            MAX_MUTATION_RESPONSE_BYTES,
        )?;
        parse_operation_outcome(
            value,
            kind,
            card_id,
            expected_run_id,
            operation_id,
            Some(expected_request_digest),
            true,
            parse_result,
        )
    }

    pub fn work_log_request_digest(
        &self,
        card_id: &str,
        append: &RunBoundWorkLog,
    ) -> Result<String, PowderError> {
        work_log_operation_request_digest(self.require_operation_identity()?, card_id, append)
    }

    pub fn criterion_review_request_digest(
        &self,
        card_id: &str,
        review: &RunBoundCriterionReview,
    ) -> Result<String, PowderError> {
        criterion_review_operation_request_digest(
            self.require_operation_identity()?,
            card_id,
            review,
        )
    }

    pub fn completion_request_digest(
        &self,
        card_id: &str,
        completion: &RunBoundCompletion,
    ) -> Result<String, PowderError> {
        completion_operation_request_digest(self.require_operation_identity()?, card_id, completion)
    }

    fn require_operation_identity(&self) -> Result<&str, PowderError> {
        self.operation_identity.as_deref().ok_or_else(|| {
            invalid_request(
                "Powder connection profile has no stable operationIdentity for run-bound mutations",
            )
        })
    }

    pub fn tail_events(&self, after: i64, limit: usize) -> Result<Vec<CardEvent>, String> {
        let limit = limit.clamp(1, 1000);
        let body = self.request_text(&format!(
            "/api/v1/events/tail?after={}&limit={limit}",
            after.max(0)
        ))?;
        parse_event_stream(&body, after)
    }

    pub fn event_head(&self) -> Result<i64, String> {
        let mut cursor = 0;
        for _ in 0..10_000 {
            let events = self.tail_events(cursor, 1000)?;
            let count = events.len();
            if let Some(last) = events.last() {
                cursor = last.sequence;
            }
            if count < 1000 {
                return Ok(cursor);
            }
        }
        Err("Powder event stream is too large to establish an initial cursor".into())
    }

    fn request_text(&self, path: &str) -> Result<String, String> {
        let url = format!("{}{path}", self.base_url);
        let mut request = self.agent.get(&url);
        if let Some(key) = self.api_key.as_deref() {
            request = request.set("Authorization", &format!("Bearer {key}"));
        }
        match request.call() {
            Ok(response) => response
                .into_string()
                .map_err(|error| format!("Powder returned unreadable text: {error}")),
            Err(error) => Err(response_error(error)),
        }
    }

    fn request(&self, method: &str, path: &str, body: Option<Value>) -> Result<Value, String> {
        self.request_typed(method, path, body)
            .map_err(|error| error.to_string())
    }

    fn request_typed(
        &self,
        method: &str,
        path: &str,
        body: Option<Value>,
    ) -> Result<Value, PowderError> {
        self.send_json_request(method, path, body)?
            .into_json()
            .map_err(|error| PowderError {
                kind: PowderErrorKind::InvalidResponse,
                message: format!("Powder returned invalid JSON: {error}"),
            })
    }

    fn request_typed_with_limit(
        &self,
        method: &str,
        path: &str,
        body: Option<Value>,
        response_limit: usize,
    ) -> Result<Value, PowderError> {
        parse_bounded_json_response(self.send_json_request(method, path, body)?, response_limit)
    }

    fn send_json_request(
        &self,
        method: &str,
        path: &str,
        body: Option<Value>,
    ) -> Result<ureq::Response, PowderError> {
        let url = format!("{}{path}", self.base_url);
        let mut request = match method {
            "GET" => self.agent.get(&url),
            "POST" => self.agent.post(&url),
            _ => {
                return Err(PowderError {
                    kind: PowderErrorKind::Upstream,
                    message: format!("unsupported Powder HTTP method '{method}'"),
                });
            }
        };
        if let Some(key) = self.api_key.as_deref() {
            request = request.set("Authorization", &format!("Bearer {key}"));
        }
        let response = match body {
            Some(body) => request.send_json(body),
            None => request.call(),
        };
        response.map_err(typed_response_error)
    }
}

pub(crate) fn work_log_operation_request_digest(
    authority: &str,
    card_id: &str,
    append: &RunBoundWorkLog,
) -> Result<String, PowderError> {
    canonical_operation_request_digest(
        authority,
        "work_log_append",
        card_id,
        &append.expected_run_id,
        &[
            ("agent", Some(append.attribution.agent.as_str())),
            ("model", append.attribution.model.as_deref()),
            ("reasoning", append.attribution.reasoning.as_deref()),
            ("harness", append.attribution.harness.as_deref()),
            ("body", Some(append.body.as_str())),
        ],
    )
}

pub(crate) fn criterion_review_operation_request_digest(
    authority: &str,
    card_id: &str,
    review: &RunBoundCriterionReview,
) -> Result<String, PowderError> {
    let criterion_index = review.criterion_index.to_string();
    let decision = match review.decision {
        CriterionReviewDecision::Approved => "approved",
        CriterionReviewDecision::Rejected => "rejected",
        CriterionReviewDecision::Cleared => "cleared",
    };
    canonical_operation_request_digest(
        authority,
        "criterion_review",
        card_id,
        &review.expected_run_id,
        &[
            ("criterion_index", Some(criterion_index.as_str())),
            ("criterion_id", Some(review.criterion_id.as_str())),
            ("decision", Some(decision)),
            ("proof", review.proof.as_deref()),
        ],
    )
}

pub(crate) fn completion_operation_request_digest(
    authority: &str,
    card_id: &str,
    completion: &RunBoundCompletion,
) -> Result<String, PowderError> {
    let criterion_proofs = serde_json::to_string(&completion.criterion_proofs)
        .map_err(|_| invalid_request("completion criterion proofs cannot be serialized"))?;
    canonical_operation_request_digest(
        authority,
        "completion",
        card_id,
        &completion.expected_run_id,
        &[
            ("proof", Some(completion.proof.as_str())),
            ("criterion_proofs", Some(criterion_proofs.as_str())),
        ],
    )
}

fn parse_bounded_json_response(
    response: ureq::Response,
    response_limit: usize,
) -> Result<Value, PowderError> {
    if response
        .header("Content-Length")
        .and_then(|length| length.parse::<usize>().ok())
        .is_some_and(|length| length > response_limit)
    {
        return Err(invalid_response(format!(
            "response exceeds the {response_limit}-byte limit"
        )));
    }
    let mut body = Vec::with_capacity(response_limit.min(64 * 1024));
    response
        .into_reader()
        .take((response_limit + 1) as u64)
        .read_to_end(&mut body)
        .map_err(|_| invalid_response("response body could not be read"))?;
    if body.len() > response_limit {
        return Err(invalid_response(format!(
            "response exceeds the {response_limit}-byte limit"
        )));
    }
    serde_json::from_slice(&body).map_err(|_| invalid_response("response body is not valid JSON"))
}

fn parse_work_log_entry(
    value: Value,
    expected_card_id: &str,
    expected_attribution: Option<&WorkLogAttribution>,
) -> Result<WorkLogEntry, PowderError> {
    let card_id = bounded_string(&value, "card_id", "work-log card id", MAX_ID_BYTES)?;
    if card_id != expected_card_id {
        return Err(invalid_response(
            "work-log entry returned a different card id",
        ));
    }
    let entry = WorkLogEntry {
        schema_version: optional_bounded_string(
            &value,
            "schema_version",
            "work-log schema version",
            MAX_SHORT_TEXT_BYTES,
        )?,
        entry_id: optional_bounded_string(&value, "id", "work-log entry id", MAX_ID_BYTES)?,
        card_id,
        actor: optional_bounded_string(
            &value,
            "actor",
            "work-log actor",
            MAX_WORK_LOG_ATTRIBUTION_BYTES,
        )?,
        agent: bounded_string(
            &value,
            "agent",
            "work-log agent",
            MAX_WORK_LOG_ATTRIBUTION_BYTES,
        )?,
        model: optional_bounded_string(
            &value,
            "model",
            "work-log model",
            MAX_WORK_LOG_ATTRIBUTION_BYTES,
        )?,
        reasoning: optional_bounded_string(
            &value,
            "reasoning",
            "work-log reasoning",
            MAX_WORK_LOG_ATTRIBUTION_BYTES,
        )?,
        harness: optional_bounded_string(
            &value,
            "harness",
            "work-log harness",
            MAX_WORK_LOG_ATTRIBUTION_BYTES,
        )?,
        run_id: optional_bounded_string(&value, "run_id", "work-log run id", MAX_ID_BYTES)?,
        body: bounded_string(&value, "body", "work-log body", MAX_WORK_LOG_BODY_BYTES)?,
        created_at: required_i64(&value, "created_at", "work-log created_at")?,
        updated_at: optional_i64(&value, "updated_at", "work-log updated_at")?,
    };
    if let Some(expected) = expected_attribution {
        if entry.agent != expected.agent
            || entry.model != expected.model
            || entry.reasoning != expected.reasoning
            || entry.harness != expected.harness
        {
            return Err(invalid_response(
                "work-log entry attribution does not match the request",
            ));
        }
    }
    Ok(entry)
}

fn parse_run_bound_work_log_entry(
    value: Value,
    expected_card_id: &str,
    expected_run_id: &str,
    expected_attribution: Option<&WorkLogAttribution>,
) -> Result<WorkLogEntry, PowderError> {
    let entry = parse_work_log_entry(value, expected_card_id, expected_attribution)?;
    if entry.schema_version.as_deref() != Some("powder.work_log_entry.v1") {
        return Err(invalid_response(
            "work-log operation returned an unsupported result schema",
        ));
    }
    if entry
        .entry_id
        .as_deref()
        .is_none_or(|entry_id| !entry_id.starts_with("work-log-"))
    {
        return Err(invalid_response(
            "work-log operation returned an invalid entry id",
        ));
    }
    if entry
        .actor
        .as_deref()
        .is_none_or(|actor| actor.trim().is_empty())
    {
        return Err(invalid_response(
            "work-log operation did not return authenticated actor attribution",
        ));
    }
    if entry.run_id.as_deref() != Some(expected_run_id) {
        return Err(invalid_response(
            "work-log operation returned a different run id",
        ));
    }
    if entry.agent.trim().is_empty() || entry.body.trim().is_empty() {
        return Err(invalid_response(
            "work-log operation returned empty required evidence",
        ));
    }
    if entry.updated_at != Some(entry.created_at) {
        return Err(invalid_response(
            "work-log operation returned inconsistent append timestamps",
        ));
    }
    Ok(entry)
}

fn parse_card_evidence(value: Value, expected_card_id: &str) -> Result<CardEvidence, PowderError> {
    let card = required_object(&value, "card", "card evidence envelope")?;
    let card_id = bounded_string(card, "id", "card id", MAX_ID_BYTES)?;
    if card_id != expected_card_id {
        return Err(invalid_response(
            "card evidence returned a different card id",
        ));
    }
    let runs = bounded_array(&value, "runs", MAX_EVIDENCE_ITEMS, "card runs")?
        .iter()
        .map(|run| parse_run_summary(run, None, Some(expected_card_id)))
        .collect::<Result<Vec<_>, _>>()?;
    let work_log = bounded_array(&value, "work_log", MAX_EVIDENCE_ITEMS, "card work log")?
        .iter()
        .cloned()
        .map(|entry| parse_work_log_entry(entry, expected_card_id, None))
        .collect::<Result<Vec<_>, _>>()?;
    let runs_total = bounded_total(&value, "runs_total", runs.len(), "card runs")?;
    let work_log_total = bounded_total(&value, "work_log_total", work_log.len(), "card work log")?;
    let criteria = parse_criteria(card)?;
    let claim = parse_evidence_claim(card.get("claim"))?;
    let current_run_criteria = match claim.as_ref() {
        Some(claim) => Some(parse_run_criteria_projection(
            &value,
            "current_run_criteria",
            &criteria,
            expected_card_id,
            &claim.run_id,
        )?),
        None => {
            if !bounded_array(
                &value,
                "current_run_criteria",
                MAX_CRITERIA,
                "current run criteria",
            )?
            .is_empty()
            {
                return Err(invalid_response(
                    "released card evidence retained current run criteria",
                ));
            }
            None
        }
    };
    Ok(CardEvidence {
        card_id,
        title: bounded_string(card, "title", "card title", MAX_EVIDENCE_TEXT_BYTES)?,
        status: card_status(card)?,
        repository: optional_bounded_string(card, "repo", "card repository", MAX_ID_BYTES)?,
        claim,
        criteria,
        current_run_criteria,
        runs,
        runs_total,
        work_log,
        work_log_total,
        truncated: runs_total > 0 && runs_total > value_array_len(&value, "runs")
            || work_log_total > 0 && work_log_total > value_array_len(&value, "work_log"),
    })
}

fn parse_run_evidence(value: Value, expected_run_id: &str) -> Result<RunEvidence, PowderError> {
    let run_value = required_object(&value, "run", "run evidence envelope")?;
    let run = parse_run_summary(run_value, Some(expected_run_id), None)?;
    let card = required_object(&value, "card", "run evidence card")?;
    let card_id = bounded_string(card, "id", "card id", MAX_ID_BYTES)?;
    if card_id != run.card_id {
        return Err(invalid_response(
            "run evidence card does not match the requested run",
        ));
    }
    let activities = bounded_array(&value, "activities", MAX_EVIDENCE_ITEMS, "run activities")?
        .iter()
        .map(|activity| parse_activity(activity, expected_run_id))
        .collect::<Result<Vec<_>, _>>()?;
    let links = bounded_array(&value, "links", MAX_EVIDENCE_ITEMS, "run links")?
        .iter()
        .map(|link| parse_evidence_link(link, &card_id))
        .collect::<Result<Vec<_>, _>>()?;
    let activities_total = bounded_total(
        &value,
        "activities_total",
        activities.len(),
        "run activities",
    )?;
    let links_total = bounded_total(&value, "links_total", links.len(), "run links")?;
    let card_criteria = parse_criteria(card)?;
    let criteria = parse_run_criteria_projection(
        &value,
        "criteria",
        &card_criteria,
        &card_id,
        expected_run_id,
    )?;
    Ok(RunEvidence {
        run,
        card_title: bounded_string(card, "title", "card title", MAX_EVIDENCE_TEXT_BYTES)?,
        card_status: card_status(card)?,
        repository: optional_bounded_string(card, "repo", "card repository", MAX_ID_BYTES)?,
        card_criteria,
        criteria,
        activities,
        activities_total,
        links,
        links_total,
        truncated: activities_total > value_array_len(&value, "activities")
            || links_total > value_array_len(&value, "links"),
    })
}

fn parse_completion_receipt(
    value: Value,
    expected_card_id: &str,
    expected_run_id: &str,
    expected_operation_id: &str,
) -> Result<CompletionReceipt, PowderError> {
    let schema_version = bounded_string(
        &value,
        "schema_version",
        "completion result schema version",
        MAX_SHORT_TEXT_BYTES,
    )?;
    if schema_version != "powder.run_bound_completion.v1" {
        return Err(invalid_response(
            "completion operation returned an unsupported result schema",
        ));
    }
    let card_id = bounded_string(&value, "card_id", "completed card id", MAX_ID_BYTES)?;
    if card_id != expected_card_id {
        return Err(invalid_response(
            "completion response returned a different card id",
        ));
    }
    let run_id = bounded_string(&value, "run_id", "completed run id", MAX_ID_BYTES)?;
    if run_id != expected_run_id {
        return Err(invalid_response(
            "completion response returned a different run id",
        ));
    }
    let operation_id = bounded_string(
        &value,
        "operation_id",
        "completion operation id",
        MAX_OPERATION_ID_BYTES,
    )?;
    if operation_id != expected_operation_id {
        return Err(invalid_response(
            "completion response returned a different operation id",
        ));
    }
    let status = card_status(&value)?;
    if status != "done" {
        return Err(invalid_response(
            "completion response did not confirm done status",
        ));
    }
    let criterion_proofs = bounded_array(
        &value,
        "criterion_proofs",
        MAX_COMPLETION_CRITERION_PROOFS,
        "completion criterion proofs",
    )?
    .iter()
    .map(|proof| {
        let criterion = proof
            .get("criterion")
            .and_then(Value::as_u64)
            .and_then(|criterion| usize::try_from(criterion).ok())
            .ok_or_else(|| invalid_response("completion criterion index is invalid"))?;
        Ok(CriterionProof {
            criterion,
            url: bounded_string(proof, "url", "criterion proof URL", MAX_PROOF_URL_BYTES)?,
        })
    })
    .collect::<Result<Vec<_>, PowderError>>()?;
    let proof = optional_bounded_string(
        &value,
        "proof",
        "completion proof",
        MAX_COMPLETION_PROOF_BYTES,
    )?;
    if proof.as_deref().is_none_or(|proof| proof.trim().is_empty()) {
        return Err(invalid_response(
            "completion operation did not return required proof",
        ));
    }
    Ok(CompletionReceipt {
        schema_version,
        card_id,
        run_id,
        operation_id,
        status,
        proof,
        criterion_proofs,
        updated_at: required_i64(&value, "updated_at", "completed card updated_at")?,
        audit_event_id: bounded_string(
            &value,
            "audit_event_id",
            "completion audit event id",
            MAX_ID_BYTES,
        )?,
    })
}

fn parse_operation_outcome<T: Serialize>(
    value: Value,
    expected_kind: &str,
    expected_card_id: &str,
    expected_run_id: &str,
    expected_operation_id: &str,
    expected_request_digest: Option<&str>,
    allow_unknown: bool,
    parse_result: impl FnOnce(Value) -> Result<T, PowderError>,
) -> Result<OperationOutcome<T>, PowderError> {
    let schema_version = bounded_string(
        &value,
        "schema_version",
        "operation status schema version",
        MAX_SHORT_TEXT_BYTES,
    )?;
    if schema_version != "powder.operation_status.v1" {
        return Err(invalid_response(
            "operation response returned an unsupported status schema",
        ));
    }
    let operation_id = bounded_string(
        &value,
        "operation_id",
        "operation id",
        MAX_OPERATION_ID_BYTES,
    )?;
    if operation_id != expected_operation_id {
        return Err(invalid_response(
            "operation response returned a different operation id",
        ));
    }
    let state =
        match bounded_string(&value, "state", "operation state", MAX_SHORT_TEXT_BYTES)?.as_str() {
            "unknown" => OperationState::Unknown,
            "pending" => OperationState::Pending,
            "succeeded" => OperationState::Succeeded,
            "rejected" => OperationState::Rejected,
            "failed" => OperationState::Failed,
            _ => return Err(invalid_response("operation response has an unknown state")),
        };
    if state == OperationState::Unknown {
        if !allow_unknown {
            return Err(invalid_response(
                "mutation response cannot have unknown operation state",
            ));
        }
        for field in [
            "request_digest",
            "kind",
            "target_card_id",
            "expected_run_id",
            "result",
            "failure",
            "audit_event_id",
            "created_at",
            "updated_at",
            "expires_at",
        ] {
            if value.get(field).is_some_and(|value| !value.is_null()) {
                return Err(invalid_response(
                    "unknown operation response contains authoritative outcome fields",
                ));
            }
        }
        return Ok(OperationOutcome {
            operation_id,
            state,
            request_digest: None,
            kind: None,
            target_card_id: None,
            expected_run_id: None,
            result: None,
            failure: None,
            audit_event_id: None,
            created_at: None,
            updated_at: None,
            expires_at: None,
        });
    }

    let request_digest = operation_request_digest(&value)?;
    if expected_request_digest.is_some_and(|expected| request_digest != expected) {
        return Err(invalid_response(
            "operation response request digest does not match the submitted mutation",
        ));
    }
    let kind = bounded_string(&value, "kind", "operation kind", MAX_SHORT_TEXT_BYTES)?;
    if kind != expected_kind {
        return Err(invalid_response(
            "operation response returned a different mutation kind",
        ));
    }
    let target_card_id = bounded_string(
        &value,
        "target_card_id",
        "operation target card id",
        MAX_ID_BYTES,
    )?;
    if target_card_id != expected_card_id {
        return Err(invalid_response(
            "operation response returned a different target card id",
        ));
    }
    let returned_run_id = bounded_string(
        &value,
        "expected_run_id",
        "operation expected run id",
        MAX_ID_BYTES,
    )?;
    if returned_run_id != expected_run_id {
        return Err(invalid_response(
            "operation response returned a different expected run id",
        ));
    }
    let failure = match value.get("failure") {
        None | Some(Value::Null) => None,
        Some(failure) if failure.is_object() => Some(OperationFailure {
            code: bounded_string(
                failure,
                "code",
                "operation failure code",
                MAX_SHORT_TEXT_BYTES,
            )?,
            message: bounded_string(
                failure,
                "message",
                "operation failure message",
                MAX_OPERATION_FAILURE_BYTES,
            )?,
        }),
        Some(_) => return Err(invalid_response("operation failure is not an object")),
    };
    let result_value = value
        .get("result")
        .filter(|value| !value.is_null())
        .cloned();
    let audit_event_id = optional_bounded_string(
        &value,
        "audit_event_id",
        "operation audit event id",
        MAX_ID_BYTES,
    )?;
    match state {
        OperationState::Succeeded => {
            if result_value.is_none() || failure.is_some() || audit_event_id.is_none() {
                return Err(invalid_response(
                    "succeeded operation response has inconsistent outcome fields",
                ));
            }
        }
        OperationState::Rejected | OperationState::Failed => {
            if result_value.is_some() || failure.is_none() || audit_event_id.is_some() {
                return Err(invalid_response(
                    "unsuccessful operation response has inconsistent outcome fields",
                ));
            }
        }
        OperationState::Pending => {
            if result_value.is_some() || failure.is_some() || audit_event_id.is_some() {
                return Err(invalid_response(
                    "pending operation response contains terminal outcome fields",
                ));
            }
        }
        OperationState::Unknown => unreachable!(),
    }
    let result = result_value.map(parse_result).transpose()?;
    if let (Some(envelope_audit), Some(result)) = (audit_event_id.as_deref(), result.as_ref()) {
        let result_value = serde_json::to_value(result)
            .map_err(|_| invalid_response("operation result could not be validated"))?;
        if expected_kind == "completion"
            && result_value.get("auditEventId").and_then(Value::as_str) != Some(envelope_audit)
        {
            return Err(invalid_response(
                "completion result audit event does not match operation status",
            ));
        }
    }
    Ok(OperationOutcome {
        operation_id,
        state,
        request_digest: Some(request_digest),
        kind: Some(kind),
        target_card_id: Some(target_card_id),
        expected_run_id: Some(returned_run_id),
        result,
        failure,
        audit_event_id,
        created_at: Some(required_i64(&value, "created_at", "operation created_at")?),
        updated_at: Some(required_i64(&value, "updated_at", "operation updated_at")?),
        expires_at: Some(required_i64(&value, "expires_at", "operation expires_at")?),
    })
}

fn parse_run_summary(
    value: &Value,
    expected_run_id: Option<&str>,
    expected_card_id: Option<&str>,
) -> Result<RunSummary, PowderError> {
    let run_id = bounded_string(value, "id", "run id", MAX_ID_BYTES)?;
    if expected_run_id.is_some_and(|expected| expected != run_id) {
        return Err(invalid_response("run evidence returned a different run id"));
    }
    let card_id = bounded_string(value, "card_id", "run card id", MAX_ID_BYTES)?;
    if expected_card_id.is_some_and(|expected| expected != card_id) {
        return Err(invalid_response(
            "card evidence contains a run for another card",
        ));
    }
    let state = bounded_string(value, "state", "run state", MAX_SHORT_TEXT_BYTES)?;
    if !matches!(
        state.as_str(),
        "active" | "awaiting_input" | "released" | "error" | "complete" | "stale"
    ) {
        return Err(invalid_response("run evidence contains an unknown state"));
    }
    required_i64(value, "claim_expires_at", "run claim_expires_at")?;
    Ok(RunSummary {
        run_id,
        card_id,
        state,
        agent: bounded_string(value, "agent", "run agent", MAX_SHORT_TEXT_BYTES)?,
        proof: optional_bounded_string(value, "proof", "run proof", MAX_COMPLETION_PROOF_BYTES)?,
        created_at: required_i64(value, "created_at", "run created_at")?,
        updated_at: required_i64(value, "updated_at", "run updated_at")?,
    })
}

fn parse_criteria(card: &Value) -> Result<Vec<CriterionEvidence>, PowderError> {
    bounded_array(card, "criteria", MAX_CRITERIA, "card criteria")?
        .iter()
        .enumerate()
        .map(|(criterion, value)| {
            let proof_links = bounded_array(
                value,
                "proof_links",
                MAX_CRITERION_PROOFS,
                "criterion proof links",
            )?
            .iter()
            .map(|proof| {
                Ok(EvidenceProofLink {
                    url: bounded_string(proof, "url", "criterion proof URL", MAX_PROOF_URL_BYTES)?,
                    actor: bounded_string(
                        proof,
                        "actor",
                        "criterion proof actor",
                        MAX_SHORT_TEXT_BYTES,
                    )?,
                    created_at: required_i64(proof, "created_at", "criterion proof created_at")?,
                })
            })
            .collect::<Result<Vec<_>, PowderError>>()?;
            Ok(CriterionEvidence {
                criterion,
                text: bounded_string(value, "text", "criterion text", MAX_EVIDENCE_TEXT_BYTES)?,
                checked_by: optional_bounded_string(
                    value,
                    "checked_by",
                    "criterion checked_by",
                    MAX_SHORT_TEXT_BYTES,
                )?,
                checked_at: optional_i64(value, "checked_at", "criterion checked_at")?,
                proof_links,
            })
        })
        .collect()
}

fn parse_run_criteria_projection(
    envelope: &Value,
    key: &str,
    card_criteria: &[CriterionEvidence],
    expected_card_id: &str,
    expected_run_id: &str,
) -> Result<Vec<RunCriterionEvidence>, PowderError> {
    if envelope.get(key).is_none() && !card_criteria.is_empty() {
        return Err(invalid_response(format!(
            "run-scoped criterion projection is missing {key}"
        )));
    }
    let values = bounded_array(envelope, key, MAX_CRITERIA, "run-scoped criteria")?;
    if values.len() != card_criteria.len() {
        return Err(invalid_response(
            "run-scoped criterion projection does not match current card criteria",
        ));
    }
    values
        .iter()
        .enumerate()
        .map(|(expected_index, value)| {
            let criterion_index = value
                .get("criterion_index")
                .and_then(Value::as_u64)
                .and_then(|index| usize::try_from(index).ok())
                .ok_or_else(|| invalid_response("run criterion index is invalid"))?;
            if criterion_index != expected_index {
                return Err(invalid_response(
                    "run criterion index does not match its current position",
                ));
            }
            let criterion_text = bounded_string(
                value,
                "criterion_text",
                "run criterion text",
                MAX_EVIDENCE_TEXT_BYTES,
            )?;
            if criterion_text != card_criteria[expected_index].text {
                return Err(invalid_response(
                    "run criterion text does not match the current card criterion",
                ));
            }
            let criterion_id =
                bounded_string(value, "criterion_id", "run criterion id", MAX_ID_BYTES)?;
            let expected_criterion_id = criterion_identity(card_criteria, expected_index);
            if criterion_id != expected_criterion_id {
                return Err(invalid_response(
                    "run criterion identity does not match its exact text and occurrence",
                ));
            }
            let review = match value.get("review") {
                None | Some(Value::Null) => None,
                Some(review) if review.is_object() => Some(parse_run_criterion_review(
                    review,
                    expected_card_id,
                    expected_run_id,
                    criterion_index,
                    &criterion_id,
                    &criterion_text,
                )?),
                Some(_) => return Err(invalid_response("run criterion review is not an object")),
            };
            Ok(RunCriterionEvidence {
                criterion_index,
                criterion_id,
                criterion_text,
                review,
            })
        })
        .collect()
}

fn parse_run_criterion_review(
    value: &Value,
    expected_card_id: &str,
    expected_run_id: &str,
    expected_criterion_index: usize,
    expected_criterion_id: &str,
    expected_criterion_text: &str,
) -> Result<RunCriterionReview, PowderError> {
    let review_id = bounded_string(value, "id", "criterion review id", MAX_ID_BYTES)?;
    if !review_id.starts_with("review-") {
        return Err(invalid_response("criterion review id is invalid"));
    }
    let operation_id = bounded_string(
        value,
        "operation_id",
        "criterion review operation id",
        MAX_OPERATION_ID_BYTES,
    )?;
    if !valid_operation_id(&operation_id) {
        return Err(invalid_response("criterion review operation id is invalid"));
    }
    let card_id = bounded_string(value, "card_id", "criterion review card id", MAX_ID_BYTES)?;
    if card_id != expected_card_id {
        return Err(invalid_response(
            "criterion review returned a different card id",
        ));
    }
    let run_id = bounded_string(value, "run_id", "criterion review run id", MAX_ID_BYTES)?;
    if run_id != expected_run_id {
        return Err(invalid_response(
            "criterion review returned a different run id",
        ));
    }
    let criterion_index = value
        .get("criterion_index")
        .and_then(Value::as_u64)
        .and_then(|index| usize::try_from(index).ok())
        .ok_or_else(|| invalid_response("criterion review index is invalid"))?;
    if criterion_index != expected_criterion_index {
        return Err(invalid_response(
            "criterion review returned a different criterion index",
        ));
    }
    let criterion_id = bounded_string(
        value,
        "criterion_id",
        "criterion review criterion id",
        MAX_ID_BYTES,
    )?;
    if criterion_id != expected_criterion_id {
        return Err(invalid_response(
            "criterion review returned a different criterion identity",
        ));
    }
    let criterion_text = bounded_string(
        value,
        "criterion_text",
        "criterion review criterion text",
        MAX_EVIDENCE_TEXT_BYTES,
    )?;
    if criterion_text != expected_criterion_text {
        return Err(invalid_response(
            "criterion review returned different criterion text",
        ));
    }
    let decision = match bounded_string(
        value,
        "decision",
        "criterion review decision",
        MAX_SHORT_TEXT_BYTES,
    )?
    .as_str()
    {
        "approved" => CriterionReviewDecision::Approved,
        "rejected" => CriterionReviewDecision::Rejected,
        "cleared" => CriterionReviewDecision::Cleared,
        _ => return Err(invalid_response("criterion review decision is invalid")),
    };
    let reviewer = bounded_string(
        value,
        "reviewer",
        "criterion reviewer",
        MAX_CRITERION_REVIEWER_BYTES,
    )?;
    let reviewer_identity = bounded_string(
        value,
        "reviewer_identity",
        "criterion reviewer identity",
        MAX_CRITERION_REVIEWER_BYTES,
    )?;
    if reviewer.trim().is_empty()
        || reviewer_identity.trim().is_empty()
        || reviewer_identity == "legacy:unverified"
    {
        return Err(invalid_response(
            "criterion review has no authoritative reviewer identity",
        ));
    }
    let proof = optional_bounded_string(
        value,
        "proof",
        "criterion review proof",
        MAX_COMPLETION_PROOF_BYTES,
    )?;
    if proof
        .as_deref()
        .is_some_and(|proof| proof.trim().is_empty())
    {
        return Err(invalid_response("criterion review proof is empty"));
    }
    let supersedes_review_id = optional_bounded_string(
        value,
        "supersedes_review_id",
        "superseded criterion review id",
        MAX_ID_BYTES,
    )?;
    if supersedes_review_id
        .as_deref()
        .is_some_and(|review_id| !review_id.starts_with("review-"))
    {
        return Err(invalid_response(
            "superseded criterion review id is invalid",
        ));
    }
    Ok(RunCriterionReview {
        review_id,
        operation_id,
        card_id,
        run_id,
        criterion_index,
        criterion_id,
        criterion_text,
        decision,
        reviewer,
        reviewer_identity,
        proof,
        supersedes_review_id,
        created_at: required_i64(value, "created_at", "criterion review created_at")?,
    })
}

fn parse_exact_run_criterion_review(
    value: Value,
    expected_card_id: &str,
    expected: &RunBoundCriterionReview,
) -> Result<RunCriterionReview, PowderError> {
    let review = parse_run_criterion_review(
        &value,
        expected_card_id,
        &expected.expected_run_id,
        expected.criterion_index,
        &expected.criterion_id,
        &expected.criterion_text,
    )?;
    if review.operation_id != expected.operation_id {
        return Err(invalid_response(
            "criterion review returned a different operation id",
        ));
    }
    if review.decision != expected.decision {
        return Err(invalid_response(
            "criterion review returned a different decision",
        ));
    }
    if review.reviewer_identity != expected.expected_reviewer_identity {
        return Err(invalid_response(
            "criterion review returned a different reviewer identity",
        ));
    }
    if expected.proof.is_some() && review.proof.is_none() {
        return Err(invalid_response(
            "criterion review omitted the authoritative proof record",
        ));
    }
    if expected.proof.is_none() && review.proof.is_some() {
        return Err(invalid_response(
            "criterion review returned an unexpected proof record",
        ));
    }
    Ok(review)
}

fn criterion_identity(criteria: &[CriterionEvidence], index: usize) -> String {
    let criterion = &criteria[index];
    let occurrence = criteria[..index]
        .iter()
        .filter(|candidate| candidate.text == criterion.text)
        .count();
    let digest = Sha256::digest(criterion.text.as_bytes());
    format!("powder.criterion.v1:sha256:{digest:x}:{occurrence}")
}

fn parse_evidence_claim(value: Option<&Value>) -> Result<Option<EvidenceClaim>, PowderError> {
    let Some(value) = value.filter(|value| !value.is_null()) else {
        return Ok(None);
    };
    if !value.is_object() {
        return Err(invalid_response("card claim is not an object"));
    }
    Ok(Some(EvidenceClaim {
        run_id: bounded_string(value, "run_id", "claim run id", MAX_ID_BYTES)?,
        agent: bounded_string(value, "agent", "claim agent", MAX_SHORT_TEXT_BYTES)?,
        expires_at: required_i64(value, "expires_at", "claim expires_at")?,
    }))
}

fn parse_activity(value: &Value, expected_run_id: &str) -> Result<EvidenceActivity, PowderError> {
    let run_id = bounded_string(value, "run_id", "activity run id", MAX_ID_BYTES)?;
    if run_id != expected_run_id {
        return Err(invalid_response(
            "run evidence contains an activity for another run",
        ));
    }
    let activity_type = bounded_string(
        value,
        "activity_type",
        "activity type",
        MAX_SHORT_TEXT_BYTES,
    )?;
    if !matches!(
        activity_type.as_str(),
        "thought" | "action" | "response" | "elicitation" | "error" | "prompt"
    ) {
        return Err(invalid_response(
            "run evidence contains an unknown activity type",
        ));
    }
    Ok(EvidenceActivity {
        activity_type,
        payload: bounded_string(
            value,
            "payload",
            "activity payload",
            MAX_EVIDENCE_TEXT_BYTES,
        )?,
        created_at: required_i64(value, "created_at", "activity created_at")?,
    })
}

fn parse_evidence_link(value: &Value, expected_card_id: &str) -> Result<EvidenceLink, PowderError> {
    let card_id = bounded_string(value, "card_id", "link card id", MAX_ID_BYTES)?;
    if card_id != expected_card_id {
        return Err(invalid_response(
            "run evidence contains a link for another card",
        ));
    }
    Ok(EvidenceLink {
        label: bounded_string(value, "label", "link label", MAX_SHORT_TEXT_BYTES)?,
        url: bounded_string(value, "url", "link URL", MAX_PROOF_URL_BYTES)?,
        created_at: required_i64(value, "created_at", "link created_at")?,
    })
}

fn card_status(value: &Value) -> Result<String, PowderError> {
    let status = bounded_string(value, "status", "card status", MAX_SHORT_TEXT_BYTES)?;
    if !matches!(
        status.as_str(),
        "backlog"
            | "ready"
            | "claimed"
            | "running"
            | "awaiting_input"
            | "blocked"
            | "done"
            | "shipped"
            | "abandoned"
    ) {
        return Err(invalid_response("card evidence contains an unknown status"));
    }
    Ok(status)
}

fn required_object<'a>(value: &'a Value, key: &str, label: &str) -> Result<&'a Value, PowderError> {
    value
        .get(key)
        .filter(|field| field.is_object())
        .ok_or_else(|| invalid_response(format!("{label} is missing {key}")))
}

fn bounded_array<'a>(
    value: &'a Value,
    key: &str,
    limit: usize,
    label: &str,
) -> Result<&'a [Value], PowderError> {
    let Some(field) = value.get(key) else {
        return Ok(&[]);
    };
    let array = field
        .as_array()
        .ok_or_else(|| invalid_response(format!("{label} is not an array")))?;
    if array.len() > limit {
        return Err(invalid_response(format!(
            "{label} exceeds the {limit}-item limit"
        )));
    }
    Ok(array)
}

fn value_array_len(value: &Value, key: &str) -> usize {
    value.get(key).and_then(Value::as_array).map_or(0, Vec::len)
}

fn bounded_total(
    value: &Value,
    key: &str,
    displayed: usize,
    label: &str,
) -> Result<usize, PowderError> {
    let total = match value.get(key) {
        None => displayed,
        Some(total) => total
            .as_u64()
            .and_then(|total| usize::try_from(total).ok())
            .ok_or_else(|| invalid_response(format!("{label} total is invalid")))?,
    };
    if total < displayed || total > MAX_EVIDENCE_TOTAL {
        return Err(invalid_response(format!("{label} total is out of bounds")));
    }
    Ok(total)
}

fn bounded_string(
    value: &Value,
    key: &str,
    label: &str,
    limit: usize,
) -> Result<String, PowderError> {
    let text = value
        .get(key)
        .and_then(Value::as_str)
        .ok_or_else(|| invalid_response(format!("{label} is missing")))?;
    validate_response_text(label, text, limit)?;
    Ok(text.to_string())
}

fn optional_bounded_string(
    value: &Value,
    key: &str,
    label: &str,
    limit: usize,
) -> Result<Option<String>, PowderError> {
    match value.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(text)) => {
            validate_response_text(label, text, limit)?;
            Ok(Some(text.clone()))
        }
        Some(_) => Err(invalid_response(format!("{label} is not a string"))),
    }
}

fn required_i64(value: &Value, key: &str, label: &str) -> Result<i64, PowderError> {
    value
        .get(key)
        .and_then(Value::as_i64)
        .ok_or_else(|| invalid_response(format!("{label} is missing")))
}

fn optional_i64(value: &Value, key: &str, label: &str) -> Result<Option<i64>, PowderError> {
    match value.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(value) => value
            .as_i64()
            .map(Some)
            .ok_or_else(|| invalid_response(format!("{label} is not an integer"))),
    }
}

fn validate_id(label: &str, value: &str) -> Result<(), PowderError> {
    validate_required_bounded_text(label, value, MAX_ID_BYTES)
}

pub fn validate_operation_id(value: &str) -> Result<(), PowderError> {
    validate_required_bounded_text("operation id", value, MAX_OPERATION_ID_BYTES)?;
    if !valid_operation_id(value) {
        return Err(invalid_request(
            "operation id must use only ASCII letters, digits, '-', '_', '.', or ':'",
        ));
    }
    Ok(())
}

fn valid_operation_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= MAX_OPERATION_ID_BYTES
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b':'))
}

fn operation_request_digest(value: &Value) -> Result<String, PowderError> {
    const PREFIX: &str = "sha256:";
    const HEX_BYTES: usize = 64;
    let digest = bounded_string(
        value,
        "request_digest",
        "operation request digest",
        PREFIX.len() + HEX_BYTES,
    )?;
    let Some(hex) = digest.strip_prefix(PREFIX) else {
        return Err(invalid_response(
            "operation request digest is not canonical sha256",
        ));
    };
    if hex.len() != HEX_BYTES
        || !hex
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
    {
        return Err(invalid_response(
            "operation request digest is not canonical sha256",
        ));
    }
    Ok(digest)
}

fn canonical_operation_request_digest(
    authority: &str,
    kind: &str,
    card_id: &str,
    expected_run_id: &str,
    payload: &[(&str, Option<&str>)],
) -> Result<String, PowderError> {
    validate_required_bounded_text(
        "operation authority",
        authority,
        MAX_OPERATION_AUTHORITY_BYTES,
    )?;
    validate_id("card id", card_id)?;
    validate_id("expected run id", expected_run_id)?;
    let mut canonical_bytes = 0usize;
    let mut hasher = Sha256::new();
    for (name, value) in [
        ("schema", Some("powder.operation_request.v1")),
        ("kind", Some(kind)),
        ("target_type", Some("card")),
        ("target", Some(card_id)),
        ("authority", Some(authority)),
        ("expected_run", Some(expected_run_id)),
    ]
    .into_iter()
    .chain(payload.iter().copied())
    {
        hash_operation_component(&mut hasher, &mut canonical_bytes, name, value)?;
    }
    Ok(format!("sha256:{:x}", hasher.finalize()))
}

fn hash_operation_component(
    hasher: &mut Sha256,
    canonical_bytes: &mut usize,
    name: &str,
    value: Option<&str>,
) -> Result<(), PowderError> {
    let value_len = value.map_or(0, str::len);
    let added = 8usize
        .checked_add(name.len())
        .and_then(|count| count.checked_add(value_len))
        .ok_or_else(|| invalid_request("Powder operation request size overflow"))?;
    *canonical_bytes = canonical_bytes
        .checked_add(added)
        .ok_or_else(|| invalid_request("Powder operation request size overflow"))?;
    if *canonical_bytes > MAX_OPERATION_REQUEST_BYTES {
        return Err(invalid_request(format!(
            "Powder operation request exceeds the {MAX_OPERATION_REQUEST_BYTES}-byte canonical limit"
        )));
    }
    let name_len = u32::try_from(name.len())
        .map_err(|_| invalid_request("Powder operation component name is too large"))?;
    hasher.update(name_len.to_be_bytes());
    hasher.update(name.as_bytes());
    match value {
        Some(value) => {
            let value_len = u32::try_from(value.len())
                .map_err(|_| invalid_request("Powder operation component value is too large"))?;
            hasher.update(value_len.to_be_bytes());
            hasher.update(value.as_bytes());
        }
        None => hasher.update(u32::MAX.to_be_bytes()),
    }
    Ok(())
}

fn validate_optional_bounded_text(
    label: &str,
    value: Option<&str>,
    limit: usize,
) -> Result<(), PowderError> {
    value.map_or(Ok(()), |value| {
        validate_required_bounded_text(label, value, limit)
    })
}

fn validate_required_bounded_text(
    label: &str,
    value: &str,
    limit: usize,
) -> Result<(), PowderError> {
    if value.trim().is_empty() {
        return Err(invalid_request(format!("{label} must not be empty")));
    }
    if value.len() > limit {
        return Err(invalid_request(format!(
            "{label} exceeds the {limit}-byte limit"
        )));
    }
    Ok(())
}

fn validate_response_text(label: &str, value: &str, limit: usize) -> Result<(), PowderError> {
    if value.trim().is_empty() {
        return Err(invalid_response(format!("{label} is empty")));
    }
    if value.len() > limit {
        return Err(invalid_response(format!(
            "{label} exceeds the {limit}-byte limit"
        )));
    }
    Ok(())
}

fn invalid_request(message: impl Into<String>) -> PowderError {
    PowderError {
        kind: PowderErrorKind::InvalidResponse,
        message: format!("Powder request is invalid: {}", message.into()),
    }
}

fn invalid_response(message: impl Into<String>) -> PowderError {
    PowderError {
        kind: PowderErrorKind::InvalidResponse,
        message: format!("Powder evidence response is invalid: {}", message.into()),
    }
}

fn parse_board_repository(value: Value) -> Result<PowderBoard, PowderError> {
    let name = value["name"]
        .as_str()
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .ok_or_else(|| invalid_board_response("repository is missing name"))?;
    let tier = value["tier"]
        .as_str()
        .map(str::trim)
        .filter(|tier| !tier.is_empty())
        .ok_or_else(|| invalid_board_response("repository is missing tier"))?;
    let aliases = value["aliases"]
        .as_array()
        .map(|aliases| {
            aliases
                .iter()
                .filter_map(Value::as_str)
                .map(str::trim)
                .filter(|alias| !alias.is_empty())
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default();
    let card_count = value["card_count"]
        .as_u64()
        .and_then(|count| usize::try_from(count).ok())
        .ok_or_else(|| invalid_board_response("repository is missing card_count"))?;
    Ok(PowderBoard {
        name: name.to_string(),
        aliases,
        tier: tier.to_string(),
        card_count,
    })
}

fn parse_board_page(value: Value) -> Result<PowderBoardPage, PowderError> {
    let cards = value["cards"]
        .as_array()
        .ok_or_else(|| invalid_board_response("card page is missing cards"))?
        .iter()
        .map(parse_board_card)
        .collect::<Result<Vec<_>, _>>()?;
    let total_count = value["total_count"]
        .as_u64()
        .and_then(|count| usize::try_from(count).ok())
        .ok_or_else(|| invalid_board_response("card page is missing total_count"))?;
    let has_more = value["has_more"]
        .as_bool()
        .ok_or_else(|| invalid_board_response("card page is missing has_more"))?;
    Ok(PowderBoardPage {
        cards,
        total_count,
        has_more,
    })
}

fn parse_board_card(value: &Value) -> Result<PowderBoardCard, PowderError> {
    let string = |key: &str| {
        value[key]
            .as_str()
            .map(str::trim)
            .filter(|text| !text.is_empty())
            .map(str::to_string)
            .ok_or_else(|| invalid_board_response(&format!("card is missing {key}")))
    };
    let labels = value["labels"]
        .as_array()
        .map(|labels| {
            labels
                .iter()
                .filter_map(Value::as_str)
                .map(str::trim)
                .filter(|label| !label.is_empty())
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default();
    let claim = value.get("claim").and_then(|claim| {
        let agent = claim["agent"].as_str()?.trim();
        let expires_at = claim["expires_at"].as_i64()?;
        (!agent.is_empty()).then(|| PowderBoardClaim {
            agent: agent.to_string(),
            expires_at,
        })
    });
    Ok(PowderBoardCard {
        id: string("id")?,
        title: string("title")?,
        status: string("status")?,
        priority: string("priority")?,
        estimate: value["estimate"].as_str().map(str::to_string),
        labels,
        claim,
        updated_at: value["updated_at"]
            .as_i64()
            .ok_or_else(|| invalid_board_response("card is missing updated_at"))?,
    })
}

fn invalid_board_response(message: &str) -> PowderError {
    PowderError {
        kind: PowderErrorKind::InvalidResponse,
        message: format!("Powder board response is invalid: {message}"),
    }
}

fn parse_board_list(value: Value) -> Result<Vec<PowderBoard>, String> {
    let payload: PowderRepositoryList = serde_json::from_value(value)
        .map_err(|error| format!("Powder repository list response is invalid: {error}"))?;
    let mut boards = payload
        .repositories
        .into_iter()
        .map(|repository| {
            let name = repository.name.trim().to_string();
            if name.is_empty() {
                return Err("Powder repository list contains an empty name".to_string());
            }
            let tier = repository.tier.trim().to_string();
            if tier.is_empty() {
                return Err(format!("Powder repository '{name}' has an empty tier"));
            }
            Ok(PowderBoard {
                name,
                aliases: repository
                    .aliases
                    .into_iter()
                    .map(|alias| alias.trim().to_string())
                    .filter(|alias| !alias.is_empty())
                    .collect(),
                tier,
                card_count: repository.card_count,
            })
        })
        .collect::<Result<Vec<_>, _>>()?;
    boards.sort_by(|left, right| {
        board_tier_rank(&left.tier)
            .cmp(&board_tier_rank(&right.tier))
            .then_with(|| left.name.cmp(&right.name))
    });
    Ok(boards)
}

fn board_tier_rank(tier: &str) -> u8 {
    match tier {
        "active" => 0,
        "backburner" => 1,
        "archived" => 2,
        _ => 3,
    }
}

fn validate_base_url(base_url: &str) -> Result<(), String> {
    if let Some(rest) = base_url.strip_prefix("https://") {
        let authority = rest
            .split(['/', '?', '#'])
            .next()
            .filter(|value| !value.is_empty())
            .ok_or("Powder baseUrl must include a host")?;
        if authority.contains('@') {
            return Err("Powder baseUrl must not contain embedded credentials".into());
        }
        return Ok(());
    }
    let Some(rest) = base_url.strip_prefix("http://") else {
        return Err("Powder baseUrl must use https://, or http:// for loopback development".into());
    };
    let authority = rest
        .split(['/', '?', '#'])
        .next()
        .filter(|value| !value.is_empty())
        .ok_or("Powder baseUrl must include a host")?;
    if authority.contains('@') {
        return Err("Powder baseUrl must not contain embedded credentials".into());
    }
    let loopback = if authority.starts_with('[') {
        authority
            .strip_prefix("[::1]")
            .is_some_and(|suffix| suffix.is_empty() || suffix.starts_with(':'))
    } else {
        let host = authority.split(':').next().unwrap_or_default();
        matches!(host, "localhost" | "127.0.0.1")
    };
    if !loopback {
        return Err(
            "Powder baseUrl must use HTTPS; plain HTTP is allowed only for localhost, 127.0.0.1, or [::1] development endpoints"
                .into(),
        );
    }
    Ok(())
}

fn response_error(error: ureq::Error) -> String {
    typed_response_error(error).to_string()
}

fn typed_response_error(error: ureq::Error) -> PowderError {
    match error {
        ureq::Error::Status(status, _) => {
            PowderError {
                kind: match status {
                    401 | 403 => PowderErrorKind::Unauthorized,
                    404 => PowderErrorKind::NotFound,
                    409 => PowderErrorKind::Conflict,
                    _ => PowderErrorKind::Upstream,
                },
                // Gateway response bodies are untrusted and can echo the full
                // protected request URL.  Preserve the typed HTTP status without
                // surfacing any upstream body text.
                message: format!("Powder HTTP status {status}"),
            }
        }
        ureq::Error::Transport(_) => PowderError {
            kind: PowderErrorKind::Unreachable,
            // `ureq::Transport` renders the full request URL, including any
            // protected path, query, or fragment material from a profile URL.
            // Preserve the typed failure kind without retaining that endpoint.
            message: "Powder transport failure".into(),
        },
    }
}

/// Return only the validated scheme and authority for externally visible links.
///
/// Protected profile paths, queries, and fragments can carry gateway credentials.
/// They remain available to the private client transport but must never be sent to
/// the UI or another caller as a board URL.
fn public_endpoint_origin(base_url: &str) -> String {
    let (scheme, rest) = if let Some(rest) = base_url.strip_prefix("https://") {
        ("https", rest)
    } else if let Some(rest) = base_url.strip_prefix("http://") {
        ("http", rest)
    } else {
        // `Client::new` validates the scheme before storing it.
        return "about:blank".into();
    };
    let authority = rest
        .split(['/', '?', '#'])
        .next()
        .filter(|value| !value.is_empty())
        .unwrap_or_default();
    format!("{scheme}://{authority}")
}

fn resolve_key_command(command: &str) -> Result<String, String> {
    #[cfg(windows)]
    let child = {
        use std::os::windows::process::CommandExt;

        let mut child = Command::new("powershell.exe");
        child
            .args(["-NoProfile", "-NonInteractive", "-Command", command])
            .creation_flags(0x0800_0000); // CREATE_NO_WINDOW
        child
    };
    #[cfg(not(windows))]
    let child = {
        let mut child = Command::new("sh");
        child.args(["-c", command]);
        child
    };
    let output = bounded_exec::output_with_timeout(child, KEY_COMMAND_TIMEOUT)
        .map_err(|error| format!("Powder apiKeyCommand failed: {error}"))?;
    if !output.status.success() {
        return Err(format!(
            "Powder apiKeyCommand exited with status {}",
            output.status
        ));
    }
    let key = String::from_utf8(output.stdout)
        .map_err(|_| "Powder apiKeyCommand returned non-UTF-8 output".to_string())?
        .trim()
        .to_string();
    if key.is_empty() {
        return Err("Powder apiKeyCommand returned an empty key".into());
    }
    Ok(key)
}

fn parse_claim(value: Value) -> Result<Claim, String> {
    Ok(Claim {
        card_id: required_string(&value, "card_id")?,
        run_id: required_string(&value, "run_id")?,
        agent: required_string(&value, "agent")?,
        expires_at: value["expires_at"]
            .as_i64()
            .ok_or("Powder claim response is missing expires_at")?,
    })
}

fn validate_claim_receipt(
    operation: &str,
    expected: &Claim,
    actual: Claim,
) -> Result<Claim, String> {
    if actual.card_id != expected.card_id
        || actual.run_id != expected.run_id
        || actual.agent != expected.agent
    {
        return Err(format!(
            "Powder {operation} response did not match the exact requested card, run, and agent"
        ));
    }
    Ok(actual)
}

fn validate_initial_claim_receipt(
    expected_card_id: &str,
    expected_agent: &str,
    actual: Claim,
) -> Result<Claim, String> {
    if actual.card_id != expected_card_id || actual.agent != expected_agent {
        return Err(
            "Powder claim response did not match the requested card and configured agent".into(),
        );
    }
    Ok(actual)
}

fn parse_event_stream(body: &str, after: i64) -> Result<Vec<CardEvent>, String> {
    let mut events = Vec::new();
    for frame in body.replace("\r\n", "\n").split("\n\n") {
        let mut sequence = None;
        let mut data = Vec::new();
        for line in frame.lines() {
            if let Some(raw) = line.strip_prefix("id:") {
                sequence = raw.trim().parse::<i64>().ok();
            } else if let Some(raw) = line.strip_prefix("data:") {
                data.push(raw.trim_start());
            }
        }
        if data.is_empty() {
            continue;
        }
        let sequence = sequence.ok_or("Powder event stream frame is missing a numeric id")?;
        if sequence <= after
            || events
                .last()
                .is_some_and(|event: &CardEvent| event.sequence >= sequence)
        {
            return Err("Powder event stream sequence is not strictly increasing".into());
        }
        let payload: Value = serde_json::from_str(&data.join("\n"))
            .map_err(|error| format!("Powder event stream contains invalid JSON: {error}"))?;
        if payload["schema_version"].as_str() != Some("powder.card_event.v1") {
            return Err("Powder event stream contains an unsupported schema_version".into());
        }
        events.push(CardEvent {
            sequence,
            event_id: required_event_string(&payload, "event_id")?,
            event_type: required_event_string(&payload, "event_type")?,
            occurred_at: payload["occurred_at"]
                .as_i64()
                .ok_or("Powder event is missing occurred_at")?,
            card_id: required_event_string(&payload["card"], "id")?,
            card_title: required_event_string(&payload["card"], "title")?,
            card_status: required_event_string(&payload["card"], "status")?,
            repository: payload["card"]["repo"].as_str().map(str::to_string),
            change: payload["change"].clone(),
        });
    }
    Ok(events)
}

fn required_event_string(value: &Value, key: &str) -> Result<String, String> {
    value[key]
        .as_str()
        .map(str::to_string)
        .ok_or_else(|| format!("Powder event is missing {key}"))
}

fn required_string(value: &Value, key: &str) -> Result<String, String> {
    let identity = value[key]
        .as_str()
        .map(str::to_string)
        .ok_or_else(|| format!("Powder claim response is missing {key}"))?;
    if identity.trim().is_empty()
        || identity.len() > MAX_CLAIM_IDENTITY_BYTES
        || identity.bytes().any(|byte| byte.is_ascii_control())
    {
        return Err(format!("Powder claim response has invalid {key}"));
    }
    Ok(identity)
}

fn parse_detailed_card(card_id: &str, value: Value) -> Result<DetailedCard, String> {
    let card = value
        .get("card")
        .and_then(Value::as_object)
        .ok_or_else(|| {
            format!("Powder detailed card response for '{card_id}' is missing its card envelope")
        })?;
    let response_id = card
        .get("id")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|id| !id.is_empty())
        .ok_or_else(|| {
            format!("Powder detailed card response for '{card_id}' is missing card id")
        })?;
    if response_id != card_id {
        return Err(format!(
            "Powder detailed card response returned card '{response_id}', not requested card '{card_id}'"
        ));
    }
    let repository = match card.get("repo") {
        None | Some(Value::Null) => None,
        Some(Value::String(repository)) => {
            let repository = repository.trim();
            (!repository.is_empty()).then(|| repository.to_string())
        }
        Some(_) => {
            return Err(format!(
                "Powder detailed card response for '{card_id}' has a non-string repository mapping"
            ));
        }
    };
    Ok(DetailedCard {
        envelope: value,
        repository,
    })
}

fn encode_path(raw: &str) -> String {
    let mut out = String::new();
    for byte in raw.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~') {
            out.push(byte as char);
        } else {
            out.push_str(&format!("%{byte:02X}"));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::net::TcpListener;

    fn test_client(addr: std::net::SocketAddr) -> Client {
        Client::new(ProfileConfig {
            base_url: format!("http://{addr}"),
            agent_name: "t-hub".into(),
            operation_identity: Some("actor-t-hub".into()),
            api_key: Some("test-key".into()),
            api_key_env: None,
            api_key_command: None,
        })
        .unwrap()
    }

    fn read_http_request(stream: &mut std::net::TcpStream) -> String {
        stream
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        let mut request = Vec::new();
        let mut buffer = [0_u8; 4096];
        loop {
            match stream.read(&mut buffer) {
                Ok(0) => break,
                Ok(count) => {
                    request.extend_from_slice(&buffer[..count]);
                    let text = String::from_utf8_lossy(&request);
                    let header_end = text.find("\r\n\r\n");
                    let content_length = text
                        .lines()
                        .find_map(|line| {
                            line.strip_prefix("Content-Length: ")
                                .or_else(|| line.strip_prefix("content-length: "))
                        })
                        .and_then(|value| value.trim().parse::<usize>().ok())
                        .unwrap_or(0);
                    if header_end.is_some_and(|end| request.len() >= end + 4 + content_length) {
                        break;
                    }
                }
                Err(error)
                    if matches!(
                        error.kind(),
                        std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                    ) =>
                {
                    break;
                }
                Err(error) => panic!("request read failed: {error}"),
            }
        }
        String::from_utf8(request).unwrap()
    }

    fn write_json_response(stream: &mut std::net::TcpStream, status: &str, body: &str) {
        write!(
            stream,
            "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
            body.len(),
            body
        )
        .unwrap();
    }

    fn operation_status(
        operation_id: &str,
        kind: &str,
        card_id: &str,
        run_id: &str,
        state: &str,
        result: Option<Value>,
        failure: Option<Value>,
    ) -> Value {
        json!({
            "schema_version": "powder.operation_status.v1",
            "operation_id": operation_id,
            "state": state,
            "request_digest": format!("sha256:{}", "ab".repeat(32)),
            "kind": kind,
            "target_card_id": card_id,
            "expected_run_id": run_id,
            "result": result,
            "failure": failure,
            "audit_event_id": (state == "succeeded").then_some("event-1"),
            "created_at": 10,
            "updated_at": 11,
            "expires_at": 9010
        })
    }

    fn with_request_digest(mut status: Value, request_digest: String) -> Value {
        status["request_digest"] = json!(request_digest);
        status
    }

    fn test_request_digest(kind: &str, card_id: &str, run_id: &str, body: &Value) -> String {
        let criterion_index;
        let criterion_proofs;
        let fields = match kind {
            "work_log_append" => vec![
                ("agent", body["agent"].as_str()),
                ("model", body["model"].as_str()),
                ("reasoning", body["reasoning"].as_str()),
                ("harness", body["harness"].as_str()),
                ("body", body["body"].as_str()),
            ],
            "criterion_review" => {
                criterion_index = body["criterion"].as_u64().unwrap().to_string();
                vec![
                    ("criterion_index", Some(criterion_index.as_str())),
                    ("criterion_id", body["criterion_id"].as_str()),
                    ("decision", body["decision"].as_str()),
                    ("proof", body["proof"].as_str()),
                ]
            }
            "completion" => {
                criterion_proofs = serde_json::to_string(&body["criterion_proofs"]).unwrap();
                vec![
                    ("proof", body["proof"].as_str()),
                    ("criterion_proofs", Some(criterion_proofs.as_str())),
                ]
            }
            _ => panic!("unsupported test operation kind"),
        };
        canonical_operation_request_digest("actor-t-hub", kind, card_id, run_id, &fields).unwrap()
    }

    fn test_criterion_id(text: &str, occurrence: usize) -> String {
        let digest = Sha256::digest(text.as_bytes());
        format!("powder.criterion.v1:sha256:{digest:x}:{occurrence}")
    }

    fn run_criterion_fixture(
        card_id: &str,
        run_id: &str,
        criterion_index: usize,
        criterion_text: &str,
        decision: Option<&str>,
    ) -> Value {
        let criterion_id = test_criterion_id(criterion_text, 0);
        let review = decision.map(|decision| {
            json!({
                "id": format!("review-{criterion_index}"),
                "operation_id": format!("review:{criterion_index}"),
                "card_id": card_id,
                "run_id": run_id,
                "criterion_index": criterion_index,
                "criterion_id": criterion_id,
                "criterion_text": criterion_text,
                "decision": decision,
                "reviewer": "crew-reviewer",
                "reviewer_identity": "actor-crew-reviewer",
                "proof": "bounded review proof",
                "created_at": 14
            })
        });
        json!({
            "criterion_index": criterion_index,
            "criterion_id": criterion_id,
            "criterion_text": criterion_text,
            "review": review
        })
    }

    fn card_criteria_fixture(decision: Option<&str>, claimed: bool, legacy_checked: bool) -> Value {
        let mut criterion = json!({"text": "ship it", "proof_links": []});
        if legacy_checked {
            criterion["checked_by"] = json!("legacy-operator");
            criterion["checked_at"] = json!(9);
        }
        let mut card = json!({
            "id": "card-criteria",
            "title": "Authoritative criteria",
            "status": if claimed { "running" } else { "ready" },
            "criteria": [criterion]
        });
        if claimed {
            card["claim"] = json!({
                "agent": "crew-reviewer",
                "run_id": "run-criteria",
                "expires_at": 99
            });
        }
        let mut envelope = json!({"card": card});
        if claimed {
            envelope["current_run_criteria"] = json!([run_criterion_fixture(
                "card-criteria",
                "run-criteria",
                0,
                "ship it",
                decision
            )]);
        }
        envelope
    }

    #[test]
    fn profile_file_resolves_a_command_backed_key() {
        let path = std::env::temp_dir().join(format!(
            "t-hub-powder-profile-{}-{}.json",
            std::process::id(),
            std::thread::current().name().unwrap_or("test")
        ));
        std::fs::write(
            &path,
            r#"{
              "schemaVersion": 1,
              "profiles": {
                "production": {
                  "baseUrl": "https://powder.example.test/",
                  "agentName": "t-hub",
                  "apiKeyCommand": "printf secret-key"
                }
              }
            }"#,
        )
        .unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        }
        let client = Client::from_profile_path("production", &path).unwrap();
        assert_eq!(client.base_url, "https://powder.example.test");
        assert_eq!(client.agent_name, "t-hub");
        assert_eq!(client.api_key.as_deref(), Some("secret-key"));
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn profile_names_are_sorted_without_loading_credentials() {
        let path = std::env::temp_dir().join(format!(
            "t-hub-powder-profile-names-{}-{}.json",
            std::process::id(),
            std::thread::current().name().unwrap_or("test")
        ));
        std::fs::write(
            &path,
            r#"{
              "schemaVersion": 1,
              "profiles": {
                "zulu": { "baseUrl": "https://z.test", "agentName": "z" },
                "alpha": { "baseUrl": "https://a.test", "agentName": "a" }
              }
            }"#,
        )
        .unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        }
        assert_eq!(
            configured_profile_names_from_path(&path).unwrap(),
            vec!["alpha".to_string(), "zulu".to_string()]
        );
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn claim_response_requires_the_authoritative_fields() {
        let claim = parse_claim(json!({
            "card_id": "card-1",
            "run_id": "run-1",
            "agent": "terminal-1",
            "expires_at": 123,
        }))
        .unwrap();
        assert_eq!(claim.card_id, "card-1");
        assert_eq!(claim.run_id, "run-1");
        assert_eq!(claim.agent, "terminal-1");
        assert_eq!(claim.expires_at, 123);
        assert!(parse_claim(json!({ "card_id": "card-1" })).is_err());
    }

    #[test]
    fn claim_response_rejects_empty_whitespace_control_and_oversized_identities() {
        for key in ["card_id", "run_id", "agent"] {
            for invalid in [
                "",
                " \t ",
                "run\n1",
                &"x".repeat(MAX_CLAIM_IDENTITY_BYTES + 1),
            ] {
                let mut receipt = json!({
                    "card_id": "card-1",
                    "run_id": "run-1",
                    "agent": "t-hub",
                    "expires_at": 123,
                });
                receipt[key] = json!(invalid);
                let error = parse_claim(receipt).unwrap_err();
                assert_eq!(error, format!("Powder claim response has invalid {key}"));
            }
        }
    }

    #[test]
    fn claim_response_accepts_identities_at_the_exact_byte_boundary() {
        let boundary = "x".repeat(MAX_CLAIM_IDENTITY_BYTES);
        let claim = parse_claim(json!({
            "card_id": boundary,
            "run_id": "r".repeat(MAX_CLAIM_IDENTITY_BYTES),
            "agent": "a".repeat(MAX_CLAIM_IDENTITY_BYTES),
            "expires_at": 123,
        }))
        .unwrap();
        assert_eq!(claim.card_id.len(), MAX_CLAIM_IDENTITY_BYTES);
        assert_eq!(claim.run_id.len(), MAX_CLAIM_IDENTITY_BYTES);
        assert_eq!(claim.agent.len(), MAX_CLAIM_IDENTITY_BYTES);
    }

    #[test]
    fn claim_response_body_is_bounded_before_json_parsing() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let request = read_http_request(&mut stream);
            assert!(request.starts_with("POST /api/v1/cards/card-1/claim "));
            let body = "x".repeat(MAX_CLAIM_RESPONSE_BYTES + 1);
            write_json_response(&mut stream, "200 OK", &body);
        });

        let error = test_client(addr).claim("card-1", 3600).unwrap_err();
        assert_eq!(
            error,
            format!(
                "Powder evidence response is invalid: response exceeds the {MAX_CLAIM_RESPONSE_BYTES}-byte limit"
            )
        );
        server.join().unwrap();
    }

    #[test]
    fn claim_rejects_card_or_agent_substitution_before_dispatch_can_bind_it() {
        for (receipt_card, receipt_agent) in [("card-other", "t-hub"), ("card-1", "other")] {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let addr = listener.local_addr().unwrap();
            let server = std::thread::spawn(move || {
                let (mut stream, _) = listener.accept().unwrap();
                let request = read_http_request(&mut stream);
                assert!(request.starts_with("POST /api/v1/cards/card-1/claim "));
                assert!(request.contains(r#""agent":"t-hub""#));
                let body = json!({
                    "card_id": receipt_card,
                    "run_id": "run-1",
                    "agent": receipt_agent,
                    "expires_at": 1234,
                })
                .to_string();
                write_json_response(&mut stream, "200 OK", &body);
            });

            let error = test_client(addr).claim("card-1", 3600).unwrap_err();

            assert_eq!(
                error,
                "Powder claim response did not match the requested card and configured agent"
            );
            server.join().unwrap();
        }
    }

    #[test]
    fn run_bound_mutations_require_schema_18_and_complete_route_contract() {
        let route_catalog = json!([
            {
                "method": "POST",
                "path": "/api/v1/cards/{id}/runs/{run_id}/work-log",
                "body_shape": "{\"operation_id\":\"...\",\"agent\":\"...\",\"body\":\"...\"}"
            },
            {
                "method": "POST",
                "path": "/api/v1/cards/{id}/runs/{run_id}/criteria/review",
                "body_shape": "{\"operation_id\":\"...\",\"criterion\":0,\"criterion_id\":\"...\",\"decision\":\"approved\"}"
            },
            {
                "method": "POST",
                "path": "/api/v1/cards/{id}/runs/{run_id}/complete",
                "body_shape": "{\"operation_id\":\"...\",\"proof\":null,\"criterion_proofs\":null}"
            },
            {
                "method": "GET",
                "path": "/api/v1/operations/{id}",
                "body_shape": null
            }
        ]);
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let server = std::thread::spawn(move || {
            for response in [
                json!({"ok": true, "auth_mode": "api_key", "schema_version": 18}),
                route_catalog,
            ] {
                let (mut stream, _) = listener.accept().unwrap();
                let request = read_http_request(&mut stream);
                assert!(request.contains("GET /"));
                write_json_response(&mut stream, "200 OK", &response.to_string());
            }
        });
        let capabilities = test_client(addr)
            .require_run_bound_mutation_capabilities()
            .unwrap();
        assert_eq!(capabilities.schema_version, 18);
        assert!(capabilities.work_log);
        assert!(capabilities.criterion_review);
        assert!(capabilities.completion);
        assert!(capabilities.operation_recovery);
        server.join().unwrap();

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let _ = read_http_request(&mut stream);
            write_json_response(
                &mut stream,
                "200 OK",
                &json!({"ok": true, "auth_mode": "api_key", "schema_version": 17}).to_string(),
            );
        });
        let error = test_client(addr)
            .require_run_bound_mutation_capabilities()
            .unwrap_err();
        assert_eq!(error.kind, PowderErrorKind::InvalidResponse);
        server.join().unwrap();
    }

    #[test]
    fn run_bound_mutations_require_a_stable_profile_operation_identity_before_network() {
        let client = Client::new(ProfileConfig {
            base_url: "http://127.0.0.1:9".into(),
            agent_name: "t-hub".into(),
            operation_identity: None,
            api_key: Some("test-key".into()),
            api_key_env: None,
            api_key_command: None,
        })
        .unwrap();

        let error = client
            .require_run_bound_mutation_capabilities()
            .unwrap_err();

        assert_eq!(error.kind, PowderErrorKind::InvalidResponse);
        assert!(error.message.contains("stable operationIdentity"));
    }

    #[test]
    fn run_bound_client_round_trips_paths_payloads_attribution_and_receipts() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let server = std::thread::spawn(move || {
            for expected_path in [
                "/api/v1/cards/card%2Fone",
                "/api/v1/runs/run%2Fone",
                "/api/v1/cards/card%2Fone/runs/run%2Fone/work-log",
                "/api/v1/cards/card%2Fone/runs/run%2Fone/criteria/review",
                "/api/v1/cards/card%2Fone/runs/run%2Fone/complete",
            ] {
                let (mut stream, _) = listener.accept().unwrap();
                let request = read_http_request(&mut stream);
                assert!(
                    request.lines().next().unwrap().contains(expected_path),
                    "request: {request}"
                );
                assert!(request.contains("Authorization: Bearer test-key"));
                let body = if expected_path.ends_with("work-log") {
                    let request_body: Value =
                        serde_json::from_str(request.split_once("\r\n\r\n").unwrap().1).unwrap();
                    assert_eq!(request_body["agent"], "crew-87ea2ca1");
                    assert_eq!(request_body["model"], "gpt-5");
                    assert_eq!(request_body["reasoning"], "high");
                    assert_eq!(request_body["harness"], "codex");
                    assert_eq!(request_body["operation_id"], "work-log:one");
                    assert_eq!(request_body["body"], "focused tests are passing");
                    assert!(request_body.get("actor").is_none());
                    assert!(request_body.get("run_id").is_none());
                    with_request_digest(
                        operation_status(
                            "work-log:one",
                            "work_log_append",
                            "card/one",
                            "run/one",
                            "succeeded",
                            Some(json!({
                            "schema_version": "powder.work_log_entry.v1",
                            "id": "work-log-one",
                            "card_id": "card/one",
                            "actor": "crew-87ea2ca1",
                            "agent": "crew-87ea2ca1",
                            "model": "gpt-5",
                            "reasoning": "high",
                            "harness": "codex",
                            "run_id": "run/one",
                            "body": "focused tests are passing",
                            "created_at": 15,
                            "updated_at": 15
                            })),
                            None,
                        ),
                        test_request_digest(
                            "work_log_append",
                            "card/one",
                            "run/one",
                            &request_body,
                        ),
                    )
                } else if expected_path.ends_with("criteria/review") {
                    let request_body: Value =
                        serde_json::from_str(request.split_once("\r\n\r\n").unwrap().1).unwrap();
                    let criterion_id = test_criterion_id("tests pass", 0);
                    assert_eq!(request_body["operation_id"], "criterion:one");
                    assert_eq!(request_body["criterion"], 0);
                    assert_eq!(request_body["criterion_id"], criterion_id);
                    assert_eq!(request_body["decision"], "approved");
                    assert_eq!(request_body["proof"], "raw secret-shaped review proof");
                    assert!(request_body.get("reviewer").is_none());
                    assert!(request_body.get("reviewer_identity").is_none());
                    with_request_digest(
                        operation_status(
                            "criterion:one",
                            "criterion_review",
                            "card/one",
                            "run/one",
                            "succeeded",
                            Some(json!({
                            "id": "review-one",
                            "operation_id": "criterion:one",
                            "card_id": "card/one",
                            "run_id": "run/one",
                            "criterion_index": 0,
                            "criterion_id": criterion_id,
                            "criterion_text": "tests pass",
                            "decision": "approved",
                            "reviewer": "captain-powder",
                            "reviewer_identity": "actor-captain-powder",
                            "proof": "[scrubbed review proof]",
                            "created_at": 19
                            })),
                            None,
                        ),
                        test_request_digest(
                            "criterion_review",
                            "card/one",
                            "run/one",
                            &request_body,
                        ),
                    )
                } else if expected_path.ends_with("complete") {
                    let request_body: Value =
                        serde_json::from_str(request.split_once("\r\n\r\n").unwrap().1).unwrap();
                    assert_eq!(request_body["operation_id"], "completion:one");
                    assert_eq!(request_body["proof"], "commit abc; tests passed");
                    assert_eq!(
                        request_body["criterion_proofs"].as_array().unwrap().len(),
                        MAX_COMPLETION_CRITERION_PROOFS
                    );
                    assert_eq!(request_body["criterion_proofs"][0]["criterion"], 0);
                    assert_eq!(
                        request_body["criterion_proofs"][0]["url"]
                            .as_str()
                            .unwrap()
                            .len(),
                        "https://example.test/proof".len()
                    );
                    assert!(request_body.get("actor").is_none());
                    assert!(request_body.get("run_id").is_none());
                    with_request_digest(
                        operation_status(
                            "completion:one",
                            "completion",
                            "card/one",
                            "run/one",
                            "succeeded",
                            Some(json!({
                            "schema_version": "powder.run_bound_completion.v1",
                            "card_id": "card/one",
                            "run_id": "run/one",
                            "operation_id": "completion:one",
                            "status": "done",
                            "proof": "commit abc; tests passed",
                            "criterion_proofs": request_body["criterion_proofs"].clone(),
                            "updated_at": 20,
                            "audit_event_id": "event-1"
                            })),
                            None,
                        ),
                        test_request_digest("completion", "card/one", "run/one", &request_body),
                    )
                } else if expected_path.contains("/runs/") {
                    json!({
                        "run": {
                            "id": "run/one",
                            "card_id": "card/one",
                            "state": "complete",
                            "agent": "t-hub",
                            "claim_expires_at": 99,
                            "proof": "commit abc; tests passed",
                            "created_at": 10,
                            "updated_at": 20
                        },
                        "card": {
                            "id": "card/one",
                            "title": "Client evidence",
                            "status": "done",
                            "repo": "t-hub",
                            "criteria": [{
                                "text": "tests pass",
                                "checked_by": "crew-87ea2ca1",
                                "checked_at": 20,
                                "proof_links": []
                            }]
                        },
                        "criteria": [run_criterion_fixture(
                            "card/one",
                            "run/one",
                            0,
                            "tests pass",
                            Some("approved")
                        )],
                        "activities": [{
                            "id": "activity-1",
                            "run_id": "run/one",
                            "activity_type": "response",
                            "payload": "commit abc; tests passed",
                            "created_at": 20
                        }],
                        "activities_total": 2,
                        "links": [{
                            "id": "link-1",
                            "card_id": "card/one",
                            "label": "proof",
                            "url": "https://example.test/proof",
                            "created_at": 20
                        }],
                        "links_total": 1
                    })
                } else {
                    json!({
                        "card": {
                            "id": "card/one",
                            "title": "Client evidence",
                            "status": "running",
                            "repo": "t-hub",
                            "claim": {
                                "agent": "t-hub",
                                "run_id": "run/one",
                                "expires_at": 99
                            },
                            "criteria": [{"text": "tests pass", "proof_links": []}]
                        },
                        "runs": [{
                            "id": "run/one",
                            "card_id": "card/one",
                            "state": "active",
                            "agent": "t-hub",
                            "claim_expires_at": 99,
                            "created_at": 10,
                            "updated_at": 11
                        }],
                        "runs_total": 2,
                        "current_run_criteria": [run_criterion_fixture(
                            "card/one",
                            "run/one",
                            0,
                            "tests pass",
                            Some("approved")
                        )],
                        "work_log": [{
                            "card_id": "card/one",
                            "agent": "crew-87ea2ca1",
                            "run_id": "run/one",
                            "body": "started",
                            "created_at": 12
                        }],
                        "work_log_total": 3
                    })
                };
                write_json_response(&mut stream, "200 OK", &body.to_string());
            }
        });

        let client = test_client(addr);
        let card = client.card_evidence("card/one").unwrap();
        assert_eq!(card.card_id, "card/one");
        assert_eq!(card.repository.as_deref(), Some("t-hub"));
        assert_eq!(card.claim.as_ref().unwrap().run_id, "run/one");
        card.require_completion_criteria_approved("run/one")
            .unwrap();
        assert_eq!(
            card.current_run_criteria.as_ref().unwrap()[0]
                .review
                .as_ref()
                .unwrap()
                .decision,
            CriterionReviewDecision::Approved
        );
        assert_eq!(card.runs_total, 2);
        assert_eq!(card.work_log_total, 3);
        assert!(card.truncated);

        let run = client.run_evidence("run/one").unwrap();
        assert_eq!(run.run.run_id, "run/one");
        assert_eq!(run.run.proof.as_deref(), Some("commit abc; tests passed"));
        assert!(run.criteria[0].is_approved());
        assert_eq!(
            run.card_criteria[0].checked_by.as_deref(),
            Some("crew-87ea2ca1")
        );
        assert_eq!(run.activities_total, 2);
        assert!(run.truncated);

        let attribution = WorkLogAttribution {
            agent: "crew-87ea2ca1".into(),
            model: Some("gpt-5".into()),
            reasoning: Some("high".into()),
            harness: Some("codex".into()),
        };
        let appended = client
            .append_run_work_log(
                "card/one",
                &RunBoundWorkLog {
                    expected_run_id: "run/one".into(),
                    operation_id: "work-log:one".into(),
                    attribution,
                    body: "focused tests are passing".into(),
                },
            )
            .unwrap();
        assert_eq!(appended.state, OperationState::Succeeded);
        let entry = appended.result.unwrap();
        assert_eq!(entry.agent, "crew-87ea2ca1");
        assert_eq!(entry.run_id.as_deref(), Some("run/one"));

        let reviewed = client
            .review_run_criterion(
                "card/one",
                &RunBoundCriterionReview {
                    expected_run_id: "run/one".into(),
                    operation_id: "criterion:one".into(),
                    criterion_index: 0,
                    criterion_id: test_criterion_id("tests pass", 0),
                    criterion_text: "tests pass".into(),
                    decision: CriterionReviewDecision::Approved,
                    proof: Some("raw secret-shaped review proof".into()),
                    expected_reviewer_identity: "actor-captain-powder".into(),
                },
            )
            .unwrap();
        let review = reviewed.result.unwrap();
        assert_eq!(review.operation_id, "criterion:one");
        assert_eq!(review.reviewer_identity, "actor-captain-powder");
        assert_eq!(review.proof.as_deref(), Some("[scrubbed review proof]"));

        let completed = client
            .complete_run_with_proof(
                "card/one",
                &RunBoundCompletion {
                    expected_run_id: "run/one".into(),
                    operation_id: "completion:one".into(),
                    proof: "commit abc; tests passed".into(),
                    criterion_proofs: (0..MAX_COMPLETION_CRITERION_PROOFS)
                        .map(|criterion| CriterionProof {
                            criterion,
                            url: "https://example.test/proof".into(),
                        })
                        .collect(),
                },
            )
            .unwrap();
        assert_eq!(completed.state, OperationState::Succeeded);
        let receipt = completed.result.unwrap();
        assert_eq!(receipt.status, "done");
        assert_eq!(receipt.run_id, "run/one");
        assert_eq!(
            receipt.criterion_proofs.len(),
            MAX_COMPLETION_CRITERION_PROOFS
        );
        assert!(receipt
            .criterion_proofs
            .iter()
            .all(|proof| proof.url == "https://example.test/proof"));
        server.join().unwrap();
    }

    #[test]
    fn authoritative_card_criteria_gate_completion_and_ignore_legacy_checks() {
        let approved = parse_card_evidence(
            card_criteria_fixture(Some("approved"), true, false),
            "card-criteria",
        )
        .unwrap();
        approved
            .require_completion_criteria_approved("run-criteria")
            .unwrap();
        assert_eq!(
            approved.current_run_criteria.as_ref().unwrap()[0]
                .review
                .as_ref()
                .unwrap()
                .decision,
            CriterionReviewDecision::Approved
        );

        let rejected = parse_card_evidence(
            card_criteria_fixture(Some("rejected"), true, false),
            "card-criteria",
        )
        .unwrap();
        assert_eq!(
            rejected.current_run_criteria.as_ref().unwrap()[0]
                .review
                .as_ref()
                .unwrap()
                .decision,
            CriterionReviewDecision::Rejected
        );
        assert!(rejected
            .require_completion_criteria_approved("run-criteria")
            .is_err());

        let cleared = parse_card_evidence(
            card_criteria_fixture(Some("cleared"), true, false),
            "card-criteria",
        )
        .unwrap();
        assert_eq!(
            cleared.current_run_criteria.as_ref().unwrap()[0]
                .review
                .as_ref()
                .unwrap()
                .decision,
            CriterionReviewDecision::Cleared
        );
        assert!(cleared
            .require_completion_criteria_approved("run-criteria")
            .is_err());

        let legacy_only =
            parse_card_evidence(card_criteria_fixture(None, true, true), "card-criteria").unwrap();
        assert_eq!(
            legacy_only.criteria[0].checked_by.as_deref(),
            Some("legacy-operator")
        );
        assert!(legacy_only.current_run_criteria.as_ref().unwrap()[0]
            .review
            .is_none());
        assert!(legacy_only
            .require_completion_criteria_approved("run-criteria")
            .is_err());

        let released =
            parse_card_evidence(card_criteria_fixture(None, false, true), "card-criteria").unwrap();
        assert!(released.claim.is_none());
        assert!(released.current_run_criteria.is_none());
        assert!(released
            .require_completion_criteria_approved("run-criteria")
            .is_err());
    }

    #[test]
    fn released_run_criteria_preserve_history_and_reject_identity_substitution() {
        let base = json!({
            "run": {
                "id": "run-criteria",
                "card_id": "card-criteria",
                "state": "released",
                "agent": "crew-reviewer",
                "claim_expires_at": 99,
                "created_at": 1,
                "updated_at": 10
            },
            "card": {
                "id": "card-criteria",
                "title": "Authoritative criteria",
                "status": "ready",
                "criteria": [{
                    "text": "ship it",
                    "checked_by": "legacy-operator",
                    "checked_at": 9,
                    "proof_links": []
                }]
            },
            "criteria": [run_criterion_fixture(
                "card-criteria",
                "run-criteria",
                0,
                "ship it",
                Some("approved")
            )]
        });
        let released = parse_run_evidence(base.clone(), "run-criteria").unwrap();
        assert_eq!(released.criteria.len(), 1);
        assert!(released.criteria[0].is_approved());
        assert_eq!(
            released.criteria[0]
                .review
                .as_ref()
                .unwrap()
                .reviewer_identity,
            "actor-crew-reviewer"
        );

        for (field, replacement) in [
            ("card_id", json!("foreign-card")),
            ("run_id", json!("foreign-run")),
            ("reviewer_identity", json!("legacy:unverified")),
            (
                "reviewer",
                json!("x".repeat(MAX_CRITERION_REVIEWER_BYTES + 1)),
            ),
            ("proof", json!("x".repeat(MAX_COMPLETION_PROOF_BYTES + 1))),
        ] {
            let mut invalid = base.clone();
            invalid["criteria"][0]["review"][field] = replacement;
            assert!(parse_run_evidence(invalid, "run-criteria").is_err());
        }

        let mut wrong_identity = base;
        wrong_identity["criteria"][0]["criterion_id"] = json!(test_criterion_id("other", 0));
        assert!(parse_run_evidence(wrong_identity, "run-criteria").is_err());
    }

    #[test]
    fn evidence_client_rejects_oversized_inputs_before_network_io() {
        let client = Client::new(ProfileConfig {
            base_url: "http://127.0.0.1:9".into(),
            agent_name: "t-hub".into(),
            operation_identity: Some("actor-t-hub".into()),
            api_key: Some("test-key".into()),
            api_key_env: None,
            api_key_command: None,
        })
        .unwrap();
        let error = client
            .append_run_work_log(
                "card-1",
                &RunBoundWorkLog {
                    expected_run_id: "run-1".into(),
                    operation_id: "work-log:bounds".into(),
                    attribution: WorkLogAttribution {
                        agent: "crew".into(),
                        model: None,
                        reasoning: None,
                        harness: Some("codex".into()),
                    },
                    body: "x".repeat(MAX_WORK_LOG_BODY_BYTES + 1),
                },
            )
            .unwrap_err();
        assert_eq!(error.kind, PowderErrorKind::InvalidResponse);
        assert!(error.message.contains("16384-byte limit"));

        let error = client
            .append_run_work_log(
                "card-1",
                &RunBoundWorkLog {
                    expected_run_id: "run-1".into(),
                    operation_id: "work-log:attribution-bounds".into(),
                    attribution: WorkLogAttribution {
                        agent: "x".repeat(MAX_WORK_LOG_ATTRIBUTION_BYTES + 1),
                        model: None,
                        reasoning: None,
                        harness: None,
                    },
                    body: "bounded".into(),
                },
            )
            .unwrap_err();
        assert!(error.message.contains("256-byte limit"));

        let error = client
            .complete_run_with_proof(
                "card-1",
                &RunBoundCompletion {
                    expected_run_id: "run-1".into(),
                    operation_id: "completion:bounds".into(),
                    proof: "proof".into(),
                    criterion_proofs: (0..=MAX_COMPLETION_CRITERION_PROOFS)
                        .map(|criterion| CriterionProof {
                            criterion,
                            url: "https://example.test/proof".into(),
                        })
                        .collect(),
                },
            )
            .unwrap_err();
        assert!(error.message.contains("proof count exceeds 128"));
        let error = client
            .complete_run_with_proof(
                "card-1",
                &RunBoundCompletion {
                    expected_run_id: "run-1".into(),
                    operation_id: "completion:url-bounds".into(),
                    proof: "proof".into(),
                    criterion_proofs: vec![CriterionProof {
                        criterion: 0,
                        url: "u".repeat(MAX_PROOF_URL_BYTES + 1),
                    }],
                },
            )
            .unwrap_err();
        assert!(error.message.contains("4096-byte limit"));
        let error = client
            .recover_completion_operation(
                "card-1",
                &RunBoundCompletion {
                    expected_run_id: "run-1".into(),
                    operation_id: "contains/slash".into(),
                    proof: "proof".into(),
                    criterion_proofs: Vec::new(),
                },
            )
            .unwrap_err();
        assert!(error.message.contains("ASCII letters"));
        let error = client
            .recover_completion_operation(
                "card-1",
                &RunBoundCompletion {
                    expected_run_id: "run-1".into(),
                    operation_id: "x".repeat(MAX_OPERATION_ID_BYTES + 1),
                    proof: "proof".into(),
                    criterion_proofs: Vec::new(),
                },
            )
            .unwrap_err();
        assert!(error.message.contains("128-byte limit"));
        assert!(client
            .card_evidence(" ")
            .unwrap_err()
            .message
            .contains("must not be empty"));
    }

    #[test]
    fn run_bound_mutations_preserve_stale_released_expired_and_reclaimed_rejections() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let server = std::thread::spawn(move || {
            for (operation_id, failure_code) in [
                ("stale-op", "conflict"),
                ("released-op", "conflict"),
                ("expired-op", "claim_expired"),
                ("reclaimed-op", "conflict"),
            ] {
                let (mut stream, _) = listener.accept().unwrap();
                let request = read_http_request(&mut stream);
                assert!(request
                    .lines()
                    .next()
                    .unwrap()
                    .contains("/api/v1/cards/card-1/runs/run-1/work-log"));
                let request_body: Value =
                    serde_json::from_str(request.split_once("\r\n\r\n").unwrap().1).unwrap();
                assert_eq!(request_body["operation_id"], operation_id);
                let response = with_request_digest(
                    operation_status(
                        operation_id,
                        "work_log_append",
                        "card-1",
                        "run-1",
                        "rejected",
                        None,
                        Some(json!({"code": failure_code, "message": "run is not current"})),
                    ),
                    test_request_digest("work_log_append", "card-1", "run-1", &request_body),
                );
                write_json_response(&mut stream, "200 OK", &response.to_string());
            }
            let (mut stream, _) = listener.accept().unwrap();
            let request = read_http_request(&mut stream);
            assert!(request
                .lines()
                .next()
                .unwrap()
                .contains("/api/v1/cards/card-1/runs/run-1/complete"));
            let request_body: Value =
                serde_json::from_str(request.split_once("\r\n\r\n").unwrap().1).unwrap();
            let response = with_request_digest(
                operation_status(
                    "completion-stale-op",
                    "completion",
                    "card-1",
                    "run-1",
                    "rejected",
                    None,
                    Some(json!({"code": "conflict", "message": "run was reclaimed"})),
                ),
                test_request_digest("completion", "card-1", "run-1", &request_body),
            );
            write_json_response(&mut stream, "200 OK", &response.to_string());
        });

        let client = test_client(addr);
        for (operation_id, failure_code) in [
            ("stale-op", "conflict"),
            ("released-op", "conflict"),
            ("expired-op", "claim_expired"),
            ("reclaimed-op", "conflict"),
        ] {
            let outcome = client
                .append_run_work_log(
                    "card-1",
                    &RunBoundWorkLog {
                        expected_run_id: "run-1".into(),
                        operation_id: operation_id.into(),
                        attribution: WorkLogAttribution {
                            agent: "crew".into(),
                            model: None,
                            reasoning: None,
                            harness: Some("codex".into()),
                        },
                        body: "entry".into(),
                    },
                )
                .unwrap();
            assert_eq!(outcome.state, OperationState::Rejected);
            assert_eq!(outcome.failure.unwrap().code, failure_code);
            assert!(outcome.result.is_none());
        }
        let completion = client
            .complete_run_with_proof(
                "card-1",
                &RunBoundCompletion {
                    expected_run_id: "run-1".into(),
                    operation_id: "completion-stale-op".into(),
                    proof: "proof".into(),
                    criterion_proofs: Vec::new(),
                },
            )
            .unwrap();
        assert_eq!(completion.state, OperationState::Rejected);
        assert_eq!(completion.failure.unwrap().code, "conflict");
        assert!(completion.result.is_none());
        server.join().unwrap();
    }

    #[test]
    fn operation_recovery_distinguishes_exact_replay_pending_and_unknown() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let server = std::thread::spawn(move || {
            for operation_id in [
                "work-log-replay",
                "completion-replay",
                "completion-rejected",
                "completion-pending",
                "completion-unknown",
            ] {
                let (mut stream, _) = listener.accept().unwrap();
                let request = read_http_request(&mut stream);
                assert!(request
                    .lines()
                    .next()
                    .unwrap()
                    .contains(&format!("/api/v1/operations/{operation_id}")));
                let response = match operation_id {
                    "work-log-replay" => operation_status(
                        operation_id,
                        "work_log_append",
                        "card-1",
                        "run-1",
                        "succeeded",
                        Some(json!({
                            "schema_version": "powder.work_log_entry.v1",
                            "id": "work-log-replayed",
                            "card_id": "card-1",
                            "actor": "crew",
                            "agent": "crew",
                            "run_id": "run-1",
                            "body": "durable evidence",
                            "created_at": 11,
                            "updated_at": 11
                        })),
                        None,
                    ),
                    "completion-replay" => operation_status(
                        operation_id,
                        "completion",
                        "card-1",
                        "run-1",
                        "succeeded",
                        Some(json!({
                            "schema_version": "powder.run_bound_completion.v1",
                            "card_id": "card-1",
                            "run_id": "run-1",
                            "operation_id": operation_id,
                            "status": "done",
                            "proof": "verified",
                            "criterion_proofs": [],
                            "updated_at": 11,
                            "audit_event_id": "event-1"
                        })),
                        None,
                    ),
                    "completion-rejected" => operation_status(
                        operation_id,
                        "completion",
                        "card-1",
                        "run-1",
                        "rejected",
                        None,
                        Some(json!({"code": "conflict", "message": "run was released"})),
                    ),
                    "completion-pending" => operation_status(
                        operation_id,
                        "completion",
                        "card-1",
                        "run-1",
                        "pending",
                        None,
                        None,
                    ),
                    _ => json!({
                        "schema_version": "powder.operation_status.v1",
                        "operation_id": operation_id,
                        "state": "unknown"
                    }),
                };
                let response = match operation_id {
                    "work-log-replay" => with_request_digest(
                        response,
                        test_request_digest(
                            "work_log_append",
                            "card-1",
                            "run-1",
                            &json!({
                                "agent": "crew",
                                "model": Value::Null,
                                "reasoning": Value::Null,
                                "harness": Value::Null,
                                "body": "durable evidence",
                            }),
                        ),
                    ),
                    "completion-unknown" => response,
                    _ => with_request_digest(
                        response,
                        test_request_digest(
                            "completion",
                            "card-1",
                            "run-1",
                            &json!({
                                "proof": "verified",
                                "criterion_proofs": [],
                            }),
                        ),
                    ),
                };
                write_json_response(&mut stream, "200 OK", &response.to_string());
            }
        });

        let client = test_client(addr);
        let work_log_request = RunBoundWorkLog {
            expected_run_id: "run-1".into(),
            operation_id: "work-log-replay".into(),
            attribution: WorkLogAttribution {
                agent: "crew".into(),
                model: None,
                reasoning: None,
                harness: None,
            },
            body: "durable evidence".into(),
        };
        let work_log_replay = client
            .recover_work_log_operation("card-1", &work_log_request)
            .unwrap();
        assert_eq!(work_log_replay.state, OperationState::Succeeded);
        assert_eq!(work_log_replay.result.unwrap().body, "durable evidence");
        let completion_request = |operation_id: &str| RunBoundCompletion {
            expected_run_id: "run-1".into(),
            operation_id: operation_id.into(),
            proof: "verified".into(),
            criterion_proofs: Vec::new(),
        };
        let replay = client
            .recover_completion_operation("card-1", &completion_request("completion-replay"))
            .unwrap();
        assert_eq!(replay.state, OperationState::Succeeded);
        assert_eq!(replay.result.unwrap().proof.as_deref(), Some("verified"));
        let rejected = client
            .recover_completion_operation("card-1", &completion_request("completion-rejected"))
            .unwrap();
        assert_eq!(rejected.state, OperationState::Rejected);
        assert_eq!(rejected.failure.unwrap().code, "conflict");
        assert!(rejected.result.is_none());
        let pending = client
            .recover_completion_operation("card-1", &completion_request("completion-pending"))
            .unwrap();
        assert_eq!(pending.state, OperationState::Pending);
        assert!(pending.result.is_none());
        let unknown = client
            .recover_completion_operation("card-1", &completion_request("completion-unknown"))
            .unwrap();
        assert_eq!(unknown.state, OperationState::Unknown);
        assert!(unknown.target_card_id.is_none());
        server.join().unwrap();
    }

    #[test]
    fn operation_response_requires_canonical_sha256_request_digest() {
        let valid = operation_status(
            "digest-op",
            "completion",
            "card-1",
            "run-1",
            "pending",
            None,
            None,
        );
        assert!(parse_operation_outcome::<Value>(
            valid.clone(),
            "completion",
            "card-1",
            "run-1",
            "digest-op",
            None,
            false,
            Ok,
        )
        .is_ok());

        let mut uppercase = valid.clone();
        uppercase["request_digest"] = json!(format!("sha256:{}", "AB".repeat(32)));
        assert!(parse_operation_outcome::<Value>(
            uppercase,
            "completion",
            "card-1",
            "run-1",
            "digest-op",
            None,
            false,
            Ok,
        )
        .is_err());

        for invalid_digest in [
            "a".repeat(64),
            format!("SHA256:{}", "a".repeat(64)),
            format!("sha256:{}", "a".repeat(63)),
            format!("sha256:{}", "a".repeat(65)),
            format!("sha256:{}", "g".repeat(64)),
        ] {
            let mut invalid = valid.clone();
            invalid["request_digest"] = json!(invalid_digest);
            assert!(parse_operation_outcome::<Value>(
                invalid,
                "completion",
                "card-1",
                "run-1",
                "digest-op",
                None,
                false,
                Ok,
            )
            .is_err());
        }
    }

    #[test]
    fn evidence_proof_urls_accept_4096_bytes_and_reject_larger_values() {
        let at_limit = json!({
            "criteria": [{
                "text": "bounded evidence",
                "proof_links": [{
                    "url": "u".repeat(MAX_PROOF_URL_BYTES),
                    "actor": "reviewer",
                    "created_at": 1
                }]
            }]
        });
        let criteria = parse_criteria(&at_limit).unwrap();
        assert_eq!(criteria[0].proof_links[0].url.len(), MAX_PROOF_URL_BYTES);

        let over_limit = json!({
            "criteria": [{
                "text": "bounded evidence",
                "proof_links": [{
                    "url": "u".repeat(MAX_PROOF_URL_BYTES + 1),
                    "actor": "reviewer",
                    "created_at": 1
                }]
            }]
        });
        let error = parse_criteria(&over_limit).unwrap_err();
        assert!(error.message.contains("4096-byte limit"));
    }

    #[test]
    fn run_bound_client_fails_closed_on_substitution_malformed_and_server_responses() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let server = std::thread::spawn(move || {
            for expected_path in [
                "/api/v1/cards/card-1/runs/run-1/work-log",
                "/api/v1/cards/card-1",
                "/api/v1/runs/run-1",
                "/api/v1/cards/card-1/runs/run-1/complete",
                "/api/v1/cards/oversized",
            ] {
                let (mut stream, _) = listener.accept().unwrap();
                let request = read_http_request(&mut stream);
                assert!(request.lines().next().unwrap().contains(expected_path));
                if expected_path.ends_with("work-log") {
                    let request_body: Value =
                        serde_json::from_str(request.split_once("\r\n\r\n").unwrap().1).unwrap();
                    let response = with_request_digest(
                        operation_status(
                            "work-log:bad",
                            "work_log_append",
                            "another-card",
                            "run-1",
                            "rejected",
                            None,
                            Some(json!({"code": "conflict", "message": "stale"})),
                        ),
                        test_request_digest("work_log_append", "card-1", "run-1", &request_body),
                    );
                    write_json_response(&mut stream, "200 OK", &response.to_string());
                } else if expected_path == "/api/v1/cards/card-1" {
                    let entries = (0..=MAX_EVIDENCE_ITEMS)
                        .map(|index| {
                            json!({
                                "card_id": "card-1", "agent": "crew", "run_id": "run-1",
                                "body": format!("entry-{index}"), "created_at": index
                            })
                        })
                        .collect::<Vec<_>>();
                    write_json_response(
                        &mut stream,
                        "200 OK",
                        &json!({
                            "card": {"id": "card-1", "title": "card", "status": "running"},
                            "work_log": entries, "work_log_total": entries.len()
                        })
                        .to_string(),
                    );
                } else if expected_path.contains("/runs/") && !expected_path.ends_with("complete") {
                    write_json_response(&mut stream, "200 OK", "{not-json");
                } else if expected_path.ends_with("complete") {
                    write_json_response(
                        &mut stream,
                        "500 Internal Server Error",
                        r#"{"error":"test-key must never escape\ninternal detail"}"#,
                    );
                } else {
                    write!(
                        stream,
                        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{{}}",
                        MAX_EVIDENCE_RESPONSE_BYTES + 1
                    )
                    .unwrap();
                }
            }
        });

        let client = test_client(addr);
        let error = client
            .append_run_work_log(
                "card-1",
                &RunBoundWorkLog {
                    expected_run_id: "run-1".into(),
                    operation_id: "work-log:bad".into(),
                    attribution: WorkLogAttribution {
                        agent: "crew".into(),
                        model: None,
                        reasoning: None,
                        harness: Some("codex".into()),
                    },
                    body: "entry".into(),
                },
            )
            .unwrap_err();
        assert!(error.message.contains("different target card id"));
        assert!(client
            .card_evidence("card-1")
            .unwrap_err()
            .message
            .contains("20-item limit"));
        assert!(client
            .run_evidence("run-1")
            .unwrap_err()
            .message
            .contains("not valid JSON"));
        let error = client
            .complete_run_with_proof(
                "card-1",
                &RunBoundCompletion {
                    expected_run_id: "run-1".into(),
                    operation_id: "completion:server-error".into(),
                    proof: "proof".into(),
                    criterion_proofs: Vec::new(),
                },
            )
            .unwrap_err();
        assert_eq!(error.kind, PowderErrorKind::Upstream);
        assert_eq!(error.message, "Powder HTTP status 500");
        assert!(!error.message.contains("test-key"));
        assert!(!error.message.contains('\n'));
        assert!(client
            .card_evidence("oversized")
            .unwrap_err()
            .message
            .contains("524288-byte limit"));
        server.join().unwrap();
    }

    #[test]
    fn completion_receipt_requires_matching_run_operation_and_done_card() {
        let base = json!({
            "schema_version": "powder.run_bound_completion.v1",
            "card_id": "card-1",
            "run_id": "run-1",
            "operation_id": "completion:one",
            "status": "done",
            "proof": "proof",
            "criterion_proofs": [],
            "updated_at": 1,
            "audit_event_id": "event-1"
        });
        let mut wrong_card_value = base.clone();
        wrong_card_value["card_id"] = json!("card-2");
        let wrong_card =
            parse_completion_receipt(wrong_card_value, "card-1", "run-1", "completion:one")
                .unwrap_err();
        assert!(wrong_card.message.contains("different card id"));

        let mut not_done_value = base.clone();
        not_done_value["status"] = json!("running");
        let not_done =
            parse_completion_receipt(not_done_value, "card-1", "run-1", "completion:one")
                .unwrap_err();
        assert!(not_done.message.contains("did not confirm done"));

        let mut wrong_run_value = base.clone();
        wrong_run_value["run_id"] = json!("run-2");
        let wrong_run =
            parse_completion_receipt(wrong_run_value, "card-1", "run-1", "completion:one")
                .unwrap_err();
        assert!(wrong_run.message.contains("different run id"));

        let mut too_many_proofs = base.clone();
        too_many_proofs["criterion_proofs"] = json!((0..=MAX_COMPLETION_CRITERION_PROOFS)
            .map(|criterion| json!({"criterion": criterion, "url": "proof"}))
            .collect::<Vec<_>>());
        let error = parse_completion_receipt(too_many_proofs, "card-1", "run-1", "completion:one")
            .unwrap_err();
        assert!(error.message.contains("128-item limit"));

        let mut oversized_url = base;
        oversized_url["criterion_proofs"] = json!([{
            "criterion": 0,
            "url": "u".repeat(MAX_PROOF_URL_BYTES + 1)
        }]);
        let error = parse_completion_receipt(oversized_url, "card-1", "run-1", "completion:one")
            .unwrap_err();
        assert!(error.message.contains("4096-byte limit"));
    }

    #[test]
    fn repository_list_becomes_a_deterministic_board_catalog() {
        let boards = parse_board_list(json!({
            "repositories": [
                { "name": "later", "aliases": [], "tier": "archived", "card_count": 0 },
                { "name": "t-hub", "aliases": ["thub"], "tier": "active", "card_count": 4 },
                { "name": "backlog", "aliases": [], "tier": "backburner", "card_count": 2 }
            ]
        }))
        .unwrap();
        assert_eq!(
            boards
                .iter()
                .map(|board| board.name.as_str())
                .collect::<Vec<_>>(),
            vec!["t-hub", "backlog", "later"]
        );
        assert_eq!(boards[0].aliases, vec!["thub"]);
        assert_eq!(boards[0].card_count, 4);
        assert!(parse_board_list(json!({ "repositories": [{ "tier": "active" }] })).is_err());
        assert!(parse_board_list(json!({ "repositories": "not-an-array" })).is_err());
    }

    #[test]
    fn board_page_keeps_only_bounded_safe_card_fields() {
        let page = parse_board_page(json!({
            "cards": [{
                "id": "t-hub-1",
                "title": "Repair Board",
                "body": "private implementation detail",
                "status": "running",
                "priority": "p1",
                "estimate": "m",
                "labels": ["desktop"],
                "repo": "t-hub",
                "claim": { "agent": "crew-one", "expires_at": 456 },
                "updated_at": 123
            }],
            "total_count": 1,
            "has_more": false
        }))
        .unwrap();
        assert_eq!(page.cards[0].id, "t-hub-1");
        assert_eq!(page.cards[0].labels, vec!["desktop"]);
        assert_eq!(page.cards[0].claim.as_ref().unwrap().agent, "crew-one");
        let serialized = serde_json::to_string(&page).unwrap();
        assert!(!serialized.contains("private implementation detail"));
        assert!(!serialized.contains("repo"));
    }

    #[test]
    fn external_board_url_is_credential_free_and_not_falsely_filtered() {
        let client = Client::new(ProfileConfig {
            base_url: "https://powder.example.test/gateway/path-token?access_token=query-token#fragment-token".into(),
            agent_name: "t-hub".into(),
            operation_identity: None,
            api_key: Some("secret-key".into()),
            api_key_env: None,
            api_key_command: None,
        })
        .unwrap();
        assert_eq!(
            client.external_board_url(),
            "https://powder.example.test/board"
        );
        assert!(!client.external_board_url().contains("secret-key"));
        assert!(!client.external_board_url().contains("path-token"));
        assert!(!client.external_board_url().contains("query-token"));
        assert!(!client.external_board_url().contains("fragment-token"));
        assert!(!client.external_board_url().contains("repo="));
    }

    #[test]
    fn transport_errors_are_endpoint_free_and_keep_unreachable_classification() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        drop(listener);
        let endpoint =
            format!("http://{addr}/gateway/path-token?access_token=query-token#fragment-token");
        let client = Client::new(ProfileConfig {
            base_url: endpoint.clone(),
            agent_name: "t-hub".into(),
            operation_identity: None,
            api_key: Some("secret-key".into()),
            api_key_env: None,
            api_key_command: None,
        })
        .unwrap();

        let error = client.repository_for_board("t-hub").unwrap_err();
        assert_eq!(error.kind, PowderErrorKind::Unreachable);
        assert_eq!(error.message, "Powder transport failure");
        for secret in [
            "path-token",
            "query-token",
            "fragment-token",
            "secret-key",
            endpoint.as_str(),
        ] {
            assert!(
                !error.to_string().contains(secret),
                "transport leaked {secret:?}"
            );
            assert!(
                !format!("{error:?}").contains(secret),
                "debug leaked {secret:?}"
            );
        }
    }

    #[test]
    fn status_error_bodies_never_surface_protected_endpoint_or_credential_material() {
        for (status, expected_kind) in [
            ("403 Forbidden", PowderErrorKind::Unauthorized),
            ("500 Internal Server Error", PowderErrorKind::Upstream),
        ] {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let addr = listener.local_addr().unwrap();
            let response_body = json!({
                "error": "gateway echoed /gateway/path-token?access_token=query-token#fragment-token with secret-key"
            })
            .to_string();
            let server = std::thread::spawn(move || {
                let (mut stream, _) = listener.accept().unwrap();
                let _ = read_http_request(&mut stream);
                write_json_response(&mut stream, status, &response_body);
            });
            let endpoint =
                format!("http://{addr}/gateway/path-token?access_token=query-token#fragment-token");
            let client = Client::new(ProfileConfig {
                base_url: endpoint.clone(),
                agent_name: "t-hub".into(),
                operation_identity: None,
                api_key: Some("secret-key".into()),
                api_key_env: None,
                api_key_command: None,
            })
            .unwrap();

            let error = client.repository_for_board("t-hub").unwrap_err();
            assert_eq!(error.kind, expected_kind);
            assert!(error.message.starts_with("Powder HTTP status "));
            for secret in [
                "path-token",
                "query-token",
                "fragment-token",
                "secret-key",
                endpoint.as_str(),
            ] {
                assert!(
                    !error.to_string().contains(secret),
                    "status leaked {secret:?}"
                );
                assert!(
                    !format!("{error:?}").contains(secret),
                    "debug leaked {secret:?}"
                );
            }
            server.join().unwrap();
        }
    }

    #[test]
    fn endpoint_identity_requires_a_protected_credential() {
        let client = Client::new(ProfileConfig {
            base_url: "https://powder.example.test".into(),
            agent_name: "t-hub".into(),
            operation_identity: None,
            api_key: None,
            api_key_env: None,
            api_key_command: None,
        })
        .unwrap();
        assert_eq!(
            client.endpoint_identity().unwrap_err(),
            "Powder protected endpoint identity cannot be derived without an API credential"
        );
    }

    #[test]
    fn board_requests_preserve_authorization_failure_kind() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = [0_u8; 2048];
            let count = stream.read(&mut request).unwrap();
            let request = String::from_utf8_lossy(&request[..count]);
            assert!(request.contains("GET /api/v1/repositories/private"));
            let body = r#"{"error":"forbidden"}"#;
            write!(
                stream,
                "HTTP/1.1 403 Forbidden\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            )
            .unwrap();
        });
        let client = Client::new(ProfileConfig {
            base_url: format!("http://{addr}"),
            agent_name: "t-hub".into(),
            operation_identity: None,
            api_key: Some("test-key".into()),
            api_key_env: None,
            api_key_command: None,
        })
        .unwrap();
        let error = client.repository_for_board("private").unwrap_err();
        assert_eq!(error.kind, PowderErrorKind::Unauthorized);
        assert!(error.message.contains("403"));
        server.join().unwrap();
    }

    #[test]
    fn path_segments_are_percent_encoded() {
        assert_eq!(encode_path("repo/card 1"), "repo%2Fcard%201");
    }

    #[test]
    fn plain_http_is_limited_to_explicit_loopback_hosts() {
        for base_url in [
            "http://powder.example.test",
            "http://10.0.0.2:8080",
            "http://localhost.example.test",
            "https://user:secret@powder.example.test",
        ] {
            let error = Client::new(ProfileConfig {
                base_url: base_url.into(),
                agent_name: "t-hub".into(),
                operation_identity: None,
                api_key: Some("secret".into()),
                api_key_env: None,
                api_key_command: None,
            })
            .err()
            .unwrap();
            assert!(
                error.contains("must use HTTPS") || error.contains("embedded credentials"),
                "{base_url}: {error}"
            );
        }

        for base_url in [
            "http://localhost:8080",
            "http://127.0.0.1:8080",
            "http://[::1]:8080",
            "https://powder.example.test",
        ] {
            Client::new(ProfileConfig {
                base_url: base_url.into(),
                agent_name: "t-hub".into(),
                operation_identity: None,
                api_key: Some("secret".into()),
                api_key_env: None,
                api_key_command: None,
            })
            .unwrap();
        }
    }

    #[test]
    fn http_client_round_trips_card_and_claim_lifecycle() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let server = std::thread::spawn(move || {
            for expected_path in [
                "/api/v1/repositories",
                "/api/v1/repositories/repo%2Fone",
                "/api/v1/repositories/repo-board",
                "/api/v1/cards?repo=repo-board&limit=1000",
                "/api/v1/cards/card-1?detail=detailed",
                "/api/v1/cards/card-1/claim",
                "/api/v1/cards/card-1/heartbeat",
                "/api/v1/cards/card-1/renew",
                "/api/v1/cards/card-1/release",
                "/api/v1/events/tail?after=4&limit=25",
            ] {
                let (mut stream, _) = listener.accept().unwrap();
                stream
                    .set_read_timeout(Some(Duration::from_secs(2)))
                    .unwrap();
                let mut request = Vec::new();
                let mut buffer = [0_u8; 4096];
                loop {
                    match stream.read(&mut buffer) {
                        Ok(0) => break,
                        Ok(count) => {
                            request.extend_from_slice(&buffer[..count]);
                            let text = String::from_utf8_lossy(&request);
                            let header_end = text.find("\r\n\r\n");
                            let content_length = text
                                .lines()
                                .find_map(|line| {
                                    line.strip_prefix("Content-Length: ")
                                        .or_else(|| line.strip_prefix("content-length: "))
                                })
                                .and_then(|value| value.trim().parse::<usize>().ok())
                                .unwrap_or(0);
                            if header_end
                                .is_some_and(|end| request.len() >= end + 4 + content_length)
                            {
                                break;
                            }
                        }
                        Err(error)
                            if matches!(
                                error.kind(),
                                std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                            ) =>
                        {
                            break;
                        }
                        Err(error) => panic!("request read failed: {error}"),
                    }
                }
                let request = String::from_utf8(request).unwrap();
                assert!(
                    request.lines().next().unwrap().contains(expected_path),
                    "request: {request}"
                );
                assert!(request.contains("Authorization: Bearer test-key"));
                if expected_path.ends_with("/claim") {
                    assert!(request.contains(r#""agent":"t-hub""#));
                }
                let body = if expected_path.contains("events/tail") {
                    concat!(
                        "id: 5\n",
                        "event: completed\n",
                        "data: {\"schema_version\":\"powder.card_event.v1\",",
                        "\"event_id\":\"evt-5\",\"event_type\":\"completed\",",
                        "\"occurred_at\":500,\"actor\":\"crew\",",
                        "\"card\":{\"id\":\"card-1\",\"title\":\"Ship it\",",
                        "\"status\":\"done\",\"repo\":\"repo-1\"},",
                        "\"change\":{\"proof\":\"tests\"}}\n\n"
                    )
                    .to_string()
                } else if expected_path == "/api/v1/repositories" {
                    json!({
                        "repositories": [{
                            "name": "repo-one",
                            "aliases": [],
                            "tier": "active",
                            "card_count": 1
                        }]
                    })
                    .to_string()
                } else if expected_path == "/api/v1/repositories/repo-board" {
                    json!({
                        "name": "repo-board",
                        "aliases": [],
                        "tier": "active",
                        "card_count": 1
                    })
                    .to_string()
                } else if expected_path == "/api/v1/cards?repo=repo-board&limit=1000" {
                    json!({
                        "cards": [{
                            "id": "repo-board-1",
                            "title": "Board card",
                            "status": "ready",
                            "priority": "p1",
                            "labels": [],
                            "updated_at": 123
                        }],
                        "total_count": 1,
                        "has_more": false
                    })
                    .to_string()
                } else if expected_path.contains("repositories/") {
                    json!({ "name": "repo/one" }).to_string()
                } else if expected_path.contains("?detail=") {
                    json!({
                        "card": {
                            "id": "card-1",
                            "repo": "repo-1",
                            "claim": null
                        }
                    })
                    .to_string()
                } else {
                    json!({
                        "card_id": "card-1",
                        "run_id": "run-1",
                        "agent": "t-hub",
                        "expires_at": 1234,
                    })
                    .to_string()
                };
                let content_type = if expected_path.contains("events/tail") {
                    "text/event-stream"
                } else {
                    "application/json"
                };
                write!(
                    stream,
                    "HTTP/1.1 200 OK\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    body.len(),
                    body
                )
                .unwrap();
            }
        });
        let client = Client::new(ProfileConfig {
            base_url: format!("http://{addr}"),
            agent_name: "t-hub".into(),
            operation_identity: None,
            api_key: Some("test-key".into()),
            api_key_env: None,
            api_key_command: None,
        })
        .unwrap();
        assert_eq!(client.list_boards().unwrap()[0].name, "repo-one");
        assert_eq!(
            client.get_repository("repo/one").unwrap()["name"],
            "repo/one"
        );
        assert_eq!(
            client
                .repository_for_board("repo-board")
                .unwrap()
                .card_count,
            1
        );
        assert_eq!(
            client.board_page("repo-board", 1000).unwrap().cards[0].id,
            "repo-board-1"
        );
        let card = client.get_card("card-1").unwrap();
        assert_eq!(card.repository(), Some("repo-1"));
        assert_eq!(card.card_value()["id"], "card-1");
        assert_eq!(card.envelope()["card"]["id"], "card-1");
        let claim = client.claim("card-1", 3600).unwrap();
        assert_eq!(claim.agent, "t-hub");
        assert_eq!(client.heartbeat(&claim).unwrap().run_id, "run-1");
        assert_eq!(client.renew(&claim, 3600).unwrap().expires_at, 1234);
        assert_eq!(client.release(&claim).unwrap().card_id, "card-1");
        let events = client.tail_events(4, 25).unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].sequence, 5);
        assert_eq!(events[0].event_type, "completed");
        assert_eq!(events[0].repository.as_deref(), Some("repo-1"));
        assert_eq!(events[0].change["proof"], "tests");
        server.join().unwrap();
    }

    #[test]
    fn release_rejects_a_structurally_valid_receipt_for_another_claim() {
        for (receipt_card, receipt_run, receipt_agent) in [
            ("card-other", "run-1", "t-hub"),
            ("card-1", "run-other", "t-hub"),
            ("card-1", "run-1", "another-agent"),
        ] {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let addr = listener.local_addr().unwrap();
            let server = std::thread::spawn(move || {
                let (mut stream, _) = listener.accept().unwrap();
                let request = read_http_request(&mut stream);
                assert!(request.starts_with("POST /api/v1/cards/card-1/release "));
                assert!(request.contains(r#""run_id":"run-1""#));
                let body = json!({
                    "card_id": receipt_card,
                    "run_id": receipt_run,
                    "agent": receipt_agent,
                    "expires_at": 1234,
                })
                .to_string();
                write_json_response(&mut stream, "200 OK", &body);
            });
            let claim = Claim {
                card_id: "card-1".into(),
                run_id: "run-1".into(),
                agent: "t-hub".into(),
                expires_at: 1234,
            };

            let error = test_client(addr).release(&claim).unwrap_err();

            assert_eq!(
                error,
                "Powder release response did not match the exact requested card, run, and agent"
            );
            server.join().unwrap();
        }
    }

    #[test]
    fn renewal_rejects_a_structurally_valid_receipt_for_another_claim() {
        for (operation, receipt_card, receipt_run, receipt_agent) in [
            ("heartbeat", "card-other", "run-1", "t-hub"),
            ("heartbeat", "card-1", "run-other", "t-hub"),
            ("heartbeat", "card-1", "run-1", "another-agent"),
            ("renewal", "card-other", "run-1", "t-hub"),
            ("renewal", "card-1", "run-other", "t-hub"),
            ("renewal", "card-1", "run-1", "another-agent"),
        ] {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let addr = listener.local_addr().unwrap();
            let server = std::thread::spawn(move || {
                let (mut stream, _) = listener.accept().unwrap();
                let request = read_http_request(&mut stream);
                assert!(request.starts_with(&format!(
                    "POST /api/v1/cards/card-1/{} ",
                    if operation == "heartbeat" {
                        "heartbeat"
                    } else {
                        "renew"
                    }
                )));
                assert!(request.contains(r#""run_id":"run-1""#));
                let body = json!({
                    "card_id": receipt_card,
                    "run_id": receipt_run,
                    "agent": receipt_agent,
                    "expires_at": 1234,
                })
                .to_string();
                write_json_response(&mut stream, "200 OK", &body);
            });
            let claim = Claim {
                card_id: "card-1".into(),
                run_id: "run-1".into(),
                agent: "t-hub".into(),
                expires_at: 1234,
            };

            let client = test_client(addr);
            let error = if operation == "heartbeat" {
                client.heartbeat(&claim).unwrap_err()
            } else {
                client.renew(&claim, 3600).unwrap_err()
            };

            assert_eq!(
                error,
                format!(
                    "Powder {operation} response did not match the exact requested card, run, and agent"
                )
            );
            server.join().unwrap();
        }
    }

    #[test]
    fn detailed_card_validates_the_authoritative_envelope_and_repository() {
        let card = parse_detailed_card(
            "card-1",
            json!({
                "card": {
                    "id": "card-1",
                    "repo": "repo-1",
                    "unknownCardField": { "preserved": true }
                },
                "unknownDetailField": ["preserved"]
            }),
        )
        .unwrap();
        assert_eq!(card.repository(), Some("repo-1"));
        assert_eq!(card.card_value()["id"], "card-1");
        assert_eq!(card.envelope()["unknownDetailField"], json!(["preserved"]));
        assert_eq!(card.card_value()["unknownCardField"]["preserved"], true);

        let missing_envelope =
            parse_detailed_card("card-1", json!({ "id": "card-1", "repo": "repo-1" })).unwrap_err();
        assert!(missing_envelope.contains("missing its card envelope"));

        for response in [
            json!({ "card": { "id": "card-1" } }),
            json!({ "card": { "id": "card-1", "repo": null } }),
            json!({ "card": { "id": "card-1", "repo": " " } }),
            json!({ "card": { "id": "card-1" }, "repo": "top-level-only" }),
        ] {
            let card = parse_detailed_card("card-1", response).unwrap();
            assert_eq!(card.repository(), None);
        }

        let malformed_card = parse_detailed_card("card-1", json!({ "card": [] })).unwrap_err();
        assert!(malformed_card.contains("missing its card envelope"));

        let malformed_repository = parse_detailed_card(
            "card-1",
            json!({ "card": { "id": "card-1", "repo": ["repo-1"] } }),
        )
        .unwrap_err();
        assert!(malformed_repository.contains("non-string repository mapping"));

        let mismatched_card = parse_detailed_card(
            "card-1",
            json!({ "card": { "id": "card-2", "repo": "repo-1" } }),
        )
        .unwrap_err();
        assert!(mismatched_card.contains("not requested card 'card-1'"));
    }

    #[test]
    fn event_stream_rejects_replayed_or_unsupported_frames() {
        let replayed = concat!(
            "id: 4\n",
            "data: {\"schema_version\":\"powder.card_event.v1\"}\n\n"
        );
        assert!(parse_event_stream(replayed, 4)
            .unwrap_err()
            .contains("strictly increasing"));

        let unsupported = concat!(
            "id: 5\n",
            "data: {\"schema_version\":\"powder.card_event.v2\"}\n\n"
        );
        assert!(parse_event_stream(unsupported, 4)
            .unwrap_err()
            .contains("unsupported"));
    }
}
