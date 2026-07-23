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

pub fn control_handler(
    service: Arc<DesktopPreviewService>,
) -> impl Fn(&str, &Value, &PreviewRootAuthority) -> Result<Value, String> + Send + Sync + 'static {
    move |command, args, root| dispatch(&service, command, args, root)
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

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn control_handler_retains_the_exact_shared_service_arc() {
        let root = tempfile::tempdir().unwrap();
        let profiles =
            Arc::new(PreviewProfileStore::open(root.path().join("profiles.json")).unwrap());
        let service = Arc::new(PreviewService::new(
            ManagedPreviewRuntime::for_test(),
            profiles,
        ));
        let weak = Arc::downgrade(&service);
        let handler = control_handler(Arc::clone(&service));
        drop(service);
        assert_eq!(weak.strong_count(), 1);
        drop(handler);
        assert!(weak.upgrade().is_none());
    }

    #[test]
    fn source_adapter_runs_the_complete_lifecycle_through_one_service() {
        let root = tempfile::tempdir().unwrap();
        std::fs::write(
            root.path().join("package.json"),
            serde_json::to_vec(&json!({
                "scripts": {
                    "dev": "node -e \"const h=require('http').createServer((q,s)=>s.end('ok'));h.listen(0,'0.0.0.0',()=>console.log('http://127.0.0.1:'+h.address().port+'/'))\""
                }
            }))
            .unwrap(),
        )
        .unwrap();
        let profiles =
            Arc::new(PreviewProfileStore::open(root.path().join("preview-profiles.json")).unwrap());
        let service = PreviewService::new(ManagedPreviewRuntime::for_test(), profiles);
        let authority = PreviewRootAuthority {
            posix_identity: root.path().to_string_lossy().into_owned(),
            host_open_path: root.path().to_path_buf(),
        };
        let scope = json!({ "projectId": "project-adapter" });
        let discovery = dispatch(
            &service,
            "preview_discover",
            &json!({ "rootPath": authority.posix_identity }),
            &authority,
        )
        .unwrap();
        let target_id = discovery["targets"][0]["id"].clone();
        let target = json!({
            "scope": scope,
            "targetId": target_id,
            "discoveryFingerprint": discovery["discoveryFingerprint"],
        });
        dispatch(
            &service,
            "preview_select",
            &json!({
                "rootPath": authority.posix_identity,
                "target": target,
                "requestId": "adapter-select-1",
            }),
            &authority,
        )
        .unwrap();
        let started = dispatch(
            &service,
            "preview_start",
            &json!({
                "rootPath": authority.posix_identity,
                "scope": scope,
                "target": target,
                "requestId": "adapter-start-1",
            }),
            &authority,
        )
        .unwrap();
        assert_eq!(started["status"]["state"], "running");
        assert!(started["status"]["previewUrl"]
            .as_str()
            .is_some_and(|url| url.starts_with("http://127.0.0.1:")));
        assert!(started["status"]["output"]
            .as_array()
            .is_some_and(|lines| !lines.is_empty()));
        assert_eq!(
            dispatch(
                &service,
                "preview_status",
                &json!({ "scope": scope }),
                &authority,
            )
            .unwrap()["state"],
            "running"
        );
        for (command, request_id) in [
            ("preview_refresh", "adapter-refresh-1"),
            ("preview_open", "adapter-open-1"),
        ] {
            dispatch(
                &service,
                command,
                &json!({ "scope": scope, "requestId": request_id }),
                &authority,
            )
            .unwrap();
        }
        let restarted = dispatch(
            &service,
            "preview_restart",
            &json!({
                "rootPath": authority.posix_identity,
                "scope": scope,
                "requestId": "adapter-restart-1",
            }),
            &authority,
        )
        .unwrap();
        assert_eq!(restarted["status"]["state"], "running");
        let stopped = dispatch(
            &service,
            "preview_stop",
            &json!({
                "scope": scope,
                "expectedRunId": restarted["status"]["runId"],
                "requestId": "adapter-stop-1",
            }),
            &authority,
        )
        .unwrap();
        assert_eq!(stopped["status"]["state"], "stopped");
    }
}
