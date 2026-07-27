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
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};

pub const MANAGED_MARKER: &str = "__t_hub_codex_managed__";
pub const OBSERVED_EVENTS: [&str; 7] = [
    "SessionStart",
    "UserPromptSubmit",
    "PermissionRequest",
    "PreToolUse",
    "PostToolUse",
    "Stop",
    "SessionEnd",
];

const LOCK_TIMEOUT: Duration = Duration::from_secs(2);
const AGENT_CAPABILITY_SCHEMA: u32 = 1;
const CODEX_HOOK_CAPABILITY: &str = "codex-native-hooks-v1";
const AGENT_CAPABILITY_OUTPUT_LIMIT: usize = 16 * 1024;

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
            question: "native_hook",
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
    pub agent_capable: bool,
    pub agent_version: Option<String>,
    pub hooks_enabled: bool,
    pub inline_user_hooks_present: bool,
    pub project_hooks_present: bool,
    pub plugin_config_present: bool,
    pub managed_hooks_present: bool,
    pub managed_only_policy: bool,
    pub capabilities: CapabilityReport,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct AgentCapabilities {
    schema_version: u32,
    agent_version: String,
    capabilities: Vec<String>,
}

#[derive(Debug, Default)]
struct AgentCapabilityProbe {
    capable: bool,
    version: Option<String>,
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimePaths {
    pub codex_home: PathBuf,
    pub requirements_path: PathBuf,
    pub agent_bin: PathBuf,
    pub hooks_state_path: String,
}

pub fn runtime_paths(agent_bin: &str, packaged_agent_bin: Option<&str>) -> Result<RuntimePaths> {
    #[cfg(unix)]
    {
        let _ = packaged_agent_bin;
        let codex_home = codex_home()?;
        let hooks_state_path = codex_home.join("hooks.json").display().to_string();
        Ok(RuntimePaths {
            codex_home,
            requirements_path: requirements_path(),
            agent_bin: resolve_unix_agent_bin(agent_bin),
            hooks_state_path,
        })
    }
    #[cfg(windows)]
    {
        let _ = agent_bin;
        let packaged_agent_bin = packaged_agent_bin
            .context("verified digest-versioned WSL helper path is unavailable")?;
        validate_canonical_absolute_posix(packaged_agent_bin)?;
        let distro = wsl_distro();
        let home = wsl_home(&distro)?;
        let runtime_codex_home = format!("{home}/.codex");
        Ok(RuntimePaths {
            codex_home: wsl_posix_to_unc(&distro, &runtime_codex_home)?,
            requirements_path: wsl_posix_to_unc(&distro, "/etc/codex/requirements.toml")?,
            agent_bin: PathBuf::from(packaged_agent_bin),
            hooks_state_path: format!("{runtime_codex_home}/hooks.json"),
        })
    }
}

pub fn runtime_paths_for_uninstall(agent_bin: &str) -> Result<RuntimePaths> {
    #[cfg(unix)]
    {
        runtime_paths(agent_bin, None)
    }
    #[cfg(windows)]
    {
        let _ = agent_bin;
        let distro = wsl_distro();
        let home = wsl_home(&distro)?;
        let runtime_codex_home = format!("{home}/.codex");
        Ok(RuntimePaths {
            codex_home: wsl_posix_to_unc(&distro, &runtime_codex_home)?,
            requirements_path: wsl_posix_to_unc(&distro, "/etc/codex/requirements.toml")?,
            agent_bin: PathBuf::from("/.t-hub-uninstall/t-hub-agent"),
            hooks_state_path: format!("{runtime_codex_home}/hooks.json"),
        })
    }
}

#[cfg(unix)]
fn resolve_unix_agent_bin(agent_bin: &str) -> PathBuf {
    resolve_unix_agent_bin_from(
        agent_bin,
        std::env::var_os("PATH").as_deref(),
        std::env::current_dir().ok().as_deref(),
    )
}

#[cfg(unix)]
fn resolve_unix_agent_bin_from(
    agent_bin: &str,
    search_path: Option<&std::ffi::OsStr>,
    current_dir: Option<&Path>,
) -> PathBuf {
    let supplied = PathBuf::from(agent_bin);
    if supplied.is_absolute() {
        return supplied;
    }

    if supplied.components().count() > 1 {
        if let Some(current_dir) = current_dir {
            let candidate = current_dir.join(&supplied);
            if executable_ok(&candidate) {
                return candidate;
            }
        }
        return supplied;
    }

    let Some(search_path) = search_path else {
        return supplied;
    };
    for directory in std::env::split_paths(search_path) {
        let directory = if directory.is_absolute() {
            directory
        } else if let Some(current_dir) = current_dir {
            current_dir.join(directory)
        } else {
            continue;
        };
        let candidate = directory.join(&supplied);
        if executable_ok(&candidate) {
            return candidate;
        }
    }
    supplied
}

#[cfg(windows)]
pub fn host_project_path(path: &str) -> Result<PathBuf> {
    host_project_path_for_distro(path, &wsl_distro())
}

#[cfg(not(windows))]
pub fn host_project_path(path: &str) -> Result<PathBuf> {
    Ok(PathBuf::from(path))
}

#[cfg(any(windows, test))]
fn host_project_path_for_distro(path: &str, distro: &str) -> Result<PathBuf> {
    wsl_posix_to_unc(distro, path)
}

#[cfg(unix)]
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
fn wsl_distro() -> String {
    std::env::var("T_HUB_DISTRO").unwrap_or_else(|_| "Ubuntu-24.04".to_string())
}

#[cfg(any(windows, test))]
fn normalize_wsl_home(output: &[u8]) -> Result<String> {
    let home = normalize_single_wsl_path_output(output)?;
    if home.len() <= 1 {
        bail!("WSL home is not an absolute POSIX path");
    }
    Ok(home)
}

#[cfg(any(windows, test))]
fn normalize_single_wsl_path_output(output: &[u8]) -> Result<String> {
    let output = std::str::from_utf8(output).context("WSL path output is not valid UTF-8")?;
    let output = output.strip_suffix('\n').unwrap_or(output);
    let output = output.strip_suffix('\r').unwrap_or(output);
    validate_canonical_absolute_posix(output)?;
    Ok(output.to_string())
}

#[cfg(any(windows, test))]
fn validate_canonical_absolute_posix(path: &str) -> Result<()> {
    if !path.starts_with('/')
        || path.contains('\\')
        || path.chars().any(char::is_control)
        || (path.len() > 1 && path.ends_with('/'))
    {
        bail!("WSL path must be a canonical absolute POSIX path");
    }
    for component in path.split('/').skip(1) {
        if component.is_empty() || component == "." || component == ".." {
            bail!("WSL path must be a canonical absolute POSIX path");
        }
    }
    Ok(())
}

#[cfg(any(windows, test))]
fn wsl_posix_to_unc(distro: &str, path: &str) -> Result<PathBuf> {
    validate_canonical_absolute_posix(path)?;
    if distro.is_empty()
        || distro == "."
        || distro == ".."
        || distro
            .chars()
            .any(|character| character.is_control() || r#"\/:*?"<>|"#.contains(character))
    {
        bail!("WSL distribution name is invalid");
    }
    let relative = path.trim_start_matches('/').replace('/', "\\");
    let suffix = if relative.is_empty() {
        String::new()
    } else {
        format!(r"\{relative}")
    };
    Ok(PathBuf::from(format!(r"\\wsl.localhost\{distro}{suffix}")))
}

#[cfg(windows)]
fn wsl_home(distro: &str) -> Result<String> {
    use std::os::windows::process::CommandExt;

    let mut command = std::process::Command::new("wsl.exe");
    command
        .arg("-d")
        .arg(distro)
        .arg("--")
        .arg("bash")
        .arg("-lc")
        .arg("printf %s \"$HOME\"")
        .creation_flags(0x0800_0000);
    let output =
        crate::bounded_exec::output_with_timeout(command, crate::bounded_exec::WSL_PROBE_TIMEOUT)
            .with_context(|| format!("resolving WSL home for {distro}"))?;
    if !output.status.success() {
        bail!(
            "could not resolve WSL home for {distro}: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    normalize_wsl_home(&output.stdout)
}

#[cfg(windows)]
fn verify_wsl_agent_bin(distro: &str, agent_bin: &str) -> Result<()> {
    use std::os::windows::process::CommandExt;

    validate_canonical_absolute_posix(agent_bin)?;
    let mut command = std::process::Command::new("wsl.exe");
    command
        .arg("-d")
        .arg(distro)
        .arg("--")
        .arg("bash")
        .arg("-c")
        .arg("test -x \"$1\"")
        .arg("t-hub-agent")
        .arg(agent_bin)
        .creation_flags(0x0800_0000);
    let output =
        crate::bounded_exec::output_with_timeout(command, crate::bounded_exec::WSL_PROBE_TIMEOUT)
            .context(format!(
            "checking deployed t-hub-agent inside WSL distribution {distro}"
        ))?;
    if !output.status.success() {
        bail!(
            "deployed t-hub-agent is unavailable at {agent_bin} inside WSL distribution {distro}: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(())
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

#[cfg(test)]
pub fn install(
    codex_home: &Path,
    requirements_path: &Path,
    agent_bin: &Path,
    consent: bool,
) -> Result<InstallReport> {
    let hooks_state_path = codex_home.join("hooks.json").display().to_string();
    install_with_state_path(
        codex_home,
        requirements_path,
        agent_bin,
        &hooks_state_path,
        consent,
    )
}

pub fn install_runtime(paths: &RuntimePaths, consent: bool) -> Result<InstallReport> {
    install_with_state_path(
        &paths.codex_home,
        &paths.requirements_path,
        &paths.agent_bin,
        &paths.hooks_state_path,
        consent,
    )
}

fn install_with_state_path(
    codex_home: &Path,
    requirements_path: &Path,
    agent_bin: &Path,
    hooks_state_path: &str,
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
    let _ = health_at_with_project_and_state_path(
        codex_home,
        requirements_path,
        agent_bin,
        None,
        hooks_state_path,
    )?;
    let merged = merge_managed(&existing, agent_bin)?;
    let changed = merged != existing;
    let backed_up = changed && backup_once(&hooks_path)?;
    if changed {
        write_json_atomic(&hooks_path, &merged)?;
    }
    let health = health_at_with_project_and_state_path(
        codex_home,
        requirements_path,
        agent_bin,
        None,
        hooks_state_path,
    )?;
    Ok(InstallReport {
        hooks_path: hooks_path.display().to_string(),
        changed,
        backed_up,
        managed_events: health.managed_events.len(),
        health,
    })
}

#[cfg(test)]
pub fn repair(
    codex_home: &Path,
    requirements_path: &Path,
    agent_bin: &Path,
    consent: bool,
) -> Result<InstallReport> {
    install(codex_home, requirements_path, agent_bin, consent)
}

pub fn repair_runtime(paths: &RuntimePaths, consent: bool) -> Result<InstallReport> {
    install_runtime(paths, consent)
}

#[cfg(test)]
pub fn uninstall(
    codex_home: &Path,
    requirements_path: &Path,
    agent_bin: &Path,
) -> Result<InstallReport> {
    let hooks_state_path = codex_home.join("hooks.json").display().to_string();
    uninstall_with_state_path(codex_home, requirements_path, agent_bin, &hooks_state_path)
}

pub fn uninstall_runtime(paths: &RuntimePaths) -> Result<InstallReport> {
    uninstall_with_state_path(
        &paths.codex_home,
        &paths.requirements_path,
        &paths.agent_bin,
        &paths.hooks_state_path,
    )
}

fn uninstall_with_state_path(
    codex_home: &Path,
    requirements_path: &Path,
    agent_bin: &Path,
    hooks_state_path: &str,
) -> Result<InstallReport> {
    let hooks_path = codex_home.join("hooks.json");
    let _lock = ConfigLock::acquire(&hooks_path)?;
    let existing = read_hooks(&hooks_path)?;
    let _ = health_at_with_project_and_state_path(
        codex_home,
        requirements_path,
        agent_bin,
        None,
        hooks_state_path,
    )?;
    let cleaned = remove_managed(&existing)?;
    let changed = cleaned != existing;
    let backed_up = changed && backup_once(&hooks_path)?;
    if changed {
        write_json_atomic(&hooks_path, &cleaned)?;
    }
    let health = health_at_with_project_and_state_path(
        codex_home,
        requirements_path,
        agent_bin,
        None,
        hooks_state_path,
    )?;
    Ok(InstallReport {
        hooks_path: hooks_path.display().to_string(),
        changed,
        backed_up,
        managed_events: health.managed_events.len(),
        health,
    })
}

#[cfg(test)]
pub fn health_at(
    codex_home: &Path,
    requirements_path: &Path,
    agent_bin: &Path,
) -> Result<ProducerHealth> {
    let hooks_state_path = codex_home.join("hooks.json").display().to_string();
    health_at_with_project_and_state_path(
        codex_home,
        requirements_path,
        agent_bin,
        None,
        &hooks_state_path,
    )
}

#[cfg(test)]
pub fn health_at_with_project(
    codex_home: &Path,
    requirements_path: &Path,
    agent_bin: &Path,
    project_root: Option<&Path>,
) -> Result<ProducerHealth> {
    let hooks_state_path = codex_home.join("hooks.json").display().to_string();
    health_at_with_project_and_state_path(
        codex_home,
        requirements_path,
        agent_bin,
        project_root,
        &hooks_state_path,
    )
}

pub fn health_runtime(paths: &RuntimePaths, project_root: Option<&Path>) -> Result<ProducerHealth> {
    health_at_with_project_and_state_path(
        &paths.codex_home,
        &paths.requirements_path,
        &paths.agent_bin,
        project_root,
        &paths.hooks_state_path,
    )
}

fn health_at_with_project_and_state_path(
    codex_home: &Path,
    requirements_path: &Path,
    agent_bin: &Path,
    project_root: Option<&Path>,
    hooks_state_path: &str,
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
    // Requirements are enforced above ordinary configuration. Of the local
    // requirement sources available here, legacy managed_config.toml has higher
    // precedence than the system requirements file. User config then overrides
    // the lower-precedence system default when no requirement pins the feature.
    let hooks_enabled = hooks_feature_flag(legacy_managed_config.as_ref())
        .or_else(|| hooks_feature_flag(requirements.as_ref()))
        .or_else(|| hooks_feature_flag(config.as_ref()))
        .or_else(|| hooks_feature_flag(system_config.as_ref()))
        .unwrap_or(true);
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
            Some((group_index, handler_index, command, matcher)) => {
                managed_events.push(event.to_string());
                let key = format!(
                    "{}:{}:{group_index}:{handler_index}",
                    hooks_state_path,
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
                    modified |=
                        trusted_hash != expected_trust_hash(event, &command, matcher.as_deref());
                }
                drifted |= command != managed_command(agent_bin, event)
                    || matcher.as_deref() != managed_matcher(event);
            }
            None => missing_events.push(event.to_string()),
        }
    }

    let executable_ok = executable_ok(agent_bin);
    let agent_probe = if executable_ok {
        probe_agent_capabilities(agent_bin)
    } else {
        AgentCapabilityProbe::default()
    };
    let has_trust_for_all = managed_events.iter().all(|event| {
        let Some((group_index, handler_index, command, matcher)) =
            find_managed_handler(&hooks, event).ok().flatten()
        else {
            return false;
        };
        let key = format!(
            "{}:{}:{group_index}:{handler_index}",
            hooks_state_path,
            event_key(event)
        );
        state
            .and_then(|state| state.get(&key))
            .and_then(toml::Value::as_table)
            .and_then(|state| state.get("trusted_hash"))
            .and_then(toml::Value::as_str)
            .is_some_and(|hash| hash == expected_trust_hash(event, &command, matcher.as_deref()))
    });
    let status = if managed_events.is_empty() {
        ProducerStatus::NotInstalled
    } else if managed_only_policy {
        ProducerStatus::BlockedByManagedPolicy
    } else if drifted || !missing_events.is_empty() || !executable_ok || !agent_probe.capable {
        ProducerStatus::Drifted
    } else if disabled || !hooks_enabled {
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
        agent_capable: agent_probe.capable,
        agent_version: agent_probe.version,
        hooks_enabled,
        inline_user_hooks_present,
        project_hooks_present,
        plugin_config_present,
        managed_hooks_present,
        managed_only_policy,
        capabilities: CapabilityReport::default(),
    })
}

fn hooks_feature_flag(value: Option<&toml::Value>) -> Option<bool> {
    let features = value?.get("features").and_then(toml::Value::as_table)?;
    features
        .get("hooks")
        .or_else(|| features.get("codex_hooks"))
        .and_then(toml::Value::as_bool)
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
    #[cfg(unix)]
    let is_absolute = path.is_absolute();
    #[cfg(windows)]
    let is_absolute = path.to_string_lossy().starts_with('/');
    if !is_absolute {
        bail!("Codex hook executable must be an absolute path");
    }
    if !executable_ok(path) {
        bail!("Codex hook executable is missing or not executable");
    }
    if !probe_agent_capabilities(path).capable {
        bail!("Codex hook executable does not support native Codex hooks; update t-hub-agent");
    }
    Ok(())
}

fn probe_agent_capabilities(path: &Path) -> AgentCapabilityProbe {
    #[cfg(windows)]
    let command = {
        use std::os::windows::process::CommandExt;

        let mut command = std::process::Command::new("wsl.exe");
        command
            .arg("-d")
            .arg(wsl_distro())
            .arg("--")
            .arg(path)
            .arg("--capabilities-json")
            .creation_flags(0x0800_0000);
        command
    };
    #[cfg(not(windows))]
    let command = {
        let mut command = std::process::Command::new(path);
        command.arg("--capabilities-json");
        command
    };

    let Ok(output) = crate::bounded_exec::output_with_timeout_and_limit(
        command,
        crate::bounded_exec::WSL_PROBE_TIMEOUT,
        AGENT_CAPABILITY_OUTPUT_LIMIT,
    ) else {
        return AgentCapabilityProbe::default();
    };
    if !output.status.success() || !output.stderr.is_empty() {
        return AgentCapabilityProbe::default();
    }
    let Ok(report) = serde_json::from_slice::<AgentCapabilities>(&output.stdout) else {
        return AgentCapabilityProbe::default();
    };
    let version_valid = !report.agent_version.is_empty()
        && report.agent_version.len() <= 64
        && !report.agent_version.chars().any(char::is_control);
    if report.schema_version != AGENT_CAPABILITY_SCHEMA || !version_valid {
        return AgentCapabilityProbe::default();
    }
    AgentCapabilityProbe {
        capable: report
            .capabilities
            .iter()
            .any(|capability| capability == CODEX_HOOK_CAPABILITY),
        version: Some(report.agent_version),
    }
}

fn executable_ok(path: &Path) -> bool {
    #[cfg(windows)]
    {
        let expected = path.to_string_lossy();
        verify_wsl_agent_bin(&wsl_distro(), &expected).is_ok()
    }
    #[cfg(not(windows))]
    let Ok(metadata) = std::fs::metadata(path) else {
        return false;
    };
    #[cfg(not(windows))]
    if !metadata.is_file() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        metadata.permissions().mode() & 0o111 != 0
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
        let mut group = json!({
            "hooks": [{
                "type": "command",
                "command": managed_command(agent_bin, event),
                "timeout": if event == "SessionEnd" { 3 } else { 10 },
            }]
        });
        if let Some(matcher) = managed_matcher(event) {
            group["matcher"] = Value::String(matcher.to_string());
        }
        groups.push(group);
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

fn find_managed_handler(
    value: &Value,
    event: &str,
) -> Result<Option<(usize, usize, String, Option<String>)>> {
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
                    let matcher = group
                        .get("matcher")
                        .and_then(Value::as_str)
                        .map(str::to_string);
                    return Ok(Some((
                        group_index,
                        handler_index,
                        command.to_string(),
                        matcher,
                    )));
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

fn managed_matcher(event: &str) -> Option<&'static str> {
    matches!(event, "PreToolUse" | "PostToolUse").then_some("^request_user_input$")
}

fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

fn event_key(event: &str) -> &'static str {
    match event {
        "SessionStart" => "session_start",
        "UserPromptSubmit" => "user_prompt_submit",
        "PermissionRequest" => "permission_request",
        "PreToolUse" => "pre_tool_use",
        "PostToolUse" => "post_tool_use",
        "Stop" => "stop",
        "SessionEnd" => "session_end",
        _ => unreachable!("managed event set is closed"),
    }
}

fn expected_trust_hash(event: &str, command: &str, matcher: Option<&str>) -> String {
    let mut identity = Map::from_iter([
        (
            "event_name".to_string(),
            Value::String(event_key(event).to_string()),
        ),
        (
            "hooks".to_string(),
            json!([{
            "type": "command",
            "command": command,
            "timeout": if event == "SessionEnd" { 3 } else { 10 },
            "async": false,
            }]),
        ),
    ]);
    if let Some(matcher) = matcher {
        identity.insert("matcher".to_string(), Value::String(matcher.to_string()));
    }
    let bytes = serde_json::to_vec(&canonical_json(Value::Object(identity))).unwrap_or_default();
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
        replace_file(&temp, path)
            .with_context(|| format!("renaming {} to {}", temp.display(), path.display()))?;
        sync_parent_directory(parent)?;
        Ok(())
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&temp);
    }
    result
}

#[cfg(not(windows))]
fn replace_file(source: &Path, destination: &Path) -> std::io::Result<()> {
    std::fs::rename(source, destination)
}

#[cfg(windows)]
fn replace_file(source: &Path, destination: &Path) -> std::io::Result<()> {
    use std::os::windows::ffi::OsStrExt;
    use windows::core::PCWSTR;
    use windows::Win32::Storage::FileSystem::{
        MoveFileExW, MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH,
    };

    let source: Vec<u16> = source.as_os_str().encode_wide().chain(Some(0)).collect();
    let destination: Vec<u16> = destination
        .as_os_str()
        .encode_wide()
        .chain(Some(0))
        .collect();
    unsafe {
        MoveFileExW(
            PCWSTR(source.as_ptr()),
            PCWSTR(destination.as_ptr()),
            MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
        )
    }
    .map_err(|_| std::io::Error::last_os_error())
}

#[cfg(unix)]
fn sync_parent_directory(parent: &Path) -> Result<()> {
    File::open(parent)
        .and_then(|directory| directory.sync_all())
        .with_context(|| format!("syncing {}", parent.display()))
}

#[cfg(not(unix))]
fn sync_parent_directory(_parent: &Path) -> Result<()> {
    // Windows has no stable directory-fsync contract through std, especially
    // for a WSL UNC share. The temp file is fully synced before the atomic
    // rename, and a successful rename is therefore the durability boundary.
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn executable(dir: &Path) -> std::path::PathBuf {
        let path = dir.join("bin").join("t-hub-agent");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let staging = path.with_file_name(".t-hub-agent.fixture");
        let mut file = OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&staging)
            .unwrap();
        file.write_all(
            b"#!/bin/sh\n\
              if [ \"$1\" = \"--capabilities-json\" ]; then\n\
                printf '%s\\n' '{\"schemaVersion\":1,\"agentVersion\":\"test\",\"capabilities\":[\"codex-native-hooks-v1\"]}'\n\
                exit 0\n\
              fi\n\
              exit 2\n",
        )
        .unwrap();
        file.sync_all().unwrap();
        drop(file);
        std::fs::rename(staging, &path).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700)).unwrap();
            let mut ready = false;
            for _ in 0..50 {
                match std::process::Command::new(&path)
                    .arg("--capabilities-json")
                    .output()
                {
                    Ok(output) if output.status.success() => {
                        ready = true;
                        break;
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::ExecutableFileBusy => {
                        std::thread::sleep(Duration::from_millis(1));
                    }
                    Ok(output) => panic!(
                        "capability fixture exited with {:?}: {}",
                        output.status,
                        String::from_utf8_lossy(&output.stderr)
                    ),
                    Err(error) => panic!("capability fixture failed to start: {error}"),
                }
            }
            assert!(ready, "capability fixture remained busy after publication");
        }
        path
    }

    #[cfg(unix)]
    #[test]
    fn executable_fixture_is_published_and_runnable() {
        use std::os::unix::fs::PermissionsExt;

        let temp = tempfile::tempdir().unwrap();
        let agent = executable(temp.path());
        let staging = agent.with_file_name(".t-hub-agent.fixture");
        let output = std::process::Command::new(&agent)
            .arg("--capabilities-json")
            .output()
            .unwrap();
        let capabilities: Value = serde_json::from_slice(&output.stdout).unwrap();
        let mode = std::fs::metadata(&agent).unwrap().permissions().mode() & 0o777;

        assert!(output.status.success());
        assert!(!staging.exists());
        assert_eq!(mode, 0o700);
        assert_eq!(
            capabilities["capabilities"],
            json!(["codex-native-hooks-v1"])
        );
        println!(
            "published={} staging_exists={} mode={mode:o} capabilities={}",
            agent.display(),
            staging.exists(),
            String::from_utf8_lossy(&output.stdout).trim()
        );
    }

    #[test]
    fn health_rejects_an_executable_without_codex_hook_capability() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join("codex");
        std::fs::create_dir_all(&home).unwrap();
        let agent = temp.path().join("old-t-hub-agent");
        std::fs::write(&agent, b"#!/bin/sh\nexit 2\n").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&agent, std::fs::Permissions::from_mode(0o700)).unwrap();
        }
        let requirements = temp.path().join("requirements.toml");
        let hooks = merge_managed(&json!({}), &agent).unwrap();
        write_json_atomic(&home.join("hooks.json"), &hooks).unwrap();

        let health = health_at(&home, &requirements, &agent).unwrap();
        assert_eq!(health.status, ProducerStatus::Drifted);
        assert!(health.executable_ok);
        assert!(!health.agent_capable);
        assert_eq!(health.agent_version, None);
        assert!(install(&home, &requirements, &agent, true).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn unix_runtime_agent_path_resolves_from_path_without_blocking_cleanup_when_missing() {
        let temp = tempfile::tempdir().unwrap();
        let working_dir = temp.path().join("work");
        let bin_dir = temp.path().join("bin");
        std::fs::create_dir_all(&working_dir).unwrap();
        let agent = executable(temp.path());
        let search_path = std::env::join_paths([bin_dir]).unwrap();

        assert_eq!(
            resolve_unix_agent_bin_from("t-hub-agent", Some(&search_path), Some(&working_dir)),
            agent
        );
        assert_eq!(
            resolve_unix_agent_bin_from("missing-agent", Some(&search_path), Some(&working_dir)),
            PathBuf::from("missing-agent")
        );
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
    fn health_respects_hook_feature_precedence_and_legacy_alias() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join("codex");
        std::fs::create_dir_all(&home).unwrap();
        let agent = executable(temp.path());
        let requirements = temp.path().join("requirements.toml");
        install(&home, &requirements, &agent, true).unwrap();

        std::fs::write(home.join("config.toml"), "[features]\nhooks = false\n").unwrap();
        let health = health_at(&home, &requirements, &agent).unwrap();
        assert_eq!(health.status, ProducerStatus::Disabled);
        assert!(!health.hooks_enabled);

        std::fs::write(&requirements, "[features]\nhooks = true\n").unwrap();
        let health = health_at(&home, &requirements, &agent).unwrap();
        assert_eq!(health.status, ProducerStatus::NeedsReview);
        assert!(health.hooks_enabled);

        std::fs::write(
            home.join("config.toml"),
            "[features]\nhooks = true\ncodex_hooks = false\n",
        )
        .unwrap();
        std::fs::write(&requirements, "[features]\nhooks = false\n").unwrap();
        let health = health_at(&home, &requirements, &agent).unwrap();
        assert_eq!(health.status, ProducerStatus::Disabled);
        assert!(!health.hooks_enabled);

        std::fs::remove_file(&requirements).unwrap();
        std::fs::write(
            home.join("config.toml"),
            "[features]\ncodex_hooks = false\n",
        )
        .unwrap();
        let health = health_at(&home, &requirements, &agent).unwrap();
        assert_eq!(health.status, ProducerStatus::Disabled);
        assert!(!health.hooks_enabled);
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
    fn health_and_repair_detect_a_broadened_question_matcher() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join("codex");
        std::fs::create_dir_all(&home).unwrap();
        let agent = executable(temp.path());
        let requirements = temp.path().join("requirements.toml");
        install(&home, &requirements, &agent, true).unwrap();

        let mut hooks = read_hooks(&home.join("hooks.json")).unwrap();
        hooks["hooks"]["PreToolUse"][0]["matcher"] = Value::String("^.*$".to_string());
        write_json_atomic(&home.join("hooks.json"), &hooks).unwrap();
        assert_eq!(
            health_at(&home, &requirements, &agent).unwrap().status,
            ProducerStatus::Drifted
        );

        let repaired = repair(&home, &requirements, &agent, true).unwrap();
        assert!(repaired.changed);
        let hooks = read_hooks(&home.join("hooks.json")).unwrap();
        assert_eq!(
            hooks["hooks"]["PreToolUse"][0]["matcher"],
            "^request_user_input$"
        );
        assert_eq!(
            hooks["hooks"]["PostToolUse"][0]["matcher"],
            "^request_user_input$"
        );
    }

    #[test]
    fn trust_hash_matches_codex_0_145_normalized_identity() {
        assert_eq!(
            expected_trust_hash("PreToolUse", "hook-command", Some("^request_user_input$")),
            "sha256:cb901ab35ff6ca62ad3d7dd6c32b0de4cc55b9614cd4eee1bb723ec3a4af0d41"
        );
        assert_eq!(
            expected_trust_hash("Stop", "hook-command", None),
            "sha256:d11af1de50a91de62c838f9019f55b3a9cef5aa3ebb434e944e2407557e10bf4"
        );
    }

    #[test]
    fn trust_health_uses_runtime_hook_path_without_writing_codex_state() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join("codex");
        std::fs::create_dir_all(&home).unwrap();
        let agent = executable(temp.path());
        let requirements = temp.path().join("requirements.toml");
        let hooks_state_path = "/home/test/.codex/hooks.json";
        install(&home, &requirements, &agent, true).unwrap();
        let hooks = read_hooks(&home.join("hooks.json")).unwrap();
        let mut state = toml::map::Map::new();
        for event in OBSERVED_EVENTS {
            let (group, handler, command, matcher) =
                find_managed_handler(&hooks, event).unwrap().unwrap();
            let key = format!("{hooks_state_path}:{}:{group}:{handler}", event_key(event),);
            let mut hook_state = toml::map::Map::new();
            hook_state.insert(
                "trusted_hash".to_string(),
                toml::Value::String(expected_trust_hash(event, &command, matcher.as_deref())),
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
            health_at_with_project_and_state_path(
                &home,
                &requirements,
                &agent,
                None,
                hooks_state_path,
            )
            .unwrap()
            .status,
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
            health_at_with_project_and_state_path(
                &home,
                &requirements,
                &agent,
                None,
                hooks_state_path,
            )
            .unwrap()
            .status,
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
            health_at_with_project_and_state_path(
                &home,
                &requirements,
                &agent,
                None,
                hooks_state_path,
            )
            .unwrap()
            .status,
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

    #[test]
    fn windows_wsl_paths_keep_host_io_separate_from_runtime_commands() {
        let distro = "Ubuntu-24.04";
        let home = normalize_wsl_home(b"/home/natkins").unwrap();
        let codex_home = wsl_posix_to_unc(distro, &format!("{home}/.codex")).unwrap();
        let requirements = wsl_posix_to_unc(distro, "/etc/codex/requirements.toml").unwrap();
        let digest = "a".repeat(64);
        let agent_path = format!("/home/natkins/.local/lib/t-hub/agents/{digest}/t-hub-agent\r\n");
        let agent = normalize_single_wsl_path_output(agent_path.as_bytes()).unwrap();
        let project = host_project_path_for_distro("/home/natkins/project", distro).unwrap();
        let hooks_state_path = format!("{home}/.codex/hooks.json");

        assert_eq!(home, "/home/natkins");
        assert_eq!(hooks_state_path, "/home/natkins/.codex/hooks.json");
        assert_eq!(
            codex_home.to_string_lossy(),
            r"\\wsl.localhost\Ubuntu-24.04\home\natkins\.codex"
        );
        assert_eq!(
            requirements.to_string_lossy(),
            r"\\wsl.localhost\Ubuntu-24.04\etc\codex\requirements.toml"
        );
        assert_eq!(
            project.to_string_lossy(),
            r"\\wsl.localhost\Ubuntu-24.04\home\natkins\project"
        );
        assert_eq!(
            agent,
            format!("/home/natkins/.local/lib/t-hub/agents/{digest}/t-hub-agent")
        );
        let command = managed_command(Path::new(&agent), "Stop");
        assert!(command.starts_with(&format!(
            "'/home/natkins/.local/lib/t-hub/agents/{digest}/t-hub-agent' "
        )));
        assert!(!command.contains(r"\\wsl"));
        assert!(!command.contains("C:\\"));
    }

    #[test]
    fn windows_runtime_agent_path_accepts_only_canonical_absolute_input() {
        let digest = "a".repeat(64);
        let resolved = format!("/home/natkins/.local/lib/t-hub/agents/{digest}/t-hub-agent");
        assert!(validate_canonical_absolute_posix(&resolved).is_ok());
        assert!(validate_canonical_absolute_posix("relative/home").is_err());
    }

    #[test]
    fn windows_wsl_path_contract_rejects_host_paths_and_traversal() {
        assert!(normalize_wsl_home(b"C:\\Users\\natha").is_err());
        assert!(normalize_wsl_home(b"/home/../root").is_err());
        assert!(normalize_wsl_home(b"/home\\..\\root").is_err());
        assert!(normalize_wsl_home(b"/home/natkins\n/root").is_err());
        assert!(normalize_wsl_home(b"/home/natkins\0").is_err());
        assert!(normalize_single_wsl_path_output(b"t-hub-agent").is_err());
        assert!(normalize_single_wsl_path_output(b"/home/natkins/../bin/t-hub-agent").is_err());
        assert!(
            normalize_single_wsl_path_output(b"/home/bin/t-hub-agent\n/root/bin/evil").is_err()
        );
        assert!(normalize_single_wsl_path_output(b"/home/bin/t-hub-agent\revil").is_err());
        assert!(normalize_single_wsl_path_output(b"/home/bin/t-hub-agent\0").is_err());
        assert!(wsl_posix_to_unc("Ubuntu-24.04", r"C:\Users\natha").is_err());
        assert!(wsl_posix_to_unc("Ubuntu-24.04", "/home/../root").is_err());
        assert!(wsl_posix_to_unc("Ubuntu-24.04", "/a\\..\\b").is_err());
        assert!(wsl_posix_to_unc("Ubuntu-24.04", "/home/\0/root").is_err());
        assert!(wsl_posix_to_unc(r"Ubuntu\other", "/home/natkins").is_err());
        assert!(host_project_path_for_distro(r"C:\Users\natha", "Ubuntu-24.04").is_err());
        assert!(host_project_path_for_distro("relative/project", "Ubuntu-24.04").is_err());
        assert!(host_project_path_for_distro(
            r"\\wsl.localhost\Debian\home\natkins",
            "Ubuntu-24.04"
        )
        .is_err());
        assert!(host_project_path_for_distro("/a\\..\\b", "Ubuntu-24.04").is_err());
        assert!(host_project_path_for_distro("/home/\n/root", "Ubuntu-24.04").is_err());
        assert!(host_project_path_for_distro("/home/\0/root", "Ubuntu-24.04").is_err());
    }

    #[cfg(windows)]
    #[test]
    fn production_windows_project_path_is_fail_closed() {
        assert!(host_project_path(r"C:\Users\natha").is_err());
        assert!(host_project_path("relative/project").is_err());
        assert!(host_project_path(r"\\wsl.localhost\Debian\home\natkins").is_err());
        assert!(host_project_path("/a\\..\\b").is_err());
        assert!(host_project_path("/home/\n/root").is_err());
        assert!(host_project_path("/home/\0/root").is_err());
    }
}
