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

#[derive(Default)]
pub(crate) struct BoundedOutput {
    bytes: Mutex<Vec<u8>>,
}

#[cfg(unix)]
pub(crate) struct PreparedPreviewCommand {
    command: Command,
    identity_reader: std::os::unix::net::UnixStream,
    identity_writer: std::os::unix::net::UnixStream,
}

#[cfg(unix)]
pub(crate) struct PreparedPreviewChild {
    pub child: std::process::Child,
    identity_reader: std::os::unix::net::UnixStream,
}

#[cfg(unix)]
impl PreparedPreviewCommand {
    pub(crate) fn spawn(mut self) -> Result<PreparedPreviewChild, String> {
        let child = self
            .command
            .spawn()
            .map_err(|error| format!("spawn supervised Preview wrapper: {error}"))?;
        drop(self.identity_writer);
        Ok(PreparedPreviewChild {
            child,
            identity_reader: self.identity_reader,
        })
    }
}

#[cfg(unix)]
impl PreparedPreviewChild {
    pub(crate) fn read_identity(
        &mut self,
        run_id: &str,
        timeout: std::time::Duration,
    ) -> Result<ManagedRunIdentity, String> {
        validate_run_id(run_id)?;
        self.identity_reader
            .set_read_timeout(Some(timeout))
            .map_err(|error| format!("bound supervised Preview identity timeout: {error}"))?;
        let mut bytes = Vec::new();
        self.identity_reader
            .by_ref()
            .take(MAX_MARKER_BYTES + 1)
            .read_to_end(&mut bytes)
            .map_err(|error| format!("read supervised Preview identity pipe: {error}"))?;
        if bytes.len() as u64 > MAX_MARKER_BYTES {
            return Err("supervised Preview identity pipe exceeds its bound".into());
        }
        let text = std::str::from_utf8(&bytes)
            .map_err(|_| "supervised Preview identity pipe is not UTF-8")?;
        let fields = text.split_whitespace().collect::<Vec<_>>();
        if fields.len() != 3 || fields[0].parse::<u32>().ok() != Some(self.child.id()) {
            return Err("supervised Preview identity is not bound to its wrapper child".into());
        }
        let identity = ManagedRunIdentity {
            run_id: run_id.to_string(),
            process_group_id: fields[1]
                .parse()
                .map_err(|_| "supervised Preview identity has an invalid process group")?,
            process_group_started_at: fields[2]
                .parse()
                .map_err(|_| "supervised Preview identity has invalid start ticks")?,
        };
        if identity.process_group_id == 0 || identity.process_group_started_at == 0 {
            return Err("supervised Preview process identity must be nonzero".into());
        }
        Ok(identity)
    }
}

#[cfg(unix)]
pub(crate) fn prepare_supervised_preview_command(
    cwd: &Path,
    run_id: &str,
    executable: &str,
    arguments: &[&str],
) -> Result<PreparedPreviewCommand, String> {
    use std::os::fd::AsRawFd;
    use std::os::unix::process::CommandExt;

    validate_run_id(run_id)?;
    let (identity_reader, identity_writer) = std::os::unix::net::UnixStream::pair()
        .map_err(|error| format!("create supervised Preview identity pipe: {error}"))?;
    let writer_fd = identity_writer.as_raw_fd();
    let mut command = Command::new("bash");
    command
        .arg("-c")
        .arg(PROCESS_TREE_SCRIPT)
        .arg("t-hub-preview")
        .arg(run_id)
        .arg(executable)
        .args(arguments)
        .current_dir(cwd)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .stdin(Stdio::piped());
    unsafe {
        command.pre_exec(move || {
            if libc::dup2(writer_fd, 4) == -1 {
                return Err(std::io::Error::last_os_error());
            }
            let flags = libc::fcntl(4, libc::F_GETFD);
            if flags == -1 || libc::fcntl(4, libc::F_SETFD, flags & !libc::FD_CLOEXEC) == -1 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    Ok(PreparedPreviewCommand {
        command,
        identity_reader,
        identity_writer,
    })
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
  t-hub-preview) IDENTITY_FD=4 ;;
  t-hub-devserver) IDENTITY_FD= ;;
  *) exit 64 ;;
esac
case "$1" in
  ""|*[!A-Za-z0-9_.:-]*) exit 65 ;;
esac
MARKER="/tmp/$0-$1.pid"
shift
export HOST=0.0.0.0 HOSTNAME=0.0.0.0 NUXT_HOST=0.0.0.0 ASTRO_HOST=0.0.0.0 TAURI_DEV_HOST=0.0.0.0
exec 3<&0
setsid "$@" 3<&- 4>&- </dev/null &
SRV=$!
START_TICKS=
identity_matches() {
  [ -n "$START_TICKS" ] || return 1
  CURRENT=$(cat "/proc/$SRV/stat" 2>/dev/null) || return 1
  CURRENT_REST=${CURRENT##*) }
  set -- $CURRENT_REST
  [ "${3:-}" = "$SRV" ] && [ "${20:-}" = "$START_TICKS" ]
}
cleanup() {
  identity_matches && kill -TERM -- -"$SRV" 2>/dev/null || true
  i=0
  while kill -0 "$SRV" 2>/dev/null && [ "$i" -lt 20 ]; do
    sleep 0.1
    i=$((i + 1))
  done
  identity_matches && kill -KILL -- -"$SRV" 2>/dev/null || true
  wait "$SRV" 2>/dev/null || true
  CURRENT_MARKER=$(cat "$MARKER" 2>/dev/null || true)
  [ "$CURRENT_MARKER" = "${IDENTITY:-}" ] && rm -f "$MARKER" 2>/dev/null || true
}
STAT=$(cat "/proc/$SRV/stat" 2>/dev/null) || { cleanup; exit 66; }
REST=${STAT##*) }
set -- $REST
START_TICKS=${20:-}
case "$START_TICKS" in
  ""|*[!0-9]*) cleanup; exit 67 ;;
esac
umask 077
IDENTITY="$SRV $START_TICKS"
set -C
if ! printf '%s\n' "$IDENTITY" > "$MARKER"; then
  cleanup
  exit 69
fi
set +C
if [ -n "$IDENTITY_FD" ]; then
  printf '%s %s %s\n' "$$" "$SRV" "$START_TICKS" >&4 || { cleanup; exit 70; }
  exec 4>&-
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
pub(crate) fn signal_exact_process_group(
    identity: &ManagedRunIdentity,
    signal: i32,
) -> Result<(), String> {
    revalidate_process_identity(identity)?;
    let result = unsafe { libc::kill(-(identity.process_group_id as i32), signal) };
    if result == -1 {
        return Err(format!(
            "signal exact managed Preview process group: {}",
            std::io::Error::last_os_error()
        ));
    }
    Ok(())
}

#[cfg(test)]
fn signal_exact_process_group_with(
    identity: &ManagedRunIdentity,
    observe: impl FnOnce(&ManagedRunIdentity) -> Result<(), String>,
    signal: impl FnOnce(u32),
) -> Result<(), String> {
    observe(identity)?;
    signal(identity.process_group_id);
    Ok(())
}

#[cfg(target_os = "linux")]
pub(crate) fn listener_ownership(port: u16) -> Result<Option<ListenerOwnership>, String> {
    super::proc_listener::listener_ownership(port)
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
        let expected =
            crate::preview::proc_listener::process_group_identity_for_pid(std::process::id())
                .unwrap();
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
        let prepared = prepare_supervised_preview_command(
            Path::new("/tmp"),
            &run_id,
            "sh",
            &["-c", "echo '999 888 777'; echo ready; sleep 30"],
        )
        .unwrap();
        let mut supervised = prepared.spawn().unwrap();
        let observed = supervised
            .read_identity(&run_id, Duration::from_secs(2))
            .unwrap();
        let stdin = supervised.child.stdin.take();
        let mut output = std::io::BufReader::new(supervised.child.stdout.take().unwrap());
        let mut spoof = String::new();
        output.read_line(&mut spoof).unwrap();
        assert_eq!(spoof.trim(), "999 888 777");
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
        assert_eq!(observed.process_group_id, process_group_id);
        assert_eq!(observed.process_group_started_at, start_ticks);
        assert_eq!(observe_marker_identity(&run_id).unwrap(), observed);
        assert!(start_ticks > 0);
        let stale = ManagedRunIdentity {
            process_group_started_at: start_ticks + 1,
            ..observed
        };
        assert!(revalidate_process_identity(&stale).is_err());

        drop(stdin);
        assert!(supervised.child.wait().unwrap().success());
        assert!(!marker.exists());
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn process_wrapper_refuses_to_replace_an_existing_recovery_marker() {
        let run_id = format!("reserved-{}", uuid::Uuid::new_v4().simple());
        let marker = Path::new("/tmp").join(format!("t-hub-preview-{run_id}.pid"));
        std::fs::write(&marker, "1 1\n").unwrap();
        let prepared = prepare_supervised_preview_command(
            Path::new("/tmp"),
            &run_id,
            "sh",
            &["-c", "sleep 30"],
        )
        .unwrap();
        let mut supervised = prepared.spawn().unwrap();
        assert!(supervised
            .read_identity(&run_id, std::time::Duration::from_secs(2))
            .is_err());
        assert!(!supervised.child.wait().unwrap().success());
        assert_eq!(std::fs::read_to_string(&marker).unwrap(), "1 1\n");
        std::fs::remove_file(marker).unwrap();
    }

    #[test]
    fn reused_process_identity_is_refused_before_any_signal() {
        let identity = ManagedRunIdentity {
            run_id: "reused-run".into(),
            process_group_id: 4242,
            process_group_started_at: 99,
        };
        let signals = std::sync::atomic::AtomicUsize::new(0);
        let result = signal_exact_process_group_with(
            &identity,
            |_| Err("process-group start ticks changed".into()),
            |_| {
                signals.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            },
        );
        assert!(result.is_err());
        assert_eq!(signals.load(std::sync::atomic::Ordering::SeqCst), 0);
        assert!(PROCESS_TREE_SCRIPT.contains("identity_matches && kill -TERM -- -\"$SRV\""));
        assert!(PROCESS_TREE_SCRIPT.contains("identity_matches && kill -KILL -- -\"$SRV\""));
    }
}
