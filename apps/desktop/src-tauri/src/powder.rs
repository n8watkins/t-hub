//! Protected Powder connection profiles and the narrow API client T-Hub uses.
//!
//! Captain state stores only a profile name and Powder repository. Endpoint and
//! credential material lives in `~/.t-hub/powder-profiles.json` or process env.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::bounded_exec;

const HTTP_TIMEOUT: Duration = Duration::from_secs(12);
const KEY_COMMAND_TIMEOUT: Duration = Duration::from_secs(10);

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
        match response {
            Ok(response) => response.into_json().map_err(|error| PowderError {
                kind: PowderErrorKind::InvalidResponse,
                message: format!("Powder returned invalid JSON: {error}"),
            }),
            Err(error) => Err(typed_response_error(error)),
        }
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
    if let Some(authority) = base_url.strip_prefix("https://") {
        if authority.is_empty() {
            return Err("Powder baseUrl must include a host".into());
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
        ureq::Error::Status(status, response) => {
            let detail = response
                .into_json::<Value>()
                .ok()
                .and_then(|body| body["error"].as_str().map(str::to_string))
                .unwrap_or_else(|| format!("HTTP {status}"));
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
            assert!(error.contains("must use HTTPS"), "{base_url}: {error}");
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
