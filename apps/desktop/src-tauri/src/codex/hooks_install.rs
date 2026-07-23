//! Consent-gated lifecycle management for the user-level Codex hook producer.
//!
//! Codex 0.145.0 discovers user hooks in `$CODEX_HOME/hooks.json`, while hook
//! enablement and trust state live in `$CODEX_HOME/config.toml`.
//! This module owns only marker-tagged T-Hub entries in `hooks.json`.
//! It reports trust and policy state but never fabricates Codex trust approval.

use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::{anyhow, bail, Context, Result};
use fs2::FileExt;
use serde::Serialize;
use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};

pub const MANAGED_MARKER: &str = "__t_hub_codex_managed__";
pub const OBSERVED_EVENTS: [&str; 5] = [
    "SessionStart",
    "UserPromptSubmit",
    "PermissionRequest",
    "Stop",
    "SessionEnd",
];

const LOCK_TIMEOUT: Duration = Duration::from_secs(2);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum ProducerStatus {
    NotInstalled,
    NeedsReview,
    Healthy,
    Disabled,
    Modified,
    Drifted,
    BlockedByManagedPolicy,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CapabilityReport {
    pub session_start: &'static str,
    pub user_prompt: &'static str,
    pub permission: &'static str,
    pub completion: &'static str,
    pub session_end: &'static str,
    pub question: &'static str,
    pub failure: &'static str,
}

impl Default for CapabilityReport {
    fn default() -> Self {
        Self {
            session_start: "native_hook",
            user_prompt: "native_hook",
            permission: "native_hook",
            completion: "native_hook",
            session_end: "native_hook",
            question: "structured_app_server_or_degraded",
            failure: "structured_app_server_or_degraded",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProducerHealth {
    pub status: ProducerStatus,
    pub hooks_path: String,
    pub config_path: String,
    pub requirements_path: String,
    pub managed_events: Vec<String>,
    pub missing_events: Vec<String>,
    pub executable_path: String,
    pub executable_ok: bool,
    pub inline_user_hooks_present: bool,
    pub project_hooks_present: bool,
    pub plugin_config_present: bool,
    pub managed_hooks_present: bool,
    pub managed_only_policy: bool,
    pub capabilities: CapabilityReport,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct InstallReport {
    pub hooks_path: String,
    pub changed: bool,
    pub backed_up: bool,
    pub managed_events: usize,
    pub health: ProducerHealth,
}

pub fn codex_home() -> Result<PathBuf> {
    if let Some(path) = std::env::var_os("CODEX_HOME") {
        let path = PathBuf::from(path);
        if !path.as_os_str().is_empty() {
            return Ok(path);
        }
    }
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .map(|home| home.join(".codex"))
        .ok_or_else(|| anyhow!("could not resolve Codex home"))
}

#[cfg(unix)]
pub fn requirements_path() -> PathBuf {
    PathBuf::from("/etc/codex/requirements.toml")
}

#[cfg(windows)]
pub fn requirements_path() -> PathBuf {
    std::env::var_os("ProgramData")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(r"C:\ProgramData"))
        .join("OpenAI")
        .join("Codex")
        .join("requirements.toml")
}

struct ConfigLock {
    file: File,
}

impl ConfigLock {
    fn acquire(path: &Path) -> Result<Self> {
        let parent = path
            .parent()
            .ok_or_else(|| anyhow!("Codex hooks path has no parent"))?;
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating Codex config directory {}", parent.display()))?;
        let lock_path = parent.join(".t-hub-hooks.lock");
        let file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(&lock_path)
            .with_context(|| format!("opening Codex hook lock {}", lock_path.display()))?;
        let started = Instant::now();
        loop {
            match file.try_lock_exclusive() {
                Ok(()) => return Ok(Self { file }),
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    if started.elapsed() >= LOCK_TIMEOUT {
                        bail!(
                            "timed out waiting for Codex hook lock {}",
                            lock_path.display()
                        );
                    }
                    std::thread::sleep(Duration::from_millis(20));
                }
                Err(error) => {
                    return Err(error).with_context(|| format!("locking {}", lock_path.display()));
                }
            }
        }
    }
}

impl Drop for ConfigLock {
    fn drop(&mut self) {
        let _ = self.file.unlock();
    }
}

pub fn install(
    codex_home: &Path,
    requirements_path: &Path,
    agent_bin: &Path,
    consent: bool,
) -> Result<InstallReport> {
    if !consent {
        bail!("refusing to modify Codex hooks without explicit consent");
    }
    validate_agent_bin(agent_bin)?;
    let hooks_path = codex_home.join("hooks.json");
    let _lock = ConfigLock::acquire(&hooks_path)?;
    let existing = read_hooks(&hooks_path)?;
    // Parse every config surface before the first mutation.
    // A malformed user or policy file must never be partially "repaired".
    let _ = health_at(codex_home, requirements_path, agent_bin)?;
    let merged = merge_managed(&existing, agent_bin)?;
    let changed = merged != existing;
    let backed_up = changed && backup_once(&hooks_path)?;
    if changed {
        write_json_atomic(&hooks_path, &merged)?;
    }
    let health = health_at(codex_home, requirements_path, agent_bin)?;
    Ok(InstallReport {
        hooks_path: hooks_path.display().to_string(),
        changed,
        backed_up,
        managed_events: health.managed_events.len(),
        health,
    })
}

pub fn repair(
    codex_home: &Path,
    requirements_path: &Path,
    agent_bin: &Path,
    consent: bool,
) -> Result<InstallReport> {
    install(codex_home, requirements_path, agent_bin, consent)
}

pub fn uninstall(
    codex_home: &Path,
    requirements_path: &Path,
    agent_bin: &Path,
) -> Result<InstallReport> {
    let hooks_path = codex_home.join("hooks.json");
    let _lock = ConfigLock::acquire(&hooks_path)?;
    let existing = read_hooks(&hooks_path)?;
    let _ = health_at(codex_home, requirements_path, agent_bin)?;
    let cleaned = remove_managed(&existing)?;
    let changed = cleaned != existing;
    let backed_up = changed && backup_once(&hooks_path)?;
    if changed {
        write_json_atomic(&hooks_path, &cleaned)?;
    }
    let health = health_at(codex_home, requirements_path, agent_bin)?;
    Ok(InstallReport {
        hooks_path: hooks_path.display().to_string(),
        changed,
        backed_up,
        managed_events: health.managed_events.len(),
        health,
    })
}

pub fn health_at(
    codex_home: &Path,
    requirements_path: &Path,
    agent_bin: &Path,
) -> Result<ProducerHealth> {
    health_at_with_project(codex_home, requirements_path, agent_bin, None)
}

pub fn health_at_with_project(
    codex_home: &Path,
    requirements_path: &Path,
    agent_bin: &Path,
    project_root: Option<&Path>,
) -> Result<ProducerHealth> {
    let hooks_path = codex_home.join("hooks.json");
    let config_path = codex_home.join("config.toml");
    let hooks = read_hooks(&hooks_path)?;
    let config = read_toml_if_present(&config_path)?;
    let requirements = read_toml_if_present(requirements_path)?;
    let system_config = read_toml_if_present(&requirements_path.with_file_name("config.toml"))?;
    let legacy_managed_config = read_toml_if_present(&codex_home.join("managed_config.toml"))?;
    let managed_only_policy = requirements
        .as_ref()
        .and_then(|value| value.get("allow_managed_hooks_only"))
        .and_then(toml::Value::as_bool)
        .unwrap_or(false);
    let inline_user_hooks_present = config
        .as_ref()
        .and_then(|value| value.get("hooks"))
        .and_then(toml::Value::as_table)
        .is_some_and(|hooks| {
            OBSERVED_EVENTS
                .iter()
                .any(|event| hooks.contains_key(*event))
        });
    let plugin_config_present = config
        .as_ref()
        .and_then(|value| value.get("plugins"))
        .and_then(toml::Value::as_table)
        .is_some_and(|plugins| !plugins.is_empty());
    let project_hooks_present = if let Some(project_root) = project_root {
        read_hooks(&project_root.join(".codex").join("hooks.json"))?
            .get("hooks")
            .and_then(Value::as_object)
            .is_some_and(|hooks| !hooks.is_empty())
    } else {
        false
    };
    let managed_hooks_present = [&requirements, &system_config, &legacy_managed_config]
        .into_iter()
        .flatten()
        .any(toml_has_hook_events);

    let mut managed_events = Vec::new();
    let mut missing_events = Vec::new();
    let mut drifted = false;
    let mut disabled = false;
    let mut modified = false;
    let state = config
        .as_ref()
        .and_then(|value| value.get("hooks"))
        .and_then(|hooks| hooks.get("state"))
        .and_then(toml::Value::as_table);

    for event in OBSERVED_EVENTS {
        match find_managed_handler(&hooks, event)? {
            Some((group_index, handler_index, command)) => {
                managed_events.push(event.to_string());
                let key = format!(
                    "{}:{}:{group_index}:{handler_index}",
                    hooks_path.display(),
                    event_key(event)
                );
                let hook_state = state
                    .and_then(|state| state.get(&key))
                    .and_then(toml::Value::as_table);
                disabled |= hook_state
                    .and_then(|state| state.get("enabled"))
                    .and_then(toml::Value::as_bool)
                    == Some(false);
                if let Some(trusted_hash) = hook_state
                    .and_then(|state| state.get("trusted_hash"))
                    .and_then(toml::Value::as_str)
                {
                    modified |= trusted_hash != expected_trust_hash(event, &command);
                }
                drifted |= command != managed_command(agent_bin, event);
            }
            None => missing_events.push(event.to_string()),
        }
    }

    let executable_ok = executable_ok(agent_bin);
    let has_trust_for_all = managed_events.iter().all(|event| {
        let Some((group_index, handler_index, command)) =
            find_managed_handler(&hooks, event).ok().flatten()
        else {
            return false;
        };
        let key = format!(
            "{}:{}:{group_index}:{handler_index}",
            hooks_path.display(),
            event_key(event)
        );
        state
            .and_then(|state| state.get(&key))
            .and_then(toml::Value::as_table)
            .and_then(|state| state.get("trusted_hash"))
            .and_then(toml::Value::as_str)
            .is_some_and(|hash| hash == expected_trust_hash(event, &command))
    });
    let status = if managed_events.is_empty() {
        ProducerStatus::NotInstalled
    } else if managed_only_policy {
        ProducerStatus::BlockedByManagedPolicy
    } else if drifted || !missing_events.is_empty() || !executable_ok {
        ProducerStatus::Drifted
    } else if disabled {
        ProducerStatus::Disabled
    } else if modified {
        ProducerStatus::Modified
    } else if !has_trust_for_all {
        ProducerStatus::NeedsReview
    } else {
        ProducerStatus::Healthy
    };

    Ok(ProducerHealth {
        status,
        hooks_path: hooks_path.display().to_string(),
        config_path: config_path.display().to_string(),
        requirements_path: requirements_path.display().to_string(),
        managed_events,
        missing_events,
        executable_path: agent_bin.display().to_string(),
        executable_ok,
        inline_user_hooks_present,
        project_hooks_present,
        plugin_config_present,
        managed_hooks_present,
        managed_only_policy,
        capabilities: CapabilityReport::default(),
    })
}

fn toml_has_hook_events(value: &toml::Value) -> bool {
    value
        .get("hooks")
        .and_then(toml::Value::as_table)
        .is_some_and(|hooks| {
            OBSERVED_EVENTS
                .iter()
                .any(|event| hooks.contains_key(*event))
        })
}

fn validate_agent_bin(path: &Path) -> Result<()> {
    if !path.is_absolute() {
        bail!("Codex hook executable must be an absolute path");
    }
    if !executable_ok(path) {
        bail!("Codex hook executable is missing or not executable");
    }
    Ok(())
}

fn executable_ok(path: &Path) -> bool {
    let Ok(metadata) = std::fs::metadata(path) else {
        return false;
    };
    if !metadata.is_file() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        metadata.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    {
        true
    }
}

fn read_hooks(path: &Path) -> Result<Value> {
    refuse_unsafe_target(path)?;
    match std::fs::read_to_string(path) {
        Ok(contents) if contents.trim().is_empty() => Ok(json!({})),
        Ok(contents) => {
            let value: Value = serde_json::from_str(&contents)
                .with_context(|| format!("parsing {} (refusing to overwrite)", path.display()))?;
            if !value.is_object() {
                bail!("{} must contain a JSON object", path.display());
            }
            Ok(value)
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(json!({})),
        Err(error) => Err(error).with_context(|| format!("reading {}", path.display())),
    }
}

fn read_toml_if_present(path: &Path) -> Result<Option<toml::Value>> {
    refuse_unsafe_target(path)?;
    match std::fs::read_to_string(path) {
        Ok(contents) if contents.trim().is_empty() => {
            Ok(Some(toml::Value::Table(toml::map::Map::new())))
        }
        Ok(contents) => toml::from_str(&contents)
            .map(Some)
            .with_context(|| format!("parsing {} (refusing config mutation)", path.display())),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error).with_context(|| format!("reading {}", path.display())),
    }
}

fn refuse_unsafe_target(path: &Path) -> Result<()> {
    let metadata = match std::fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error).with_context(|| format!("inspecting {}", path.display())),
    };
    if metadata.file_type().is_symlink() {
        bail!("refusing symlinked Codex config path {}", path.display());
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if metadata.nlink() > 1 {
            bail!("refusing hard-linked Codex config path {}", path.display());
        }
    }
    Ok(())
}

fn merge_managed(existing: &Value, agent_bin: &Path) -> Result<Value> {
    let mut merged = remove_managed(existing)?;
    let root = merged
        .as_object_mut()
        .ok_or_else(|| anyhow!("hooks root must be an object"))?;
    let hooks = root
        .entry("hooks")
        .or_insert_with(|| Value::Object(Map::new()))
        .as_object_mut()
        .ok_or_else(|| anyhow!("hooks field must be an object"))?;
    for event in OBSERVED_EVENTS {
        let groups = hooks
            .entry(event)
            .or_insert_with(|| Value::Array(Vec::new()))
            .as_array_mut()
            .ok_or_else(|| anyhow!("{event} hooks must be an array"))?;
        groups.push(json!({
            "hooks": [{
                "type": "command",
                "command": managed_command(agent_bin, event),
                "timeout": if event == "SessionEnd" { 3 } else { 10 },
            }]
        }));
    }
    Ok(merged)
}

fn remove_managed(existing: &Value) -> Result<Value> {
    let mut cleaned = existing.clone();
    let Some(root) = cleaned.as_object_mut() else {
        bail!("hooks root must be an object");
    };
    let Some(hooks_value) = root.get_mut("hooks") else {
        return Ok(cleaned);
    };
    let hooks = hooks_value
        .as_object_mut()
        .ok_or_else(|| anyhow!("hooks field must be an object"))?;
    for event in OBSERVED_EVENTS {
        let Some(groups_value) = hooks.get_mut(event) else {
            continue;
        };
        let groups = groups_value
            .as_array_mut()
            .ok_or_else(|| anyhow!("{event} hooks must be an array"))?;
        groups.retain_mut(|group| {
            let Some(handlers) = group.get_mut("hooks").and_then(Value::as_array_mut) else {
                return true;
            };
            handlers.retain(|handler| !handler_is_managed(handler));
            !handlers.is_empty()
        });
        if groups.is_empty() {
            hooks.remove(event);
        }
    }
    if hooks.is_empty() {
        root.remove("hooks");
    }
    Ok(cleaned)
}

fn find_managed_handler(value: &Value, event: &str) -> Result<Option<(usize, usize, String)>> {
    let Some(groups_value) = value.get("hooks").and_then(|hooks| hooks.get(event)) else {
        return Ok(None);
    };
    let groups = groups_value
        .as_array()
        .ok_or_else(|| anyhow!("{event} hooks must be an array"))?;
    for (group_index, group) in groups.iter().enumerate() {
        let Some(handlers) = group.get("hooks").and_then(Value::as_array) else {
            continue;
        };
        for (handler_index, handler) in handlers.iter().enumerate() {
            if let Some(command) = handler.get("command").and_then(Value::as_str) {
                if command
                    .split_whitespace()
                    .any(|word| word == MANAGED_MARKER)
                {
                    return Ok(Some((group_index, handler_index, command.to_string())));
                }
            }
        }
    }
    Ok(None)
}

fn handler_is_managed(handler: &Value) -> bool {
    handler
        .get("command")
        .and_then(Value::as_str)
        .is_some_and(|command| {
            command
                .split_whitespace()
                .any(|word| word == MANAGED_MARKER)
        })
}

fn managed_command(agent_bin: &Path, event: &str) -> String {
    format!(
        "{} --codex-hook {event} # {MANAGED_MARKER}",
        shell_quote(&agent_bin.display().to_string())
    )
}

fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

fn event_key(event: &str) -> &'static str {
    match event {
        "SessionStart" => "session_start",
        "UserPromptSubmit" => "user_prompt_submit",
        "PermissionRequest" => "permission_request",
        "Stop" => "stop",
        "SessionEnd" => "session_end",
        _ => unreachable!("managed event set is closed"),
    }
}

fn expected_trust_hash(event: &str, command: &str) -> String {
    let identity = json!({
        "event_name": event_key(event),
        "hooks": [{
            "type": "command",
            "command": command,
            "timeout": if event == "SessionEnd" { 3 } else { 10 },
            "async": false,
        }]
    });
    let bytes = serde_json::to_vec(&canonical_json(identity)).unwrap_or_default();
    format!("sha256:{:x}", Sha256::digest(bytes))
}

fn canonical_json(value: Value) -> Value {
    match value {
        Value::Object(map) => {
            let mut entries: Vec<_> = map.into_iter().collect();
            entries.sort_by(|left, right| left.0.cmp(&right.0));
            Value::Object(
                entries
                    .into_iter()
                    .map(|(key, value)| (key, canonical_json(value)))
                    .collect(),
            )
        }
        Value::Array(values) => Value::Array(values.into_iter().map(canonical_json).collect()),
        other => other,
    }
}

fn backup_once(path: &Path) -> Result<bool> {
    if !path.exists() {
        return Ok(false);
    }
    let backup = path.with_extension("json.t-hub-bak");
    if backup.exists() {
        return Ok(true);
    }
    let bytes = std::fs::read(path).with_context(|| format!("reading {}", path.display()))?;
    write_bytes_atomic(&backup, &bytes)?;
    Ok(true)
}

fn write_json_atomic(path: &Path, value: &Value) -> Result<()> {
    let mut bytes = serde_json::to_vec_pretty(value).context("serializing Codex hooks")?;
    bytes.push(b'\n');
    write_bytes_atomic(path, &bytes)
}

fn write_bytes_atomic(path: &Path, bytes: &[u8]) -> Result<()> {
    refuse_unsafe_target(path)?;
    let parent = path
        .parent()
        .ok_or_else(|| anyhow!("Codex config path has no parent"))?;
    std::fs::create_dir_all(parent).with_context(|| format!("creating {}", parent.display()))?;
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let temp = parent.join(format!(".t-hub-hooks-{}-{nonce}.tmp", std::process::id()));
    let mut options = OpenOptions::new();
    options.create_new(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options
        .open(&temp)
        .with_context(|| format!("creating {}", temp.display()))?;
    let result = (|| -> Result<()> {
        file.write_all(bytes)
            .with_context(|| format!("writing {}", temp.display()))?;
        file.sync_all()
            .with_context(|| format!("syncing {}", temp.display()))?;
        std::fs::rename(&temp, path)
            .with_context(|| format!("renaming {} to {}", temp.display(), path.display()))?;
        File::open(parent)
            .and_then(|directory| directory.sync_all())
            .with_context(|| format!("syncing {}", parent.display()))?;
        Ok(())
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&temp);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    fn executable(dir: &Path) -> std::path::PathBuf {
        let path = dir.join("bin").join("t-hub-agent");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, b"agent").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700)).unwrap();
        }
        path
    }

    #[test]
    fn install_is_idempotent_and_uninstall_preserves_user_hooks() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join("codex");
        std::fs::create_dir_all(&home).unwrap();
        let agent = executable(temp.path());
        let requirements = temp.path().join("requirements.toml");
        std::fs::write(
            home.join("hooks.json"),
            r#"{"description":"keep","hooks":{"Stop":[{"hooks":[{"type":"command","command":"user-hook"}]}]}}"#,
        )
        .unwrap();

        let first = install(&home, &requirements, &agent, true).unwrap();
        assert!(first.changed);
        assert!(first.backed_up);
        assert_eq!(first.health.status, ProducerStatus::NeedsReview);
        let installed = std::fs::read(home.join("hooks.json")).unwrap();
        let second = install(&home, &requirements, &agent, true).unwrap();
        assert!(!second.changed);
        assert_eq!(installed, std::fs::read(home.join("hooks.json")).unwrap());

        let mut value: Value = serde_json::from_slice(&installed).unwrap();
        value["hooks"]["SessionStart"][0]["hooks"]
            .as_array_mut()
            .unwrap()
            .push(json!({"type": "command", "command": "user-added-sibling"}));
        std::fs::write(
            home.join("hooks.json"),
            serde_json::to_vec_pretty(&value).unwrap(),
        )
        .unwrap();
        let removed = uninstall(&home, &requirements, &agent).unwrap();
        assert!(removed.changed);
        assert_eq!(removed.health.status, ProducerStatus::NotInstalled);
        let value: Value =
            serde_json::from_slice(&std::fs::read(home.join("hooks.json")).unwrap()).unwrap();
        assert_eq!(value["description"], "keep");
        assert_eq!(value["hooks"]["Stop"].as_array().unwrap().len(), 1);
        assert_eq!(
            value["hooks"]["Stop"][0]["hooks"][0]["command"],
            "user-hook"
        );
        assert_eq!(
            value["hooks"]["SessionStart"][0]["hooks"][0]["command"],
            "user-added-sibling"
        );
    }

    #[test]
    fn malformed_files_and_unsafe_paths_are_refused() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join("codex");
        std::fs::create_dir_all(&home).unwrap();
        let agent = executable(temp.path());
        let requirements = temp.path().join("requirements.toml");
        std::fs::write(home.join("hooks.json"), b"{broken").unwrap();
        assert!(install(&home, &requirements, &agent, true).is_err());
        assert_eq!(std::fs::read(home.join("hooks.json")).unwrap(), b"{broken");

        std::fs::remove_file(home.join("hooks.json")).unwrap();
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink("/tmp/elsewhere", home.join("hooks.json")).unwrap();
            assert!(install(&home, &requirements, &agent, true).is_err());
        }
    }

    #[test]
    fn health_reports_policy_inline_hooks_disabled_and_drift() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join("codex");
        std::fs::create_dir_all(&home).unwrap();
        let agent = executable(temp.path());
        let requirements = temp.path().join("requirements.toml");
        install(&home, &requirements, &agent, true).unwrap();
        std::fs::write(&requirements, "allow_managed_hooks_only = true\n").unwrap();
        std::fs::write(
            home.join("config.toml"),
            "[hooks]\nSessionStart = []\n[plugins.example]\nenabled = true\n",
        )
        .unwrap();
        let project = temp.path().join("project");
        std::fs::create_dir_all(project.join(".codex")).unwrap();
        std::fs::write(
            project.join(".codex").join("hooks.json"),
            r#"{"hooks":{"Stop":[{"hooks":[{"type":"command","command":"project-hook"}]}]}}"#,
        )
        .unwrap();
        let health = health_at_with_project(&home, &requirements, &agent, Some(&project)).unwrap();
        assert_eq!(health.status, ProducerStatus::BlockedByManagedPolicy);
        assert!(health.inline_user_hooks_present);
        assert!(health.project_hooks_present);
        assert!(health.plugin_config_present);
        assert!(!health.managed_hooks_present);
        assert_eq!(
            health.capabilities.failure,
            "structured_app_server_or_degraded"
        );
    }

    #[test]
    fn repair_converges_stale_executable_without_duplicate_groups() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join("codex");
        std::fs::create_dir_all(&home).unwrap();
        let first_agent = executable(temp.path());
        let requirements = temp.path().join("requirements.toml");
        install(&home, &requirements, &first_agent, true).unwrap();
        let second_root = temp.path().join("replacement");
        let second_agent = executable(&second_root);
        let report = repair(&home, &requirements, &second_agent, true).unwrap();
        assert!(report.changed);
        assert_eq!(report.health.status, ProducerStatus::NeedsReview);
        let hooks = read_hooks(&home.join("hooks.json")).unwrap();
        for event in OBSERVED_EVENTS {
            let groups = hooks["hooks"][event].as_array().unwrap();
            assert_eq!(groups.len(), 1);
            assert!(groups[0]["hooks"][0]["command"]
                .as_str()
                .unwrap()
                .contains(&second_agent.display().to_string()));
        }
    }

    #[test]
    fn trust_and_enablement_health_follow_codex_state_without_writing_it() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join("codex");
        std::fs::create_dir_all(&home).unwrap();
        let agent = executable(temp.path());
        let requirements = temp.path().join("requirements.toml");
        install(&home, &requirements, &agent, true).unwrap();
        let hooks = read_hooks(&home.join("hooks.json")).unwrap();
        let mut state = toml::map::Map::new();
        for event in OBSERVED_EVENTS {
            let (group, handler, command) = find_managed_handler(&hooks, event).unwrap().unwrap();
            let key = format!(
                "{}:{}:{group}:{handler}",
                home.join("hooks.json").display(),
                event_key(event)
            );
            let mut hook_state = toml::map::Map::new();
            hook_state.insert(
                "trusted_hash".to_string(),
                toml::Value::String(expected_trust_hash(event, &command)),
            );
            state.insert(key, toml::Value::Table(hook_state));
        }
        let config = toml::Value::Table(toml::map::Map::from_iter([(
            "hooks".to_string(),
            toml::Value::Table(toml::map::Map::from_iter([(
                "state".to_string(),
                toml::Value::Table(state),
            )])),
        )]));
        std::fs::write(home.join("config.toml"), toml::to_string(&config).unwrap()).unwrap();
        assert_eq!(
            health_at(&home, &requirements, &agent).unwrap().status,
            ProducerStatus::Healthy
        );

        let mut config: toml::Value =
            toml::from_str(&std::fs::read_to_string(home.join("config.toml")).unwrap()).unwrap();
        let first_key = config["hooks"]["state"]
            .as_table()
            .unwrap()
            .keys()
            .next()
            .unwrap()
            .clone();
        config
            .get_mut("hooks")
            .and_then(|hooks| hooks.get_mut("state"))
            .and_then(toml::Value::as_table_mut)
            .and_then(|state| state.get_mut(&first_key))
            .and_then(toml::Value::as_table_mut)
            .unwrap()
            .insert("enabled".to_string(), toml::Value::Boolean(false));
        std::fs::write(home.join("config.toml"), toml::to_string(&config).unwrap()).unwrap();
        assert_eq!(
            health_at(&home, &requirements, &agent).unwrap().status,
            ProducerStatus::Disabled
        );
        let hook_state = config
            .get_mut("hooks")
            .and_then(|hooks| hooks.get_mut("state"))
            .and_then(toml::Value::as_table_mut)
            .and_then(|state| state.get_mut(&first_key))
            .and_then(toml::Value::as_table_mut)
            .unwrap();
        hook_state.insert("enabled".to_string(), toml::Value::Boolean(true));
        hook_state.insert(
            "trusted_hash".to_string(),
            toml::Value::String("sha256:modified".to_string()),
        );
        std::fs::write(home.join("config.toml"), toml::to_string(&config).unwrap()).unwrap();
        assert_eq!(
            health_at(&home, &requirements, &agent).unwrap().status,
            ProducerStatus::Modified
        );
    }

    #[test]
    fn concurrent_repairs_serialize_without_duplicate_hooks() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join("codex");
        std::fs::create_dir_all(&home).unwrap();
        let agent = executable(temp.path());
        let requirements = temp.path().join("requirements.toml");
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(5));
        let mut workers = Vec::new();
        for _ in 0..4 {
            let home = home.clone();
            let agent = agent.clone();
            let requirements = requirements.clone();
            let barrier = barrier.clone();
            workers.push(std::thread::spawn(move || {
                barrier.wait();
                repair(&home, &requirements, &agent, true).unwrap();
            }));
        }
        barrier.wait();
        for worker in workers {
            worker.join().unwrap();
        }
        let hooks = read_hooks(&home.join("hooks.json")).unwrap();
        for event in OBSERVED_EVENTS {
            assert_eq!(hooks["hooks"][event].as_array().unwrap().len(), 1);
        }
    }

    #[test]
    fn malformed_toml_refuses_before_hooks_mutation() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join("codex");
        std::fs::create_dir_all(&home).unwrap();
        let agent = executable(temp.path());
        let requirements = temp.path().join("requirements.toml");
        std::fs::write(home.join("config.toml"), "[hooks\nbroken").unwrap();
        assert!(install(&home, &requirements, &agent, true).is_err());
        assert!(!home.join("hooks.json").exists());
    }
}
