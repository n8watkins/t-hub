//! Shared managed Preview command construction.
//!
//! This module prepares only backend-selected typed commands.
//! Process reservation, durable identity, bounded output, endpoint ownership,
//! and cleanup remain the caller's responsibility.

use std::io::Read;
use std::path::Path;
use std::process::{Command, Stdio};

use cap_fs_ext::{ambient_authority, FollowSymlinks, OpenOptionsFollowExt};
use cap_std::fs::{Dir as CapDir, OpenOptions as CapOpenOptions};
use parking_lot::Mutex;

use super::endpoint::{ListenerOwnership, ManagedRunIdentity};
use super::model::{PreviewPackageManager, PreviewTarget, PreviewTargetKind};

const MAX_MANAGED_OUTPUT_BYTES: usize = 64 * 1024;
const MAX_MARKER_BYTES: u64 = 512;
const MAX_PROC_STAT_BYTES: u64 = 4096;
#[cfg(target_os = "linux")]
const MAX_PROC_ENTRIES: usize = 32_768;
#[cfg(target_os = "linux")]
const MAX_FDS_PER_PROCESS: usize = 4096;

#[derive(Default)]
pub(crate) struct BoundedOutput {
    bytes: Mutex<Vec<u8>>,
}

impl BoundedOutput {
    pub(crate) fn append(&self, chunk: &[u8]) {
        let mut bytes = self.bytes.lock();
        if chunk.len() >= MAX_MANAGED_OUTPUT_BYTES {
            bytes.clear();
            bytes.extend_from_slice(&chunk[chunk.len() - MAX_MANAGED_OUTPUT_BYTES..]);
            return;
        }
        let excess = bytes
            .len()
            .saturating_add(chunk.len())
            .saturating_sub(MAX_MANAGED_OUTPUT_BYTES);
        if excess > 0 {
            bytes.drain(..excess);
        }
        bytes.extend_from_slice(chunk);
    }

    pub(crate) fn snapshot(&self) -> Vec<u8> {
        self.bytes.lock().clone()
    }
}

/// Supervise the complete package-manager process group behind a stdin
/// lifeline.
///
/// The package manager and validated script remain argv data after this fixed
/// shell program.
/// EOF from T-Hub triggers TERM, a bounded grace period, and KILL for the exact
/// owned process group.
pub(crate) const PROCESS_TREE_SCRIPT: &str = r#"set -u
case "$0" in
  t-hub-preview|t-hub-devserver) ;;
  *) exit 64 ;;
esac
case "$1" in
  ""|*[!A-Za-z0-9_.:-]*) exit 65 ;;
esac
MARKER="/tmp/$0-$1.pid"
shift
export HOST=0.0.0.0 HOSTNAME=0.0.0.0 NUXT_HOST=0.0.0.0 ASTRO_HOST=0.0.0.0 TAURI_DEV_HOST=0.0.0.0
exec 3<&0
setsid "$@" 3<&- </dev/null &
SRV=$!
cleanup() {
  kill -TERM -- -"$SRV" 2>/dev/null || true
  i=0
  while kill -0 "$SRV" 2>/dev/null && [ "$i" -lt 20 ]; do
    sleep 0.1
    i=$((i + 1))
  done
  kill -KILL -- -"$SRV" 2>/dev/null || true
  wait "$SRV" 2>/dev/null || true
  rm -f "$MARKER" 2>/dev/null || true
}
STAT=$(cat "/proc/$SRV/stat" 2>/dev/null) || { cleanup; exit 66; }
REST=${STAT##*) }
set -- $REST
START_TICKS=${20:-}
case "$START_TICKS" in
  ""|*[!0-9]*) cleanup; exit 67 ;;
esac
umask 077
TMP=$(mktemp "${MARKER}.XXXXXX") || { cleanup; exit 68; }
if ! printf '%s %s\n' "$SRV" "$START_TICKS" > "$TMP" || ! mv -f "$TMP" "$MARKER"; then
  rm -f "$TMP" 2>/dev/null || true
  cleanup
  exit 69
fi
trap 'cleanup; exit 0' TERM INT HUP
(cat <&3 >/dev/null; kill -TERM "$$" 2>/dev/null || true) &
LIFE=$!
wait "$SRV"
CODE=$?
kill "$LIFE" 2>/dev/null || true
wait "$LIFE" 2>/dev/null || true
cleanup
exit "$CODE"
"#;

pub(crate) fn package_command(cwd: &Path, run_id: &str, executable: &str, script: &str) -> Command {
    package_command_with_marker(cwd, run_id, executable, script, "t-hub-preview")
}

pub(crate) fn package_command_with_marker(
    cwd: &Path,
    run_id: &str,
    executable: &str,
    script: &str,
    marker_namespace: &str,
) -> Command {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        let posix_cwd = unc_to_posix(&cwd.to_string_lossy())
            .unwrap_or_else(|| cwd.to_string_lossy().into_owned());
        let mut command = Command::new("wsl.exe");
        command.arg("-d").arg(crate::files::host_distro());
        if !posix_cwd.is_empty() {
            command.arg("--cd").arg(posix_cwd);
        }
        command
            .arg("-e")
            .arg("bash")
            .arg("-c")
            .arg(PROCESS_TREE_SCRIPT)
            .arg(marker_namespace)
            .arg(run_id)
            .arg(executable)
            .arg("run")
            .arg(script);
        command.creation_flags(0x0800_0000);
        command
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .stdin(Stdio::piped());
        command
    }
    #[cfg(not(windows))]
    {
        let mut command = Command::new("bash");
        command
            .arg("-c")
            .arg(PROCESS_TREE_SCRIPT)
            .arg(marker_namespace)
            .arg(run_id)
            .arg(executable)
            .arg("run")
            .arg(script)
            .current_dir(cwd)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .stdin(Stdio::piped());
        command
    }
}

pub(crate) fn typed_package_command(
    canonical_root: &Path,
    target: &PreviewTarget,
    run_id: &str,
) -> Result<Command, String> {
    validate_run_id(run_id)?;
    let relative_root = Path::new(&target.relative_root);
    if relative_root.is_absolute()
        || relative_root
            .components()
            .any(|component| matches!(component, std::path::Component::ParentDir))
    {
        return Err("Preview target root is not confined to its canonical Project".into());
    }
    let cwd = canonical_root.join(relative_root);
    let PreviewTargetKind::PackageScript {
        package_manager,
        script,
    } = &target.kind
    else {
        return Err("static Preview targets require the supervised static helper".into());
    };
    Ok(package_command(
        &cwd,
        run_id,
        package_manager_executable(*package_manager),
        script,
    ))
}

fn validate_run_id(run_id: &str) -> Result<(), String> {
    if run_id.is_empty()
        || run_id.len() > 160
        || !run_id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b':'))
    {
        return Err("managed Preview run id has an invalid marker identity".into());
    }
    Ok(())
}

#[cfg(target_os = "linux")]
pub(crate) fn observe_marker_identity(run_id: &str) -> Result<ManagedRunIdentity, String> {
    validate_run_id(run_id)?;
    let temporary = CapDir::open_ambient_dir("/tmp", ambient_authority())
        .map_err(|error| format!("open managed Preview marker directory: {error}"))?;
    let mut options = CapOpenOptions::new();
    options.read(true).follow(FollowSymlinks::No);
    let marker_name = format!("t-hub-preview-{run_id}.pid");
    let marker = temporary
        .open_with(&marker_name, &options)
        .map_err(|error| format!("open managed Preview identity marker: {error}"))?;
    let metadata = marker
        .metadata()
        .map_err(|error| format!("inspect managed Preview identity marker: {error}"))?;
    if !metadata.is_file() || metadata.len() > MAX_MARKER_BYTES {
        return Err("managed Preview identity marker is not a bounded regular file".into());
    }
    let mut marker_bytes = Vec::new();
    marker
        .take(MAX_MARKER_BYTES + 1)
        .read_to_end(&mut marker_bytes)
        .map_err(|error| format!("read managed Preview identity marker: {error}"))?;
    if marker_bytes.len() as u64 > MAX_MARKER_BYTES {
        return Err("managed Preview identity marker exceeds its bound".into());
    }
    let marker_text = std::str::from_utf8(&marker_bytes)
        .map_err(|_| "managed Preview identity marker is not UTF-8")?;
    let fields = marker_text.split_whitespace().collect::<Vec<_>>();
    if fields.len() != 2 {
        return Err("managed Preview identity marker has an invalid shape".into());
    }
    let process_group_id = fields[0]
        .parse::<u32>()
        .map_err(|_| "managed Preview identity marker has an invalid process group")?;
    let process_group_started_at = fields[1]
        .parse::<u64>()
        .map_err(|_| "managed Preview identity marker has invalid start ticks")?;
    let identity = ManagedRunIdentity {
        run_id: run_id.to_string(),
        process_group_id,
        process_group_started_at,
    };
    identity.validate()?;
    revalidate_process_identity(&identity)?;
    Ok(identity)
}

#[cfg(target_os = "linux")]
pub(crate) fn revalidate_process_identity(identity: &ManagedRunIdentity) -> Result<(), String> {
    identity.validate()?;
    let stat_path = format!("/proc/{}/stat", identity.process_group_id);
    let file = std::fs::File::open(&stat_path)
        .map_err(|error| format!("open managed Preview process identity: {error}"))?;
    let metadata = file
        .metadata()
        .map_err(|error| format!("inspect managed Preview process identity: {error}"))?;
    if !metadata.is_file() || metadata.len() > MAX_PROC_STAT_BYTES {
        return Err("managed Preview process stat is not a bounded regular file".into());
    }
    let mut stat = String::new();
    file.take(MAX_PROC_STAT_BYTES + 1)
        .read_to_string(&mut stat)
        .map_err(|error| format!("read managed Preview process identity: {error}"))?;
    if stat.len() as u64 > MAX_PROC_STAT_BYTES {
        return Err("managed Preview process stat exceeds its bound".into());
    }
    let fields = stat
        .rsplit_once(") ")
        .ok_or("managed Preview process stat has an invalid shape")?
        .1
        .split_whitespace()
        .collect::<Vec<_>>();
    let process_group = fields
        .get(2)
        .and_then(|value| value.parse::<u32>().ok())
        .ok_or("managed Preview process stat has an invalid process group")?;
    let start_ticks = fields
        .get(19)
        .and_then(|value| value.parse::<u64>().ok())
        .ok_or("managed Preview process stat has invalid start ticks")?;
    if process_group != identity.process_group_id
        || start_ticks != identity.process_group_started_at
    {
        return Err("managed Preview process identity no longer matches".into());
    }
    Ok(())
}

#[cfg(target_os = "linux")]
pub(crate) fn listener_ownership(port: u16) -> Result<Option<ListenerOwnership>, String> {
    if port == 0 {
        return Err("managed Preview listener port must be nonzero".into());
    }
    let mut socket_inodes = std::collections::BTreeSet::new();
    for table in ["/proc/net/tcp", "/proc/net/tcp6"] {
        let contents = std::fs::read_to_string(table)
            .map_err(|error| format!("read managed Preview listener table {table}: {error}"))?;
        if contents.len() > 8 * 1024 * 1024 {
            return Err("managed Preview listener table exceeds its bound".into());
        }
        for line in contents.lines().skip(1) {
            let fields = line.split_whitespace().collect::<Vec<_>>();
            let Some((_, encoded_port)) = fields.get(1).and_then(|local| local.rsplit_once(':'))
            else {
                continue;
            };
            let Some(observed_port) = u16::from_str_radix(encoded_port, 16).ok() else {
                continue;
            };
            if observed_port == port && fields.get(3) == Some(&"0A") {
                let inode = fields
                    .get(9)
                    .ok_or("managed Preview listener table omitted its socket inode")?;
                if !inode.bytes().all(|byte| byte.is_ascii_digit()) {
                    return Err("managed Preview listener table has an invalid socket inode".into());
                }
                socket_inodes.insert((*inode).to_string());
            }
        }
    }
    if socket_inodes.is_empty() {
        return Ok(None);
    }

    let mut processes = std::fs::read_dir("/proc")
        .map_err(|error| format!("enumerate managed Preview listener owners: {error}"))?
        .filter_map(Result::ok)
        .filter_map(|entry| {
            let name = entry.file_name();
            let name = name.to_str()?;
            if !name.bytes().all(|byte| byte.is_ascii_digit()) {
                return None;
            }
            Some((name.parse::<u32>().ok()?, entry.path()))
        })
        .collect::<Vec<_>>();
    if processes.len() > MAX_PROC_ENTRIES {
        return Err("managed Preview process enumeration exceeds its bound".into());
    }
    processes.sort_by_key(|(pid, _)| *pid);

    let mut ownership = std::collections::BTreeSet::new();
    for (pid, process_path) in processes {
        let Ok(entries) = std::fs::read_dir(process_path.join("fd")) else {
            continue;
        };
        let mut inspected = 0usize;
        let mut owns_listener = false;
        for entry in entries {
            inspected += 1;
            if inspected > MAX_FDS_PER_PROCESS {
                return Err("managed Preview file descriptor enumeration exceeds its bound".into());
            }
            let Ok(entry) = entry else { continue };
            let Ok(target) = std::fs::read_link(entry.path()) else {
                continue;
            };
            let target = target.to_string_lossy();
            let Some(inode) = target
                .strip_prefix("socket:[")
                .and_then(|value| value.strip_suffix(']'))
            else {
                continue;
            };
            if socket_inodes.contains(inode) {
                owns_listener = true;
                break;
            }
        }
        if owns_listener {
            let owner = process_group_identity_for_pid(pid)?;
            ownership.insert((owner.process_group_id, owner.process_group_started_at));
        }
    }
    match ownership.into_iter().collect::<Vec<_>>().as_slice() {
        [] => Ok(None),
        [(process_group_id, process_group_started_at)] => Ok(Some(ListenerOwnership {
            process_group_id: *process_group_id,
            process_group_started_at: *process_group_started_at,
        })),
        _ => Err("managed Preview listener ownership is ambiguous".into()),
    }
}

#[cfg(target_os = "linux")]
fn process_group_identity_for_pid(pid: u32) -> Result<ListenerOwnership, String> {
    let fields = read_proc_stat_fields(pid)?;
    let process_group_id = fields
        .get(2)
        .and_then(|value| value.parse::<u32>().ok())
        .filter(|value| *value > 0)
        .ok_or("managed Preview listener owner has an invalid process group")?;
    let leader = read_proc_stat_fields(process_group_id)?;
    let leader_group = leader
        .get(2)
        .and_then(|value| value.parse::<u32>().ok())
        .ok_or("managed Preview process-group leader has an invalid group")?;
    let process_group_started_at = leader
        .get(19)
        .and_then(|value| value.parse::<u64>().ok())
        .filter(|value| *value > 0)
        .ok_or("managed Preview process-group leader has invalid start ticks")?;
    if leader_group != process_group_id {
        return Err("managed Preview process-group leader identity changed".into());
    }
    Ok(ListenerOwnership {
        process_group_id,
        process_group_started_at,
    })
}

#[cfg(target_os = "linux")]
fn read_proc_stat_fields(pid: u32) -> Result<Vec<String>, String> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat"))
        .map_err(|error| format!("read managed Preview listener process identity: {error}"))?;
    if stat.len() as u64 > MAX_PROC_STAT_BYTES {
        return Err("managed Preview listener process stat exceeds its bound".into());
    }
    Ok(stat
        .rsplit_once(") ")
        .ok_or("managed Preview listener process stat has an invalid shape")?
        .1
        .split_whitespace()
        .map(str::to_string)
        .collect())
}

fn package_manager_executable(manager: PreviewPackageManager) -> &'static str {
    match manager {
        PreviewPackageManager::Npm => "npm",
        PreviewPackageManager::Pnpm => "pnpm",
        PreviewPackageManager::Yarn => "yarn",
        PreviewPackageManager::Bun => "bun",
    }
}

#[cfg(windows)]
pub(crate) fn unc_to_posix(path: &str) -> Option<String> {
    let path = if let Some(rest) = path.strip_prefix("\\\\?\\UNC\\") {
        format!("\\\\{rest}")
    } else if let Some(rest) = path.strip_prefix("\\\\?\\") {
        rest.to_string()
    } else {
        path.to_string()
    };
    for prefix in ["\\\\wsl.localhost\\", "\\\\wsl$\\"] {
        if let Some(rest) = path.strip_prefix(prefix) {
            let tail = rest.split_once('\\').map_or("", |(_, tail)| tail);
            return Some(format!("/{}", tail.replace('\\', "/")));
        }
    }
    path.starts_with('/').then_some(path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::preview::model::{PreviewTargetId, PreviewTargetSource};

    fn target(relative_root: &str) -> PreviewTarget {
        PreviewTarget {
            id: PreviewTargetId::parse("workspace:web:dev").unwrap(),
            label: "Web".into(),
            source: PreviewTargetSource::WorkspaceManifest,
            relative_root: relative_root.into(),
            kind: PreviewTargetKind::PackageScript {
                package_manager: PreviewPackageManager::Pnpm,
                script: "dev; touch /tmp/never-shell".into(),
            },
            recommended: true,
        }
    }

    #[test]
    fn typed_command_keeps_script_as_one_argument_and_uses_supplied_run_id() {
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir(root.path().join("apps")).unwrap();
        let command = typed_package_command(root.path(), &target("apps"), "caller-run-1").unwrap();
        let expected_cwd = root.path().join("apps");
        let args: Vec<String> = command
            .get_args()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect();
        assert!(args.iter().any(|arg| arg == "caller-run-1"));
        assert_eq!(
            args.iter()
                .filter(|arg| arg.as_str() == "dev; touch /tmp/never-shell")
                .count(),
            1
        );
        assert_eq!(command.get_current_dir(), Some(expected_cwd.as_path()));
    }

    #[test]
    fn typed_command_rejects_escape_and_static_targets() {
        let root = tempfile::tempdir().unwrap();
        assert!(typed_package_command(root.path(), &target("../outside"), "run-1").is_err());
        let mut static_target = target("");
        static_target.kind = PreviewTargetKind::StaticSite {
            entrypoint: "index.html".into(),
        };
        assert!(typed_package_command(root.path(), &static_target, "run-1").is_err());
        assert!(typed_package_command(root.path(), &target(""), "../marker-escape").is_err());
    }

    #[test]
    fn managed_output_retains_only_the_bounded_tail() {
        let output = BoundedOutput::default();
        output.append(&vec![b'a'; MAX_MANAGED_OUTPUT_BYTES - 4]);
        output.append(b"0123456789");
        let snapshot = output.snapshot();
        assert_eq!(snapshot.len(), MAX_MANAGED_OUTPUT_BYTES);
        assert!(snapshot.ends_with(b"0123456789"));

        output.append(&vec![b'z'; MAX_MANAGED_OUTPUT_BYTES + 20]);
        assert_eq!(output.snapshot(), vec![b'z'; MAX_MANAGED_OUTPUT_BYTES]);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn listener_inspection_resolves_exact_process_group_identity() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let expected = process_group_identity_for_pid(std::process::id()).unwrap();
        let observed = listener_ownership(port)
            .unwrap()
            .expect("bound listener should have one owner");
        assert_eq!(observed, expected);
        assert!(listener_ownership(0).is_err());

        drop(listener);
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        loop {
            if listener_ownership(port).unwrap().is_none() {
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "closed listener ownership did not disappear"
            );
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn process_wrapper_publishes_exact_group_start_ticks_and_cleans_marker() {
        use std::io::BufRead;
        use std::time::{Duration, Instant};

        let run_id = format!("identity-{}", uuid::Uuid::new_v4().simple());
        let marker = Path::new("/tmp").join(format!("t-hub-preview-{run_id}.pid"));
        let mut command = Command::new("bash");
        command
            .arg("-c")
            .arg(PROCESS_TREE_SCRIPT)
            .arg("t-hub-preview")
            .arg(&run_id)
            .arg("sh")
            .arg("-c")
            .arg("echo ready; sleep 30")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut child = command.spawn().unwrap();
        let stdin = child.stdin.take();
        let mut output = std::io::BufReader::new(child.stdout.take().unwrap());
        let mut ready = String::new();
        output.read_line(&mut ready).unwrap();
        assert_eq!(ready.trim(), "ready");

        let deadline = Instant::now() + Duration::from_secs(2);
        let identity = loop {
            if let Ok(identity) = std::fs::read_to_string(&marker) {
                break identity;
            }
            assert!(
                Instant::now() < deadline,
                "identity marker was not published"
            );
            std::thread::sleep(Duration::from_millis(10));
        };
        let fields = identity.split_whitespace().collect::<Vec<_>>();
        assert_eq!(fields.len(), 2);
        let process_group_id = fields[0].parse::<u32>().unwrap();
        let start_ticks = fields[1].parse::<u64>().unwrap();
        let observed = observe_marker_identity(&run_id).unwrap();
        assert_eq!(observed.process_group_id, process_group_id);
        assert_eq!(observed.process_group_started_at, start_ticks);
        assert!(start_ticks > 0);
        let stale = ManagedRunIdentity {
            process_group_started_at: start_ticks + 1,
            ..observed
        };
        assert!(revalidate_process_identity(&stale).is_err());

        drop(stdin);
        assert!(child.wait().unwrap().success());
        assert!(!marker.exists());
    }
}
