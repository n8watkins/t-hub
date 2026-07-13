//! Protected Powder connection profiles and the narrow API client T-Hub uses.
//!
//! Captain state stores only a profile name and Powder repository. Endpoint and
//! credential material lives in `~/.t-hub/powder-profiles.json` or process env.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use serde::Deserialize;
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
        if !(base_url.starts_with("http://") || base_url.starts_with("https://")) {
            return Err("Powder baseUrl must start with http:// or https://".into());
        }
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

    pub fn get_card(&self, card_id: &str) -> Result<Value, String> {
        self.request(
            "GET",
            &format!("/api/v1/cards/{}?detail=detailed", encode_path(card_id)),
            None,
        )
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

    fn request(&self, method: &str, path: &str, body: Option<Value>) -> Result<Value, String> {
        let url = format!("{}{path}", self.base_url);
        let mut request = match method {
            "GET" => self.agent.get(&url),
            "POST" => self.agent.post(&url),
            _ => return Err(format!("unsupported Powder HTTP method '{method}'")),
        };
        if let Some(key) = self.api_key.as_deref() {
            request = request.set("Authorization", &format!("Bearer {key}"));
        }
        let response = match body {
            Some(body) => request.send_json(body),
            None => request.call(),
        };
        match response {
            Ok(response) => response
                .into_json()
                .map_err(|error| format!("Powder returned invalid JSON: {error}")),
            Err(ureq::Error::Status(status, response)) => {
                let detail = response
                    .into_json::<Value>()
                    .ok()
                    .and_then(|body| body["error"].as_str().map(str::to_string))
                    .unwrap_or_else(|| format!("HTTP {status}"));
                Err(format!("Powder HTTP {status}: {detail}"))
            }
            Err(ureq::Error::Transport(error)) => Err(format!("Powder is unreachable: {error}")),
        }
    }
}

fn resolve_key_command(command: &str) -> Result<String, String> {
    #[cfg(windows)]
    let child = {
        let mut child = Command::new("powershell.exe");
        child.args(["-NoProfile", "-NonInteractive", "-Command", command]);
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

fn required_string(value: &Value, key: &str) -> Result<String, String> {
    value[key]
        .as_str()
        .map(str::to_string)
        .ok_or_else(|| format!("Powder claim response is missing {key}"))
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
    fn path_segments_are_percent_encoded() {
        assert_eq!(encode_path("repo/card 1"), "repo%2Fcard%201");
    }

    #[test]
    fn http_client_round_trips_card_and_claim_lifecycle() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let server = std::thread::spawn(move || {
            for expected_path in [
                "/api/v1/cards/card-1?detail=detailed",
                "/api/v1/cards/card-1/claim",
                "/api/v1/cards/card-1/heartbeat",
                "/api/v1/cards/card-1/renew",
                "/api/v1/cards/card-1/release",
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
                let body = if expected_path.contains("?detail=") {
                    json!({ "id": "card-1", "repo": "repo-1" })
                } else {
                    json!({
                        "card_id": "card-1",
                        "run_id": "run-1",
                        "agent": "t-hub",
                        "expires_at": 1234,
                    })
                }
                .to_string();
                write!(
                    stream,
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
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
        assert_eq!(client.get_card("card-1").unwrap()["repo"], "repo-1");
        let claim = client.claim("card-1", 3600).unwrap();
        assert_eq!(claim.agent, "t-hub");
        assert_eq!(client.heartbeat(&claim).unwrap().run_id, "run-1");
        assert_eq!(client.renew(&claim, 3600).unwrap().expires_at, 1234);
        assert_eq!(client.release(&claim).unwrap().card_id, "card-1");
        server.join().unwrap();
    }
}
