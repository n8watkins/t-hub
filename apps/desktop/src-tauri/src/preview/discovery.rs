//! Bounded, deterministic Preview target discovery.

use std::collections::{BTreeSet, HashMap};
use std::fs;
use std::path::{Component, Path, PathBuf};

use parking_lot::Mutex;
use serde::Deserialize;
use sha2::{Digest, Sha256};

use super::model::{
    PreviewPackageManager, PreviewTarget, PreviewTargetId, PreviewTargetKind, PreviewTargetSource,
};

const MAX_FILE_BYTES: u64 = 1024 * 1024;
const MAX_DEPTH: usize = 8;
const MAX_VISITED_DIRECTORIES: usize = 4096;
const MAX_TARGETS: usize = 256;
const CONFIG_SCHEMA_VERSION: u32 = 1;
const PACKAGE_SCRIPTS: [&str; 3] = ["dev", "preview", "start"];
const LOCK_FILES: [&str; 5] = [
    "pnpm-lock.yaml",
    "yarn.lock",
    "bun.lockb",
    "bun.lock",
    "package-lock.json",
];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreviewDiscovery {
    pub canonical_root: PathBuf,
    pub canonical_root_fingerprint: String,
    pub discovery_fingerprint: String,
    pub targets: Vec<PreviewTarget>,
}

#[derive(Default)]
pub struct PreviewDiscoveryCache {
    entries: Mutex<HashMap<(PathBuf, String), PreviewDiscovery>>,
}

impl PreviewDiscoveryCache {
    pub fn discover(&self, root: &Path) -> Result<PreviewDiscovery, String> {
        let discovered = discover(root)?;
        let key = (
            discovered.canonical_root.clone(),
            discovered.discovery_fingerprint.clone(),
        );
        let mut entries = self.entries.lock();
        if let Some(cached) = entries.get(&key) {
            return Ok(cached.clone());
        }
        entries.retain(|(candidate_root, _), _| candidate_root != &key.0);
        entries.insert(key, discovered.clone());
        Ok(discovered)
    }
}

pub fn discover(root: &Path) -> Result<PreviewDiscovery, String> {
    let canonical_root = fs::canonicalize(root)
        .map_err(|error| format!("canonicalize Preview root {}: {error}", root.display()))?;
    if !canonical_root.is_dir() {
        return Err("Preview root must be a directory".into());
    }

    let mut fingerprint = Sha256::new();
    let root_identity = canonical_root.to_string_lossy().replace('\\', "/");
    let canonical_root_fingerprint = format!("sha256:{:x}", Sha256::digest(root_identity));
    let root_manifest_path = canonical_root.join("package.json");
    let root_manifest =
        read_optional_bounded(&root_manifest_path, &canonical_root, &mut fingerprint)?;
    let config_path = canonical_root.join(".t-hub/preview.json");
    let config = read_optional_bounded(&config_path, &canonical_root, &mut fingerprint)?;
    for lock_file in LOCK_FILES {
        let _ = read_optional_bounded(
            &canonical_root.join(lock_file),
            &canonical_root,
            &mut fingerprint,
        )?;
    }

    let mut workspace_patterns = Vec::new();
    if let Some(bytes) = root_manifest.as_deref() {
        workspace_patterns.extend(package_workspace_patterns(bytes)?);
    }
    if let Some(bytes) = read_optional_bounded(
        &canonical_root.join("pnpm-workspace.yaml"),
        &canonical_root,
        &mut fingerprint,
    )? {
        workspace_patterns.extend(pnpm_workspace_patterns(&bytes)?);
    }
    workspace_patterns.sort();
    workspace_patterns.dedup();

    let mut manifest_roots = vec![(canonical_root.clone(), PreviewTargetSource::Root)];
    if !workspace_patterns.is_empty() {
        for directory in bounded_directories(&canonical_root)? {
            let relative = relative_slash(&canonical_root, &directory)?;
            if workspace_patterns
                .iter()
                .any(|pattern| glob_matches(pattern, &relative))
                && directory.join("package.json").is_file()
            {
                manifest_roots.push((directory, PreviewTargetSource::WorkspaceManifest));
            }
        }
    }
    manifest_roots.sort_by(|left, right| left.0.cmp(&right.0));
    manifest_roots.dedup_by(|left, right| left.0 == right.0);

    let mut targets = Vec::new();
    for (manifest_root, source) in manifest_roots {
        let manifest_path = manifest_root.join("package.json");
        let bytes = if manifest_path == root_manifest_path {
            root_manifest.clone()
        } else {
            read_optional_bounded(&manifest_path, &canonical_root, &mut fingerprint)?
        };
        let Some(bytes) = bytes else { continue };
        let package = parse_package_manifest(&bytes)?;
        let relative_root = relative_slash(&canonical_root, &manifest_root)?;
        if manifest_root != canonical_root {
            for lock_file in LOCK_FILES {
                let _ = read_optional_bounded(
                    &manifest_root.join(lock_file),
                    &canonical_root,
                    &mut fingerprint,
                )?;
            }
        }
        let manager = package_manager(&manifest_root, &canonical_root);
        for script in PACKAGE_SCRIPTS {
            if package.scripts.contains(script) {
                push_target(
                    &mut targets,
                    PreviewTarget {
                        id: target_id(&relative_root, script)?,
                        label: target_label(package.name.as_deref(), &relative_root, script),
                        source,
                        relative_root: relative_root.clone(),
                        kind: PreviewTargetKind::PackageScript {
                            package_manager: manager,
                            script: script.into(),
                        },
                        recommended: script == "dev",
                    },
                )?;
            }
        }
    }

    if let Some(bytes) = config {
        for configured in parse_config(&bytes)? {
            let target = accept_config_target(&canonical_root, configured, &mut fingerprint)?;
            push_target(&mut targets, target)?;
        }
    }

    targets.sort_by(|left, right| left.id.as_str().cmp(right.id.as_str()));
    for target in &targets {
        let encoded = serde_json::to_vec(target)
            .map_err(|error| format!("serialize discovered Preview target: {error}"))?;
        fingerprint.update((encoded.len() as u64).to_le_bytes());
        fingerprint.update(encoded);
    }
    Ok(PreviewDiscovery {
        canonical_root,
        canonical_root_fingerprint,
        discovery_fingerprint: format!("sha256:{:x}", fingerprint.finalize()),
        targets,
    })
}

#[derive(Deserialize)]
struct PackageManifest {
    name: Option<String>,
    #[serde(default)]
    scripts: serde_json::Map<String, serde_json::Value>,
}

fn parse_package_manifest(bytes: &[u8]) -> Result<ParsedPackage, String> {
    let package: PackageManifest =
        serde_json::from_slice(bytes).map_err(|error| format!("invalid package.json: {error}"))?;
    let scripts = package
        .scripts
        .into_iter()
        .filter_map(|(name, value)| value.is_string().then_some(name))
        .collect();
    Ok(ParsedPackage {
        name: package.name,
        scripts,
    })
}

struct ParsedPackage {
    name: Option<String>,
    scripts: BTreeSet<String>,
}

fn package_workspace_patterns(bytes: &[u8]) -> Result<Vec<String>, String> {
    let value: serde_json::Value = serde_json::from_slice(bytes)
        .map_err(|error| format!("invalid root package.json: {error}"))?;
    let Some(workspaces) = value.get("workspaces") else {
        return Ok(Vec::new());
    };
    let values = match workspaces {
        serde_json::Value::Array(values) => values,
        serde_json::Value::Object(map) => map
            .get("packages")
            .and_then(serde_json::Value::as_array)
            .ok_or_else(|| "package.json workspaces.packages must be an array".to_string())?,
        _ => return Err("package.json workspaces must be an array or object".into()),
    };
    values
        .iter()
        .map(|value| {
            value
                .as_str()
                .ok_or_else(|| "workspace patterns must be strings".to_string())
                .and_then(validate_pattern)
        })
        .collect()
}

fn pnpm_workspace_patterns(bytes: &[u8]) -> Result<Vec<String>, String> {
    let text = std::str::from_utf8(bytes)
        .map_err(|error| format!("pnpm-workspace.yaml is not UTF-8: {error}"))?;
    let mut in_packages = false;
    let mut patterns = Vec::new();
    for raw_line in text.lines() {
        let trimmed = raw_line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        if !raw_line.starts_with(char::is_whitespace) {
            in_packages = trimmed == "packages:";
            continue;
        }
        if in_packages {
            let Some(value) = trimmed.strip_prefix('-') else {
                continue;
            };
            let value = value
                .trim()
                .trim_matches(|character| character == '\'' || character == '"');
            patterns.push(validate_pattern(value)?);
        }
    }
    Ok(patterns)
}

fn validate_pattern(value: &str) -> Result<String, String> {
    let normalized = value.trim().replace('\\', "/");
    if normalized.is_empty()
        || normalized.starts_with('/')
        || normalized.starts_with('!')
        || normalized.split('/').any(|part| part == "..")
        || normalized.contains('[')
        || normalized.contains('{')
    {
        return Err(format!("unsupported workspace pattern {value:?}"));
    }
    Ok(normalized.trim_end_matches('/').to_string())
}

fn bounded_directories(root: &Path) -> Result<Vec<PathBuf>, String> {
    let mut result = Vec::new();
    let mut pending = vec![(root.to_path_buf(), 0usize)];
    let mut visited = 0usize;
    while let Some((directory, depth)) = pending.pop() {
        if depth >= MAX_DEPTH {
            continue;
        }
        let mut children = fs::read_dir(&directory)
            .map_err(|error| format!("read directory {}: {error}", directory.display()))?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|error| format!("read directory {}: {error}", directory.display()))?;
        children.sort_by_key(|entry| entry.file_name());
        for child in children.into_iter().rev() {
            let name = child.file_name();
            if matches!(name.to_str(), Some(".git" | "node_modules" | "target")) {
                continue;
            }
            if path_is_link_or_reparse(&child.path())? {
                continue;
            }
            let metadata = child
                .metadata()
                .map_err(|error| format!("inspect {}: {error}", child.path().display()))?;
            if !metadata.is_dir() {
                continue;
            }
            visited += 1;
            if visited > MAX_VISITED_DIRECTORIES {
                return Err("Preview workspace discovery exceeded its directory bound".into());
            }
            let canonical = confined_canonical(root, &child.path())?;
            result.push(canonical.clone());
            pending.push((canonical, depth + 1));
        }
    }
    Ok(result)
}

fn glob_matches(pattern: &str, relative: &str) -> bool {
    let pattern = pattern
        .split('/')
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>();
    let value = relative
        .split('/')
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>();
    match_segments(&pattern, &value)
}

fn match_segments(pattern: &[&str], value: &[&str]) -> bool {
    match pattern.split_first() {
        None => value.is_empty(),
        Some((&"**", rest)) => {
            match_segments(rest, value)
                || (!value.is_empty() && match_segments(pattern, &value[1..]))
        }
        Some((head, rest)) => {
            !value.is_empty()
                && match_component(head, value[0])
                && match_segments(rest, &value[1..])
        }
    }
}

fn match_component(pattern: &str, value: &str) -> bool {
    let pattern = pattern.as_bytes();
    let value = value.as_bytes();
    let (mut pattern_index, mut value_index, mut star, mut checkpoint) = (0, 0, None, 0);
    while value_index < value.len() {
        if pattern_index < pattern.len()
            && (pattern[pattern_index] == b'?' || pattern[pattern_index] == value[value_index])
        {
            pattern_index += 1;
            value_index += 1;
        } else if pattern_index < pattern.len() && pattern[pattern_index] == b'*' {
            star = Some(pattern_index);
            pattern_index += 1;
            checkpoint = value_index;
        } else if let Some(star_index) = star {
            pattern_index = star_index + 1;
            checkpoint += 1;
            value_index = checkpoint;
        } else {
            return false;
        }
    }
    pattern[pattern_index..].iter().all(|byte| *byte == b'*')
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct PreviewConfig {
    schema_version: u32,
    targets: Vec<ConfiguredTarget>,
}

#[derive(Deserialize)]
#[serde(
    tag = "type",
    rename_all = "camelCase",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
enum ConfiguredTarget {
    PackageScript {
        id: String,
        label: String,
        #[serde(default)]
        relative_root: String,
        #[serde(default)]
        recommended: bool,
        package_manager: PreviewPackageManager,
        script: String,
    },
    StaticSite {
        id: String,
        label: String,
        #[serde(default)]
        relative_root: String,
        #[serde(default)]
        recommended: bool,
        entrypoint: String,
    },
}

fn parse_config(bytes: &[u8]) -> Result<Vec<ConfiguredTarget>, String> {
    let config: PreviewConfig = serde_json::from_slice(bytes)
        .map_err(|error| format!("invalid .t-hub/preview.json: {error}"))?;
    if config.schema_version != CONFIG_SCHEMA_VERSION {
        return Err(format!(
            "unsupported Preview config schemaVersion {}; expected {CONFIG_SCHEMA_VERSION}",
            config.schema_version
        ));
    }
    if config.targets.len() > MAX_TARGETS {
        return Err("Preview config exceeds its target bound".into());
    }
    Ok(config.targets)
}

fn accept_config_target(
    root: &Path,
    configured: ConfiguredTarget,
    fingerprint: &mut Sha256,
) -> Result<PreviewTarget, String> {
    let (id, label, relative_root, recommended, kind) = match configured {
        ConfiguredTarget::PackageScript {
            id,
            label,
            relative_root,
            recommended,
            package_manager,
            script,
        } => (
            id,
            label,
            relative_root,
            recommended,
            ConfiguredTargetKind::PackageScript {
                package_manager,
                script,
            },
        ),
        ConfiguredTarget::StaticSite {
            id,
            label,
            relative_root,
            recommended,
            entrypoint,
        } => (
            id,
            label,
            relative_root,
            recommended,
            ConfiguredTargetKind::StaticSite { entrypoint },
        ),
    };
    let id = PreviewTargetId::parse(id)?;
    if label.trim().is_empty() || label.len() > 200 {
        return Err("configured Preview target label must contain 1 to 200 bytes".into());
    }
    let relative_root = normalize_relative(&relative_root)?;
    let target_root = confined_canonical(root, &root.join(&relative_root))?;
    let kind = match kind {
        ConfiguredTargetKind::PackageScript {
            package_manager,
            script,
        } => {
            if !PACKAGE_SCRIPTS.contains(&script.as_str()) {
                return Err("configured package script must be dev, preview, or start".into());
            }
            let manifest =
                read_required_bounded(&target_root.join("package.json"), root, fingerprint)?;
            if !parse_package_manifest(&manifest)?.scripts.contains(&script) {
                return Err(format!(
                    "configured package script {script:?} does not exist"
                ));
            }
            PreviewTargetKind::PackageScript {
                package_manager,
                script,
            }
        }
        ConfiguredTargetKind::StaticSite { entrypoint } => {
            let entrypoint = normalize_relative(&entrypoint)?;
            let entry = confined_canonical(root, &target_root.join(&entrypoint))?;
            if !entry.is_file() {
                return Err("configured static Preview entrypoint must be a file".into());
            }
            fingerprint_metadata(root, &entry, fingerprint)?;
            PreviewTargetKind::StaticSite { entrypoint }
        }
    };
    Ok(PreviewTarget {
        id,
        label,
        source: PreviewTargetSource::Config,
        relative_root,
        kind,
        recommended,
    })
}

enum ConfiguredTargetKind {
    PackageScript {
        package_manager: PreviewPackageManager,
        script: String,
    },
    StaticSite {
        entrypoint: String,
    },
}

fn push_target(targets: &mut Vec<PreviewTarget>, target: PreviewTarget) -> Result<(), String> {
    if targets.iter().any(|existing| existing.id == target.id) {
        return Err(format!(
            "duplicate Preview target id {}",
            target.id.as_str()
        ));
    }
    if targets.len() >= MAX_TARGETS {
        return Err("Preview discovery exceeded its target bound".into());
    }
    targets.push(target);
    Ok(())
}

fn package_manager(path: &Path, root: &Path) -> PreviewPackageManager {
    for directory in [path, root] {
        if directory.join("pnpm-lock.yaml").is_file() {
            return PreviewPackageManager::Pnpm;
        }
        if directory.join("yarn.lock").is_file() {
            return PreviewPackageManager::Yarn;
        }
        if directory.join("bun.lockb").is_file() || directory.join("bun.lock").is_file() {
            return PreviewPackageManager::Bun;
        }
    }
    PreviewPackageManager::Npm
}

fn target_id(relative_root: &str, script: &str) -> Result<PreviewTargetId, String> {
    let workspace = if relative_root.is_empty() {
        "root".to_string()
    } else {
        format!("workspace:{:x}", Sha256::digest(relative_root.as_bytes()))
    };
    PreviewTargetId::parse(format!("{workspace}:{script}"))
}

fn target_label(name: Option<&str>, relative_root: &str, script: &str) -> String {
    let owner =
        name.filter(|name| !name.trim().is_empty())
            .unwrap_or(if relative_root.is_empty() {
                "Root"
            } else {
                relative_root
            });
    format!("{owner}: {script}")
}

fn normalize_relative(value: &str) -> Result<String, String> {
    let normalized = value.trim().replace('\\', "/");
    let path = Path::new(&normalized);
    if path.is_absolute()
        || path.components().any(|component| {
            matches!(
                component,
                Component::ParentDir | Component::RootDir | Component::Prefix(_)
            )
        })
    {
        return Err("Preview paths must be canonical-root-relative".into());
    }
    Ok(normalized.trim_matches('/').to_string())
}

fn confined_canonical(root: &Path, candidate: &Path) -> Result<PathBuf, String> {
    reject_symlink_components(root, candidate)?;
    let canonical = fs::canonicalize(candidate)
        .map_err(|error| format!("canonicalize {}: {error}", candidate.display()))?;
    if !canonical.starts_with(root) {
        return Err(format!(
            "Preview path escapes canonical root: {}",
            candidate.display()
        ));
    }
    Ok(canonical)
}

fn reject_symlink_components(root: &Path, candidate: &Path) -> Result<(), String> {
    let relative = candidate
        .strip_prefix(root)
        .map_err(|_| "Preview path is outside the canonical root".to_string())?;
    let mut current = root.to_path_buf();
    for component in relative.components() {
        current.push(component);
        let metadata = fs::symlink_metadata(&current)
            .map_err(|error| format!("inspect {}: {error}", current.display()))?;
        if metadata.file_type().is_symlink() || metadata_is_reparse(&metadata) {
            return Err(format!(
                "Preview path contains a symlink: {}",
                current.display()
            ));
        }
    }
    Ok(())
}

fn relative_slash(root: &Path, path: &Path) -> Result<String, String> {
    path.strip_prefix(root)
        .map(|relative| relative.to_string_lossy().replace('\\', "/"))
        .map_err(|_| format!("{} is outside Preview root", path.display()))
}

fn read_optional_bounded(
    path: &Path,
    root: &Path,
    fingerprint: &mut Sha256,
) -> Result<Option<Vec<u8>>, String> {
    match fs::symlink_metadata(path) {
        Ok(_) => read_required_bounded(path, root, fingerprint).map(Some),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            fingerprint.update(b"missing\0");
            fingerprint.update(relative_slash(root, path)?.as_bytes());
            Ok(None)
        }
        Err(error) => Err(format!("inspect {}: {error}", path.display())),
    }
}

fn path_is_link_or_reparse(path: &Path) -> Result<bool, String> {
    let metadata = fs::symlink_metadata(path)
        .map_err(|error| format!("inspect {}: {error}", path.display()))?;
    Ok(metadata.file_type().is_symlink() || metadata_is_reparse(&metadata))
}

#[cfg(windows)]
fn metadata_is_reparse(metadata: &fs::Metadata) -> bool {
    use std::os::windows::fs::MetadataExt;

    const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;
    metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
}

#[cfg(not(windows))]
fn metadata_is_reparse(_metadata: &fs::Metadata) -> bool {
    false
}

fn read_required_bounded(
    path: &Path,
    root: &Path,
    fingerprint: &mut Sha256,
) -> Result<Vec<u8>, String> {
    let canonical = confined_canonical(root, path)?;
    let metadata = fs::metadata(&canonical)
        .map_err(|error| format!("inspect {}: {error}", canonical.display()))?;
    if !metadata.is_file() || metadata.len() > MAX_FILE_BYTES {
        return Err(format!(
            "Preview file is absent or too large: {}",
            path.display()
        ));
    }
    let bytes =
        fs::read(&canonical).map_err(|error| format!("read {}: {error}", canonical.display()))?;
    fingerprint.update(relative_slash(root, &canonical)?.as_bytes());
    fingerprint.update((bytes.len() as u64).to_le_bytes());
    fingerprint.update(&bytes);
    Ok(bytes)
}

fn fingerprint_metadata(root: &Path, path: &Path, fingerprint: &mut Sha256) -> Result<(), String> {
    let metadata =
        fs::metadata(path).map_err(|error| format!("inspect {}: {error}", path.display()))?;
    fingerprint.update(relative_slash(root, path)?.as_bytes());
    fingerprint.update(metadata.len().to_le_bytes());
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        fingerprint.update(metadata.mtime().to_le_bytes());
        fingerprint.update(metadata.mtime_nsec().to_le_bytes());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    static TEMP_SEQUENCE: AtomicU64 = AtomicU64::new(0);

    fn fixture(tag: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!(
            "t-hub-preview-discovery-{tag}-{}-{}",
            std::process::id(),
            TEMP_SEQUENCE.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&path).unwrap();
        path
    }

    #[test]
    fn discovers_root_and_bounded_workspace_scripts_deterministically() {
        let root = fixture("workspaces");
        fs::write(
            root.join("package.json"),
            r#"{"name":"app","workspaces":["packages/*"],"scripts":{"dev":"vite","test":"x"}}"#,
        )
        .unwrap();
        fs::write(root.join("pnpm-lock.yaml"), "lockfileVersion: 9").unwrap();
        fs::create_dir_all(root.join("packages/web")).unwrap();
        fs::write(
            root.join("packages/web/package.json"),
            r#"{"name":"web","scripts":{"preview":"vite preview"}}"#,
        )
        .unwrap();

        let result = discover(&root).unwrap();
        assert_eq!(result.targets.len(), 2);
        assert_eq!(result.targets[0].id.as_str(), "root:dev");
        assert!(matches!(
            result.targets[0].kind,
            PreviewTargetKind::PackageScript {
                package_manager: PreviewPackageManager::Pnpm,
                ..
            }
        ));
        assert_eq!(result.targets[1].relative_root, "packages/web");
        assert_eq!(result, discover(&root).unwrap());
    }

    #[test]
    fn fingerprint_changes_when_relevant_manifest_changes() {
        let root = fixture("fingerprint");
        fs::write(root.join("package.json"), r#"{"scripts":{"dev":"vite"}}"#).unwrap();
        let first = discover(&root).unwrap();
        fs::write(root.join("package.json"), r#"{"scripts":{"start":"vite"}}"#).unwrap();
        let second = discover(&root).unwrap();
        assert_ne!(first.discovery_fingerprint, second.discovery_fingerprint);
        assert_ne!(first.targets, second.targets);
    }

    #[test]
    fn accepts_only_typed_config_targets_with_confined_paths() {
        let root = fixture("config");
        fs::create_dir_all(root.join(".t-hub")).unwrap();
        fs::create_dir_all(root.join("site")).unwrap();
        fs::write(root.join("site/index.html"), "hello").unwrap();
        fs::write(
            root.join(".t-hub/preview.json"),
            r#"{"schemaVersion":1,"targets":[{"id":"docs","label":"Docs","relativeRoot":"site","type":"staticSite","entrypoint":"index.html","recommended":true}]}"#,
        )
        .unwrap();
        let result = discover(&root).unwrap();
        assert_eq!(result.targets[0].id.as_str(), "docs");
        assert!(matches!(
            result.targets[0].kind,
            PreviewTargetKind::StaticSite { .. }
        ));

        fs::write(
            root.join(".t-hub/preview.json"),
            r#"{"schemaVersion":1,"targets":[{"id":"bad","label":"Bad","relativeRoot":"../outside","type":"staticSite","entrypoint":"index.html"}]}"#,
        )
        .unwrap();
        assert!(discover(&root).is_err());

        fs::write(
            root.join(".t-hub/preview.json"),
            r#"{"schemaVersion":1,"targets":[{"id":"bad","label":"Bad","type":"staticSite","entrypoint":"site/index.html","command":"curl evil"}]}"#,
        )
        .unwrap();
        assert!(discover(&root).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn rejects_symlinked_workspace_and_config_paths() {
        use std::os::unix::fs::symlink;

        let root = fixture("symlink");
        let outside = fixture("outside");
        fs::write(outside.join("index.html"), "secret").unwrap();
        fs::create_dir_all(root.join(".t-hub")).unwrap();
        symlink(&outside, root.join("linked")).unwrap();
        fs::write(
            root.join(".t-hub/preview.json"),
            r#"{"schemaVersion":1,"targets":[{"id":"bad","label":"Bad","relativeRoot":"linked","type":"staticSite","entrypoint":"index.html"}]}"#,
        )
        .unwrap();
        assert!(discover(&root).is_err());
    }
}
