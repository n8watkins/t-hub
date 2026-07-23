//! Thin JSON adapter shared by desktop control, CLI, and MCP callers.

use std::sync::Arc;

use serde::de::DeserializeOwned;
use serde_json::{json, Value};

use super::discovery::PreviewProjectRoot;
use super::endpoint::ProbeCancellation;
use super::managed_runtime::ManagedPreviewRuntime;
use super::model::{PreviewScope, PreviewTargetRef};
use super::profile::PreviewProfileStore;
use super::service::PreviewService;
use crate::control::PreviewRootAuthority;

pub type DesktopPreviewService = PreviewService<ManagedPreviewRuntime>;

pub fn profiles_path() -> std::path::PathBuf {
    if let Ok(path) = std::env::var("T_HUB_PREVIEW_PROFILES_FILE") {
        return path.into();
    }
    let home = std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| ".".into());
    home.join(".t-hub").join("preview-profiles.json")
}

pub fn build(
    app: tauri::AppHandle,
    agent: crate::agent::AgentBridge,
) -> Result<Arc<DesktopPreviewService>, String> {
    let profiles = Arc::new(PreviewProfileStore::open(profiles_path())?);
    Ok(Arc::new(PreviewService::new(
        ManagedPreviewRuntime::new(app, agent),
        profiles,
    )))
}

pub fn dispatch(
    service: &DesktopPreviewService,
    command: &str,
    args: &Value,
    root: &PreviewRootAuthority,
) -> Result<Value, String> {
    let authority =
        PreviewProjectRoot::new(root.posix_identity.clone(), root.host_open_path.clone())?;
    match command {
        "preview_discover" => {
            let discovery = service.discover_authorized(&authority)?;
            let count = discovery.targets.len();
            Ok(json!({
                "canonicalRoot": discovery.registered_posix_root,
                "canonicalRootFingerprint": discovery.canonical_root_fingerprint,
                "discoveryFingerprint": discovery.discovery_fingerprint,
                "targets": discovery.targets,
                "count": count,
            }))
        }
        "preview_status" => serialize(service.status(&field(args, "scope")?)?),
        "preview_select" => serialize(service.select_authorized(
            &authority,
            &field(args, "target")?,
            string(args, "requestId")?,
        )?),
        "preview_start" => {
            let scope = field::<PreviewScope>(args, "scope")?;
            let target = optional::<PreviewTargetRef>(args, "target")?;
            serialize(service.start_authorized(
                &authority,
                &scope,
                target.as_ref(),
                string(args, "requestId")?,
                &ProbeCancellation::default(),
            )?)
        }
        "preview_stop" => serialize(service.stop(
            &field(args, "scope")?,
            optional_string(args, "expectedRunId")?,
            string(args, "requestId")?,
        )?),
        "preview_restart" => serialize(service.restart_authorized(
            &authority,
            &field(args, "scope")?,
            string(args, "requestId")?,
            &ProbeCancellation::default(),
        )?),
        "preview_refresh" => serialize(service.refresh(
            &field(args, "scope")?,
            string(args, "requestId")?,
            &ProbeCancellation::default(),
        )?),
        "preview_open" => {
            serialize(service.open(&field(args, "scope")?, string(args, "requestId")?)?)
        }
        _ => Err(format!("unknown Preview command '{command}'")),
    }
}

fn field<T: DeserializeOwned>(args: &Value, name: &str) -> Result<T, String> {
    serde_json::from_value(
        args.get(name)
            .cloned()
            .ok_or_else(|| format!("Preview command requires '{name}'"))?,
    )
    .map_err(|error| format!("invalid Preview {name}: {error}"))
}

fn optional<T: DeserializeOwned>(args: &Value, name: &str) -> Result<Option<T>, String> {
    args.get(name)
        .cloned()
        .map(serde_json::from_value)
        .transpose()
        .map_err(|error| format!("invalid Preview {name}: {error}"))
}

fn string<'a>(args: &'a Value, name: &str) -> Result<&'a str, String> {
    args.get(name)
        .and_then(Value::as_str)
        .ok_or_else(|| format!("Preview command requires string '{name}'"))
}

fn optional_string<'a>(args: &'a Value, name: &str) -> Result<Option<&'a str>, String> {
    args.get(name)
        .map(|value| {
            value
                .as_str()
                .ok_or_else(|| format!("Preview command requires string '{name}'"))
        })
        .transpose()
}

fn serialize(value: impl serde::Serialize) -> Result<Value, String> {
    serde_json::to_value(value).map_err(|error| format!("serialize Preview response: {error}"))
}
