//! Shared managed Preview command construction.
//!
//! This module prepares only backend-selected typed commands.
//! Process reservation, durable identity, bounded output, endpoint ownership,
//! and cleanup remain the caller's responsibility.

#[cfg(target_os = "linux")]
use std::io::Read;
use std::path::Path;

use parking_lot::Mutex;

#[cfg(target_os = "linux")]
use super::endpoint::ListenerOwnership;
#[cfg(any(target_os = "linux", test))]
use super::endpoint::ManagedRunIdentity;
use super::model::{PreviewPackageManager, PreviewTarget, PreviewTargetKind};
use super::supervisor::prepare_confined_supervised_preview_command;
pub(crate) use super::supervisor::{prepare_supervised_preview_command, PreparedPreviewCommand};

const MAX_MANAGED_OUTPUT_BYTES: usize = 64 * 1024;
#[cfg(target_os = "linux")]
const MAX_PROC_STAT_BYTES: u64 = 4096;

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

pub(crate) fn package_command(
    cwd: &Path,
    run_id: &str,
    executable: &str,
    script: &str,
) -> Result<PreparedPreviewCommand, String> {
    prepare_supervised_preview_command(cwd, run_id, executable, &["run", script])
}

pub(crate) fn typed_package_command(
    canonical_root: &Path,
    target: &PreviewTarget,
    run_id: &str,
) -> Result<PreparedPreviewCommand, String> {
    validate_run_id(run_id)?;
    let relative_root = Path::new(&target.relative_root);
    if relative_root.is_absolute()
        || relative_root
            .components()
            .any(|component| matches!(component, std::path::Component::ParentDir))
    {
        return Err("Preview target root is not confined to its canonical Project".into());
    }
    let PreviewTargetKind::PackageScript {
        package_manager,
        script,
    } = &target.kind
    else {
        return Err("static Preview targets require the supervised static helper".into());
    };
    prepare_confined_supervised_preview_command(
        canonical_root,
        relative_root,
        run_id,
        package_manager_executable(*package_manager),
        &["run", script],
    )
}

pub(crate) fn typed_static_command(
    canonical_root: &Path,
    target: &PreviewTarget,
    run_id: &str,
) -> Result<PreparedPreviewCommand, String> {
    validate_run_id(run_id)?;
    let relative_root = Path::new(&target.relative_root);
    if relative_root.is_absolute()
        || relative_root
            .components()
            .any(|component| matches!(component, std::path::Component::ParentDir))
    {
        return Err("Preview target root is not confined to its canonical Project".into());
    }
    let PreviewTargetKind::StaticSite { entrypoint } = &target.kind else {
        return Err("package Preview targets require the typed package runner".into());
    };
    let entrypoint = Path::new(entrypoint);
    if entrypoint.is_absolute()
        || entrypoint.as_os_str().is_empty()
        || entrypoint.components().any(|component| {
            !matches!(
                component,
                std::path::Component::Normal(_) | std::path::Component::CurDir
            )
        })
    {
        return Err("static Preview entrypoint is not a confined relative path".into());
    }
    let entrypoint = entrypoint
        .to_str()
        .ok_or("static Preview entrypoint must be UTF-8")?;
    prepare_confined_supervised_preview_command(
        canonical_root,
        relative_root,
        run_id,
        "/usr/bin/python3",
        &["-I", "-c", STATIC_HELPER_PY, entrypoint],
    )
}

pub(crate) fn validate_run_id(run_id: &str) -> Result<(), String> {
    if run_id.is_empty()
        || run_id.len() > 160
        || !run_id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b':'))
    {
        return Err("managed Preview run id is invalid".into());
    }
    Ok(())
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
    let group = i32::try_from(identity.process_group_id)
        .map_err(|_| "managed Preview process group exceeds the Linux pid range")?;
    let result = unsafe { libc::kill(-group, signal) };
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

const STATIC_HELPER_PY: &str = r#"import mimetypes, os, socket, stat, sys, urllib.parse

MAX_REQUEST = 16384
MAX_FILE = 16 * 1024 * 1024
entrypoint = sys.argv[1]
root = os.open('.', os.O_RDONLY | os.O_DIRECTORY)

def confined_file(request_path):
    decoded = urllib.parse.unquote(urllib.parse.urlsplit(request_path).path)
    if decoded == '/':
        decoded = '/' + entrypoint
    if '\x00' in decoded or '\\' in decoded:
        raise ValueError()
    parts = [part for part in decoded.split('/') if part not in ('', '.')]
    if not parts or any(part == '..' for part in parts):
        raise ValueError()
    directory = os.dup(root)
    try:
        for part in parts[:-1]:
            next_directory = os.open(
                part,
                os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW,
                dir_fd=directory,
            )
            os.close(directory)
            directory = next_directory
        descriptor = os.open(parts[-1], os.O_RDONLY | os.O_NOFOLLOW, dir_fd=directory)
    finally:
        os.close(directory)
    details = os.fstat(descriptor)
    if not stat.S_ISREG(details.st_mode) or details.st_size > MAX_FILE:
        os.close(descriptor)
        raise ValueError()
    return descriptor, details.st_size, parts[-1]

listener = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
listener.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
listener.bind(('0.0.0.0', 0))
listener.listen(16)
port = listener.getsockname()[1]
quoted = '/'.join(urllib.parse.quote(part, safe='') for part in entrypoint.split('/'))
print(f'http://127.0.0.1:{port}/{quoted}', flush=True)

while True:
    connection, _ = listener.accept()
    with connection:
        connection.settimeout(1.0)
        request = b''
        descriptor = None
        try:
            while b'\r\n\r\n' not in request:
                chunk = connection.recv(4096)
                if not chunk:
                    raise ValueError()
                request += chunk
                if len(request) > MAX_REQUEST:
                    raise ValueError()
            line = request.split(b'\r\n', 1)[0].decode('ascii')
            method, path, version = line.split(' ')
            if method not in ('GET', 'HEAD') or version not in ('HTTP/1.0', 'HTTP/1.1'):
                raise ValueError()
            descriptor, length, name = confined_file(path)
            content_type = mimetypes.guess_type(name)[0] or 'application/octet-stream'
            header = (
                f'HTTP/1.1 200 OK\r\nContent-Length: {length}\r\n'
                f'Content-Type: {content_type}\r\nConnection: close\r\n\r\n'
            ).encode('ascii')
            connection.sendall(header)
            if method == 'GET':
                with os.fdopen(descriptor, 'rb') as source:
                    descriptor = None
                    while True:
                        chunk = source.read(65536)
                        if not chunk:
                            break
                        connection.sendall(chunk)
            else:
                os.close(descriptor)
                descriptor = None
        except Exception:
            try:
                connection.sendall(
                    b'HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n'
                )
            except Exception:
                pass
        finally:
            if descriptor is not None:
                os.close(descriptor)
"#;

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
        let prepared = typed_package_command(root.path(), &target("apps"), "caller-run-1").unwrap();
        let command = prepared.command();
        let expected_cwd = root.path().join("apps");
        let args: Vec<String> = command
            .get_args()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect();
        assert!(args.iter().any(|arg| arg == "caller-run-1"));
        assert_eq!(args.iter().filter(|arg| arg.as_str() == "apps").count(), 1);
        assert_eq!(
            args.iter()
                .filter(|arg| arg.as_str() == "dev; touch /tmp/never-shell")
                .count(),
            1
        );
        assert_ne!(command.get_current_dir(), Some(expected_cwd.as_path()));
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
    fn typed_static_command_is_backend_owned_and_confines_its_entrypoint() {
        let root = tempfile::tempdir().unwrap();
        std::fs::write(root.path().join("index.html"), "STATIC SENTINEL").unwrap();
        let mut static_target = target("");
        static_target.kind = PreviewTargetKind::StaticSite {
            entrypoint: "index.html".into(),
        };
        let prepared =
            typed_static_command(root.path(), &static_target, "static-command-1").unwrap();
        let command = prepared.command();
        let arguments = command
            .get_args()
            .map(|argument| argument.to_string_lossy().into_owned())
            .collect::<Vec<_>>();
        assert!(arguments
            .iter()
            .any(|argument| argument == STATIC_HELPER_PY));
        assert!(arguments.iter().any(|argument| argument == "index.html"));
        assert!(!arguments
            .iter()
            .any(|argument| argument.contains("STATIC SENTINEL")));

        static_target.kind = PreviewTargetKind::StaticSite {
            entrypoint: "../outside.html".into(),
        };
        assert!(typed_static_command(root.path(), &static_target, "static-command-2").is_err());
    }

    #[cfg(unix)]
    #[test]
    fn effect_time_symlink_swap_is_refused_before_static_target_launch() {
        let root = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        std::fs::create_dir(root.path().join("site")).unwrap();
        std::fs::write(root.path().join("site/index.html"), "inside").unwrap();
        std::fs::write(outside.path().join("index.html"), "outside").unwrap();
        let mut static_target = target("site");
        static_target.kind = PreviewTargetKind::StaticSite {
            entrypoint: "index.html".into(),
        };
        let prepared =
            typed_static_command(root.path(), &static_target, "static-swap-run").unwrap();
        std::fs::remove_dir_all(root.path().join("site")).unwrap();
        std::os::unix::fs::symlink(outside.path(), root.path().join("site")).unwrap();

        let error = match prepared.spawn_authenticated(std::time::Duration::from_secs(2)) {
            Ok(_) => panic!("effect-time symlink swap launched a Preview target"),
            Err(error) => error,
        };
        assert!(
            error.contains("handshake")
                || error.contains("supervisor")
                || error.contains("authentication"),
            "unexpected effect-time refusal: {error}"
        );
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
    }
}
