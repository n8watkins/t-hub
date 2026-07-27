//! Authenticated persistent process-tree supervision for managed Preview runs.

use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStderr, ChildStdin, ChildStdout, Command, Stdio};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use super::endpoint::ManagedRunIdentity;

const HANDSHAKE_LIMIT: usize = 1024;
const HELPER_OBSERVATION_LIMIT: usize = 4096;
const ABORT_TIMEOUT: Duration = Duration::from_secs(2);
const PROTOCOL_VERSION: &str = "1";
const READY_PREFIX: &str = "T_HUB_PREVIEW_READY";
const GO_PREFIX: &str = "T_HUB_PREVIEW_GO";
const CWD_GATE_PY: &str = r#"import os, sys
root, relative, python = sys.argv[1:4]
flags = os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW
directory = os.open(root, flags)
try:
    for part in relative.split('/'):
        if part in ('', '.'):
            continue
        next_directory = os.open(part, flags, dir_fd=directory)
        os.close(directory)
        directory = next_directory
    os.fchdir(directory)
    os.execv(python, sys.argv[3:])
finally:
    os.close(directory)
"#;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct ExecutableIdentity {
    path: PathBuf,
    device: u64,
    inode: u64,
}

pub(crate) struct PreparedPreviewCommand {
    command: Command,
    run_id: String,
    generation: String,
    python: ExecutableIdentity,
    helper_argv: Vec<String>,
}

pub(crate) struct SupervisedPreviewChild {
    pub child: Child,
    pub stdin: Option<ChildStdin>,
    pub stdout: Option<ChildStdout>,
    pub stderr: Option<ChildStderr>,
    #[allow(dead_code)]
    pub identity: ManagedRunIdentity,
    #[allow(dead_code)]
    pub(crate) generation: String,
    pub(crate) _job: Option<crate::engine_supervisor::platform::KillOnCloseJob>,
}

struct AuthenticationGuard {
    child: Option<Child>,
    stdin: Option<ChildStdin>,
    stdout: Option<ChildStdout>,
    stderr: Option<ChildStderr>,
}

impl Drop for AuthenticationGuard {
    fn drop(&mut self) {
        self.stdin.take();
        let Some(mut child) = self.child.take() else {
            return;
        };
        let deadline = Instant::now() + ABORT_TIMEOUT;
        loop {
            match child.try_wait() {
                Ok(Some(_)) => return,
                Ok(None) if Instant::now() < deadline => {
                    std::thread::sleep(Duration::from_millis(10));
                }
                _ => {
                    let _ = child.kill();
                    let _ = child.wait();
                    return;
                }
            }
        }
    }
}

impl PreparedPreviewCommand {
    #[cfg(test)]
    pub(crate) fn command(&self) -> &Command {
        &self.command
    }

    pub(crate) fn spawn_authenticated(
        mut self,
        timeout: Duration,
    ) -> Result<SupervisedPreviewChild, String> {
        revalidate_python_identity(&self.python)?;
        let child = self
            .command
            .spawn()
            .map_err(|error| format!("spawn managed Preview supervisor: {error}"))?;
        let job = crate::engine_supervisor::platform::assign_kill_on_close_job(&child).ok();
        let mut guard = AuthenticationGuard {
            child: Some(child),
            stdin: None,
            stdout: None,
            stderr: None,
        };
        let (stdin, stdout, stderr) = {
            let child = guard
                .child
                .as_mut()
                .expect("authentication guard owns child");
            (child.stdin.take(), child.stdout.take(), child.stderr.take())
        };
        guard.stdin = stdin;
        guard.stdout = stdout;
        guard.stderr = stderr;
        if guard.stdin.is_none() || guard.stdout.is_none() {
            return Err("managed Preview supervisor protocol pipes are unavailable".into());
        }

        let (line, stdout) = read_handshake(
            guard
                .stdout
                .take()
                .expect("authentication guard owns stdout"),
            timeout,
        )?;
        guard.stdout = Some(stdout);
        let handshake = parse_handshake(&line, &self.generation)?;
        #[cfg(unix)]
        if guard
            .child
            .as_ref()
            .expect("authentication guard owns child")
            .id()
            != handshake.process_id
        {
            return Err("managed Preview helper pid is not its direct child identity".into());
        }
        verify_helper(
            &self.python,
            &self.run_id,
            &self.generation,
            &handshake,
            &self.helper_argv,
            timeout,
        )?;
        writeln!(
            guard
                .stdin
                .as_mut()
                .expect("authentication guard owns stdin"),
            "{GO_PREFIX} {}",
            self.generation
        )
        .and_then(|_| {
            guard
                .stdin
                .as_mut()
                .expect("authentication guard owns stdin")
                .flush()
        })
        .map_err(|error| format!("release authenticated managed Preview target: {error}"))?;

        let identity = ManagedRunIdentity {
            run_id: self.run_id,
            process_group_id: handshake.process_group_id,
            process_group_started_at: handshake.process_group_started_at,
        };
        identity.validate()?;
        Ok(SupervisedPreviewChild {
            child: guard.child.take().expect("authentication guard owns child"),
            stdin: guard.stdin.take(),
            stdout: guard.stdout.take(),
            stderr: guard.stderr.take(),
            identity,
            generation: self.generation,
            _job: job,
        })
    }
}

impl SupervisedPreviewChild {
    /// Resolve listener ownership through the correlated WSL agent contract.
    ///
    /// This is intentionally not wired to the live Preview service yet. It is
    /// the packaged-Windows adapter over the same canonical Linux algorithm the
    /// desktop Linux build calls directly.
    #[allow(dead_code)]
    pub(crate) fn inspect_wsl_listener(
        &self,
        agent: &crate::agent::AgentBridge,
        port: u16,
    ) -> Result<Option<super::endpoint::ListenerOwnership>, String> {
        let ownership = agent.inspect_preview_listener(
            &self.identity.run_id,
            &self.generation,
            port,
            self.identity.process_group_id,
            self.identity.process_group_started_at,
        )?;
        ownership
            .map(|ownership| {
                if ownership.process_group_id == 0 || ownership.process_group_started_at == 0 {
                    return Err("WSL Preview listener ownership is invalid".into());
                }
                Ok(super::endpoint::ListenerOwnership {
                    process_group_id: ownership.process_group_id,
                    process_group_started_at: ownership.process_group_started_at,
                })
            })
            .transpose()
    }
}

#[derive(Debug, PartialEq, Eq)]
struct Handshake {
    process_id: u32,
    process_group_id: u32,
    process_group_started_at: u64,
}

fn parse_handshake(line: &[u8], generation: &str) -> Result<Handshake, String> {
    let text = std::str::from_utf8(line).map_err(|_| "managed Preview handshake is not UTF-8")?;
    let fields = text.split_whitespace().collect::<Vec<_>>();
    if fields.len() != 7
        || fields[0] != READY_PREFIX
        || fields[1] != PROTOCOL_VERSION
        || fields[2] != generation
    {
        return Err("managed Preview handshake has an invalid authenticated shape".into());
    }
    let process_id = parse_nonzero(fields[3], "helper pid")?;
    let process_group_id = parse_nonzero(fields[4], "helper process group")?;
    let process_group_started_at = parse_nonzero_u64(fields[5], "helper start ticks")?;
    if fields[6] != "WAITING" || process_id != process_group_id {
        return Err("managed Preview helper is not its process-group leader".into());
    }
    Ok(Handshake {
        process_id,
        process_group_id,
        process_group_started_at,
    })
}

fn parse_nonzero(value: &str, field: &str) -> Result<u32, String> {
    value
        .parse::<u32>()
        .ok()
        .filter(|value| *value > 0)
        .ok_or_else(|| format!("managed Preview {field} is invalid"))
}

fn parse_nonzero_u64(value: &str, field: &str) -> Result<u64, String> {
    value
        .parse::<u64>()
        .ok()
        .filter(|value| *value > 0)
        .ok_or_else(|| format!("managed Preview {field} is invalid"))
}

fn read_handshake(
    mut stdout: ChildStdout,
    timeout: Duration,
) -> Result<(Vec<u8>, ChildStdout), String> {
    let (sender, receiver) = std::sync::mpsc::sync_channel(1);
    std::thread::spawn(move || {
        let mut line = Vec::new();
        let result = loop {
            let mut byte = [0u8; 1];
            match stdout.read(&mut byte) {
                Ok(0) => break Err("managed Preview helper exited before authentication".into()),
                Ok(_) if byte[0] == b'\n' => break Ok(()),
                Ok(_) if line.len() == HANDSHAKE_LIMIT => {
                    break Err("managed Preview handshake exceeds its bound".into())
                }
                Ok(_) => line.push(byte[0]),
                Err(error) => break Err(format!("read managed Preview handshake: {error}")),
            }
        };
        let _ = sender.send((result, line, stdout));
    });
    let (result, line, stdout) = receiver
        .recv_timeout(timeout)
        .map_err(|_| "managed Preview authentication timed out".to_string())?;
    result?;
    Ok((line, stdout))
}

pub(crate) fn prepare_supervised_preview_command(
    cwd: &Path,
    run_id: &str,
    executable: &str,
    arguments: &[&str],
) -> Result<PreparedPreviewCommand, String> {
    prepare_confined_supervised_preview_command(cwd, Path::new(""), run_id, executable, arguments)
}

pub(crate) fn prepare_confined_supervised_preview_command(
    canonical_root: &Path,
    relative_root: &Path,
    run_id: &str,
    executable: &str,
    arguments: &[&str],
) -> Result<PreparedPreviewCommand, String> {
    super::managed_runner::validate_run_id(run_id)?;
    if executable.is_empty() || executable.as_bytes().contains(&0) {
        return Err("managed Preview executable is invalid".into());
    }
    let generation = uuid::Uuid::new_v4().simple().to_string();
    let python = trusted_python_identity()?;
    revalidate_python_identity(&python)?;
    let relative_root = validate_confined_relative_root(relative_root)?;
    let command = supervisor_command(
        canonical_root,
        &relative_root,
        &python.path,
        run_id,
        &generation,
        executable,
        arguments,
    )?;
    let helper_argv =
        expected_helper_argv(&python.path, run_id, &generation, executable, arguments);
    Ok(PreparedPreviewCommand {
        command,
        run_id: run_id.to_string(),
        generation,
        python,
        helper_argv,
    })
}

fn validate_confined_relative_root(relative_root: &Path) -> Result<String, String> {
    if relative_root.is_absolute()
        || relative_root.components().any(|component| {
            !matches!(
                component,
                std::path::Component::Normal(_) | std::path::Component::CurDir
            )
        })
    {
        return Err("managed Preview working directory is not confined".into());
    }
    relative_root
        .to_str()
        .map(str::to_string)
        .ok_or_else(|| "managed Preview working directory must be UTF-8".into())
}

fn expected_helper_argv(
    python: &Path,
    run_id: &str,
    generation: &str,
    executable: &str,
    arguments: &[&str],
) -> Vec<String> {
    let mut result = vec![
        python.to_string_lossy().into_owned(),
        "-I".into(),
        "-c".into(),
        SUPERVISOR_PY.into(),
        run_id.into(),
        generation.into(),
        executable.into(),
    ];
    result.extend(arguments.iter().map(|argument| (*argument).to_string()));
    result
}

fn supervisor_command(
    canonical_root: &Path,
    relative_root: &str,
    python: &Path,
    run_id: &str,
    generation: &str,
    executable: &str,
    arguments: &[&str],
) -> Result<Command, String> {
    #[cfg(windows)]
    {
        let posix_root = super::managed_runner::unc_to_posix(&canonical_root.to_string_lossy())
            .ok_or("managed Preview root is not a WSL path")?;
        Ok(windows_supervisor_command(
            &trusted_wsl_path()?,
            &crate::files::host_distro(),
            &posix_root,
            relative_root,
            python,
            run_id,
            generation,
            executable,
            arguments,
        ))
    }
    #[cfg(not(windows))]
    {
        let canonical_root = canonical_root
            .to_str()
            .ok_or("managed Preview canonical root must be UTF-8")?;
        let mut command = Command::new(python);
        command
            .arg("-I")
            .arg("-c")
            .arg(CWD_GATE_PY)
            .arg(canonical_root)
            .arg(relative_root)
            .arg(python)
            .arg("-I")
            .arg("-c")
            .arg(SUPERVISOR_PY)
            .arg(run_id)
            .arg(generation)
            .arg(executable)
            .args(arguments)
            .current_dir("/")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        Ok(command)
    }
}

#[cfg(any(windows, test))]
fn windows_supervisor_command(
    wsl: &Path,
    distro: &str,
    posix_root: &str,
    relative_root: &str,
    python: &Path,
    run_id: &str,
    generation: &str,
    executable: &str,
    arguments: &[&str],
) -> Command {
    let mut command = Command::new(wsl);
    command
        .arg("-d")
        .arg(distro)
        .arg("--cd")
        .arg("/")
        .arg("-e")
        .arg(python)
        .arg("-I")
        .arg("-c")
        .arg(CWD_GATE_PY)
        .arg(posix_root)
        .arg(relative_root)
        .arg(python)
        .arg("-I")
        .arg("-c")
        .arg(SUPERVISOR_PY)
        .arg(run_id)
        .arg(generation)
        .arg(executable)
        .args(arguments)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x0800_0000);
    }
    command
}

#[cfg(unix)]
fn trusted_python_identity() -> Result<ExecutableIdentity, String> {
    use std::os::unix::fs::{MetadataExt, PermissionsExt};

    for candidate in ["/usr/bin/python3", "/bin/python3"] {
        let Ok(path) = std::fs::canonicalize(candidate) else {
            continue;
        };
        let Ok(metadata) = std::fs::symlink_metadata(&path) else {
            continue;
        };
        if path.is_absolute()
            && metadata.is_file()
            && metadata.uid() == 0
            && metadata.permissions().mode() & 0o022 == 0
            && metadata.permissions().mode() & 0o100 != 0
        {
            return Ok(ExecutableIdentity {
                path,
                device: metadata.dev(),
                inode: metadata.ino(),
            });
        }
    }
    Err("trusted absolute Python interpreter is unavailable".into())
}

#[cfg(unix)]
fn revalidate_python_identity(expected: &ExecutableIdentity) -> Result<(), String> {
    if &trusted_python_identity()? == expected {
        Ok(())
    } else {
        Err("trusted Python interpreter identity changed".into())
    }
}

#[cfg(unix)]
#[derive(Debug, PartialEq, Eq)]
struct HelperObservation {
    uid: u32,
    process_group_id: u32,
    process_group_started_at: u64,
    executable_device: u64,
    executable_inode: u64,
    command: Vec<Vec<u8>>,
}

#[cfg(unix)]
fn verify_helper(
    python: &ExecutableIdentity,
    _run_id: &str,
    _generation: &str,
    handshake: &Handshake,
    expected_argv: &[String],
    _timeout: Duration,
) -> Result<(), String> {
    let before = observe_helper(handshake.process_id)?;
    if before.uid != unsafe { libc::geteuid() }
        || before.process_group_id != handshake.process_group_id
        || before.process_group_started_at != handshake.process_group_started_at
        || before.executable_device != python.device
        || before.executable_inode != python.inode
        || !command_matches(&before.command, expected_argv)
    {
        return Err("managed Preview helper process identity changed".into());
    }
    let after = observe_helper(handshake.process_id)?;
    if after != before {
        return Err("managed Preview helper changed during authentication".into());
    }
    Ok(())
}

#[cfg(unix)]
fn observe_helper(pid: u32) -> Result<HelperObservation, String> {
    use std::os::unix::fs::MetadataExt;

    let stat_path = format!("/proc/{pid}/stat");
    let before = helper_stat_identity(&read_bounded_bytes(&stat_path, HELPER_OBSERVATION_LIMIT)?)?;
    let process = std::fs::metadata(format!("/proc/{pid}"))
        .map_err(|error| format!("inspect managed Preview helper uid: {error}"))?;
    let executable = std::fs::metadata(format!("/proc/{pid}/exe"))
        .map_err(|error| format!("inspect managed Preview helper executable: {error}"))?;
    if !executable.is_file() {
        return Err("managed Preview helper executable is not a regular file".into());
    }
    let command = read_bounded_bytes(&format!("/proc/{pid}/cmdline"), 128 * 1024)?
        .split(|byte| *byte == 0)
        .filter(|argument| !argument.is_empty())
        .map(|argument| argument.to_vec())
        .collect::<Vec<_>>();
    let after = helper_stat_identity(&read_bounded_bytes(&stat_path, HELPER_OBSERVATION_LIMIT)?)?;
    if after != before {
        return Err("managed Preview helper changed during observation".into());
    }
    Ok(HelperObservation {
        uid: process.uid(),
        process_group_id: before.0,
        process_group_started_at: before.1,
        executable_device: executable.dev(),
        executable_inode: executable.ino(),
        command,
    })
}

#[cfg(unix)]
fn helper_stat_identity(stat: &[u8]) -> Result<(u32, u64), String> {
    let text = std::str::from_utf8(stat).map_err(|_| "managed Preview helper stat is not UTF-8")?;
    let fields = text
        .rsplit_once(") ")
        .ok_or("managed Preview helper process stat is malformed")?
        .1
        .split_whitespace()
        .collect::<Vec<_>>();
    let group = fields
        .get(2)
        .and_then(|value| value.parse::<u32>().ok())
        .filter(|value| *value > 0)
        .ok_or("managed Preview helper process group is invalid")?;
    let started = fields
        .get(19)
        .and_then(|value| value.parse::<u64>().ok())
        .filter(|value| *value > 0)
        .ok_or("managed Preview helper start ticks are invalid")?;
    Ok((group, started))
}

#[cfg(any(unix, test))]
fn command_matches(actual: &[Vec<u8>], expected: &[String]) -> bool {
    actual.len() == expected.len()
        && actual
            .iter()
            .zip(expected)
            .all(|(actual, expected)| actual == expected.as_bytes())
}

#[cfg(unix)]
fn read_bounded_bytes(path: &str, max: usize) -> Result<Vec<u8>, String> {
    let file = std::fs::File::open(path).map_err(|error| format!("open {path}: {error}"))?;
    let mut bytes = Vec::new();
    file.take(max as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| format!("read {path}: {error}"))?;
    if bytes.len() > max {
        return Err(format!("{path} exceeds its byte bound"));
    }
    Ok(bytes)
}

#[cfg(windows)]
pub(super) fn trusted_wsl_path() -> Result<PathBuf, String> {
    use windows::Win32::System::SystemInformation::GetSystemDirectoryW;

    let mut buffer = vec![0u16; 32_768];
    let length = unsafe { GetSystemDirectoryW(Some(&mut buffer)) } as usize;
    if length == 0 || length >= buffer.len() {
        return Err("Windows system directory is unavailable".into());
    }
    let system = PathBuf::from(String::from_utf16_lossy(&buffer[..length]));
    let canonical_system = std::fs::canonicalize(&system)
        .map_err(|error| format!("validate Windows system directory: {error}"))?;
    let wsl = std::fs::canonicalize(system.join("wsl.exe"))
        .map_err(|error| format!("validate Windows WSL executable: {error}"))?;
    let metadata = std::fs::metadata(&wsl)
        .map_err(|error| format!("inspect Windows WSL executable: {error}"))?;
    if !metadata.is_file()
        || !wsl.parent().is_some_and(|parent| {
            parent
                .to_string_lossy()
                .eq_ignore_ascii_case(&canonical_system.to_string_lossy())
        })
        || !wsl
            .file_name()
            .is_some_and(|name| name.to_string_lossy().eq_ignore_ascii_case("wsl.exe"))
    {
        return Err("Windows WSL executable is outside the system directory".into());
    }
    Ok(wsl)
}

#[cfg(windows)]
fn trusted_python_identity() -> Result<ExecutableIdentity, String> {
    let mut command = Command::new(trusted_wsl_path()?);
    command
        .arg("-d")
        .arg(crate::files::host_distro())
        .arg("-e")
        .arg("/usr/bin/python3")
        .arg("-I")
        .arg("-c")
        .arg(PYTHON_IDENTITY_PY);
    use std::os::windows::process::CommandExt;
    command.creation_flags(0x0800_0000);
    let output = crate::bounded_exec::output_with_timeout_and_limit(
        command,
        crate::bounded_exec::WSL_PROBE_TIMEOUT,
        HELPER_OBSERVATION_LIMIT,
    )
    .map_err(|error| format!("resolve trusted WSL Python interpreter: {error}"))?;
    if !output.status.success() || !output.stderr.is_empty() {
        return Err("trusted WSL Python interpreter is unavailable".into());
    }
    serde_json::from_slice(&output.stdout)
        .map_err(|_| "trusted WSL Python identity is malformed".into())
}

#[cfg(windows)]
fn revalidate_python_identity(expected: &ExecutableIdentity) -> Result<(), String> {
    if &trusted_python_identity()? == expected {
        Ok(())
    } else {
        Err("trusted WSL Python interpreter identity changed".into())
    }
}

#[cfg(windows)]
fn verify_helper(
    python: &ExecutableIdentity,
    _run_id: &str,
    _generation: &str,
    handshake: &Handshake,
    expected_argv: &[String],
    timeout: Duration,
) -> Result<(), String> {
    let expected = serde_json::json!({
        "pid": handshake.process_id,
        "group": handshake.process_group_id,
        "started": handshake.process_group_started_at,
        "python": python,
        "argv": expected_argv,
    });
    let mut command = Command::new(trusted_wsl_path()?);
    command
        .arg("-d")
        .arg(crate::files::host_distro())
        .arg("-e")
        .arg(&python.path)
        .arg("-I")
        .arg("-c")
        .arg(VERIFY_HELPER_PY)
        .arg(expected.to_string());
    use std::os::windows::process::CommandExt;
    command.creation_flags(0x0800_0000);
    let output = crate::bounded_exec::output_with_timeout_and_limit(
        command,
        timeout,
        HELPER_OBSERVATION_LIMIT,
    )
    .map_err(|error| format!("observe managed Preview WSL helper: {error}"))?;
    if output.status.success() && output.stdout == b"ok\n" && output.stderr.is_empty() {
        Ok(())
    } else {
        Err("managed Preview WSL helper identity could not be authenticated".into())
    }
}

#[cfg(windows)]
const PYTHON_IDENTITY_PY: &str = r#"import json, os, stat
for candidate in ('/usr/bin/python3', '/bin/python3'):
    path = os.path.realpath(candidate)
    try:
        details = os.stat(path, follow_symlinks=False)
    except OSError:
        continue
    if (path.startswith('/') and stat.S_ISREG(details.st_mode) and details.st_uid == 0
            and not details.st_mode & (stat.S_IWGRP | stat.S_IWOTH)
            and details.st_mode & stat.S_IXUSR):
        print(json.dumps({'path': path, 'device': details.st_dev, 'inode': details.st_ino}, separators=(',', ':')))
        raise SystemExit(0)
raise SystemExit(1)"#;

#[cfg(windows)]
const VERIFY_HELPER_PY: &str = r#"import json, os, stat, sys
expected = json.loads(sys.argv[1])
pid = expected['pid']
def stat_fields():
    with open(f'/proc/{pid}/stat', 'rb') as handle:
        data = handle.read(4097)
    if len(data) > 4096:
        raise SystemExit(2)
    fields = data.rsplit(b') ', 1)[1].split()
    return int(fields[2]), int(fields[19])
def snapshot():
    before = stat_fields()
    process = os.stat(f'/proc/{pid}')
    executable = os.stat(f'/proc/{pid}/exe')
    with open(f'/proc/{pid}/cmdline', 'rb') as handle:
        command = handle.read(131073)
    if len(command) > 131072:
        raise SystemExit(2)
    after = stat_fields()
    if after != before:
        raise SystemExit(2)
    return (process.st_uid, before[0], before[1], executable.st_dev,
            executable.st_ino, stat.S_ISREG(executable.st_mode),
            tuple(part for part in command.split(b'\0') if part))
before = snapshot()
after = snapshot()
wanted = tuple(value.encode() for value in expected['argv'])
valid = (before == after and before[0] == os.geteuid()
         and before[1] == expected['group'] and before[2] == expected['started']
         and before[3] == expected['python']['device']
         and before[4] == expected['python']['inode'] and before[5]
         and before[6] == wanted)
if not valid:
    raise SystemExit(3)
print('ok')"#;

pub(crate) const SUPERVISOR_PY: &str = r#"import ctypes, os, select, signal, stat, subprocess, sys, time

READY = 'T_HUB_PREVIEW_READY'
GO = 'T_HUB_PREVIEW_GO'
VERSION = '1'
run_id, generation, executable, *arguments = sys.argv[1:]

if not run_id or len(run_id) > 160 or not all(c.isalnum() or c in '-_.:' for c in run_id):
    raise SystemExit(64)
if not generation or len(generation) != 32 or not generation.isalnum():
    raise SystemExit(65)

unshare_path = '/usr/bin/unshare'
try:
    unshare = os.stat(unshare_path, follow_symlinks=False)
except OSError:
    raise SystemExit(66)
if (not stat.S_ISREG(unshare.st_mode) or unshare.st_uid != 0
        or unshare.st_mode & (stat.S_IWGRP | stat.S_IWOTH)
        or not unshare.st_mode & stat.S_IXUSR):
    raise SystemExit(66)

os.setsid()
libc = ctypes.CDLL(None, use_errno=True)
if libc.prctl(36, 1, 0, 0, 0) != 0:
    raise SystemExit(67)

pid = os.getpid()
pgid = os.getpgrp()
with open(f'/proc/{pid}/stat', 'rb') as handle:
    process_stat = handle.read(4097)
if len(process_stat) > 4096:
    raise SystemExit(68)
fields = process_stat.rsplit(b') ', 1)[1].split()
started = int(fields[19])
if not hasattr(os, 'pidfd_open') or not hasattr(signal, 'pidfd_send_signal'):
    raise SystemExit(69)
print(READY, VERSION, generation, pid, pgid, started, 'WAITING', flush=True)

gate = sys.stdin.readline()
if gate != f'{GO} {generation}\n':
    raise SystemExit(70)
if libc.prctl(4, 0, 0, 0, 0) != 0:
    raise SystemExit(71)

environment = os.environ.copy()
environment.update({
    'HOST': '0.0.0.0',
    'HOSTNAME': '0.0.0.0',
    'NUXT_HOST': '0.0.0.0',
    'ASTRO_HOST': '0.0.0.0',
    'TAURI_DEV_HOST': '0.0.0.0',
})
launcher_script = r'''import ctypes, signal, subprocess, sys
libc = ctypes.CDLL(None, use_errno=True)
if libc.prctl(4, 0, 0, 0, 0) != 0:
    raise SystemExit(74)
target = subprocess.Popen(
    sys.argv[1:],
    stdin=subprocess.DEVNULL,
    stdout=sys.stdout,
    stderr=sys.stderr,
    close_fds=True,
)
try:
    status = target.wait()
except BaseException:
    try:
        target.kill()
    except ProcessLookupError:
        pass
    target.wait()
    raise
raise SystemExit(status)'''
launcher = subprocess.Popen(
    [
        unshare_path,
        '--user',
        '--map-current-user',
        '--pid',
        '--fork',
        '--mount',
        '--mount-proc',
        '--kill-child=KILL',
        sys.executable,
        '-I',
        '-c',
        launcher_script,
        executable,
        *arguments,
    ],
    stdin=subprocess.DEVNULL,
    stdout=sys.stdout,
    stderr=sys.stderr,
    env=environment,
    close_fds=True,
)

tracked = {}
SCAN_LIMIT = 32768
SCAN_SLICE = 0.1

def process_identity(candidate):
    with open(f'/proc/{candidate}/stat', 'rb') as handle:
        value = handle.read(4097)
    if len(value) > 4096:
        raise OSError('oversized process stat')
    fields = value.rsplit(b') ', 1)[1].split()
    return int(fields[1]), int(fields[19])

def discover_descendants(deadline):
    snapshot = {}
    complete = True
    with os.scandir('/proc') as entries:
        for index, entry in enumerate(entries):
            if index >= SCAN_LIMIT or time.monotonic() >= deadline:
                complete = False
                break
            if not entry.name.isdigit():
                continue
            candidate = int(entry.name)
            if candidate == pid:
                continue
            try:
                snapshot[candidate] = process_identity(candidate)
            except (OSError, ValueError, IndexError):
                pass
    descendants = set()
    changed = True
    while changed:
        changed = False
        for candidate, (parent, _) in snapshot.items():
            if candidate not in descendants and (parent == pid or parent in descendants):
                descendants.add(candidate)
                changed = True
    return ({candidate: snapshot[candidate][1] for candidate in descendants}, complete)

def track_descendants(deadline):
    descendants, complete = discover_descendants(deadline)
    for candidate, start in descendants.items():
        current = tracked.get(candidate)
        if current is not None and current[0] == start:
            continue
        if current is not None:
            os.close(current[1])
            del tracked[candidate]
        try:
            descriptor = os.pidfd_open(candidate, 0)
            if process_identity(candidate)[1] != start:
                os.close(descriptor)
                continue
            tracked[candidate] = (start, descriptor)
        except (OSError, ValueError, IndexError):
            pass
    for candidate, (start, descriptor) in list(tracked.items()):
        try:
            if process_identity(candidate)[1] == start:
                continue
        except (OSError, ValueError, IndexError):
            pass
        os.close(descriptor)
        del tracked[candidate]
    return complete

def reap():
    while True:
        try:
            child, _ = os.waitpid(-1, os.WNOHANG)
        except ChildProcessError:
            return
        if child == 0:
            return

def signal_tracked(kind):
    for _, descriptor in list(tracked.values()):
        try:
            signal.pidfd_send_signal(descriptor, kind)
        except ProcessLookupError:
            pass

def clean_descendants():
    signal.signal(signal.SIGTERM, signal.SIG_IGN)
    grace = time.monotonic() + 2.0
    while time.monotonic() < grace:
        complete = track_descendants(min(grace, time.monotonic() + SCAN_SLICE))
        signal_tracked(signal.SIGTERM)
        reap()
        complete = track_descendants(min(grace, time.monotonic() + SCAN_SLICE)) and complete
        if complete and not tracked:
            return
        time.sleep(0.02)
    kill_deadline = time.monotonic() + 2.0
    while time.monotonic() < kill_deadline:
        complete = track_descendants(min(kill_deadline, time.monotonic() + SCAN_SLICE))
        signal_tracked(signal.SIGKILL)
        reap()
        complete = track_descendants(min(kill_deadline, time.monotonic() + SCAN_SLICE)) and complete
        if complete and not tracked:
            reap()
            return
        time.sleep(0.02)
    try:
        print(f'T_HUB_PREVIEW_CLEANUP_PENDING {generation}', file=sys.stderr, flush=True)
    except BrokenPipeError:
        pass
    while True:
        complete = track_descendants(time.monotonic() + SCAN_SLICE)
        signal_tracked(signal.SIGKILL)
        reap()
        complete = track_descendants(time.monotonic() + SCAN_SLICE) and complete
        if complete and not tracked:
            return
        time.sleep(0.02)

status = None
stopped = False
while status is None:
    if not track_descendants(time.monotonic() + SCAN_SLICE):
        stopped = True
        break
    try:
        observed, code = os.waitpid(launcher.pid, os.WNOHANG)
    except ChildProcessError:
        observed, code = launcher.pid, 0
    if observed == launcher.pid:
        status = code
        break
    readable, _, _ = select.select([sys.stdin.fileno()], [], [], 0.02)
    if readable:
        if not os.read(sys.stdin.fileno(), 4096):
            stopped = True
            break

clean_descendants()
try:
    print(f'T_HUB_PREVIEW_STOPPED {generation}', file=sys.stderr, flush=True)
except BrokenPipeError:
    pass
if stopped:
    raise SystemExit(0)
if os.WIFEXITED(status):
    raise SystemExit(os.WEXITSTATUS(status))
if os.WIFSIGNALED(status):
    raise SystemExit(128 + os.WTERMSIG(status))
raise SystemExit(71)"#;

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(target_os = "linux")]
    use std::io::BufRead;

    #[cfg(target_os = "linux")]
    fn assert_exclusive_lock_available(path: &Path) {
        use std::os::fd::AsRawFd;

        let file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(path)
            .unwrap();
        assert_eq!(
            unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) },
            0,
            "surviving process still holds {}",
            path.display()
        );
    }

    #[test]
    fn handshake_requires_generation_and_group_leader_identity() {
        let generation = "a".repeat(32);
        let line = format!("{READY_PREFIX} {PROTOCOL_VERSION} {generation} 42 42 99 WAITING");
        assert_eq!(
            parse_handshake(line.as_bytes(), &generation).unwrap(),
            Handshake {
                process_id: 42,
                process_group_id: 42,
                process_group_started_at: 99,
            }
        );
        assert!(parse_handshake(line.as_bytes(), &"b".repeat(32)).is_err());
        assert!(parse_handshake(
            format!("{READY_PREFIX} 1 {generation} 42 41 99 WAITING").as_bytes(),
            &generation
        )
        .is_err());
    }

    #[test]
    fn packaged_windows_command_uses_explicit_wsl_python_and_typed_argv() {
        let command = windows_supervisor_command(
            Path::new(r"C:\Windows\System32\wsl.exe"),
            "Ubuntu-24.04",
            "/home/user/project",
            "apps/web",
            Path::new("/usr/bin/python3.12"),
            "run-1",
            &"a".repeat(32),
            "pnpm",
            &["run", "odd; $(unsafe)"],
        );
        assert_eq!(
            command.get_program(),
            std::ffi::OsStr::new(r"C:\Windows\System32\wsl.exe")
        );
        let arguments = command
            .get_args()
            .map(|argument| argument.to_string_lossy().into_owned())
            .collect::<Vec<_>>();
        assert_eq!(
            arguments,
            [
                "-d",
                "Ubuntu-24.04",
                "--cd",
                "/",
                "-e",
                "/usr/bin/python3.12",
                "-I",
                "-c",
                CWD_GATE_PY,
                "/home/user/project",
                "apps/web",
                "/usr/bin/python3.12",
                "-I",
                "-c",
                SUPERVISOR_PY,
                "run-1",
                &"a".repeat(32),
                "pnpm",
                "run",
                "odd; $(unsafe)",
            ]
        );
    }

    #[test]
    fn helper_command_authentication_requires_the_complete_exact_argv() {
        let expected = vec![
            "/usr/bin/python3".to_string(),
            "-I".to_string(),
            "-c".to_string(),
            "trusted-script".to_string(),
            "run-1".to_string(),
        ];
        let actual = expected
            .iter()
            .map(|argument| argument.as_bytes().to_vec())
            .collect::<Vec<_>>();
        assert!(command_matches(&actual, &expected));
        assert!(!command_matches(&actual[..4], &expected));
        let mut appended = actual.clone();
        appended.push(b"untrusted-extra".to_vec());
        assert!(!command_matches(&appended, &expected));
        let mut replaced = actual;
        replaced[0] = b"/tmp/python3".to_vec();
        assert!(!command_matches(&replaced, &expected));
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn target_starts_only_after_authentication_and_lifeline_reaps_tree() {
        let fixture = tempfile::tempdir().unwrap();
        let started = fixture.path().join("started");
        let descendant = fixture.path().join("descendant");
        let lock = fixture.path().join("descendant-lock");
        let script = format!(
            "echo 'T_HUB_PREVIEW_READY 1 spoof 9 9 9 9 9 WAITING'; test ! -e '{0}'; touch '{0}'; flock -x '{2}' sleep 30 & echo ready > '{1}'; wait",
            started.display(),
            descendant.display(),
            lock.display()
        );
        let prepared =
            prepare_supervised_preview_command(fixture.path(), "gated-run", "sh", &["-c", &script])
                .unwrap();
        assert!(!started.exists());
        let mut supervised = prepared
            .spawn_authenticated(Duration::from_secs(3))
            .unwrap();
        let mut spoof = String::new();
        std::io::BufReader::new(supervised.stdout.take().unwrap())
            .read_line(&mut spoof)
            .unwrap();
        assert_eq!(
            spoof.trim(),
            "T_HUB_PREVIEW_READY 1 spoof 9 9 9 9 9 WAITING"
        );
        let deadline = Instant::now() + Duration::from_secs(2);
        while !descendant.exists() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(10));
        }
        supervised.stdin.take();
        assert!(supervised.child.wait().unwrap().success());
        assert_exclusive_lock_available(&lock);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn natural_parent_exit_reaps_surviving_descendant() {
        let fixture = tempfile::tempdir().unwrap();
        let descendant = fixture.path().join("descendant");
        let lock = fixture.path().join("descendant-lock");
        let script = format!(
            "flock -x '{}' sleep 30 & echo ready > '{}'; exit 0",
            lock.display(),
            descendant.display()
        );
        let prepared = prepare_supervised_preview_command(
            fixture.path(),
            "natural-exit-run",
            "sh",
            &["-c", &script],
        )
        .unwrap();
        let mut supervised = prepared
            .spawn_authenticated(Duration::from_secs(3))
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(3);
        let status = loop {
            match supervised.child.try_wait().unwrap() {
                Some(status) => break status,
                None if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(10)),
                None => panic!("persistent supervisor did not finish"),
            }
        };
        let mut stderr = String::new();
        supervised
            .stderr
            .take()
            .unwrap()
            .read_to_string(&mut stderr)
            .unwrap();
        assert!(status.success(), "status {status:?}, stderr: {stderr}");
        assert_exclusive_lock_available(&lock);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn isolated_helper_ignores_project_and_pythonpath_shadow_modules_before_ready() {
        let fixture = tempfile::tempdir().unwrap();
        let pythonpath = tempfile::tempdir().unwrap();
        let project_marker = fixture.path().join("project-shadow-loaded");
        let pythonpath_marker = fixture.path().join("pythonpath-shadow-loaded");
        std::fs::write(
            fixture.path().join("subprocess.py"),
            format!(
                "open({:?}, 'w').close()\n",
                project_marker.to_string_lossy()
            ),
        )
        .unwrap();
        std::fs::write(
            pythonpath.path().join("select.py"),
            format!(
                "open({:?}, 'w').close()\n",
                pythonpath_marker.to_string_lossy()
            ),
        )
        .unwrap();
        let mut prepared =
            prepare_supervised_preview_command(fixture.path(), "isolated-run", "sleep", &["30"])
                .unwrap();
        prepared.command.env("PYTHONPATH", pythonpath.path());
        assert!(!project_marker.exists());
        assert!(!pythonpath_marker.exists());
        let mut supervised = prepared
            .spawn_authenticated(Duration::from_secs(3))
            .unwrap();
        assert!(!project_marker.exists());
        assert!(!pythonpath_marker.exists());
        supervised.stdin.take();
        assert!(supervised.child.wait().unwrap().success());
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn target_cannot_reopen_supervisor_lifeline_through_proc() {
        let fixture = tempfile::tempdir().unwrap();
        let result = fixture.path().join("fd-result");
        let script = r#"import os,sys,time
try:
    descriptor = os.open(f'/proc/{os.getppid()}/fd/0', os.O_WRONLY)
except PermissionError:
    outcome = 'denied'
else:
    outcome = 'opened'
    os.set_inheritable(descriptor, True)
open(sys.argv[1], 'w').write(outcome)
time.sleep(30)"#;
        let prepared = prepare_supervised_preview_command(
            fixture.path(),
            "protected-lifeline",
            "/usr/bin/python3",
            &["-I", "-c", script, result.to_str().unwrap()],
        )
        .unwrap();
        let generation = prepared.generation.clone();
        let mut supervised = prepared
            .spawn_authenticated(Duration::from_secs(3))
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(2);
        let outcome = loop {
            match std::fs::read_to_string(&result) {
                Ok(outcome) if !outcome.is_empty() => break outcome,
                Ok(_) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => panic!("read lifeline probe result: {error}"),
            }
            assert!(
                Instant::now() < deadline,
                "lifeline probe did not publish a result"
            );
            std::thread::sleep(Duration::from_millis(10));
        };
        assert_eq!(outcome, "denied");
        let stopped = Instant::now();
        supervised.stdin.take();
        assert!(supervised.child.wait().unwrap().success());
        assert!(stopped.elapsed() < Duration::from_secs(5));
        let mut stderr = String::new();
        supervised
            .stderr
            .take()
            .unwrap()
            .read_to_string(&mut stderr)
            .unwrap();
        assert!(
            stderr.contains(&format!("T_HUB_PREVIEW_STOPPED {generation}")),
            "missing correlated WSL cleanup acknowledgement: {stderr}"
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn target_killing_its_direct_parent_is_still_reaped_by_watchdog() {
        let fixture = tempfile::tempdir().unwrap();
        let result = fixture.path().join("parent-result");
        let lock_path = fixture.path().join("parent-killer-lock");
        let script = r#"import fcntl,os,signal,sys,time
lock = open(sys.argv[2], 'w')
fcntl.flock(lock, fcntl.LOCK_EX)
parent = os.getppid()
os.kill(os.getppid(), signal.SIGKILL)
time.sleep(0.1)
os.kill(parent, 0)
open(sys.argv[1], 'w').write(f'protected {parent}')
time.sleep(30)"#;
        let prepared = prepare_supervised_preview_command(
            fixture.path(),
            "parent-killer",
            "/usr/bin/python3",
            &[
                "-I",
                "-c",
                script,
                result.to_str().unwrap(),
                lock_path.to_str().unwrap(),
            ],
        )
        .unwrap();
        let mut supervised = prepared
            .spawn_authenticated(Duration::from_secs(3))
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(2);
        while !result.exists() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(std::fs::read_to_string(result).unwrap(), "protected 1");
        supervised.stdin.take();
        assert!(supervised.child.wait().unwrap().success());
        assert_exclusive_lock_available(&lock_path);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn fork_churn_cannot_escape_bounded_watchdog_scans() {
        let fixture = tempfile::tempdir().unwrap();
        let ready = fixture.path().join("fork-ready");
        let lock = fixture.path().join("fork-lock");
        let script = r#"import fcntl,os,signal,sys,time
signal.signal(signal.SIGTERM, signal.SIG_IGN)
lock = open(sys.argv[2], 'w')
fcntl.flock(lock, fcntl.LOCK_EX)
children = []
for index in range(64):
    child = os.fork()
    if child == 0:
        signal.signal(signal.SIGTERM, signal.SIG_IGN)
        time.sleep(30)
        os._exit(0)
    children.append(child)
temporary = sys.argv[1] + '.tmp'
with open(temporary, 'w') as ready:
    ready.write(' '.join(str(pid) for pid in children))
    ready.flush()
    os.fsync(ready.fileno())
os.replace(temporary, sys.argv[1])
while True:
    time.sleep(1)"#;
        let prepared = prepare_supervised_preview_command(
            fixture.path(),
            "fork-churn",
            "/usr/bin/python3",
            &[
                "-I",
                "-c",
                script,
                ready.to_str().unwrap(),
                lock.to_str().unwrap(),
            ],
        )
        .unwrap();
        let mut supervised = prepared
            .spawn_authenticated(Duration::from_secs(3))
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(3);
        while !ready.exists() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(10));
        }
        let pids = std::fs::read_to_string(&ready)
            .unwrap()
            .split_whitespace()
            .map(|value| value.parse::<u32>().unwrap())
            .collect::<Vec<_>>();
        assert_eq!(pids.len(), 64);
        let stopped = Instant::now();
        supervised.stdin.take();
        assert!(supervised.child.wait().unwrap().success());
        assert!(stopped.elapsed() < Duration::from_secs(6));
        assert_exclusive_lock_available(&lock);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn setsid_target_and_escaped_descendant_are_reaped_within_bound() {
        let fixture = tempfile::tempdir().unwrap();
        let identities = fixture.path().join("escaped-pids");
        let lock_path = fixture.path().join("escaped-lock");
        let script = r#"import fcntl,os,signal,sys,time
lock = open(sys.argv[2], 'w')
fcntl.flock(lock, fcntl.LOCK_EX)
os.setsid()
signal.signal(signal.SIGTERM, signal.SIG_IGN)
child = os.fork()
if child == 0:
    os.setsid()
    signal.signal(signal.SIGTERM, signal.SIG_IGN)
    time.sleep(30)
    os._exit(0)
open(sys.argv[1], 'w').write(f'{os.getpid()} {child}')
os._exit(0)"#;
        let prepared = prepare_supervised_preview_command(
            fixture.path(),
            "escaped-tree",
            "/usr/bin/python3",
            &[
                "-I",
                "-c",
                script,
                identities.to_str().unwrap(),
                lock_path.to_str().unwrap(),
            ],
        )
        .unwrap();
        let mut supervised = prepared
            .spawn_authenticated(Duration::from_secs(3))
            .unwrap();
        let started = Instant::now();
        let status = supervised.child.wait().unwrap();
        assert!(status.success());
        assert!(started.elapsed() < Duration::from_secs(5));
        let pids = std::fs::read_to_string(&identities)
            .unwrap()
            .split_whitespace()
            .map(|value| value.parse::<u32>().unwrap())
            .collect::<Vec<_>>();
        assert_eq!(pids.len(), 2);
        assert_exclusive_lock_available(&lock_path);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn correlation_failure_closes_gate_without_starting_target() {
        let fixture = tempfile::tempdir().unwrap();
        let started = fixture.path().join("started");
        let script = format!("touch '{}'", started.display());
        let mut prepared = prepare_supervised_preview_command(
            fixture.path(),
            "mismatched-generation",
            "sh",
            &["-c", &script],
        )
        .unwrap();
        prepared.generation = "f".repeat(32);
        assert!(prepared
            .spawn_authenticated(Duration::from_secs(2))
            .is_err());
        assert!(!started.exists());
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn authentication_timeout_kills_and_reaps_wrapper() {
        let fixture = tempfile::tempdir().unwrap();
        let pid_file = fixture.path().join("wrapper-pid");
        let mut prepared =
            prepare_supervised_preview_command(fixture.path(), "timeout-run", "true", &[]).unwrap();
        let mut command = Command::new(&prepared.python.path);
        command
            .arg("-I")
            .arg("-c")
            .arg(
                "import os,sys,time; open(sys.argv[1],'w').write(str(os.getpid())); time.sleep(30)",
            )
            .arg(&pid_file)
            .current_dir(fixture.path())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        prepared.command = command;
        let started = Instant::now();
        assert!(prepared
            .spawn_authenticated(Duration::from_millis(200))
            .is_err());
        assert!(started.elapsed() < Duration::from_secs(4));
        let pid = std::fs::read_to_string(pid_file)
            .unwrap()
            .parse::<u32>()
            .unwrap();
        assert!(!Path::new(&format!("/proc/{pid}")).exists());
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn pid_mismatch_closes_gate_and_reaps_wrapper() {
        let fixture = tempfile::tempdir().unwrap();
        let pid_file = fixture.path().join("wrapper-pid");
        let mut prepared =
            prepare_supervised_preview_command(fixture.path(), "pid-mismatch", "true", &[])
                .unwrap();
        let script = r#"import os,sys
os.setsid()
pid=os.getpid()
open(sys.argv[1],'w').write(str(pid))
fields=open(f'/proc/{pid}/stat','rb').read().rsplit(b') ',1)[1].split()
fake=pid+1
print('T_HUB_PREVIEW_READY', '1', sys.argv[2], fake, fake, int(fields[19]), 'WAITING', flush=True)
sys.stdin.buffer.read()"#;
        let mut command = Command::new(&prepared.python.path);
        command
            .arg("-I")
            .arg("-c")
            .arg(script)
            .arg(&pid_file)
            .arg(&prepared.generation)
            .current_dir(fixture.path())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        prepared.command = command;
        assert!(prepared
            .spawn_authenticated(Duration::from_secs(2))
            .is_err());
        let pid = std::fs::read_to_string(pid_file)
            .unwrap()
            .parse::<u32>()
            .unwrap();
        assert!(!Path::new(&format!("/proc/{pid}")).exists());
    }
}
