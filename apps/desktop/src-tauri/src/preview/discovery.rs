//! Bounded, deterministic Preview target discovery.

use std::collections::{BTreeSet, HashMap};
use std::fs;
use std::io::Read;
use std::path::{Component, Path, PathBuf};

use cap_fs_ext::{
    ambient_authority, DirExt, FollowSymlinks, OpenOptionsFollowExt, OpenOptionsMaybeDirExt,
};
use cap_std::fs::{Dir as CapDir, File as CapFile, OpenOptions as CapOpenOptions};
use parking_lot::Mutex;
use serde::Deserialize;
use sha2::{Digest, Sha256};

use super::model::{
    PreviewPackageManager, PreviewTarget, PreviewTargetId, PreviewTargetKind, PreviewTargetSource,
};

const MAX_FILE_BYTES: u64 = 1024 * 1024;
const MAX_DEPTH: usize = 8;
const MAX_DIRECTORY_ENTRIES: usize = 4096;
const MAX_VISITED_DIRECTORIES: usize = 4096;
const MAX_TARGETS: usize = 256;
const MAX_WORKSPACE_PATTERNS: usize = 256;
const MAX_PATTERN_BYTES: usize = 512;
const MAX_LABEL_BYTES: usize = 200;
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
    let root_directory = open_root(&canonical_root)?;

    let mut fingerprint = Sha256::new();
    let root_identity = canonical_root.to_string_lossy().replace('\\', "/");
    let canonical_root_fingerprint = format!("sha256:{:x}", Sha256::digest(root_identity));
    let root_manifest =
        read_relative_optional(&root_directory, Path::new("package.json"), &mut fingerprint)?;
    let config = read_relative_optional(
        &root_directory,
        Path::new(".t-hub/preview.json"),
        &mut fingerprint,
    )?;
    for lock_file in LOCK_FILES {
        let _ = read_relative_optional(&root_directory, Path::new(lock_file), &mut fingerprint)?;
    }

    let mut workspace_patterns = Vec::new();
    if let Some(bytes) = root_manifest.as_deref() {
        workspace_patterns.extend(package_workspace_patterns(bytes)?);
    }
    if let Some(bytes) = read_relative_optional(
        &root_directory,
        Path::new("pnpm-workspace.yaml"),
        &mut fingerprint,
    )? {
        workspace_patterns.extend(pnpm_workspace_patterns(&bytes)?);
    }
    workspace_patterns.sort();
    workspace_patterns.dedup();
    if workspace_patterns.len() > MAX_WORKSPACE_PATTERNS {
        return Err("Preview workspace discovery exceeds its pattern bound".into());
    }

    let mut manifest_roots = vec![DiscoveryDirectory {
        relative: String::new(),
        source: PreviewTargetSource::Root,
        directory: root_directory
            .try_clone()
            .map_err(|error| format!("clone Preview root handle: {error}"))?,
    }];
    if !workspace_patterns.is_empty() {
        for mut directory in bounded_directories(&root_directory)? {
            if workspace_patterns
                .iter()
                .any(|pattern| glob_matches(pattern, &directory.relative))
                && regular_file_exists(&directory.directory, Path::new("package.json"))?
            {
                directory.source = PreviewTargetSource::WorkspaceManifest;
                manifest_roots.push(directory);
            }
        }
    }
    manifest_roots.sort_by(|left, right| left.relative.cmp(&right.relative));
    manifest_roots.dedup_by(|left, right| left.relative == right.relative);

    let mut targets = Vec::new();
    for manifest_root in manifest_roots {
        let bytes = if manifest_root.relative.is_empty() {
            root_manifest.clone()
        } else {
            read_in_directory_optional(
                &manifest_root.directory,
                Path::new("package.json"),
                &joined_relative(&manifest_root.relative, "package.json"),
                &mut fingerprint,
            )?
        };
        let Some(bytes) = bytes else { continue };
        let package = parse_package_manifest(&bytes)?;
        let relative_root = manifest_root.relative.clone();
        if !relative_root.is_empty() {
            for lock_file in LOCK_FILES {
                let _ = read_in_directory_optional(
                    &manifest_root.directory,
                    Path::new(lock_file),
                    &joined_relative(&relative_root, lock_file),
                    &mut fingerprint,
                )?;
            }
        }
        let manager = package_manager(&manifest_root.directory, &root_directory)?;
        for script in PACKAGE_SCRIPTS {
            if package.scripts.contains(script) {
                push_target(
                    &mut targets,
                    PreviewTarget {
                        id: target_id(&relative_root, script)?,
                        label: target_label(package.name.as_deref(), &relative_root, script),
                        source: manifest_root.source,
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
            let target = accept_config_target(&root_directory, configured, &mut fingerprint)?;
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
        || normalized.len() > MAX_PATTERN_BYTES
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

struct DiscoveryDirectory {
    relative: String,
    source: PreviewTargetSource,
    directory: CapDir,
}

fn bounded_directories(root: &CapDir) -> Result<Vec<DiscoveryDirectory>, String> {
    let mut result = Vec::new();
    let mut pending = vec![(
        root.try_clone()
            .map_err(|error| format!("clone Preview root handle: {error}"))?,
        String::new(),
        0usize,
    )];
    let mut visited = 0usize;
    while let Some((directory, relative, depth)) = pending.pop() {
        if depth >= MAX_DEPTH {
            continue;
        }
        let mut children = Vec::new();
        for entry in directory
            .entries()
            .map_err(|error| format!("read Preview directory {relative:?}: {error}"))?
        {
            if children.len() >= MAX_DIRECTORY_ENTRIES {
                return Err(format!(
                    "Preview workspace discovery exceeded the entry bound in {relative:?}"
                ));
            }
            children.push(entry.map_err(|error| {
                format!("read Preview directory entry in {relative:?}: {error}")
            })?);
        }
        children.sort_by_key(|entry| entry.file_name());
        for child in children.into_iter().rev() {
            let name = child.file_name();
            if matches!(name.to_str(), Some(".git" | "node_modules" | "target")) {
                continue;
            }
            let Ok(opened) = directory.open_dir_nofollow(&name) else {
                continue;
            };
            let metadata = opened
                .dir_metadata()
                .map_err(|error| format!("inspect Preview directory entry {name:?}: {error}"))?;
            if cap_metadata_has_reparse_point(&metadata) {
                continue;
            }
            visited += 1;
            if visited > MAX_VISITED_DIRECTORIES {
                return Err("Preview workspace discovery exceeded its directory bound".into());
            }
            let name = name
                .to_str()
                .ok_or_else(|| "Preview paths must be valid UTF-8".to_string())?;
            let child_relative = joined_relative(&relative, name);
            result.push(DiscoveryDirectory {
                relative: child_relative.clone(),
                source: PreviewTargetSource::WorkspaceManifest,
                directory: opened
                    .try_clone()
                    .map_err(|error| format!("clone Preview directory handle: {error}"))?,
            });
            pending.push((opened, child_relative, depth + 1));
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
    root: &CapDir,
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
    let target_root = open_relative_directory(root, Path::new(&relative_root))?;
    let kind = match kind {
        ConfiguredTargetKind::PackageScript {
            package_manager,
            script,
        } => {
            if !PACKAGE_SCRIPTS.contains(&script.as_str()) {
                return Err("configured package script must be dev, preview, or start".into());
            }
            let manifest = read_in_directory_required(
                &target_root,
                Path::new("package.json"),
                &joined_relative(&relative_root, "package.json"),
                fingerprint,
            )?;
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
            fingerprint_relative_file(
                &target_root,
                Path::new(&entrypoint),
                &joined_relative(&relative_root, &entrypoint),
                fingerprint,
            )?;
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

fn package_manager(path: &CapDir, root: &CapDir) -> Result<PreviewPackageManager, String> {
    for directory in [path, root] {
        if regular_file_exists(directory, Path::new("pnpm-lock.yaml"))? {
            return Ok(PreviewPackageManager::Pnpm);
        }
        if regular_file_exists(directory, Path::new("yarn.lock"))? {
            return Ok(PreviewPackageManager::Yarn);
        }
        if regular_file_exists(directory, Path::new("bun.lockb"))?
            || regular_file_exists(directory, Path::new("bun.lock"))?
        {
            return Ok(PreviewPackageManager::Bun);
        }
    }
    Ok(PreviewPackageManager::Npm)
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
    bounded_text(&format!("{owner}: {script}"), MAX_LABEL_BYTES)
}

fn bounded_text(value: &str, max_bytes: usize) -> String {
    if value.len() <= max_bytes {
        return value.to_string();
    }
    let mut end = max_bytes;
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    value[..end].to_string()
}

fn normalize_relative(value: &str) -> Result<String, String> {
    let normalized = value.trim().replace('\\', "/");
    let path = Path::new(&normalized);
    if path.is_absolute() {
        return Err("Preview paths must be canonical-root-relative".into());
    }
    let mut parts = Vec::new();
    for component in path.components() {
        match component {
            Component::Normal(part) => parts.push(
                part.to_str()
                    .ok_or_else(|| "Preview paths must be valid UTF-8".to_string())?,
            ),
            Component::CurDir => {}
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => {
                return Err("Preview paths must be canonical-root-relative".into());
            }
        }
    }
    Ok(parts.join("/"))
}

fn nofollow_options(maybe_dir: bool) -> CapOpenOptions {
    let mut options = CapOpenOptions::new();
    options
        .read(true)
        .follow(FollowSymlinks::No)
        .maybe_dir(maybe_dir);
    options
}

fn cap_metadata_has_reparse_point(metadata: &cap_std::fs::Metadata) -> bool {
    #[cfg(windows)]
    {
        use cap_fs_ext::OsMetadataExt;
        metadata.file_attributes() & 0x400 != 0
    }
    #[cfg(not(windows))]
    {
        let _ = metadata;
        false
    }
}

fn open_root(root: &Path) -> Result<CapDir, String> {
    let file = CapFile::open_ambient_with(root, &nofollow_options(true), ambient_authority())
        .map_err(|error| format!("open Preview root {}: {error}", root.display()))?;
    let metadata = file
        .metadata()
        .map_err(|error| format!("inspect Preview root {}: {error}", root.display()))?;
    if !metadata.is_dir() || cap_metadata_has_reparse_point(&metadata) {
        return Err("Preview root must be a regular directory".into());
    }
    Ok(CapDir::from_std_file(file.into_std()))
}

fn open_relative_directory(root: &CapDir, relative: &Path) -> Result<CapDir, String> {
    let mut directory = root
        .try_clone()
        .map_err(|error| format!("clone Preview root handle: {error}"))?;
    for component in relative.components() {
        let name = match component {
            Component::Normal(name) => name,
            Component::CurDir => continue,
            _ => return Err("Preview paths must be canonical-root-relative".into()),
        };
        directory = directory
            .open_dir_nofollow(name)
            .map_err(|error| format!("open Preview directory {}: {error}", relative.display()))?;
        let metadata = directory.dir_metadata().map_err(|error| {
            format!("inspect Preview directory {}: {error}", relative.display())
        })?;
        if cap_metadata_has_reparse_point(&metadata) {
            return Err(format!(
                "Preview path contains a reparse point: {}",
                relative.display()
            ));
        }
    }
    Ok(directory)
}

fn read_relative_optional(
    root: &CapDir,
    relative: &Path,
    fingerprint: &mut Sha256,
) -> Result<Option<Vec<u8>>, String> {
    let display = relative.to_string_lossy().replace('\\', "/");
    let Some(name) = relative.file_name() else {
        return Err("Preview file path must include a file name".into());
    };
    let parent = relative.parent().unwrap_or_else(|| Path::new(""));
    let directory = match open_relative_directory(root, parent) {
        Ok(directory) => directory,
        Err(error) if error.contains("No such file") => {
            fingerprint_missing(&display, fingerprint);
            return Ok(None);
        }
        Err(error) => return Err(error),
    };
    read_in_directory_optional(&directory, name.as_ref(), &display, fingerprint)
}

fn read_in_directory_optional(
    directory: &CapDir,
    name: &Path,
    display: &str,
    fingerprint: &mut Sha256,
) -> Result<Option<Vec<u8>>, String> {
    match directory.open_with(name, &nofollow_options(false)) {
        Ok(file) => read_opened_bounded(file, display, fingerprint).map(Some),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            fingerprint_missing(display, fingerprint);
            Ok(None)
        }
        Err(error) => Err(format!("open Preview file {display}: {error}")),
    }
}

fn read_in_directory_required(
    directory: &CapDir,
    name: &Path,
    display: &str,
    fingerprint: &mut Sha256,
) -> Result<Vec<u8>, String> {
    let file = directory
        .open_with(name, &nofollow_options(false))
        .map_err(|error| format!("open Preview file {display}: {error}"))?;
    read_opened_bounded(file, display, fingerprint)
}

fn read_opened_bounded(
    file: CapFile,
    display: &str,
    fingerprint: &mut Sha256,
) -> Result<Vec<u8>, String> {
    let metadata = file
        .metadata()
        .map_err(|error| format!("inspect Preview file {display}: {error}"))?;
    if !metadata.is_file()
        || metadata.len() > MAX_FILE_BYTES
        || cap_metadata_has_reparse_point(&metadata)
    {
        return Err(format!(
            "Preview file is not regular or exceeds its size bound: {display}"
        ));
    }
    let mut bytes = Vec::new();
    file.take(MAX_FILE_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| format!("read Preview file {display}: {error}"))?;
    if bytes.len() as u64 > MAX_FILE_BYTES {
        return Err(format!("Preview file exceeds its size bound: {display}"));
    }
    fingerprint.update(display.as_bytes());
    fingerprint.update((bytes.len() as u64).to_le_bytes());
    fingerprint.update(&bytes);
    Ok(bytes)
}

fn fingerprint_relative_file(
    root: &CapDir,
    relative: &Path,
    display: &str,
    fingerprint: &mut Sha256,
) -> Result<(), String> {
    let Some(name) = relative.file_name() else {
        return Err("configured static Preview entrypoint must be a file".into());
    };
    let directory =
        open_relative_directory(root, relative.parent().unwrap_or_else(|| Path::new("")))?;
    let file = directory
        .open_with(name, &nofollow_options(false))
        .map_err(|error| format!("open configured static Preview entrypoint {display}: {error}"))?;
    let metadata = file.metadata().map_err(|error| {
        format!("inspect configured static Preview entrypoint {display}: {error}")
    })?;
    if !metadata.is_file() || cap_metadata_has_reparse_point(&metadata) {
        return Err("configured static Preview entrypoint must be a regular file".into());
    }
    fingerprint.update(display.as_bytes());
    fingerprint.update(metadata.len().to_le_bytes());
    if let Ok(modified) = metadata.modified() {
        if let Ok(elapsed) = modified.into_std().duration_since(std::time::UNIX_EPOCH) {
            fingerprint.update(elapsed.as_secs().to_le_bytes());
            fingerprint.update(elapsed.subsec_nanos().to_le_bytes());
        }
    }
    Ok(())
}

fn regular_file_exists(directory: &CapDir, name: &Path) -> Result<bool, String> {
    match directory.open_with(name, &nofollow_options(false)) {
        Ok(file) => {
            let metadata = file
                .metadata()
                .map_err(|error| format!("inspect Preview file {}: {error}", name.display()))?;
            Ok(metadata.is_file() && !cap_metadata_has_reparse_point(&metadata))
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(format!("open Preview file {}: {error}", name.display())),
    }
}

fn fingerprint_missing(display: &str, fingerprint: &mut Sha256) {
    fingerprint.update(b"missing\0");
    fingerprint.update(display.as_bytes());
}

fn joined_relative(parent: &str, child: &str) -> String {
    if parent.is_empty() {
        child.to_string()
    } else {
        format!("{parent}/{child}")
    }
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
    fn generated_labels_and_workspace_patterns_are_bounded() {
        let root = fixture("bounds");
        let long_name = "船".repeat(300);
        fs::write(
            root.join("package.json"),
            format!(r#"{{"name":"{long_name}","scripts":{{"dev":"vite"}}}}"#),
        )
        .unwrap();
        let result = discover(&root).unwrap();
        assert!(result.targets[0].label.len() <= MAX_LABEL_BYTES);
        assert!(result.targets[0]
            .label
            .is_char_boundary(result.targets[0].label.len()));
        assert!(validate_pattern(&"x".repeat(MAX_PATTERN_BYTES + 1)).is_err());
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

    #[cfg(unix)]
    #[test]
    fn retained_directory_handle_cannot_be_rebound_to_an_outside_symlink() {
        use std::os::unix::fs::symlink;

        let root = fixture("retained-handle");
        let outside = fixture("retained-handle-outside");
        fs::create_dir(root.join("site")).unwrap();
        fs::write(root.join("site/index.html"), "inside").unwrap();
        fs::write(outside.join("index.html"), "outside").unwrap();

        let canonical_root = fs::canonicalize(&root).unwrap();
        let root_handle = open_root(&canonical_root).unwrap();
        let site_handle = open_relative_directory(&root_handle, Path::new("site")).unwrap();
        fs::rename(root.join("site"), root.join("original-site")).unwrap();
        symlink(&outside, root.join("site")).unwrap();

        let mut fingerprint = Sha256::new();
        let bytes = read_in_directory_required(
            &site_handle,
            Path::new("index.html"),
            "site/index.html",
            &mut fingerprint,
        )
        .unwrap();
        assert_eq!(bytes, b"inside");
        assert!(open_relative_directory(&root_handle, Path::new("site")).is_err());
    }
}
