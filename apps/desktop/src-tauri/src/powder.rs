//! Protected Powder connection profiles and the narrow API client T-Hub uses.
//!
//! Captain state stores only a profile name and Powder repository. Endpoint and
//! credential material lives in `~/.t-hub/powder-profiles.json` or process env.

use std::collections::HashMap;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::bounded_exec;

const HTTP_TIMEOUT: Duration = Duration::from_secs(12);
const KEY_COMMAND_TIMEOUT: Duration = Duration::from_secs(10);
const MAX_EVIDENCE_RESPONSE_BYTES: usize = 512 * 1024;
const MAX_MUTATION_RESPONSE_BYTES: usize = 256 * 1024;
const MAX_ERROR_RESPONSE_BYTES: usize = 4096;
const MAX_EVIDENCE_ITEMS: usize = 20;
const MAX_CRITERIA: usize = 100;
const MAX_CRITERION_PROOFS: usize = 20;
const MAX_EVIDENCE_TOTAL: usize = 1_000_000;
const MAX_ID_BYTES: usize = 256;
const MAX_SHORT_TEXT_BYTES: usize = 512;
const MAX_EVIDENCE_TEXT_BYTES: usize = 4096;
pub const MAX_WORK_LOG_BODY_BYTES: usize = 16 * 1024;
pub const MAX_COMPLETION_PROOF_BYTES: usize = 4096;
pub const MAX_COMPLETION_CRITERION_PROOFS: usize = 100;
pub const MAX_PROOF_URL_BYTES: usize = 2048;

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
    pub run_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkLogEntry {
    pub card_id: String,
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
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompletionProof {
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
    pub runs: Vec<RunSummary>,
    pub runs_total: usize,
    pub work_log: Vec<WorkLogEntry>,
    pub work_log_total: usize,
    pub truncated: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RunEvidence {
    pub run: RunSummary,
    pub card_title: String,
    pub card_status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub repository: Option<String>,
    pub criteria: Vec<CriterionEvidence>,
    pub activities: Vec<EvidenceActivity>,
    pub activities_total: usize,
    pub links: Vec<EvidenceLink>,
    pub links_total: usize,
    pub truncated: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CompletionReceipt {
    pub card_id: String,
    pub status: String,
    pub updated_at: i64,
    pub criteria: Vec<CriterionEvidence>,
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
        format!("{}/board", self.base_url)
    }

    pub fn claim(&self, card_id: &str, ttl_seconds: u64) -> Result<Claim, String> {
        let value = self.request(
            "POST",
            &format!("/api/v1/cards/{}/claim", encode_path(card_id)),
            Some(json!({ "agent": self.agent_name, "ttl_seconds": ttl_seconds })),
        )?;
        parse_claim(value)
    }

    pub fn heartbeat(&self, claim: &Claim) -> Result<Claim, String> {
        let value = self.request(
            "POST",
            &format!("/api/v1/cards/{}/heartbeat", encode_path(&claim.card_id)),
            Some(json!({ "run_id": claim.run_id })),
        )?;
        parse_claim(value)
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
        parse_claim(value)
    }

    pub fn release(&self, claim: &Claim) -> Result<Claim, String> {
        let value = self.request(
            "POST",
            &format!("/api/v1/cards/{}/release", encode_path(&claim.card_id)),
            Some(json!({ "run_id": claim.run_id })),
        )?;
        parse_claim(value)
    }

    /// Append one Crew-attributed entry to Powder's work log.
    ///
    /// T-Hub supplies the exact Crew and run identity from its durable binding.
    /// Input is rejected before any request when it would exceed the bounded
    /// control contract, and the response must echo the authoritative binding.
    pub fn append_work_log(
        &self,
        card_id: &str,
        attribution: &WorkLogAttribution,
        body: &str,
    ) -> Result<WorkLogEntry, PowderError> {
        validate_id("card id", card_id)?;
        validate_short_text("work-log agent", &attribution.agent)?;
        validate_optional_short_text("work-log model", attribution.model.as_deref())?;
        validate_optional_short_text("work-log reasoning", attribution.reasoning.as_deref())?;
        validate_optional_short_text("work-log harness", attribution.harness.as_deref())?;
        validate_id("work-log run id", &attribution.run_id)?;
        validate_required_bounded_text("work-log body", body, MAX_WORK_LOG_BODY_BYTES)?;

        let value = self.request_typed_with_limit(
            "POST",
            &format!("/api/v1/cards/{}/work-log", encode_path(card_id)),
            Some(json!({
                "agent": attribution.agent,
                "model": attribution.model,
                "reasoning": attribution.reasoning,
                "harness": attribution.harness,
                "run_id": attribution.run_id,
                "body": body,
            })),
            MAX_MUTATION_RESPONSE_BYTES,
        )?;
        parse_work_log_entry(value, card_id, Some(attribution))
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

    /// Complete a card with a required bounded proof and optional criterion links.
    pub fn complete_with_proof(
        &self,
        card_id: &str,
        completion: &CompletionProof,
    ) -> Result<CompletionReceipt, PowderError> {
        validate_id("card id", card_id)?;
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
        let value = self.request_typed_with_limit(
            "POST",
            &format!("/api/v1/cards/{}/complete", encode_path(card_id)),
            Some(json!({
                "proof": completion.proof,
                "criterion_proofs": completion.criterion_proofs,
            })),
            MAX_MUTATION_RESPONSE_BYTES,
        )?;
        parse_completion_receipt(value, card_id)
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
            Err(error) => Err(response_error(error, self.api_key.as_deref())),
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
        response.map_err(|error| typed_response_error(error, self.api_key.as_deref()))
    }
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
        card_id,
        agent: bounded_string(&value, "agent", "work-log agent", MAX_SHORT_TEXT_BYTES)?,
        model: optional_bounded_string(&value, "model", "work-log model", MAX_SHORT_TEXT_BYTES)?,
        reasoning: optional_bounded_string(
            &value,
            "reasoning",
            "work-log reasoning",
            MAX_SHORT_TEXT_BYTES,
        )?,
        harness: optional_bounded_string(
            &value,
            "harness",
            "work-log harness",
            MAX_SHORT_TEXT_BYTES,
        )?,
        run_id: optional_bounded_string(&value, "run_id", "work-log run id", MAX_ID_BYTES)?,
        body: bounded_string(&value, "body", "work-log body", MAX_WORK_LOG_BODY_BYTES)?,
        created_at: required_i64(&value, "created_at", "work-log created_at")?,
    };
    if let Some(expected) = expected_attribution {
        if entry.agent != expected.agent
            || entry.model != expected.model
            || entry.reasoning != expected.reasoning
            || entry.harness != expected.harness
            || entry.run_id.as_deref() != Some(expected.run_id.as_str())
        {
            return Err(invalid_response(
                "work-log entry attribution does not match the request",
            ));
        }
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
    Ok(CardEvidence {
        card_id,
        title: bounded_string(card, "title", "card title", MAX_EVIDENCE_TEXT_BYTES)?,
        status: card_status(card)?,
        repository: optional_bounded_string(card, "repo", "card repository", MAX_ID_BYTES)?,
        claim: parse_evidence_claim(card.get("claim"))?,
        criteria: parse_criteria(card)?,
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
    Ok(RunEvidence {
        run,
        card_title: bounded_string(card, "title", "card title", MAX_EVIDENCE_TEXT_BYTES)?,
        card_status: card_status(card)?,
        repository: optional_bounded_string(card, "repo", "card repository", MAX_ID_BYTES)?,
        criteria: parse_criteria(card)?,
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
) -> Result<CompletionReceipt, PowderError> {
    let card_id = bounded_string(&value, "id", "completed card id", MAX_ID_BYTES)?;
    if card_id != expected_card_id {
        return Err(invalid_response(
            "completion response returned a different card id",
        ));
    }
    let status = card_status(&value)?;
    if status != "done" {
        return Err(invalid_response(
            "completion response did not confirm done status",
        ));
    }
    if value.get("claim").is_some_and(|claim| !claim.is_null()) {
        return Err(invalid_response(
            "completion response retained an active claim",
        ));
    }
    Ok(CompletionReceipt {
        card_id,
        status,
        updated_at: required_i64(&value, "updated_at", "completed card updated_at")?,
        criteria: parse_criteria(&value)?,
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

fn validate_short_text(label: &str, value: &str) -> Result<(), PowderError> {
    validate_required_bounded_text(label, value, MAX_SHORT_TEXT_BYTES)
}

fn validate_optional_short_text(label: &str, value: Option<&str>) -> Result<(), PowderError> {
    value.map_or(Ok(()), |value| validate_short_text(label, value))
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

fn response_error(error: ureq::Error, credential: Option<&str>) -> String {
    typed_response_error(error, credential).to_string()
}

fn typed_response_error(error: ureq::Error, credential: Option<&str>) -> PowderError {
    match error {
        ureq::Error::Status(status, response) => {
            let detail = bounded_error_detail(response).unwrap_or_else(|| format!("HTTP {status}"));
            let detail = safe_error_detail(&detail, credential);
            PowderError {
                kind: match status {
                    401 | 403 => PowderErrorKind::Unauthorized,
                    404 => PowderErrorKind::NotFound,
                    _ => PowderErrorKind::Upstream,
                },
                message: format!("Powder HTTP {status}: {detail}"),
            }
        }
        ureq::Error::Transport(error) => PowderError {
            kind: PowderErrorKind::Unreachable,
            message: format!("Powder is unreachable: {error}"),
        },
    }
}

fn bounded_error_detail(response: ureq::Response) -> Option<String> {
    if response
        .header("Content-Length")
        .and_then(|length| length.parse::<usize>().ok())
        .is_some_and(|length| length > MAX_ERROR_RESPONSE_BYTES)
    {
        return None;
    }
    let mut body = Vec::with_capacity(MAX_ERROR_RESPONSE_BYTES);
    response
        .into_reader()
        .take((MAX_ERROR_RESPONSE_BYTES + 1) as u64)
        .read_to_end(&mut body)
        .ok()?;
    if body.len() > MAX_ERROR_RESPONSE_BYTES {
        return None;
    }
    serde_json::from_slice::<Value>(&body)
        .ok()?
        .get("error")?
        .as_str()
        .map(str::to_string)
}

fn safe_error_detail(detail: &str, credential: Option<&str>) -> String {
    let mut safe = credential
        .filter(|credential| !credential.is_empty())
        .map_or_else(
            || detail.to_string(),
            |credential| detail.replace(credential, "[REDACTED]"),
        );
    safe.retain(|character| !character.is_control() || character == ' ');
    if safe.len() > MAX_SHORT_TEXT_BYTES {
        let mut end = MAX_SHORT_TEXT_BYTES;
        while !safe.is_char_boundary(end) {
            end -= 1;
        }
        safe.truncate(end);
        safe.push_str("...");
    }
    safe
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
    value[key]
        .as_str()
        .map(str::to_string)
        .ok_or_else(|| format!("Powder claim response is missing {key}"))
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
    fn evidence_client_round_trips_bounded_paths_payloads_and_attribution() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let server = std::thread::spawn(move || {
            for expected_path in [
                "/api/v1/cards/card%2Fone",
                "/api/v1/runs/run%2Fone",
                "/api/v1/cards/card%2Fone/work-log",
                "/api/v1/cards/card%2Fone/complete",
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
                    assert_eq!(request_body["run_id"], "run/one");
                    assert_eq!(request_body["body"], "focused tests are passing");
                    json!({
                        "card_id": "card/one",
                        "agent": "crew-87ea2ca1",
                        "model": "gpt-5",
                        "reasoning": "high",
                        "harness": "codex",
                        "run_id": "run/one",
                        "body": "focused tests are passing",
                        "created_at": 15
                    })
                } else if expected_path.ends_with("complete") {
                    let request_body: Value =
                        serde_json::from_str(request.split_once("\r\n\r\n").unwrap().1).unwrap();
                    assert_eq!(request_body["proof"], "commit abc; tests passed");
                    assert_eq!(request_body["criterion_proofs"][0]["criterion"], 0);
                    assert_eq!(
                        request_body["criterion_proofs"][0]["url"],
                        "https://example.test/proof"
                    );
                    json!({
                        "id": "card/one",
                        "status": "done",
                        "claim": null,
                        "updated_at": 20,
                        "criteria": [{
                            "text": "tests pass",
                            "proof_links": [{
                                "url": "https://example.test/proof",
                                "actor": "t-hub",
                                "created_at": 20
                            }]
                        }]
                    })
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
        assert_eq!(card.claim.unwrap().run_id, "run/one");
        assert_eq!(card.runs_total, 2);
        assert_eq!(card.work_log_total, 3);
        assert!(card.truncated);

        let run = client.run_evidence("run/one").unwrap();
        assert_eq!(run.run.run_id, "run/one");
        assert_eq!(run.run.proof.as_deref(), Some("commit abc; tests passed"));
        assert_eq!(run.activities_total, 2);
        assert!(run.truncated);

        let attribution = WorkLogAttribution {
            agent: "crew-87ea2ca1".into(),
            model: Some("gpt-5".into()),
            reasoning: Some("high".into()),
            harness: Some("codex".into()),
            run_id: "run/one".into(),
        };
        let entry = client
            .append_work_log("card/one", &attribution, "focused tests are passing")
            .unwrap();
        assert_eq!(entry.agent, "crew-87ea2ca1");
        assert_eq!(entry.run_id.as_deref(), Some("run/one"));

        let receipt = client
            .complete_with_proof(
                "card/one",
                &CompletionProof {
                    proof: "commit abc; tests passed".into(),
                    criterion_proofs: vec![CriterionProof {
                        criterion: 0,
                        url: "https://example.test/proof".into(),
                    }],
                },
            )
            .unwrap();
        assert_eq!(receipt.status, "done");
        assert_eq!(receipt.criteria[0].proof_links.len(), 1);
        server.join().unwrap();
    }

    #[test]
    fn evidence_client_rejects_oversized_inputs_before_network_io() {
        let client = Client::new(ProfileConfig {
            base_url: "http://127.0.0.1:9".into(),
            agent_name: "t-hub".into(),
            api_key: Some("test-key".into()),
            api_key_env: None,
            api_key_command: None,
        })
        .unwrap();
        let attribution = WorkLogAttribution {
            agent: "crew".into(),
            model: None,
            reasoning: None,
            harness: Some("codex".into()),
            run_id: "run-1".into(),
        };
        let error = client
            .append_work_log(
                "card-1",
                &attribution,
                &"x".repeat(MAX_WORK_LOG_BODY_BYTES + 1),
            )
            .unwrap_err();
        assert_eq!(error.kind, PowderErrorKind::InvalidResponse);
        assert!(error.message.contains("16384-byte limit"));

        let error = client
            .complete_with_proof(
                "card-1",
                &CompletionProof {
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
        assert!(error.message.contains("proof count exceeds 100"));
        assert!(client
            .card_evidence(" ")
            .unwrap_err()
            .message
            .contains("must not be empty"));
    }

    #[test]
    fn evidence_client_fails_closed_on_malformed_unbounded_and_server_responses() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let server = std::thread::spawn(move || {
            for expected_path in [
                "/api/v1/cards/card-1/work-log",
                "/api/v1/cards/card-1",
                "/api/v1/runs/run-1",
                "/api/v1/cards/card-1/complete",
                "/api/v1/cards/oversized",
            ] {
                let (mut stream, _) = listener.accept().unwrap();
                let request = read_http_request(&mut stream);
                assert!(request.lines().next().unwrap().contains(expected_path));
                if expected_path.ends_with("work-log") {
                    write_json_response(
                        &mut stream,
                        "200 OK",
                        &json!({
                            "card_id": "card-1",
                            "agent": "different-crew",
                            "run_id": "run-1",
                            "body": "entry",
                            "created_at": 1
                        })
                        .to_string(),
                    );
                } else if expected_path == "/api/v1/cards/card-1" {
                    let entries = (0..=MAX_EVIDENCE_ITEMS)
                        .map(|index| {
                            json!({
                                "card_id": "card-1",
                                "agent": "crew",
                                "run_id": "run-1",
                                "body": format!("entry-{index}"),
                                "created_at": index
                            })
                        })
                        .collect::<Vec<_>>();
                    write_json_response(
                        &mut stream,
                        "200 OK",
                        &json!({
                            "card": {
                                "id": "card-1",
                                "title": "card",
                                "status": "running"
                            },
                            "work_log": entries,
                            "work_log_total": entries.len()
                        })
                        .to_string(),
                    );
                } else if expected_path.contains("/runs/") {
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
        let attribution = WorkLogAttribution {
            agent: "crew".into(),
            model: None,
            reasoning: None,
            harness: Some("codex".into()),
            run_id: "run-1".into(),
        };
        assert!(client
            .append_work_log("card-1", &attribution, "entry")
            .unwrap_err()
            .message
            .contains("attribution does not match"));
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
            .complete_with_proof(
                "card-1",
                &CompletionProof {
                    proof: "proof".into(),
                    criterion_proofs: Vec::new(),
                },
            )
            .unwrap_err();
        assert_eq!(error.kind, PowderErrorKind::Upstream);
        assert!(error.message.contains("[REDACTED]"));
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
    fn completion_receipt_requires_matching_done_card_and_criterion_evidence() {
        let wrong_card = parse_completion_receipt(
            json!({"id": "card-2", "status": "done", "updated_at": 1}),
            "card-1",
        )
        .unwrap_err();
        assert!(wrong_card.message.contains("different card id"));

        let not_done = parse_completion_receipt(
            json!({"id": "card-1", "status": "running", "updated_at": 1}),
            "card-1",
        )
        .unwrap_err();
        assert!(not_done.message.contains("did not confirm done"));

        let active_claim = parse_completion_receipt(
            json!({
                "id": "card-1",
                "status": "done",
                "updated_at": 1,
                "claim": {"run_id": "run-1"}
            }),
            "card-1",
        )
        .unwrap_err();
        assert!(active_claim.message.contains("retained an active claim"));
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
            base_url: "https://powder.example.test/".into(),
            agent_name: "t-hub".into(),
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
        assert!(!client.external_board_url().contains("repo="));
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
