//! Shared managed Preview command construction.
//!
//! This module prepares only backend-selected typed commands.
//! Process reservation, durable identity, bounded output, endpoint ownership,
//! and cleanup remain the caller's responsibility.

use std::path::Path;
use std::process::{Command, Stdio};

use super::model::{PreviewPackageManager, PreviewTarget, PreviewTargetKind};

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
        let stat = std::fs::read_to_string(format!("/proc/{process_group_id}/stat")).unwrap();
        let observed = stat.rsplit_once(") ").unwrap().1.split_whitespace().nth(19);
        assert_eq!(observed, Some(fields[1]));
        assert!(start_ticks > 0);

        drop(stdin);
        assert!(child.wait().unwrap().success());
        assert!(!marker.exists());
    }
}
