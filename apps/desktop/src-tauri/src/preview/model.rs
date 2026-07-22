use serde::{Deserialize, Serialize};

const MAX_ID_BYTES: usize = 160;

fn validate_identifier(value: &str, field: &str) -> Result<(), String> {
    if value.is_empty() || value.len() > MAX_ID_BYTES {
        return Err(format!(
            "{field} must contain between 1 and {MAX_ID_BYTES} bytes"
        ));
    }
    if !value
        .bytes()
        .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b':'))
    {
        return Err(format!("{field} contains unsupported characters"));
    }
    Ok(())
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize)]
#[serde(transparent)]
pub struct PreviewTargetId(String);

impl PreviewTargetId {
    pub fn parse(value: impl Into<String>) -> Result<Self, String> {
        let value = value.into();
        validate_identifier(&value, "preview target id")?;
        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl<'de> Deserialize<'de> for PreviewTargetId {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        Self::parse(String::deserialize(deserializer)?).map_err(serde::de::Error::custom)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PreviewScope {
    pub project_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub workspace_id: Option<String>,
}

impl<'de> Deserialize<'de> for PreviewScope {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase", deny_unknown_fields)]
        struct RawScope {
            project_id: String,
            workspace_id: Option<String>,
        }

        let raw = RawScope::deserialize(deserializer)?;
        Self::new(raw.project_id, raw.workspace_id).map_err(serde::de::Error::custom)
    }
}

impl PreviewScope {
    pub fn new(
        project_id: impl Into<String>,
        workspace_id: Option<String>,
    ) -> Result<Self, String> {
        let project_id = project_id.into();
        validate_identifier(&project_id, "project id")?;
        if let Some(workspace_id) = workspace_id.as_deref() {
            validate_identifier(workspace_id, "workspace id")?;
        }
        Ok(Self {
            project_id,
            workspace_id,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum PreviewTargetSource {
    Root,
    WorkspaceManifest,
    Config,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum PreviewPackageManager {
    Npm,
    Pnpm,
    Yarn,
    Bun,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase", deny_unknown_fields)]
pub enum PreviewTargetKind {
    PackageScript {
        package_manager: PreviewPackageManager,
        script: String,
    },
    StaticSite {
        entrypoint: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PreviewTarget {
    pub id: PreviewTargetId,
    pub label: String,
    pub source: PreviewTargetSource,
    /// Canonical-root-relative directory. An empty string represents the root.
    pub relative_root: String,
    pub kind: PreviewTargetKind,
    pub recommended: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PreviewTargetRef {
    pub scope: PreviewScope,
    pub target_id: PreviewTargetId,
    pub discovery_fingerprint: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum PreviewState {
    Starting,
    Running,
    Unreachable,
    Stale,
    Failed,
    Stopped,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum PreviewOperation {
    Discover,
    Status,
    Select,
    Start,
    Stop,
    Restart,
    Open,
    Refresh,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum PreviewOperationOutcome {
    Applied,
    Unchanged,
    Recovered,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PreviewStatus {
    pub scope: PreviewScope,
    pub state: PreviewState,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub target_id: Option<PreviewTargetId>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub run_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub preview_url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    pub observed_at_ms: u64,
}

impl PreviewStatus {
    pub fn stopped(scope: PreviewScope, observed_at_ms: u64) -> Self {
        Self {
            scope,
            state: PreviewState::Stopped,
            target_id: None,
            run_id: None,
            preview_url: None,
            reason: None,
            observed_at_ms,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PreviewOperationResult {
    pub operation: PreviewOperation,
    pub outcome: PreviewOperationOutcome,
    pub request_id: String,
    pub status: PreviewStatus,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn target_ids_are_bounded_and_path_free() {
        assert!(PreviewTargetId::parse("workspace:web:dev").is_ok());
        assert!(PreviewTargetId::parse("../outside").is_err());
        assert!(PreviewTargetId::parse("with spaces").is_err());
        assert!(PreviewTargetId::parse("x".repeat(MAX_ID_BYTES + 1)).is_err());
    }

    #[test]
    fn scope_identity_is_typed_and_collision_free() {
        let project = PreviewScope::new("project-1", None).unwrap();
        let workspace = PreviewScope::new("project-1", Some("web".into())).unwrap();
        assert_ne!(project, workspace);
        assert_ne!(
            PreviewScope::new("a:b", None).unwrap(),
            PreviewScope::new("a", Some("b".into())).unwrap()
        );
        assert!(PreviewScope::new("project/escape", None).is_err());
    }

    #[test]
    fn deserialization_enforces_identifier_and_exact_scope_shape() {
        assert!(serde_json::from_str::<PreviewTargetId>(r#""../outside""#).is_err());
        assert!(serde_json::from_value::<PreviewScope>(serde_json::json!({
            "projectId": "project/escape"
        }))
        .is_err());
        assert!(serde_json::from_value::<PreviewScope>(serde_json::json!({
            "projectId": "project-1",
            "unexpected": true
        }))
        .is_err());
    }

    #[test]
    fn public_contract_uses_exact_states_and_outcomes() {
        let states = [
            PreviewState::Starting,
            PreviewState::Running,
            PreviewState::Unreachable,
            PreviewState::Stale,
            PreviewState::Failed,
            PreviewState::Stopped,
        ];
        assert_eq!(
            serde_json::to_value(states).unwrap(),
            serde_json::json!([
                "starting",
                "running",
                "unreachable",
                "stale",
                "failed",
                "stopped"
            ])
        );
        assert_eq!(
            serde_json::to_value([
                PreviewOperationOutcome::Applied,
                PreviewOperationOutcome::Unchanged,
                PreviewOperationOutcome::Recovered,
            ])
            .unwrap(),
            serde_json::json!(["applied", "unchanged", "recovered"])
        );
    }

    #[test]
    fn target_shape_contains_no_command_surface() {
        let target = PreviewTarget {
            id: PreviewTargetId::parse("root:dev").unwrap(),
            label: "Development server".into(),
            source: PreviewTargetSource::Root,
            relative_root: String::new(),
            kind: PreviewTargetKind::PackageScript {
                package_manager: PreviewPackageManager::Pnpm,
                script: "dev".into(),
            },
            recommended: true,
        };
        let json = serde_json::to_value(target).unwrap();
        assert!(json.get("command").is_none());
        assert!(json.get("argv").is_none());
        assert!(json.get("env").is_none());
        assert!(json.get("shell").is_none());
    }
}
