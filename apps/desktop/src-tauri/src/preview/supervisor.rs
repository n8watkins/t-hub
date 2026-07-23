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
}

pub(crate) struct SupervisedPreviewChild {
    pub child: Child,
    pub stdin: Option<ChildStdin>,
    pub stdout: Option<ChildStdout>,
    pub stderr: Option<ChildStderr>,
    #[allow(dead_code)]
    pub identity: ManagedRunIdentity,
    #[allow(dead_code)]
    generation: String,
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
    python_device: u64,
    python_inode: u64,
}

fn parse_handshake(line: &[u8], generation: &str) -> Result<Handshake, String> {
    let text = std::str::from_utf8(line).map_err(|_| "managed Preview handshake is not UTF-8")?;
    let fields = text.split_whitespace().collect::<Vec<_>>();
    if fields.len() != 9
        || fields[0] != READY_PREFIX
        || fields[1] != PROTOCOL_VERSION
        || fields[2] != generation
    {
        return Err("managed Preview handshake has an invalid authenticated shape".into());
    }
    let process_id = parse_nonzero(fields[3], "helper pid")?;
    let process_group_id = parse_nonzero(fields[4], "helper process group")?;
    let process_group_started_at = parse_nonzero_u64(fields[5], "helper start ticks")?;
    let python_device = parse_nonzero_u64(fields[6], "Python device")?;
    let python_inode = parse_nonzero_u64(fields[7], "Python inode")?;
    if fields[8] != "WAITING" || process_id != process_group_id {
        return Err("managed Preview helper is not its process-group leader".into());
    }
    Ok(Handshake {
        process_id,
        process_group_id,
        process_group_started_at,
        python_device,
        python_inode,
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
    super::managed_runner::validate_run_id(run_id)?;
    if executable.is_empty() || executable.as_bytes().contains(&0) {
        return Err("managed Preview executable is invalid".into());
    }
    let generation = uuid::Uuid::new_v4().simple().to_string();
    let python = trusted_python_identity()?;
    revalidate_python_identity(&python)?;
    let command = supervisor_command(
        cwd,
        &python.path,
        run_id,
        &generation,
        executable,
        arguments,
    )?;
    Ok(PreparedPreviewCommand {
        command,
        run_id: run_id.to_string(),
        generation,
        python,
    })
}

fn supervisor_command(
    cwd: &Path,
    python: &Path,
    run_id: &str,
    generation: &str,
    executable: &str,
    arguments: &[&str],
) -> Result<Command, String> {
    #[cfg(windows)]
    {
        let posix_cwd = super::managed_runner::unc_to_posix(&cwd.to_string_lossy())
            .ok_or("managed Preview root is not a WSL path")?;
        Ok(windows_supervisor_command(
            &trusted_wsl_path()?,
            &crate::files::host_distro(),
            &posix_cwd,
            python,
            run_id,
            generation,
            executable,
            arguments,
        ))
    }
    #[cfg(not(windows))]
    {
        let mut command = Command::new(python);
        command
            .arg("-c")
            .arg(SUPERVISOR_PY)
            .arg(run_id)
            .arg(generation)
            .arg(executable)
            .args(arguments)
            .current_dir(cwd)
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
    posix_cwd: &str,
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
        .arg(posix_cwd)
        .arg("-e")
        .arg(python)
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
fn verify_helper(
    python: &ExecutableIdentity,
    run_id: &str,
    generation: &str,
    handshake: &Handshake,
    _timeout: Duration,
) -> Result<(), String> {
    use std::os::unix::fs::MetadataExt;

    if handshake.python_device != python.device || handshake.python_inode != python.inode {
        return Err("managed Preview helper reported an unexpected Python identity".into());
    }
    let stat = read_bounded(
        &format!("/proc/{}/stat", handshake.process_id),
        HELPER_OBSERVATION_LIMIT,
    )?;
    let fields = stat
        .rsplit_once(") ")
        .ok_or("managed Preview helper process stat is malformed")?
        .1
        .split_whitespace()
        .collect::<Vec<_>>();
    let group = fields
        .get(2)
        .and_then(|value| value.parse::<u32>().ok())
        .ok_or("managed Preview helper process group is invalid")?;
    let started = fields
        .get(19)
        .and_then(|value| value.parse::<u64>().ok())
        .ok_or("managed Preview helper start ticks are invalid")?;
    let metadata = std::fs::metadata(format!("/proc/{}", handshake.process_id))
        .map_err(|error| format!("inspect managed Preview helper uid: {error}"))?;
    if metadata.uid() != unsafe { libc::geteuid() }
        || group != handshake.process_group_id
        || started != handshake.process_group_started_at
    {
        return Err("managed Preview helper process identity changed".into());
    }
    let cmdline = read_bounded_bytes(
        &format!("/proc/{}/cmdline", handshake.process_id),
        128 * 1024,
    )?;
    let arguments = cmdline
        .split(|byte| *byte == 0)
        .filter(|argument| !argument.is_empty())
        .collect::<Vec<_>>();
    let expected = [
        python.path.as_os_str().as_encoded_bytes(),
        b"-c",
        SUPERVISOR_PY.as_bytes(),
        run_id.as_bytes(),
        generation.as_bytes(),
    ];
    if arguments.len() < expected.len()
        || arguments
            .iter()
            .zip(expected)
            .any(|(actual, expected)| *actual != expected)
    {
        return Err("managed Preview helper command identity is unexpected".into());
    }
    let final_stat = read_bounded(
        &format!("/proc/{}/stat", handshake.process_id),
        HELPER_OBSERVATION_LIMIT,
    )?;
    let final_fields = final_stat
        .rsplit_once(") ")
        .ok_or("managed Preview helper process stat is malformed")?
        .1
        .split_whitespace()
        .collect::<Vec<_>>();
    let final_group = final_fields
        .get(2)
        .and_then(|value| value.parse::<u32>().ok());
    let final_started = final_fields
        .get(19)
        .and_then(|value| value.parse::<u64>().ok());
    if final_group != Some(group) || final_started != Some(started) {
        return Err("managed Preview helper changed during authentication".into());
    }
    Ok(())
}

#[cfg(unix)]
fn read_bounded(path: &str, max: usize) -> Result<String, String> {
    let bytes = read_bounded_bytes(path, max)?;
    String::from_utf8(bytes).map_err(|_| format!("{path} is not UTF-8"))
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
fn trusted_wsl_path() -> Result<PathBuf, String> {
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
    run_id: &str,
    generation: &str,
    handshake: &Handshake,
    timeout: Duration,
) -> Result<(), String> {
    if handshake.python_device != python.device || handshake.python_inode != python.inode {
        return Err("managed Preview helper reported an unexpected Python identity".into());
    }
    let expected = serde_json::json!({
        "pid": handshake.process_id,
        "group": handshake.process_group_id,
        "started": handshake.process_group_started_at,
        "python": python,
        "run": run_id,
        "generation": generation,
        "script": SUPERVISOR_PY,
    });
    let mut command = Command::new(trusted_wsl_path()?);
    command
        .arg("-d")
        .arg(crate::files::host_distro())
        .arg("-e")
        .arg(&python.path)
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
const VERIFY_HELPER_PY: &str = r#"import json, os, sys
expected = json.loads(sys.argv[1])
pid = expected['pid']
def stat_fields():
    with open(f'/proc/{pid}/stat', 'rb') as handle:
        data = handle.read(4097)
    if len(data) > 4096:
        raise SystemExit(2)
    return data.rsplit(b') ', 1)[1].split(), data
fields, _ = stat_fields()
details = os.stat(f'/proc/{pid}')
command = open(f'/proc/{pid}/cmdline', 'rb').read(131073)
wanted = [expected['python']['path'].encode(), b'-c', expected['script'].encode(),
          expected['run'].encode(), expected['generation'].encode()]
actual = [part for part in command.split(b'\0') if part]
fields2, _ = stat_fields()
valid = (details.st_uid == os.geteuid() and int(fields[2]) == expected['group']
         and int(fields[19]) == expected['started']
         and int(fields2[2]) == expected['group']
         and int(fields2[19]) == expected['started']
         and len(command) <= 131072 and actual[:len(wanted)] == wanted)
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

os.setsid()
libc = ctypes.CDLL(None, use_errno=True)
if libc.prctl(36, 1, 0, 0, 0) != 0:
    raise SystemExit(66)

pid = os.getpid()
pgid = os.getpgrp()
with open(f'/proc/{pid}/stat', 'rb') as handle:
    process_stat = handle.read(4097)
if len(process_stat) > 4096:
    raise SystemExit(67)
fields = process_stat.rsplit(b') ', 1)[1].split()
started = int(fields[19])
python = os.stat(os.path.realpath(sys.executable), follow_symlinks=False)
print(READY, VERSION, generation, pid, pgid, started, python.st_dev, python.st_ino, 'WAITING', flush=True)

gate = sys.stdin.readline()
if gate != f'{GO} {generation}\n':
    raise SystemExit(68)

environment = os.environ.copy()
environment.update({
    'HOST': '0.0.0.0',
    'HOSTNAME': '0.0.0.0',
    'NUXT_HOST': '0.0.0.0',
    'ASTRO_HOST': '0.0.0.0',
    'TAURI_DEV_HOST': '0.0.0.0',
})
target = subprocess.Popen(
    [executable, *arguments],
    stdin=subprocess.DEVNULL,
    stdout=sys.stdout,
    stderr=sys.stderr,
    env=environment,
    close_fds=True,
)

def reap():
    while True:
        try:
            child, _ = os.waitpid(-1, os.WNOHANG)
        except ChildProcessError:
            return
        if child == 0:
            return

def group_members():
    result = []
    for name in os.listdir('/proc'):
        if not name.isdigit():
            continue
        candidate = int(name)
        if candidate == pid:
            continue
        try:
            with open(f'/proc/{candidate}/stat', 'rb') as handle:
                value = handle.read(4097)
            if len(value) <= 4096 and int(value.rsplit(b') ', 1)[1].split()[2]) == pgid:
                result.append(candidate)
        except (OSError, ValueError, IndexError):
            pass
    return result

def clean_group():
    signal.signal(signal.SIGTERM, signal.SIG_IGN)
    try:
        os.killpg(pgid, signal.SIGTERM)
    except ProcessLookupError:
        pass
    grace = time.monotonic() + 2.0
    while time.monotonic() < grace:
        reap()
        if not group_members():
            return
        time.sleep(0.02)
    while True:
        members = group_members()
        if not members:
            reap()
            return
        for member in members:
            try:
                os.kill(member, signal.SIGKILL)
            except ProcessLookupError:
                pass
        reap()
        time.sleep(0.02)

status = None
stopped = False
while status is None:
    try:
        observed, code = os.waitpid(target.pid, os.WNOHANG)
    except ChildProcessError:
        observed, code = target.pid, 0
    if observed == target.pid:
        status = code
        break
    readable, _, _ = select.select([sys.stdin.fileno()], [], [], 0.02)
    if readable:
        if not os.read(sys.stdin.fileno(), 4096):
            stopped = True
            break

clean_group()
if stopped:
    raise SystemExit(0)
if os.WIFEXITED(status):
    raise SystemExit(os.WEXITSTATUS(status))
if os.WIFSIGNALED(status):
    raise SystemExit(128 + os.WTERMSIG(status))
raise SystemExit(70)"#;

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::BufRead;

    #[test]
    fn handshake_requires_generation_and_group_leader_identity() {
        let generation = "a".repeat(32);
        let line = format!("{READY_PREFIX} {PROTOCOL_VERSION} {generation} 42 42 99 8 123 WAITING");
        assert_eq!(
            parse_handshake(line.as_bytes(), &generation).unwrap(),
            Handshake {
                process_id: 42,
                process_group_id: 42,
                process_group_started_at: 99,
                python_device: 8,
                python_inode: 123,
            }
        );
        assert!(parse_handshake(line.as_bytes(), &"b".repeat(32)).is_err());
        assert!(parse_handshake(
            format!("{READY_PREFIX} 1 {generation} 42 41 99 8 123 WAITING").as_bytes(),
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
                "/home/user/project",
                "-e",
                "/usr/bin/python3.12",
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

    #[cfg(target_os = "linux")]
    #[test]
    fn target_starts_only_after_authentication_and_lifeline_reaps_tree() {
        let fixture = tempfile::tempdir().unwrap();
        let started = fixture.path().join("started");
        let descendant = fixture.path().join("descendant");
        let script = format!(
            "echo 'T_HUB_PREVIEW_READY 1 spoof 9 9 9 9 9 WAITING'; test ! -e '{0}'; touch '{0}'; sleep 30 & echo $! > '{1}'; wait",
            started.display(),
            descendant.display()
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
        let pid = std::fs::read_to_string(&descendant)
            .unwrap()
            .trim()
            .parse::<u32>()
            .unwrap();
        supervised.stdin.take();
        assert!(supervised.child.wait().unwrap().success());
        assert!(!Path::new(&format!("/proc/{pid}")).exists());
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn natural_parent_exit_reaps_surviving_descendant() {
        let fixture = tempfile::tempdir().unwrap();
        let descendant = fixture.path().join("descendant");
        let script = format!("sleep 30 & echo $! > '{}'; exit 0", descendant.display());
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
        let pid = std::fs::read_to_string(descendant)
            .unwrap()
            .trim()
            .parse::<u32>()
            .unwrap();
        assert!(!Path::new(&format!("/proc/{pid}")).exists());
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
details=os.stat(os.path.realpath(sys.executable),follow_symlinks=False)
fake=pid+1
print('T_HUB_PREVIEW_READY', '1', sys.argv[2], fake, fake, int(fields[19]), details.st_dev, details.st_ino, 'WAITING', flush=True)
sys.stdin.buffer.read()"#;
        let mut command = Command::new(&prepared.python.path);
        command
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
