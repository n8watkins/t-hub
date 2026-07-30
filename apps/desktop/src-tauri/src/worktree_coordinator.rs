//! Durable coordination for artifact cleanup reservations.
//!
//! A reservation is intentionally separate from Git worktree removal.
//! It blocks new activity in one exact linked worktree while an external storage
//! provider reclaims Cargo artifacts.
//! Completed records remain durable for recovery and audit, but only active
//! records participate in admission decisions.

use base64::{
    engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD},
    Engine as _,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::{self, Receiver};
use std::sync::{Arc, Mutex};
use std::time::Duration;

const SCHEMA_VERSION: u32 = 1;
const PROVIDER_OUTPUT_LIMIT: usize = 16 * 1024 * 1024;
const INSPECTION_OUTPUT_LIMIT: usize = 1024 * 1024;
const LAST_ERROR_LIMIT: usize = 8192;
const CONTAINMENT_EVIDENCE_LIMIT: usize = 64 * 1024;

const PROVIDER_SUPERVISOR_SCRIPT: &str = r#"
import base64
import ctypes
import json
import os
import pathlib
import selectors
import signal
import stat
import subprocess
import sys
import time

command = json.loads(sys.argv[1])
generation = sys.argv[2]
timeout = int(sys.argv[3])
unit = sys.argv[4]
output_limit = int(sys.argv[5])
terminator_script = sys.argv[6]
if (
    not command
    or len(generation) != 32
    or any(value not in "0123456789abcdef" for value in generation)
    or timeout < 1
    or timeout > 21600
    or unit != f"t-hub-provider-{generation}.scope"
    or output_limit != 16777216
):
    raise RuntimeError("provider supervisor arguments are invalid")

libc = ctypes.CDLL(None, use_errno=True)
if libc.prctl(36, 1, 0, 0, 0) != 0:
    raise RuntimeError("provider supervisor could not become a child subreaper")

def cgroup_path(pid):
    rows = pathlib.Path(f"/proc/{pid}/cgroup").read_text().splitlines()
    if len(rows) != 1 or not rows[0].startswith("0::/"):
        raise RuntimeError("provider cgroup identity is malformed")
    return rows[0][3:]

path = cgroup_path(os.getpid())
if pathlib.PurePosixPath(path).name != unit:
    raise RuntimeError("provider supervisor is outside its exact managed scope")
directory = os.open(
    "/sys/fs/cgroup" + path,
    os.O_RDONLY | os.O_DIRECTORY | os.O_CLOEXEC,
)
try:
    for control in ("cgroup.events", "cgroup.procs", "cgroup.kill"):
        if not stat.S_ISREG(os.stat(control, dir_fd=directory, follow_symlinks=False).st_mode):
            raise RuntimeError("provider managed cgroup controls are incomplete")
    identity = {
        "generation": generation,
        "unit": unit,
        "cgroupPath": path,
        "cgroupDevice": os.fstat(directory).st_dev,
        "cgroupInode": os.fstat(directory).st_ino,
    }
    print(json.dumps({"kind": "ready", "identity": identity}, separators=(",", ":")), flush=True)

    def terminate_owned_cgroup_on_parent_loss():
        terminator_unit = f"t-hub-provider-terminator-{generation}.service"
        encoded_identity = json.dumps(identity, separators=(",", ":"))
        while True:
            try:
                subprocess.run(
                    [
                        "/usr/bin/systemd-run",
                        "--user",
                        f"--unit={terminator_unit}",
                        "--service-type=exec",
                        "--collect",
                        "--quiet",
                        "/usr/bin/python3",
                        "-c",
                        terminator_script,
                        encoded_identity,
                    ],
                    stdin=subprocess.DEVNULL,
                    stdout=subprocess.DEVNULL,
                    stderr=subprocess.DEVNULL,
                    timeout=5,
                    check=False,
                )
            except subprocess.TimeoutExpired:
                pass
            time.sleep(0.1)

    authorization = sys.stdin.buffer.readline(64)
    if authorization != b"start\n":
        raise RuntimeError("provider authorization was not received")

    lifeline_pid = os.fork()
    if lifeline_pid == 0:
        lifeline_selector = selectors.DefaultSelector()
        lifeline_selector.register(sys.stdin.buffer, selectors.EVENT_READ, "parent")
        while True:
            for _ in lifeline_selector.select():
                os.read(sys.stdin.fileno(), 1)
                terminate_owned_cgroup_on_parent_loss()

    child = subprocess.Popen(
        command,
        stdin=subprocess.DEVNULL,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        start_new_session=True,
    )
    selector = selectors.DefaultSelector()
    selector.register(child.stdout, selectors.EVENT_READ, "stdout")
    selector.register(child.stderr, selectors.EVENT_READ, "stderr")
    output = {"stdout": bytearray(), "stderr": bytearray()}
    deadline = time.monotonic() + timeout
    failure = None
    while child.poll() is None:
        remaining = deadline - time.monotonic()
        if remaining <= 0:
            failure = "provider generation exceeded its timeout"
            break
        for key, _ in selector.select(min(remaining, 0.1)):
            chunk = os.read(key.fileobj.fileno(), 8192)
            if chunk:
                output[key.data].extend(chunk)
                if len(output[key.data]) > output_limit:
                    failure = "provider output exceeded its safe bound"
                    break
        if failure is not None:
            break

    if failure is not None:
        try:
            os.killpg(child.pid, signal.SIGTERM)
        except ProcessLookupError:
            pass
        stop_deadline = time.monotonic() + 2
        while child.poll() is None and time.monotonic() < stop_deadline:
            time.sleep(0.01)
        if child.poll() is None:
            try:
                os.killpg(child.pid, signal.SIGKILL)
            except ProcessLookupError:
                pass
        child.wait()
    else:
        child.wait()

    group_deadline = time.monotonic() + 2
    group_killed = False
    while True:
        while True:
            try:
                waited, _ = os.waitpid(-1, os.WNOHANG)
            except ChildProcessError:
                break
            if waited == 0:
                break
        try:
            os.killpg(child.pid, 0)
        except ProcessLookupError:
            break
        if time.monotonic() >= group_deadline:
            if group_killed:
                raise RuntimeError("provider descendant generation could not be terminated")
            if failure is None:
                failure = "provider descendant generation outlived its leader"
            try:
                os.killpg(child.pid, signal.SIGKILL)
            except ProcessLookupError:
                break
            group_killed = True
            group_deadline = time.monotonic() + 2
        time.sleep(0.01)

    for stream in ("stdout", "stderr"):
        file = getattr(child, stream)
        while True:
            chunk = os.read(file.fileno(), 8192)
            if not chunk:
                break
            output[stream].extend(chunk)
            if len(output[stream]) > output_limit:
                failure = "provider output exceeded its safe bound"
                break

    while True:
        try:
            waited, _ = os.waitpid(-1, os.WNOHANG)
        except ChildProcessError:
            break
        if waited == 0:
            break

    remaining = set()
    cgroup_directories = 0
    for current, directories, _ in os.walk("/sys/fs/cgroup" + path):
        cgroup_directories += 1
        if cgroup_directories > 256:
            raise RuntimeError("provider cgroup tree exceeds the safe bound")
        directories.sort()
        values = pathlib.Path(current, "cgroup.procs").read_text().split()
        remaining.update(int(value) for value in values)
    if remaining != {os.getpid(), lifeline_pid}:
        terminate_owned_cgroup_on_parent_loss()

    result = {
        "kind": "completed" if failure is None else "terminated",
        "identity": identity,
        "exitCode": child.returncode,
        "stdout": base64.b64encode(output["stdout"]).decode("ascii"),
        "stderr": base64.b64encode(output["stderr"]).decode("ascii"),
        "error": failure,
    }
    print(json.dumps(result, separators=(",", ":")), flush=True)
finally:
    os.close(directory)
"#;

const TERMINATE_PROVIDER_SCRIPT: &str = r#"
import json
import os
import pathlib
import stat
import sys
import time

identity = json.loads(sys.argv[1])
try:
    directory = os.open(
        "/sys/fs/cgroup" + identity["cgroupPath"],
        os.O_RDONLY | os.O_DIRECTORY | os.O_CLOEXEC,
    )
except FileNotFoundError:
    raise SystemExit(0)
try:
    if (
        os.fstat(directory).st_dev != identity["cgroupDevice"]
        or os.fstat(directory).st_ino != identity["cgroupInode"]
        or pathlib.Path(identity["cgroupPath"]).name != identity["unit"]
        or not stat.S_ISREG(
            os.stat("cgroup.kill", dir_fd=directory, follow_symlinks=False).st_mode
        )
    ):
        raise RuntimeError("provider managed cgroup identity is mismatched")
    kill_descriptor = os.open(
        "cgroup.kill",
        os.O_WRONLY | os.O_CLOEXEC,
        dir_fd=directory,
    )
    try:
        if os.write(kill_descriptor, b"1") != 1:
            raise RuntimeError("provider managed cgroup kill was partial")
    finally:
        os.close(kill_descriptor)
    deadline = time.monotonic() + 5
    while True:
        try:
            events = pathlib.Path(
                f"/sys/fs/cgroup{identity['cgroupPath']}/cgroup.events"
            ).read_text()
        except FileNotFoundError:
            break
        populated = dict(row.split() for row in events.splitlines()).get("populated")
        if populated == "0":
            break
        if populated != "1" or time.monotonic() >= deadline:
            raise RuntimeError("provider managed cgroup did not terminate")
        time.sleep(0.005)
finally:
    os.close(directory)
"#;

#[cfg(test)]
const PROBE_PROVIDER_STOPPED_SCRIPT: &str = r#"
import json
import os
import pathlib
import sys

identity = json.loads(sys.argv[1])
try:
    directory = os.open(
        "/sys/fs/cgroup" + identity["cgroupPath"],
        os.O_RDONLY | os.O_DIRECTORY | os.O_CLOEXEC,
    )
except FileNotFoundError:
    raise SystemExit(0)
try:
    if (
        os.fstat(directory).st_dev != identity["cgroupDevice"]
        or os.fstat(directory).st_ino != identity["cgroupInode"]
        or pathlib.Path(identity["cgroupPath"]).name != identity["unit"]
    ):
        raise RuntimeError("provider managed cgroup identity is mismatched")
    descriptor = os.open(
        "cgroup.events",
        os.O_RDONLY | os.O_CLOEXEC,
        dir_fd=directory,
    )
    try:
        raw = os.read(descriptor, 4097)
    finally:
        os.close(descriptor)
    values = dict(row.split() for row in raw.decode("ascii").splitlines())
    if len(raw) > 4096 or values.get("populated") != "0":
        raise RuntimeError("provider managed cgroup remains populated")
finally:
    os.close(directory)
"#;

const CONTAINMENT_FREEZE_WATCHDOG_SCRIPT: &str = r#"
import fcntl
import hashlib
import hmac
import json
import os
import pathlib
import select
import socket
import stat
import sys
import time

lease = json.loads(sys.argv[1])
secret = bytes.fromhex(sys.argv[2])
socket_name = sys.argv[3]
lease_path = lease["leasePath"]
provider = lease["provider"]
directory = None
freeze_descriptor = None
connection = None
changed = False
lease_owned = False

def canonical(value):
    return json.dumps(value, sort_keys=True, separators=(",", ":")).encode()

def signed(kind):
    body = {"kind": kind, "lease": lease}
    body["mac"] = hmac.new(secret, canonical(body), hashlib.sha256).hexdigest()
    return canonical(body)

def verify(raw, kind):
    value = json.loads(raw)
    mac = value.pop("mac", "")
    if (
        value != {"kind": kind, "lease": lease}
        or not hmac.compare_digest(mac, hmac.new(secret, canonical(value), hashlib.sha256).hexdigest())
    ):
        raise RuntimeError("freeze watchdog message authentication failed")

def event_value(key):
    descriptor = os.open("cgroup.events", os.O_RDONLY | os.O_CLOEXEC, dir_fd=directory)
    try:
        raw = os.read(descriptor, 4097)
    finally:
        os.close(descriptor)
    values = {}
    for row in raw.decode("ascii").splitlines():
        name, value = row.split()
        if name in values:
            raise RuntimeError("freeze watchdog cgroup events are ambiguous")
        values[name] = int(value)
    if len(raw) > 4096 or key not in values:
        raise RuntimeError("freeze watchdog cgroup events are incomplete")
    return values[key]

def write_freeze(value):
    if os.write(freeze_descriptor, value) != len(value):
        raise RuntimeError("freeze watchdog write was partial")

def wait_frozen(expected):
    deadline = time.monotonic() + 5
    while event_value("frozen") != expected:
        if time.monotonic() >= deadline:
            raise RuntimeError("freeze watchdog state did not converge")
        time.sleep(0.005)

def exact_identity():
    return (
        os.fstat(directory).st_dev == lease["cgroupDevice"]
        and os.fstat(directory).st_ino == lease["cgroupInode"]
        and pathlib.Path(lease["cgroupPath"]).name == lease["managedUnit"]
    )

def provider_directory():
    try:
        descriptor = os.open(
            "/sys/fs/cgroup" + provider["cgroupPath"],
            os.O_RDONLY | os.O_DIRECTORY | os.O_CLOEXEC,
        )
    except FileNotFoundError:
        return None
    if (
        os.fstat(descriptor).st_dev != provider["cgroupDevice"]
        or os.fstat(descriptor).st_ino != provider["cgroupInode"]
        or pathlib.Path(provider["cgroupPath"]).name != provider["unit"]
    ):
        os.close(descriptor)
        raise RuntimeError("provider managed cgroup identity is mismatched")
    return descriptor

def provider_populated(descriptor):
    provider_events = os.open(
        "cgroup.events",
        os.O_RDONLY | os.O_CLOEXEC,
        dir_fd=descriptor,
    )
    try:
        raw = os.read(provider_events, 4097)
    finally:
        os.close(provider_events)
    values = dict(row.split() for row in raw.decode("ascii").splitlines())
    if len(raw) > 4096 or values.get("populated") not in ("0", "1"):
        raise RuntimeError("provider managed cgroup events are incomplete")
    return values["populated"] == "1"

def terminate_provider():
    provider_cgroup = provider_directory()
    if provider_cgroup is None:
        return
    try:
        if provider_populated(provider_cgroup):
            kill_descriptor = os.open(
                "cgroup.kill",
                os.O_WRONLY | os.O_CLOEXEC,
                dir_fd=provider_cgroup,
            )
            try:
                if os.write(kill_descriptor, b"1") != 1:
                    raise RuntimeError("provider managed cgroup kill was partial")
            finally:
                os.close(kill_descriptor)
        deadline = time.monotonic() + 5
        while provider_populated(provider_cgroup):
            if time.monotonic() >= deadline:
                raise RuntimeError("provider managed cgroup did not terminate")
            time.sleep(0.005)
    finally:
        os.close(provider_cgroup)

def persist(state):
    value = dict(lease)
    value["state"] = state
    temporary = f"{lease_path}.{lease['watchdogNonce']}.tmp"
    descriptor = os.open(
        temporary,
        os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_CLOEXEC,
        0o600,
    )
    try:
        raw = canonical(value)
        if os.write(descriptor, raw) != len(raw):
            raise RuntimeError("freeze watchdog lease write was partial")
        os.fsync(descriptor)
    finally:
        os.close(descriptor)
    os.replace(temporary, lease_path)
    parent = os.open(
        str(pathlib.Path(lease_path).parent),
        os.O_RDONLY | os.O_DIRECTORY | os.O_CLOEXEC,
    )
    try:
        os.fsync(parent)
    finally:
        os.close(parent)

def read_existing():
    try:
        value = json.loads(pathlib.Path(lease_path).read_text())
    except FileNotFoundError:
        return None
    state = value.pop("state", None)
    if value != lease or state not in ("prepared", "armed", "thawed"):
        raise RuntimeError("freeze watchdog durable lease is mismatched")
    return state

def remove_lease():
    os.unlink(lease_path)
    parent = os.open(
        str(pathlib.Path(lease_path).parent),
        os.O_RDONLY | os.O_DIRECTORY | os.O_CLOEXEC,
    )
    try:
        os.fsync(parent)
    finally:
        os.close(parent)

def recv_until(deadline):
    remaining = deadline - time.monotonic()
    if remaining <= 0 or not select.select([connection], [], [], remaining)[0]:
        raise RuntimeError("freeze watchdog command deadline expired")
    raw = connection.recv(65537)
    if not raw or len(raw) > 65536:
        raise RuntimeError("freeze watchdog parent connection was lost")
    return raw

try:
    directory = os.open(
        "/sys/fs/cgroup" + lease["cgroupPath"],
        os.O_RDONLY | os.O_DIRECTORY | os.O_CLOEXEC,
    )
    if not exact_identity():
        raise RuntimeError("freeze watchdog cgroup identity is mismatched")
    for control in ("cgroup.freeze", "cgroup.events", "cgroup.procs"):
        if not stat.S_ISREG(os.stat(control, dir_fd=directory, follow_symlinks=False).st_mode):
            raise RuntimeError("freeze watchdog cgroup controls are incomplete")
    freeze_descriptor = os.open(
        "cgroup.freeze",
        os.O_WRONLY | os.O_CLOEXEC,
        dir_fd=directory,
    )
    fcntl.flock(freeze_descriptor, fcntl.LOCK_EX | fcntl.LOCK_NB)
    existing = read_existing()
    if existing is not None:
        lease_owned = True
        if existing == "armed" and exact_identity() and event_value("frozen") == 1:
            terminate_provider()
            write_freeze(b"0")
            wait_frozen(0)
        if exact_identity() and event_value("frozen") == 0:
            remove_lease()
            raise SystemExit(0)
        raise RuntimeError("freeze watchdog restart recovery is ambiguous")
    if event_value("frozen") != 0:
        raise RuntimeError("managed cgroup is already frozen by another owner")
    persist("prepared")
    lease_owned = True
    connection = socket.socket(socket.AF_UNIX, socket.SOCK_SEQPACKET)
    connection.settimeout(2)
    connection.connect("\0" + socket_name)
    connection.sendall(signed("ready"))
    verify(recv_until(min(lease["deadline"], time.monotonic() + 2)), "arm")
    persist("armed")
    write_freeze(b"1")
    changed = True
    wait_frozen(1)
    if not exact_identity():
        raise RuntimeError("freeze watchdog cgroup identity changed after freeze")
    connection.sendall(signed("frozen"))
    verify(recv_until(lease["deadline"]), "thaw")
    if not exact_identity() or event_value("frozen") != 1:
        raise RuntimeError("freeze watchdog thaw identity is mismatched")
    terminate_provider()
    write_freeze(b"0")
    wait_frozen(0)
    changed = False
    persist("thawed")
    connection.sendall(signed("thawed"))
    verify(recv_until(min(lease["deadline"], time.monotonic() + 2)), "disarm")
    if not exact_identity() or event_value("frozen") != 0:
        raise RuntimeError("freeze watchdog disarm identity is mismatched")
    remove_lease()
    connection.sendall(signed("disarmed"))
except SystemExit:
    raise
except Exception:
    recovered = False
    try:
        if (
            directory is not None
            and freeze_descriptor is not None
            and lease_owned
            and exact_identity()
            and changed
            and event_value("frozen") == 1
        ):
            terminate_provider()
            write_freeze(b"0")
            wait_frozen(0)
            changed = False
        if (
            directory is not None
            and lease_owned
            and exact_identity()
            and event_value("frozen") == 0
        ):
            try:
                remove_lease()
            except FileNotFoundError:
                pass
            recovered = True
    finally:
        if not recovered:
            raise
finally:
    if connection is not None:
        connection.close()
    if freeze_descriptor is not None:
        os.close(freeze_descriptor)
    if directory is not None:
        os.close(directory)
"#;

const ATOMIC_CONTAINMENT_INSPECTION_SCRIPT: &str = r#"
import base64
import hashlib
import hmac
import json
import os
import pathlib
import re
import secrets
import socket
import signal
import stat
import struct
import subprocess
import sys
import time

root_pid = int(sys.argv[1])
target = json.loads(sys.argv[2])
operation_id = sys.argv[3]
request_path = sys.argv[4]
provider = json.loads(sys.argv[5])
watchdog_script = sys.argv[6]
MAX_PROCESSES = 256
MAX_TASKS = 1024

def inspection_timeout(signum, frame):
    raise RuntimeError("managed containment inspection timed out")

signal.signal(signal.SIGALRM, inspection_timeout)
containment_timeout = int(sys.argv[7])
if containment_timeout < 60 or containment_timeout > 21630:
    raise RuntimeError("managed containment timeout is invalid")
signal.alarm(containment_timeout)

def validate_provider():
    if (
        not re.fullmatch(r"[0-9a-f]{32}", provider.get("generation", ""))
        or provider.get("unit") != f"t-hub-provider-{provider.get('generation')}.scope"
        or pathlib.PurePosixPath(provider.get("cgroupPath", "")).name != provider.get("unit")
    ):
        raise RuntimeError("provider managed cgroup identity is invalid")
    descriptor = os.open(
        "/sys/fs/cgroup" + provider["cgroupPath"],
        os.O_RDONLY | os.O_DIRECTORY | os.O_CLOEXEC,
    )
    try:
        if (
            os.fstat(descriptor).st_dev != provider["cgroupDevice"]
            or os.fstat(descriptor).st_ino != provider["cgroupInode"]
        ):
            raise RuntimeError("provider managed cgroup identity is mismatched")
        for control in ("cgroup.events", "cgroup.procs", "cgroup.kill"):
            if not stat.S_ISREG(
                os.stat(control, dir_fd=descriptor, follow_symlinks=False).st_mode
            ):
                raise RuntimeError("provider managed cgroup controls are incomplete")
    finally:
        os.close(descriptor)

validate_provider()

def proc_start(pid):
    value = pathlib.Path(f"/proc/{pid}/stat").read_text()
    end = value.rfind(")")
    fields = value[end + 2:].split()
    if end < 1 or len(fields) < 20:
        raise RuntimeError("managed process identity is incomplete")
    return fields[19]

def tasks(pid):
    values = [int(path.name) for path in pathlib.Path(f"/proc/{pid}/task").iterdir()]
    if not values or len(values) > MAX_TASKS:
        raise RuntimeError("managed task set is empty or exceeds the safe bound")
    return sorted(values)

def task_children(tid):
    return [
        int(value)
        for value in pathlib.Path(f"/proc/{tid}/task/{tid}/children").read_text().split()
    ]

def process_tree(roots):
    pending = list(roots)
    identities = {}
    edges = {}
    task_identities = {}
    while pending:
        pid = pending.pop()
        if pid in identities:
            continue
        if len(identities) >= MAX_PROCESSES:
            raise RuntimeError("managed process tree exceeds the safe bound")
        identities[pid] = proc_start(pid)
        descendants = set()
        for tid in tasks(pid):
            if len(task_identities) >= MAX_TASKS:
                raise RuntimeError("managed task tree exceeds the safe bound")
            task_identities[tid] = proc_start(tid)
            descendants.update(task_children(tid))
        edges[pid] = sorted(descendants)
        pending.extend(descendants)
    return identities, edges, task_identities

def descendants(edges, root):
    pending = [root]
    result = set()
    while pending:
        pid = pending.pop()
        if pid in result:
            continue
        result.add(pid)
        pending.extend(edges.get(pid, []))
    return result

def executable_matches(pid, expected):
    actual = pathlib.Path(f"/proc/{pid}/exe").stat()
    trusted = pathlib.Path(expected).stat()
    return (actual.st_dev, actual.st_ino) == (trusted.st_dev, trusted.st_ino)

def containment_pair(identities, edges):
    candidates = {
        pid for pid in identities if executable_matches(pid, "/usr/bin/bwrap")
    }
    if not candidates:
        return None
    pairs = [
        (outer, inner)
        for outer in candidates
        for inner in edges.get(outer, [])
        if inner in candidates
    ]
    if len(candidates) != 2 or len(pairs) != 1:
        raise RuntimeError("managed runtime lacks one exact containment boundary")
    return pairs[0]

def environment(pid):
    values = pathlib.Path(f"/proc/{pid}/environ").read_bytes()
    if len(values) > 131072:
        raise RuntimeError("managed process environment exceeds the safe bound")
    return values.split(b"\0")

def evidence_for(pid):
    encoded = next(
        (
            item.split(b"=", 1)[1]
            for item in environment(pid)
            if item.startswith(b"T_HUB_WORKTREE_CONTAINMENT=")
        ),
        None,
    )
    if encoded is None or len(encoded) > 65536:
        raise RuntimeError("managed process lacks bounded containment evidence")
    padding = b"=" * (-len(encoded) % 4)
    evidence = json.loads(base64.urlsafe_b64decode(encoded + padding))
    if (
        evidence.get("version") != 1
        or not re.fullmatch(r"[0-9a-f]{32}", evidence.get("launchNonce", ""))
        or not isinstance(evidence.get("blockers"), list)
    ):
        raise RuntimeError("managed process containment evidence is mismatched")
    return evidence

def cgroup_path(pid):
    rows = [row for row in pathlib.Path(f"/proc/{pid}/cgroup").read_text().splitlines() if row]
    if len(rows) != 1 or not rows[0].startswith("0::/"):
        raise RuntimeError("managed process cgroup identity is malformed")
    return rows[0][3:]

def namespace(tid, name):
    return os.readlink(f"/proc/{tid}/ns/{name}")

def target_is_masked(root):
    visible = os.stat(root + target["path"])
    return visible.st_dev != target["device"] or visible.st_ino != target["inode"]

def target_is_unreachable_or_masked(root):
    try:
        return target_is_masked(root)
    except (FileNotFoundError, PermissionError):
        return True

def mount_is_masked(tid):
    for line in pathlib.Path(f"/proc/{tid}/mountinfo").read_text().splitlines():
        fields = line.split()
        separator = fields.index("-")
        mountpoint = fields[4]
        for encoded, decoded in (
            ("\\040", " "),
            ("\\011", "\t"),
            ("\\012", "\n"),
            ("\\134", "\\"),
        ):
            mountpoint = mountpoint.replace(encoded, decoded)
        if mountpoint == target["path"] and fields[separator + 1] == "tmpfs":
            return True
    return False

def descriptors(pid):
    values = {}
    for fd in pathlib.Path(f"/proc/{pid}/fd").iterdir():
        values[fd.name] = os.readlink(fd)
    return values

def event_value(directory, key):
    values = {}
    descriptor = os.open("cgroup.events", os.O_RDONLY | os.O_CLOEXEC, dir_fd=directory)
    try:
        raw = os.read(descriptor, 4097)
    finally:
        os.close(descriptor)
    if len(raw) > 4096:
        raise RuntimeError("managed cgroup events exceed the safe bound")
    for row in raw.decode("ascii").splitlines():
        name, value = row.split()
        if name in values:
            raise RuntimeError("managed cgroup events are ambiguous")
        values[name] = int(value)
    if key not in values:
        raise RuntimeError("managed cgroup freeze evidence is incomplete")
    return values[key]

def managed_cgroup(pid):
    path = cgroup_path(pid)
    nonce = next(
        (
            value.split(b"=", 1)[1].decode("ascii")
            for value in environment(pid)
            if value.startswith(b"T_HUB_LAUNCH_NONCE=")
        ),
        None,
    )
    unit = pathlib.PurePosixPath(path).name
    expected = (
        f"/user.slice/user-{os.getuid()}.slice/user@{os.getuid()}.service/"
        f"app.slice/{unit}"
    )
    managed_shape = re.fullmatch(r"t-hub-[0-9a-f]{32}\.scope", unit) is not None
    if managed_shape or nonce is not None:
        if not managed_shape or nonce != unit[6:-6] or path != expected:
            raise RuntimeError("managed runtime ownership evidence is mismatched")
    if (
        not managed_shape
        or nonce != unit[6:-6]
        or path != expected
    ):
        return None
    directory = os.open(
        "/sys/fs/cgroup" + path,
        os.O_RDONLY | os.O_DIRECTORY | os.O_CLOEXEC,
    )
    inode = os.fstat(directory).st_ino
    for control in ("cgroup.freeze", "cgroup.events", "cgroup.procs"):
        if not stat.S_ISREG(os.stat(control, dir_fd=directory, follow_symlinks=False).st_mode):
            os.close(directory)
            raise RuntimeError("managed cgroup controls are incomplete")
    return path, inode, directory

def cgroup_processes(directory):
    descriptor = os.open("cgroup.procs", os.O_RDONLY | os.O_CLOEXEC, dir_fd=directory)
    try:
        raw = os.read(descriptor, 65537)
    finally:
        os.close(descriptor)
    if len(raw) > 65536:
        raise RuntimeError("managed cgroup process set exceeds the safe bound")
    values = sorted({int(value) for value in raw.split()})
    if not values or len(values) > MAX_PROCESSES:
        raise RuntimeError("managed cgroup process set is empty or exceeds the safe bound")
    return values

def canonical(value):
    return json.dumps(value, sort_keys=True, separators=(",", ":")).encode()

def signed(kind, lease, secret):
    body = {"kind": kind, "lease": lease}
    body["mac"] = hmac.new(secret, canonical(body), hashlib.sha256).hexdigest()
    return canonical(body)

def verify_message(raw, kind, lease, secret):
    value = json.loads(raw)
    mac = value.pop("mac", "")
    if (
        value != {"kind": kind, "lease": lease}
        or not hmac.compare_digest(mac, hmac.new(secret, canonical(value), hashlib.sha256).hexdigest())
    ):
        raise RuntimeError("freeze watchdog message authentication failed")

def receive(connection, kind, lease, secret, deadline):
    connection.settimeout(max(0.001, deadline - time.monotonic()))
    raw = connection.recv(65537)
    if not raw or len(raw) > 65536:
        raise RuntimeError("freeze watchdog response is missing or oversized")
    verify_message(raw, kind, lease, secret)

def command(operation):
    raw = sys.stdin.buffer.readline(65537)
    if not raw or len(raw) > 65536:
        raise RuntimeError("watchdog backend command is missing or oversized")
    value = json.loads(raw)
    if value != {"operation": operation}:
        raise RuntimeError("watchdog backend command is out of state")

def completed(operation, authorization=None):
    value = {"operation": operation}
    if authorization is not None:
        value["authorization"] = authorization
    sys.stdout.write(json.dumps(value, separators=(",", ":")) + "\n")
    sys.stdout.flush()

cgroup_directory = None
cgroup_identity = None
watchdog_listener = None
watchdog_connection = None
watchdog_lease = None
watchdog_secret = None
freeze_changed = False
watchdog_thawed = False
watchdog_disarmed = False
try:
    command("captureCgroup")
    managed = managed_cgroup(root_pid)
    if managed is None:
        raise RuntimeError("exact managed cgroup-v2 freezer ownership is unavailable")
    if (
        re.fullmatch(r"[0-9a-f]{32}", operation_id) is None
        or not os.path.isabs(request_path)
        or "\0" in request_path
    ):
        raise RuntimeError("freeze watchdog operation identity is invalid")
    managed_path, managed_inode, cgroup_directory = managed
    if event_value(cgroup_directory, "frozen") != 0:
        raise RuntimeError("managed cgroup is already frozen by another owner")
    completed("captureCgroup")
    command("createLease")
    watchdog_nonce = secrets.token_hex(16)
    watchdog_secret = secrets.token_bytes(32)
    watchdog_unit = f"t-hub-freeze-watchdog-{watchdog_nonce}.service"
    watchdog_socket = f"t-hub-freeze-watchdog-{watchdog_nonce}"
    watchdog_deadline = time.monotonic() + containment_timeout
    watchdog_lease = {
        "version": 1,
        "operationId": operation_id,
        "watchdogNonce": watchdog_nonce,
        "managedUnit": pathlib.PurePosixPath(managed_path).name,
        "cgroupPath": managed_path,
        "cgroupDevice": os.fstat(cgroup_directory).st_dev,
        "cgroupInode": managed_inode,
        "deadline": watchdog_deadline,
        "leasePath": f"{request_path}.freeze-{root_pid}-{watchdog_nonce}.json",
        "provider": provider,
    }
    completed("createLease")
    command("launchWatchdog")
    watchdog_listener = socket.socket(socket.AF_UNIX, socket.SOCK_SEQPACKET)
    watchdog_listener.bind("\0" + watchdog_socket)
    watchdog_listener.listen(1)
    watchdog_listener.settimeout(3)
    launch = subprocess.run(
        [
            "/usr/bin/systemd-run",
            "--user",
            f"--unit={watchdog_unit}",
            "--property=Type=exec",
            "--property=Restart=on-failure",
            "--property=RestartSec=100ms",
            f"--property=RuntimeMaxSec={containment_timeout}s",
            "--collect",
            "--quiet",
            "/usr/bin/python3",
            "-c",
            watchdog_script,
            json.dumps(watchdog_lease, separators=(",", ":")),
            watchdog_secret.hex(),
            watchdog_socket,
        ],
        stdin=subprocess.DEVNULL,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        timeout=3,
        check=False,
    )
    if launch.returncode != 0 or launch.stdout or launch.stderr:
        raise RuntimeError("freeze watchdog transient unit could not be started")
    completed("launchWatchdog")
    command("verifyReady")
    watchdog_connection, _ = watchdog_listener.accept()
    peer_pid, peer_uid, _ = struct.unpack(
        "3i",
        watchdog_connection.getsockopt(socket.SOL_SOCKET, socket.SO_PEERCRED, 12),
    )
    expected_watchdog_cgroup = (
        f"/user.slice/user-{os.getuid()}.slice/user@{os.getuid()}.service/"
        f"app.slice/{watchdog_unit}"
    )
    if (
        peer_uid != os.getuid()
        or cgroup_path(peer_pid) != expected_watchdog_cgroup
        or not executable_matches(peer_pid, "/usr/bin/python3")
    ):
        raise RuntimeError("freeze watchdog transient unit identity is mismatched")
    receive(
        watchdog_connection,
        "ready",
        watchdog_lease,
        watchdog_secret,
        min(watchdog_deadline, time.monotonic() + 2),
    )
    completed("verifyReady")
    command("arm")
    watchdog_connection.sendall(signed("arm", watchdog_lease, watchdog_secret))
    completed("arm")
    command("freeze")
    receive(
        watchdog_connection,
        "frozen",
        watchdog_lease,
        watchdog_secret,
        min(watchdog_deadline, time.monotonic() + 6),
    )
    freeze_changed = True
    completed("freeze")
    command("inspectEvidence")
    if (
        cgroup_path(root_pid) != managed_path
        or os.fstat(cgroup_directory).st_ino != managed_inode
        or event_value(cgroup_directory, "frozen") != 1
    ):
        raise RuntimeError("managed cgroup identity changed during freeze")
    roots = cgroup_processes(cgroup_directory)
    identities, edges, task_identities = process_tree(roots)
    cgroup_identity = (managed_path, managed_inode, roots)

    if any(cgroup_path(pid) != managed_path for pid in identities):
        raise RuntimeError("managed runtime crossed its exact cgroup")
    if cgroup_processes(cgroup_directory) != cgroup_identity[2]:
        raise RuntimeError("managed cgroup process set changed while frozen")

    pair = containment_pair(identities, edges)
    stable_descriptors = {}
    if pair is not None:
        supervisor_pid, namespace_supervisor = pair
        if (
            pathlib.Path(f"/proc/{supervisor_pid}").stat().st_uid != os.getuid()
            or pathlib.Path(f"/proc/{namespace_supervisor}").stat().st_uid != os.getuid()
        ):
            raise RuntimeError("containment supervisor ownership is mismatched")
        if not executable_matches(root_pid, "/usr/bin/systemd-run") and root_pid != supervisor_pid:
            raise RuntimeError("managed runtime root has an ambiguous executable identity")
        workload_roots = edges.get(namespace_supervisor, [])
        if len(workload_roots) != 1:
            raise RuntimeError("containment supervisor has an ambiguous child set")
        workload = descendants(edges, workload_roots[0])
        permitted = workload | {supervisor_pid, namespace_supervisor, root_pid}
        if set(identities) != permitted:
            raise RuntimeError("managed runtime contains an uncontained sibling process")
        for pid in (supervisor_pid, namespace_supervisor):
            stable_descriptors[pid] = descriptors(pid)
        expected_evidence = evidence_for(workload_roots[0])
        if target in expected_evidence["blockers"]:
            expected_mount = namespace(workload_roots[0], "mnt")
            expected_pid = namespace(workload_roots[0], "pid")
            expected_cgroup = cgroup_path(workload_roots[0])
            if (
                expected_mount != namespace(namespace_supervisor, "mnt")
                or expected_pid != namespace(namespace_supervisor, "pid")
                or expected_mount == namespace(supervisor_pid, "mnt")
                or expected_pid == namespace(supervisor_pid, "pid")
            ):
                raise RuntimeError("managed workload lacks private mount or PID namespaces")
            for pid in workload:
                if evidence_for(pid) != expected_evidence or cgroup_path(pid) != expected_cgroup:
                    raise RuntimeError("managed workload identity or cgroup changed")
                for tid in tasks(pid):
                    if (
                        namespace(tid, "mnt") != expected_mount
                        or namespace(tid, "pid") != expected_pid
                        or not target_is_masked(f"/proc/{tid}/root")
                        or not mount_is_masked(tid)
                    ):
                        raise RuntimeError("managed task escaped exact target containment")
                    for alias in (
                        f"/proc/{tid}/root/proc/1/root",
                        f"/proc/{tid}/root/proc/self/root",
                    ):
                        if not target_is_unreachable_or_masked(alias):
                            raise RuntimeError("managed task can access the target through proc root")
        else:
            for pid in identities:
                stable_descriptors[pid] = descriptors(pid)
    else:
        for pid in identities:
            stable_descriptors[pid] = descriptors(pid)

    if pair is None or target not in expected_evidence["blockers"]:
        for pid in identities:
            cwd = os.readlink(f"/proc/{pid}/cwd")
            if cwd == target["path"] or cwd.startswith(target["path"] + "/"):
                raise RuntimeError("preexisting managed workload cwd reaches the target")
            if any(
                value == target["path"] or value.startswith(target["path"] + "/")
                for value in stable_descriptors[pid].values()
            ):
                raise RuntimeError("preexisting managed workload retains a target descriptor")

    after_identities, after_edges, after_tasks = process_tree(
        cgroup_processes(cgroup_directory)
    )
    if (
        identities != after_identities
        or edges != after_edges
        or task_identities != after_tasks
        or any(stable_descriptors[pid] != descriptors(pid) for pid in stable_descriptors)
    ):
        raise RuntimeError("managed process or task set changed during containment inspection")
    completed("inspectEvidence", {
        "watchdogNonce": watchdog_nonce,
        "leaseDigest": hashlib.sha256(canonical(watchdog_lease)).hexdigest(),
    })
    command("thaw")
    if (
        cgroup_path(root_pid) != managed_path
        or os.fstat(cgroup_directory).st_ino != managed_inode
        or event_value(cgroup_directory, "frozen") != 1
    ):
        raise RuntimeError("managed cgroup identity changed before unfreeze")
    watchdog_connection.sendall(
        signed("thaw", watchdog_lease, watchdog_secret)
    )
    receive(
        watchdog_connection,
        "thawed",
        watchdog_lease,
        watchdog_secret,
        min(watchdog_lease["deadline"], time.monotonic() + 6),
    )
    freeze_changed = False
    watchdog_thawed = True
    completed("thaw")
    command("verifyThaw")
    if (
        cgroup_path(root_pid) != managed_path
        or os.fstat(cgroup_directory).st_ino != managed_inode
        or event_value(cgroup_directory, "frozen") != 0
    ):
        raise RuntimeError("managed cgroup identity changed after unfreeze")
    completed("verifyThaw")
    command("disarm")
    watchdog_connection.sendall(
        signed("disarm", watchdog_lease, watchdog_secret)
    )
    receive(
        watchdog_connection,
        "disarmed",
        watchdog_lease,
        watchdog_secret,
        min(watchdog_lease["deadline"], time.monotonic() + 2),
    )
    watchdog_disarmed = True
    completed("disarm")
finally:
    signal.alarm(0)
    unfreeze_error = None
    if watchdog_connection is not None and freeze_changed:
        try:
            if (
                cgroup_path(root_pid) != managed_path
                or os.fstat(cgroup_directory).st_ino != managed_inode
                or event_value(cgroup_directory, "frozen") != 1
            ):
                raise RuntimeError("managed cgroup identity changed before unfreeze")
            watchdog_connection.sendall(
                signed("thaw", watchdog_lease, watchdog_secret)
            )
            receive(
                watchdog_connection,
                "thawed",
                watchdog_lease,
                watchdog_secret,
                min(watchdog_lease["deadline"], time.monotonic() + 6),
            )
            watchdog_thawed = True
            if (
                cgroup_path(root_pid) != managed_path
                or os.fstat(cgroup_directory).st_ino != managed_inode
                or event_value(cgroup_directory, "frozen") != 0
            ):
                raise RuntimeError("managed cgroup identity changed after unfreeze")
            freeze_changed = False
        except Exception as error:
            unfreeze_error = error
    if (
        watchdog_connection is not None
        and watchdog_thawed
        and not watchdog_disarmed
        and unfreeze_error is None
    ):
        try:
            watchdog_connection.sendall(
                signed("disarm", watchdog_lease, watchdog_secret)
            )
            receive(
                watchdog_connection,
                "disarmed",
                watchdog_lease,
                watchdog_secret,
                min(watchdog_lease["deadline"], time.monotonic() + 2),
            )
            watchdog_disarmed = True
        except Exception as error:
            unfreeze_error = error
    if watchdog_connection is not None:
        watchdog_connection.close()
    if watchdog_listener is not None:
        watchdog_listener.close()
    if cgroup_directory is not None:
        os.close(cgroup_directory)
    if unfreeze_error is not None:
        raise RuntimeError(f"managed process unfreeze failed: {unfreeze_error}")
"#;

const CONTAINMENT_PREFLIGHT_SCRIPT: &str = r#"
import json
import os
import pathlib
import sys

targets = json.loads(sys.argv[1])
if not pathlib.Path("/bin/sh").is_file():
    raise RuntimeError("unrelated filesystem paths are unavailable")
for target in targets:
    path = target["path"]
    expected = (target["device"], target["inode"])
    parent = str(pathlib.Path(path).parent)
    name = pathlib.Path(path).name
    aliases = [
        path,
        parent + "/./" + name,
        path + "/../" + name,
        "/proc/1/root" + path,
        "/proc/self/root" + path,
    ]
    symlink_alias = pathlib.Path(path, ".t-hub-target-alias")
    symlink_alias.unlink(missing_ok=True)
    symlink_alias.symlink_to(path)
    aliases.append(str(symlink_alias))
    root = os.open("/", os.O_RDONLY | os.O_DIRECTORY)
    try:
        opened = os.open(path.lstrip("/"), os.O_RDONLY | os.O_DIRECTORY, dir_fd=root)
        opened_stat = os.fstat(opened)
        os.close(opened)
    finally:
        os.close(root)
    if (opened_stat.st_dev, opened_stat.st_ino) == expected:
        raise RuntimeError("openat reached the real target")
    for alias in aliases:
        try:
            os.chdir(alias)
            visible = os.stat(".")
        except (FileNotFoundError, PermissionError):
            continue
        if (visible.st_dev, visible.st_ino) == expected:
            raise RuntimeError("namespace alias reached the real target")
"#;

const INSPECTION_SCRIPT: &str = r#"
import json
import os
import pathlib
import stat
import subprocess
import sys

def run_git(root, *args, check=True):
    result = subprocess.run(
        ["git", "-C", root, *args],
        check=False,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True,
    )
    if check and result.returncode != 0:
        raise RuntimeError(
            "git " + " ".join(args) + " failed: " + result.stderr.strip()
        )
    return result

requested = sys.argv[1]
if not requested.startswith("/"):
    raise RuntimeError("worktree path must be absolute")
worktree = pathlib.Path(requested)
worktree_lstat = worktree.lstat()
if stat.S_ISLNK(worktree_lstat.st_mode) or not stat.S_ISDIR(worktree_lstat.st_mode):
    raise RuntimeError("worktree must be a real directory, not a symlink")
resolved = str(worktree.resolve(strict=True))
if resolved != requested.rstrip("/"):
    raise RuntimeError("worktree path must already be canonical")

root = run_git(resolved, "rev-parse", "--show-toplevel").stdout.strip()
if root != resolved:
    raise RuntimeError("path is not the exact Git worktree root")
head = run_git(resolved, "rev-parse", "--verify", "HEAD^{commit}").stdout.strip()
branch_result = run_git(
    resolved,
    "symbolic-ref",
    "--quiet",
    "--short",
    "HEAD",
    check=False,
)
branch = branch_result.stdout.strip() if branch_result.returncode == 0 else None
if branch is None:
    raise RuntimeError("detached worktrees are not eligible for Cargo cleanup")
dirty = bool(run_git(resolved, "status", "--porcelain", "-z").stdout)

porcelain = run_git(resolved, "worktree", "list", "--porcelain").stdout
listed = [
    line.removeprefix("worktree ")
    for line in porcelain.splitlines()
    if line.startswith("worktree ")
]
if resolved not in listed:
    raise RuntimeError("worktree is not present in Git's worktree registry")
is_linked = listed.index(resolved) != 0

remote_head = run_git(
    resolved,
    "symbolic-ref",
    "--quiet",
    "refs/remotes/origin/HEAD",
    check=False,
)
default_ref = remote_head.stdout.strip() if remote_head.returncode == 0 else None
if not default_ref:
    for candidate in (
        "refs/remotes/origin/main",
        "refs/remotes/origin/master",
    ):
        probe = run_git(
            resolved,
            "show-ref",
            "--verify",
            "--quiet",
            candidate,
            check=False,
        )
        if probe.returncode == 0:
            default_ref = candidate
            break
if not default_ref:
    raise RuntimeError("remote default branch is unavailable")
merged = run_git(
    resolved,
    "merge-base",
    "--is-ancestor",
    head,
    default_ref,
    check=False,
).returncode == 0

targets = []
for relative_root in ("apps/cli", "apps/desktop/src-tauri"):
    cargo_root = pathlib.Path(resolved, relative_root)
    if not cargo_root.is_dir():
        raise RuntimeError("required Cargo workspace root is missing: " + str(cargo_root))
    for candidate in sorted(cargo_root.iterdir()):
        if candidate.name != "target" and not candidate.name.startswith("target-"):
            continue
        candidate_lstat = candidate.lstat()
        if stat.S_ISLNK(candidate_lstat.st_mode):
            raise RuntimeError("Cargo target must not be a symlink: " + str(candidate))
        if not stat.S_ISDIR(candidate_lstat.st_mode):
            continue
        target_stat = candidate.stat()
        targets.append({
            "path": str(candidate),
            "device": target_stat.st_dev,
            "inode": target_stat.st_ino,
        })
if not targets:
    raise RuntimeError("worktree has no Cargo target directories to clean")

print(json.dumps({
    "worktree": {
        "path": resolved,
        "device": worktree_lstat.st_dev,
        "inode": worktree_lstat.st_ino,
        "head": head,
        "branch": branch,
    },
    "targets": targets,
    "dirty": dirty,
    "merged": merged,
    "isLinked": is_linked,
}))
"#;

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or(0)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum RetirementState {
    Reserved,
    Running,
    Succeeded,
    Failed,
    RecoveryRequired,
}

#[derive(Debug, PartialEq, Eq)]
enum ProviderCompletion {
    Succeeded,
    Failed(String),
    RecoveryRequired(String),
}

impl RetirementState {
    pub fn is_active(self) -> bool {
        matches!(
            self,
            Self::Reserved | Self::Running | Self::RecoveryRequired
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorktreeRetirement {
    pub operation_id: String,
    pub worktree_path: String,
    pub request_path: String,
    pub state: RetirementState,
    pub created_at: u64,
    pub updated_at: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub request_sha256: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    request_identity: Option<BoundProviderRequestIdentity>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_error: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RetirementReservation {
    pub operation_id: String,
    pub state: RetirementState,
    pub created_at: u64,
    pub updated_at: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CapturedPathIdentity {
    pub path: String,
    pub device: u64,
    pub inode: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CapturedWorktreeIdentity {
    pub path: String,
    pub device: u64,
    pub inode: u64,
    pub head: String,
    pub branch: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RetirementCleanupCapture {
    pub worktree: CapturedWorktreeIdentity,
    pub targets: Vec<CapturedPathIdentity>,
    pub dirty: bool,
    pub merged: bool,
    pub is_linked: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RetirementCleanupRequest {
    schema_version: u32,
    operation_id: String,
    project: String,
    worktree: CapturedWorktreeIdentity,
    targets: Vec<CapturedPathIdentity>,
    allow_unmerged: bool,
    inventory_complete: bool,
}

impl From<&WorktreeRetirement> for RetirementReservation {
    fn from(record: &WorktreeRetirement) -> Self {
        Self {
            operation_id: record.operation_id.clone(),
            state: record.state,
            created_at: record.created_at,
            updated_at: record.updated_at,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct WorktreeRetirementSnapshot {
    schema_version: u32,
    retirements: BTreeMap<String, WorktreeRetirement>,
}

impl Default for WorktreeRetirementSnapshot {
    fn default() -> Self {
        Self {
            schema_version: SCHEMA_VERSION,
            retirements: BTreeMap::new(),
        }
    }
}

#[derive(Debug)]
pub enum WorktreeCoordinatorError {
    CorruptState(String),
    Io(String),
    Persistence(String),
    Conflict(String),
    UnknownOperation(String),
}

impl std::fmt::Display for WorktreeCoordinatorError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::CorruptState(reason) => {
                write!(formatter, "worktree retirement state is corrupt: {reason}")
            }
            Self::Io(reason) => write!(formatter, "worktree retirement I/O failed: {reason}"),
            Self::Persistence(reason) => {
                write!(
                    formatter,
                    "worktree retirement persistence failed: {reason}"
                )
            }
            Self::Conflict(reason) => write!(formatter, "{reason}"),
            Self::UnknownOperation(operation_id) => {
                write!(formatter, "unknown worktree retirement '{operation_id}'")
            }
        }
    }
}

impl std::error::Error for WorktreeCoordinatorError {}

#[derive(Debug)]
pub struct WorktreeCoordinator {
    path: Option<PathBuf>,
    inner: Mutex<WorktreeRetirementSnapshot>,
    workers: Mutex<BTreeSet<String>>,
    publication: Mutex<()>,
    boundaries: Mutex<BTreeMap<String, Arc<WorktreeBoundary>>>,
}

#[derive(Debug)]
struct WorktreeBoundary {
    coordination: Mutex<()>,
    admissions: AtomicUsize,
    retirement: Mutex<Option<WorktreeRetirement>>,
}

pub struct WorktreeAdmissionGuard {
    coordinator: Arc<WorktreeCoordinator>,
    boundary: Arc<WorktreeBoundary>,
    path: String,
    blockers: Vec<CapturedWorktreeIdentity>,
}

impl WorktreeAdmissionGuard {
    pub fn canonical_path(&self) -> &str {
        &self.path
    }

    pub fn contain_process(
        &self,
        command: Option<&str>,
        mut env: Vec<(String, String)>,
    ) -> Result<(Option<String>, Vec<(String, String)>), String> {
        if self.blockers.is_empty() {
            return Ok((command.map(str::to_string), env));
        }
        let evidence = serde_json::to_vec(&serde_json::json!({
            "version": 1,
            "launchNonce": uuid::Uuid::new_v4().simple().to_string(),
            "blockers": &self.blockers,
        }))
        .map(|bytes| URL_SAFE_NO_PAD.encode(bytes))
        .map_err(|error| format!("worktree containment evidence failed: {error}"))?;
        if evidence.len() > CONTAINMENT_EVIDENCE_LIMIT {
            return Err("worktree containment evidence exceeds the safe bound".into());
        }
        require_namespace_containment(&self.blockers, &evidence)?;
        env.push(("T_HUB_WORKTREE_CONTAINMENT".into(), evidence.clone()));
        let mut wrapper = vec![
            "exec /usr/bin/bwrap".to_string(),
            "--die-with-parent".into(),
            "--unshare-pid".into(),
            "--bind / /".into(),
            "--proc /proc".into(),
            "--dev /dev".into(),
        ];
        for blocker in &self.blockers {
            wrapper.push(format!("--tmpfs {}", shell_quote(&blocker.path)));
        }
        wrapper.push(format!(
            "--setenv T_HUB_WORKTREE_CONTAINMENT {}",
            shell_quote(&evidence)
        ));
        wrapper.push("--".into());
        match command {
            Some(command) => {
                wrapper.push(format!("${{SHELL:-/bin/sh}} -lc {}", shell_quote(command)))
            }
            None => wrapper.push("${SHELL:-/bin/sh} -l".into()),
        }
        Ok((Some(wrapper.join(" ")), env))
    }
}

impl Drop for WorktreeAdmissionGuard {
    fn drop(&mut self) {
        if self.boundary.admissions.fetch_sub(1, Ordering::SeqCst) == 1 {
            let active = self
                .boundary
                .retirement
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .as_ref()
                .is_some_and(|record| record.state.is_active());
            if !active {
                let mut boundaries = self
                    .coordinator
                    .boundaries
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner());
                if boundaries.get(&self.path).is_some_and(|boundary| {
                    Arc::ptr_eq(boundary, &self.boundary) && Arc::strong_count(boundary) == 2
                }) {
                    boundaries.remove(&self.path);
                }
            }
        }
    }
}

impl WorktreeCoordinator {
    /// Load durable reservation state and fail closed if it cannot be decoded or
    /// validated.
    pub fn load(path: PathBuf) -> Result<Self, WorktreeCoordinatorError> {
        let snapshot = match std::fs::read(&path) {
            Ok(bytes) => {
                serde_json::from_slice::<WorktreeRetirementSnapshot>(&bytes).map_err(|error| {
                    WorktreeCoordinatorError::CorruptState(format!("{}: {error}", path.display()))
                })?
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                WorktreeRetirementSnapshot::default()
            }
            Err(error) => {
                return Err(WorktreeCoordinatorError::Io(format!(
                    "{}: {error}",
                    path.display()
                )))
            }
        };
        validate_snapshot(&snapshot)?;
        let boundaries = snapshot
            .retirements
            .values()
            .filter(|record| record.state.is_active())
            .map(|record| {
                (
                    record.worktree_path.clone(),
                    Arc::new(WorktreeBoundary {
                        coordination: Mutex::new(()),
                        admissions: AtomicUsize::new(0),
                        retirement: Mutex::new(Some(record.clone())),
                    }),
                )
            })
            .collect();
        Ok(Self {
            path: Some(path),
            inner: Mutex::new(snapshot),
            workers: Mutex::new(BTreeSet::new()),
            publication: Mutex::new(()),
            boundaries: Mutex::new(boundaries),
        })
    }

    pub fn load_default() -> Result<Self, WorktreeCoordinatorError> {
        Self::load(default_store_path())
    }

    pub fn ephemeral() -> Self {
        Self {
            path: None,
            inner: Mutex::new(WorktreeRetirementSnapshot::default()),
            workers: Mutex::new(BTreeSet::new()),
            publication: Mutex::new(()),
            boundaries: Mutex::new(BTreeMap::new()),
        }
    }

    fn boundary_for(&self, path: &str) -> Arc<WorktreeBoundary> {
        let mut boundaries = self
            .boundaries
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        Arc::clone(boundaries.entry(path.to_string()).or_insert_with(|| {
            Arc::new(WorktreeBoundary {
                coordination: Mutex::new(()),
                admissions: AtomicUsize::new(0),
                retirement: Mutex::new(None),
            })
        }))
    }

    fn boundary_snapshot(&self) -> Vec<(String, Arc<WorktreeBoundary>)> {
        self.boundaries
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .iter()
            .map(|(path, boundary)| (path.clone(), Arc::clone(boundary)))
            .collect()
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, WorktreeRetirementSnapshot> {
        self.inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn persist(
        &self,
        snapshot: &WorktreeRetirementSnapshot,
    ) -> Result<(), WorktreeCoordinatorError> {
        let Some(path) = &self.path else {
            return Ok(());
        };
        write_atomic(path, snapshot).map_err(|error| {
            WorktreeCoordinatorError::Persistence(format!("{}: {error}", path.display()))
        })
    }

    pub fn begin_retirement(
        &self,
        worktree_path: &str,
        request_path: &str,
    ) -> Result<WorktreeRetirement, WorktreeCoordinatorError> {
        self.begin_retirement_if_idle(worktree_path, request_path, |_| Ok(Vec::new()))
    }

    pub fn begin_retirement_if_idle<F>(
        &self,
        worktree_path: &str,
        request_path: &str,
        inspect_activity: F,
    ) -> Result<WorktreeRetirement, WorktreeCoordinatorError>
    where
        F: FnOnce(&str) -> Result<Vec<String>, String>,
    {
        let worktree_path = normalize_path(worktree_path);
        if worktree_path.is_empty() {
            return Err(WorktreeCoordinatorError::Conflict(
                "cleanupWorktree requires a non-empty worktree path".into(),
            ));
        }
        if request_path.trim().is_empty() {
            return Err(WorktreeCoordinatorError::Conflict(
                "cleanupWorktree requires a durable provider request path".into(),
            ));
        }

        let boundary = self.boundary_for(&worktree_path);
        let _boundary = boundary
            .coordination
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some(record) = boundary
            .retirement
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .as_ref()
            .filter(|record| record.state.is_active())
            .cloned()
        {
            return Err(WorktreeCoordinatorError::Conflict(format!(
                "worktree '{}' already has active retirement reservation '{}'",
                record.worktree_path, record.operation_id
            )));
        }
        let live_activity =
            inspect_activity(&worktree_path).map_err(WorktreeCoordinatorError::Conflict)?;
        if !live_activity.is_empty() {
            return Err(WorktreeCoordinatorError::Conflict(format!(
                "worktree '{worktree_path}' has live sessions: {}",
                live_activity.join(", ")
            )));
        }
        let timestamp = now_ms();
        let record = WorktreeRetirement {
            operation_id: uuid::Uuid::new_v4().simple().to_string(),
            worktree_path: worktree_path.clone(),
            request_path: request_path.to_string(),
            state: RetirementState::Reserved,
            created_at: timestamp,
            updated_at: timestamp,
            request_sha256: None,
            request_identity: None,
            last_error: None,
        };
        let _publication = self
            .publication
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if self
            .boundary_snapshot()
            .iter()
            .any(|(candidate, boundary)| {
                path_within(candidate, &worktree_path)
                    && boundary.admissions.load(Ordering::SeqCst) > 0
            })
        {
            return Err(WorktreeCoordinatorError::Conflict(format!(
                "worktree '{worktree_path}' has activity being admitted"
            )));
        }
        *boundary
            .retirement
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(record.clone());
        drop(_publication);
        let mut snapshot = self.lock();
        let previous = snapshot.clone();
        snapshot
            .retirements
            .insert(record.operation_id.clone(), record.clone());
        if let Err(error) = self.persist(&snapshot) {
            *snapshot = previous;
            let _publication = self
                .publication
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            let mut retirement = boundary
                .retirement
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            if retirement
                .as_ref()
                .is_some_and(|candidate| candidate.operation_id == record.operation_id)
            {
                *retirement = None;
            }
            return Err(error);
        }
        Ok(record)
    }

    pub fn transition(
        &self,
        operation_id: &str,
        state: RetirementState,
        last_error: Option<String>,
    ) -> Result<WorktreeRetirement, WorktreeCoordinatorError> {
        let mut snapshot = self.lock();
        let previous = snapshot.clone();
        let record = snapshot
            .retirements
            .get_mut(operation_id)
            .ok_or_else(|| WorktreeCoordinatorError::UnknownOperation(operation_id.to_string()))?;
        if matches!(
            state,
            RetirementState::Running | RetirementState::RecoveryRequired
        ) && record.request_identity.is_none()
        {
            return Err(WorktreeCoordinatorError::Conflict(
                "Cargo cleanup cannot enter a provider state before request identity binding"
                    .into(),
            ));
        }
        record.state = state;
        record.updated_at = now_ms();
        record.last_error = last_error;
        let updated = record.clone();
        if let Err(error) = self.persist(&snapshot) {
            *snapshot = previous;
            return Err(error);
        }
        let boundary = self.boundary_for(&updated.worktree_path);
        let _publication = self
            .publication
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        *boundary
            .retirement
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(updated.clone());
        if !state.is_active() {
            let worktree_path = updated.worktree_path.clone();
            drop(snapshot);
            drop(_publication);
            let mut boundaries = self
                .boundaries
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            if boundaries.get(&worktree_path).is_some_and(|boundary| {
                boundary.admissions.load(Ordering::SeqCst) == 0 && Arc::strong_count(boundary) == 2
            }) {
                boundaries.remove(&worktree_path);
            }
        }
        Ok(updated)
    }

    pub fn pending_retirements(&self) -> Vec<WorktreeRetirement> {
        self.lock()
            .retirements
            .values()
            .filter(|record| record.state.is_active())
            .cloned()
            .collect()
    }

    pub fn next_request_path(&self) -> PathBuf {
        let parent = self
            .path
            .as_deref()
            .and_then(Path::parent)
            .map(Path::to_path_buf)
            .unwrap_or_else(std::env::temp_dir);
        parent
            .join("worktree-retirement-requests")
            .join(format!("{}.json", uuid::Uuid::new_v4().simple()))
    }

    pub fn write_provider_request(
        &self,
        record: &WorktreeRetirement,
        capture: RetirementCleanupCapture,
    ) -> Result<WorktreeRetirement, WorktreeCoordinatorError> {
        let request = RetirementCleanupRequest {
            schema_version: SCHEMA_VERSION,
            operation_id: record.operation_id.clone(),
            project: "t-hub".into(),
            worktree: capture.worktree,
            targets: capture.targets,
            allow_unmerged: false,
            inventory_complete: true,
        };
        let body = serde_json::to_vec_pretty(&request).map_err(|error| {
            WorktreeCoordinatorError::Persistence(format!("{}: {error}", record.request_path))
        })?;
        write_bytes_atomic(Path::new(&record.request_path), &body).map_err(|error| {
            WorktreeCoordinatorError::Persistence(format!("{}: {error}", record.request_path))
        })?;
        let digest = format!("{:x}", Sha256::digest(&body));
        let provider_path = provider_request_path(&record.request_path)
            .map_err(WorktreeCoordinatorError::Persistence)?;
        let (identity, captured_body) =
            capture_provider_request_identity(&record.request_path, &provider_path)
                .map_err(WorktreeCoordinatorError::Persistence)?;
        if captured_body != body {
            return Err(WorktreeCoordinatorError::Conflict(
                "Cargo cleanup request changed before identity binding".into(),
            ));
        }
        let bound_identity = BoundProviderRequestIdentity {
            provider_path,
            identity,
        };
        let mut snapshot = self.lock();
        let previous = snapshot.clone();
        let current = snapshot
            .retirements
            .get_mut(&record.operation_id)
            .filter(|current| {
                *current == record
                    && current.state == RetirementState::Reserved
                    && current.request_sha256.is_none()
                    && current.request_identity.is_none()
            })
            .ok_or_else(|| {
                WorktreeCoordinatorError::Conflict(
                    "Cargo cleanup reservation changed before request identity binding".into(),
                )
            })?;
        current.request_sha256 = Some(digest);
        current.request_identity = Some(bound_identity);
        current.updated_at = now_ms();
        let updated = current.clone();
        if let Err(error) = self.persist(&snapshot) {
            *snapshot = previous;
            return Err(error);
        }
        let boundary = self.boundary_for(&record.worktree_path);
        *boundary
            .retirement
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(updated.clone());
        Ok(updated)
    }

    pub fn require_provider_configured(&self) -> Result<(), String> {
        configured_provider_command().map(|_| ())
    }

    pub fn start_provider_worker(
        self: &Arc<Self>,
        record: WorktreeRetirement,
    ) -> Result<bool, String> {
        self.start_provider_worker_for_state(record, None)
    }

    fn start_provider_worker_for_state(
        self: &Arc<Self>,
        record: WorktreeRetirement,
        required_state: Option<RetirementState>,
    ) -> Result<bool, String> {
        {
            let mut workers = self
                .workers
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            if !workers.insert(record.operation_id.clone()) {
                return Ok(false);
            }
            let snapshot = self.lock();
            let Some(current) = snapshot
                .retirements
                .get(&record.operation_id)
                .filter(|current| **current == record)
            else {
                workers.remove(&record.operation_id);
                return Err("Cargo cleanup reservation changed before worker ownership".into());
            };
            if required_state.is_some_and(|state| current.state != state) {
                workers.remove(&record.operation_id);
                return Err("Cargo cleanup reservation is not recoverable".into());
            }
        }
        let coordinator = Arc::clone(self);
        let operation_id = record.operation_id.clone();
        let thread_name = format!(
            "t-hub-cargo-cleanup-{}",
            &operation_id[..operation_id.len().min(8)]
        );
        if let Err(error) = std::thread::Builder::new()
            .name(thread_name)
            .spawn(move || {
                coordinator.run_provider_worker(record);
            })
        {
            self.release_worker(&operation_id);
            return Err(format!("could not start Cargo cleanup worker: {error}"));
        }
        Ok(true)
    }

    pub fn recovery_record(
        &self,
        operation_id: &str,
        worktree_path: &str,
    ) -> Result<WorktreeRetirement, WorktreeCoordinatorError> {
        let worktree_path = normalize_path(worktree_path);
        let snapshot = self.lock();
        let record = snapshot
            .retirements
            .get(operation_id)
            .ok_or_else(|| WorktreeCoordinatorError::UnknownOperation(operation_id.to_string()))?;
        if record.state != RetirementState::RecoveryRequired
            || record.worktree_path != worktree_path
            || record.last_error.as_deref().is_none_or(str::is_empty)
        {
            return Err(WorktreeCoordinatorError::Conflict(
                "cleanup recovery requires one exact recoveryRequired reservation".into(),
            ));
        }
        Ok(record.clone())
    }

    pub fn validate_recovery_capture(
        &self,
        record: &WorktreeRetirement,
        capture: &RetirementCleanupCapture,
    ) -> Result<(), String> {
        if capture.dirty || !capture.merged || !capture.is_linked {
            return Err(
                "cleanup recovery requires a clean, merged, linked non-primary worktree".into(),
            );
        }
        let request = read_provider_request(record)?;
        if request.worktree != capture.worktree || request.targets != capture.targets {
            return Err(
                "cleanup recovery target or complete Cargo inventory changed since reservation"
                    .into(),
            );
        }
        Ok(())
    }

    pub fn resume_recovery_worker(
        self: &Arc<Self>,
        record: WorktreeRetirement,
    ) -> Result<bool, String> {
        self.start_provider_worker_for_state(record, Some(RetirementState::RecoveryRequired))
    }

    pub fn recover_pending(self: &Arc<Self>) {
        for record in self.pending_retirements() {
            match record.state {
                RetirementState::Reserved => {
                    if record.request_identity.is_none() {
                        if let Err(error) = self.transition(
                            &record.operation_id,
                            RetirementState::Failed,
                            Some(
                                "Cargo cleanup stopped before provider request identity binding"
                                    .into(),
                            ),
                        ) {
                            eprintln!(
                                "t-hub-cargo-cleanup: could not release unbound operation '{}': {error}",
                                record.operation_id
                            );
                        }
                    } else if let Err(error) = self.start_provider_worker(record.clone()) {
                        eprintln!(
                            "t-hub-cargo-cleanup: could not recover operation '{}': {error}",
                            record.operation_id
                        );
                    }
                }
                RetirementState::Running => {
                    if let Err(error) = self.transition(
                        &record.operation_id,
                        RetirementState::RecoveryRequired,
                        Some("Cargo cleanup was interrupted during its provider commit".into()),
                    ) {
                        eprintln!(
                            "t-hub-cargo-cleanup: could not preserve interrupted operation '{}': {error}",
                            record.operation_id
                        );
                    }
                }
                RetirementState::RecoveryRequired => {}
                RetirementState::Succeeded | RetirementState::Failed => unreachable!(),
            }
        }
    }

    fn run_provider_worker(self: &Arc<Self>, record: WorktreeRetirement) {
        let operation_id = record.operation_id.clone();
        let completion = (|| {
            if !Path::new(&record.request_path).is_file() {
                return missing_request_completion(&record);
            }
            let request = match capture_provider_request(&record) {
                Ok(request) => request,
                Err(error) => return ProviderCompletion::RecoveryRequired(error),
            };
            if let Err(error) = self.transition(&operation_id, RetirementState::Running, None) {
                return ProviderCompletion::RecoveryRequired(format!(
                    "could not persist the running provider state: {error}"
                ));
            }
            match self.run_provider(&record, &request) {
                Ok(output) => classify_provider_output(&output, &request.request.targets),
                Err(error) => ProviderCompletion::RecoveryRequired(error),
            }
        })();
        let transition = match completion {
            ProviderCompletion::Succeeded => {
                self.transition(&operation_id, RetirementState::Succeeded, None)
            }
            ProviderCompletion::Failed(error) => self.transition(
                &operation_id,
                RetirementState::Failed,
                Some(bounded_last_error(&error)),
            ),
            ProviderCompletion::RecoveryRequired(error) => self.transition(
                &operation_id,
                RetirementState::RecoveryRequired,
                Some(bounded_last_error(&error)),
            ),
        };
        if let Err(error) = transition {
            eprintln!(
                "t-hub-cargo-cleanup: operation '{operation_id}' needs recovery because its terminal state could not be persisted: {error}"
            );
        }
        self.release_worker(&operation_id);
    }

    fn release_worker(&self, operation_id: &str) {
        self.workers
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .remove(operation_id);
    }

    pub fn reservation_for(&self, worktree_path: &str) -> Option<RetirementReservation> {
        let worktree_path = crate::files::canonical_posix_path_allow_missing(worktree_path)
            .map(|path| normalize_path(&path))
            .unwrap_or_else(|_| normalize_path(worktree_path));
        self.lock()
            .retirements
            .values()
            .find(|record| {
                record.state.is_active() && normalize_path(&record.worktree_path) == worktree_path
            })
            .map(RetirementReservation::from)
    }

    /// Admit activity that is about to run in `candidate_path`, refusing it while
    /// that directory is reserved for Cargo cleanup and carrying the containment
    /// evidence for every other in-flight retirement.
    ///
    /// An EMPTY `candidate_path` means the caller cannot name the directory: the
    /// spawn path hands tmux an empty `-c` when the WSL home probe fails, so the
    /// pane inherits wsl.exe's working directory and no path exists to gate on.
    /// Refusing it is not an option - that made every default Windows spawn fail
    /// with "could not resolve WSL path ''" - so admit it UNSCOPED instead. This is
    /// still the conservative direction, not a hole: `path_within` answers `true`
    /// for an unresolvable candidate, so an unscoped admission is treated as
    /// possibly inside EVERY active retirement (refused while one is running) and
    /// blocks a new retirement from starting underneath it.
    pub fn admit_activity(
        self: &Arc<Self>,
        candidate_path: &str,
        operation: &str,
    ) -> Result<WorktreeAdmissionGuard, String> {
        let candidate_path = if candidate_path.trim().is_empty() {
            String::new()
        } else {
            crate::files::canonical_posix_path_allow_missing(candidate_path)
                .map(|path| normalize_path(&path))
                .map_err(|error| {
                    format!("{operation}: could not resolve worktree activity: {error}")
                })?
        };
        let boundary = self.boundary_for(&candidate_path);
        let records = {
            let _publication = self
                .publication
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            boundary.admissions.fetch_add(1, Ordering::SeqCst);
            self.boundary_snapshot()
                .into_iter()
                .filter_map(|(_, boundary)| {
                    boundary
                        .retirement
                        .lock()
                        .unwrap_or_else(|poisoned| poisoned.into_inner())
                        .as_ref()
                        .filter(|record| record.state.is_active())
                        .cloned()
                })
                .collect::<Vec<_>>()
        };
        if let Some(record) = records
            .iter()
            .find(|record| path_within(&candidate_path, &record.worktree_path))
        {
            boundary.admissions.fetch_sub(1, Ordering::SeqCst);
            return Err(format!(
                "{operation}: worktree '{}' is reserved for Cargo cleanup by operation '{}'",
                record.worktree_path, record.operation_id
            ));
        }
        let blockers = match records
            .iter()
            .map(read_provider_request)
            .collect::<Result<Vec<_>, _>>()
        {
            Ok(requests) => requests
                .into_iter()
                .map(|request| request.worktree)
                .collect(),
            Err(error) => {
                boundary.admissions.fetch_sub(1, Ordering::SeqCst);
                return Err(error);
            }
        };
        Ok(WorktreeAdmissionGuard {
            coordinator: Arc::clone(self),
            boundary,
            path: candidate_path,
            blockers,
        })
    }

    fn run_provider(
        &self,
        record: &WorktreeRetirement,
        request: &CapturedProviderRequest,
    ) -> Result<std::process::Output, String> {
        let mut provider = PreparedProviderProcess::spawn(&request.provider_path)?;
        let boundary = self.boundary_for(&record.worktree_path);
        let _boundary = boundary
            .coordination
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let panes = crate::tmux::pane_info()
            .map_err(|error| format!("Cargo cleanup containment inspection failed: {error}"))?;
        let mut containment = verify_process_containment(
            &panes,
            &request.request.worktree,
            &record.operation_id,
            &request.provider_path,
            &provider.identity,
        )?;
        if self
            .boundary_snapshot()
            .iter()
            .any(|(candidate, boundary)| {
                path_within(candidate, &record.worktree_path)
                    && boundary.admissions.load(Ordering::SeqCst) > 0
            })
        {
            return Err(
                "Cargo cleanup commit refused because worktree activity is admitted".into(),
            );
        }
        let running = self
            .lock()
            .retirements
            .get(&record.operation_id)
            .filter(|current| current.state == RetirementState::Running)
            .cloned()
            .ok_or_else(|| {
                "Cargo cleanup reservation changed before provider authorization".to_string()
            })?;
        let mut authorization = ProviderAuthorizationGuard::issue(
            &running,
            &request.request.worktree,
            request,
            containment.evidence.clone(),
        )?;
        drop(_boundary);
        let completion =
            authorization.launch(&running, &request.request.worktree, request, &mut provider);
        match completion {
            ManagedProviderCompletion::Completed(output) => {
                containment.release()?;
                Ok(output)
            }
            ManagedProviderCompletion::Terminated(error) => {
                containment.release().map_err(|release_error| {
                    format!("{error}; managed runtime containment release failed: {release_error}")
                })?;
                Err(error)
            }
            ManagedProviderCompletion::Indeterminate(error) => Err(error),
        }
    }
}

fn missing_request_completion(record: &WorktreeRetirement) -> ProviderCompletion {
    let error = format!(
        "durable provider request is missing: {}",
        record.request_path
    );
    if record.request_identity.is_some() {
        ProviderCompletion::RecoveryRequired(error)
    } else {
        ProviderCompletion::Failed(error)
    }
}

fn read_provider_request(record: &WorktreeRetirement) -> Result<RetirementCleanupRequest, String> {
    capture_provider_request(record).map(|capture| capture.request)
}

fn parse_provider_request_bytes(
    record: &WorktreeRetirement,
    bytes: &[u8],
) -> Result<RetirementCleanupRequest, String> {
    let request: RetirementCleanupRequest = serde_json::from_slice(bytes)
        .map_err(|error| format!("durable provider request is invalid: {error}"))?;
    if request.schema_version != SCHEMA_VERSION
        || request.operation_id != record.operation_id
        || request.project != "t-hub"
        || request.worktree.path != record.worktree_path
        || request.targets.is_empty()
        || request.allow_unmerged
        || !request.inventory_complete
    {
        return Err("durable provider request does not match its retirement reservation".into());
    }
    Ok(request)
}

fn classify_provider_output(
    output: &std::process::Output,
    expected_targets: &[CapturedPathIdentity],
) -> ProviderCompletion {
    let exit = output
        .status
        .code()
        .map(|code| code.to_string())
        .unwrap_or_else(|| "no exit code".into());
    let stderr = String::from_utf8_lossy(&output.stderr);
    let report: serde_json::Value = match serde_json::from_slice(&output.stdout) {
        Ok(report) => report,
        Err(error) => {
            return ProviderCompletion::RecoveryRequired(format!(
                "rust-storage retirement-clean exited with {exit} and returned invalid JSON: {error}; stderr: {}",
                stderr.trim()
            ));
        }
    };
    let expected_inventory = expected_targets
        .iter()
        .map(path_identity)
        .collect::<BTreeSet<_>>();
    let actions = report.get("actions").and_then(serde_json::Value::as_array);
    let reported_inventory = actions.and_then(|actions| {
        if actions.len() != expected_targets.len() {
            return None;
        }
        actions
            .iter()
            .map(|action| {
                let identity = action.get("target").unwrap_or(action);
                serde_json::from_value::<CapturedPathIdentity>(identity.clone())
                    .ok()
                    .map(|identity| path_identity(&identity))
            })
            .collect::<Option<BTreeSet<_>>>()
    });
    if reported_inventory.as_ref() != Some(&expected_inventory) {
        return ProviderCompletion::RecoveryRequired(
            "rust-storage returned a report without the complete target inventory".into(),
        );
    }

    if output.status.success() {
        let completed_clean = actions.is_some_and(|actions| {
            actions.iter().all(|action| {
                action.get("status").and_then(serde_json::Value::as_str) == Some("completed")
                    && action
                        .get("recoveryState")
                        .and_then(serde_json::Value::as_str)
                        == Some("clean")
                    && action
                        .get("quarantinePath")
                        .is_some_and(serde_json::Value::is_null)
            })
        });
        return if report.get("complete").and_then(serde_json::Value::as_bool) == Some(true)
            && completed_clean
        {
            ProviderCompletion::Succeeded
        } else {
            ProviderCompletion::RecoveryRequired(
                "rust-storage returned success without an exact completed-clean report".into(),
            )
        };
    }

    let clean_refusal = report
        .get("actions")
        .and_then(serde_json::Value::as_array)
        .is_some_and(|actions| {
            actions.iter().all(|action| {
                action.get("status").and_then(serde_json::Value::as_str) == Some("refused")
                    && action
                        .get("recoveryState")
                        .and_then(serde_json::Value::as_str)
                        == Some("original")
                    && action
                        .get("quarantinePath")
                        .is_none_or(serde_json::Value::is_null)
            })
        });
    let error = format!(
        "rust-storage retirement-clean exited with {exit}: {}",
        stderr.trim()
    );
    if clean_refusal {
        ProviderCompletion::Failed(error)
    } else {
        ProviderCompletion::RecoveryRequired(error)
    }
}

fn path_identity(identity: &CapturedPathIdentity) -> (String, u64, u64) {
    (
        normalize_path(&identity.path),
        identity.device,
        identity.inode,
    )
}

fn bounded_last_error(error: &str) -> String {
    error.chars().take(LAST_ERROR_LIMIT).collect()
}

fn normalize_path(path: &str) -> String {
    let replaced = path.trim().replace('\\', "/");
    let mut lexical = PathBuf::new();
    for component in Path::new(&replaced).components() {
        match component {
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                lexical.pop();
            }
            component => lexical.push(component.as_os_str()),
        }
    }
    let normalized = lexical.to_string_lossy().trim_end_matches('/').to_string();
    if normalized.is_empty() && replaced.starts_with('/') {
        "/".into()
    } else {
        normalized
    }
}

pub(crate) fn path_within(candidate: &str, root: &str) -> bool {
    let canonical = |path: &str| {
        crate::files::canonical_posix_path_allow_missing(path).map(|path| normalize_path(&path))
    };
    match (canonical(candidate), canonical(root)) {
        (Ok(candidate), Ok(root)) => {
            candidate == root || candidate.starts_with(&format!("{root}/"))
        }
        _ => true,
    }
}

fn validate_snapshot(
    snapshot: &WorktreeRetirementSnapshot,
) -> Result<(), WorktreeCoordinatorError> {
    if snapshot.schema_version != SCHEMA_VERSION {
        return Err(WorktreeCoordinatorError::CorruptState(format!(
            "unsupported schema version {}",
            snapshot.schema_version
        )));
    }
    let mut active_paths = BTreeSet::new();
    for (operation_id, record) in &snapshot.retirements {
        if operation_id.is_empty() || operation_id != &record.operation_id {
            return Err(WorktreeCoordinatorError::CorruptState(
                "retirement map key does not match its operationId".into(),
            ));
        }
        if normalize_path(&record.worktree_path).is_empty() {
            return Err(WorktreeCoordinatorError::CorruptState(format!(
                "retirement '{operation_id}' has an empty worktree path"
            )));
        }
        if record.request_path.trim().is_empty() {
            return Err(WorktreeCoordinatorError::CorruptState(format!(
                "retirement '{operation_id}' has an empty provider request path"
            )));
        }
        if record.updated_at < record.created_at {
            return Err(WorktreeCoordinatorError::CorruptState(format!(
                "retirement '{operation_id}' was updated before it was created"
            )));
        }
        if record.request_sha256.is_some() != record.request_identity.is_some() {
            return Err(WorktreeCoordinatorError::CorruptState(format!(
                "retirement '{operation_id}' has partial provider request identity"
            )));
        }
        if let (Some(digest), Some(identity)) = (&record.request_sha256, &record.request_identity) {
            if digest.len() != 64
                || !digest.bytes().all(|byte| byte.is_ascii_hexdigit())
                || identity
                    .identity
                    .digest
                    .iter()
                    .map(|byte| format!("{byte:02x}"))
                    .collect::<String>()
                    != *digest
            {
                return Err(WorktreeCoordinatorError::CorruptState(format!(
                    "retirement '{operation_id}' has mismatched provider request identity"
                )));
            }
        }
        if matches!(
            record.state,
            RetirementState::Running | RetirementState::RecoveryRequired
        ) && record.request_identity.is_none()
        {
            return Err(WorktreeCoordinatorError::CorruptState(format!(
                "retirement '{operation_id}' has no bound provider request identity"
            )));
        }
        if record.state.is_active() && !active_paths.insert(normalize_path(&record.worktree_path)) {
            return Err(WorktreeCoordinatorError::CorruptState(format!(
                "worktree '{}' has duplicate active retirements",
                record.worktree_path
            )));
        }
    }
    Ok(())
}

pub fn inspect_cleanup_candidate(worktree_path: &str) -> Result<RetirementCleanupCapture, String> {
    let command = inspection_command(worktree_path);
    let output = crate::bounded_exec::output_with_timeout_and_limit(
        command,
        Duration::from_secs(30),
        INSPECTION_OUTPUT_LIMIT,
    )
    .map_err(|error| format!("Cargo cleanup inspection failed: {error}"))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!(
            "Cargo cleanup inspection refused: {}",
            stderr.trim()
        ));
    }
    serde_json::from_slice(&output.stdout)
        .map_err(|error| format!("Cargo cleanup inspection returned invalid JSON: {error}"))
}

#[cfg(not(windows))]
fn inspection_command(worktree_path: &str) -> Command {
    let mut command = Command::new("/usr/bin/python3");
    command.args(["-c", INSPECTION_SCRIPT, worktree_path]);
    command
}

#[cfg(windows)]
fn inspection_command(worktree_path: &str) -> Command {
    let mut command = Command::new("wsl.exe");
    command.args([
        "-d",
        &crate::files::host_distro(),
        "--cd",
        "~",
        "-e",
        "/usr/bin/python3",
        "-c",
        INSPECTION_SCRIPT,
        worktree_path,
    ]);
    command
}

fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\"'\"'"))
}

fn require_namespace_containment(
    blockers: &[CapturedWorktreeIdentity],
    evidence: &str,
) -> Result<(), String> {
    let mut command = containment_command();
    command.args([
        "/usr/bin/bwrap",
        "--die-with-parent",
        "--unshare-pid",
        "--bind",
        "/",
        "/",
        "--proc",
        "/proc",
        "--dev",
        "/dev",
    ]);
    for blocker in blockers {
        command.args(["--tmpfs", &blocker.path]);
    }
    let targets = serde_json::to_string(blockers)
        .map_err(|error| format!("worktree containment preflight identity failed: {error}"))?;
    command.args([
        "--setenv",
        "T_HUB_WORKTREE_CONTAINMENT",
        evidence,
        "--",
        "/usr/bin/python3",
        "-c",
        CONTAINMENT_PREFLIGHT_SCRIPT,
        &targets,
    ]);
    let output =
        crate::bounded_exec::output_with_timeout_and_limit(command, Duration::from_secs(5), 4096)
            .map_err(|error| format!("worktree namespace containment is unavailable: {error}"))?;
    if !output.status.success() {
        return Err(format!(
            "worktree namespace containment is unavailable: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    Ok(())
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum WatchdogOperation {
    CaptureCgroup,
    CreateLease,
    LaunchWatchdog,
    VerifyReady,
    Arm,
    Freeze,
    InspectEvidence,
    Thaw,
    VerifyThaw,
    Disarm,
    AuthorizeProvider,
}

const WATCHDOG_PREPARE_LIFECYCLE: [WatchdogOperation; 8] = [
    WatchdogOperation::CaptureCgroup,
    WatchdogOperation::CreateLease,
    WatchdogOperation::LaunchWatchdog,
    WatchdogOperation::VerifyReady,
    WatchdogOperation::Arm,
    WatchdogOperation::Freeze,
    WatchdogOperation::InspectEvidence,
    WatchdogOperation::AuthorizeProvider,
];

const WATCHDOG_RELEASE_LIFECYCLE: [WatchdogOperation; 3] = [
    WatchdogOperation::Thaw,
    WatchdogOperation::VerifyThaw,
    WatchdogOperation::Disarm,
];

impl WatchdogOperation {
    fn name(self) -> &'static str {
        match self {
            Self::CaptureCgroup => "captureCgroup",
            Self::CreateLease => "createLease",
            Self::LaunchWatchdog => "launchWatchdog",
            Self::VerifyReady => "verifyReady",
            Self::Arm => "arm",
            Self::Freeze => "freeze",
            Self::InspectEvidence => "inspectEvidence",
            Self::Thaw => "thaw",
            Self::VerifyThaw => "verifyThaw",
            Self::Disarm => "disarm",
            Self::AuthorizeProvider => "authorizeProvider",
        }
    }
}

trait WatchdogBackend {
    fn perform(&mut self, operation: WatchdogOperation) -> Result<(), String>;
    fn recover(&mut self) -> Result<(), String>;
    fn finish(&mut self) -> Result<(), String>;
    fn take_authorization(&mut self) -> Result<WatchdogAuthorizationEvidence, String>;
}

#[cfg(test)]
fn execute_watchdog_lifecycle(
    backend: &mut impl WatchdogBackend,
) -> Result<WatchdogAuthorizationEvidence, String> {
    let authorization = prepare_watchdog_lifecycle(backend)?;
    release_watchdog_lifecycle(backend)?;
    Ok(authorization)
}

fn prepare_watchdog_lifecycle(
    backend: &mut impl WatchdogBackend,
) -> Result<WatchdogAuthorizationEvidence, String> {
    for operation in WATCHDOG_PREPARE_LIFECYCLE {
        if let Err(error) = backend.perform(operation) {
            backend.recover()?;
            return Err(error);
        }
    }
    backend.take_authorization()
}

fn release_watchdog_lifecycle(backend: &mut impl WatchdogBackend) -> Result<(), String> {
    for operation in WATCHDOG_RELEASE_LIFECYCLE {
        if let Err(error) = backend.perform(operation) {
            backend.recover()?;
            return Err(error);
        }
    }
    backend.finish()?;
    Ok(())
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct WatchdogBackendCommand {
    operation: &'static str,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct WatchdogBackendCompletion {
    operation: String,
    #[serde(default)]
    authorization: Option<WatchdogAuthorizationEvidence>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct WatchdogAuthorizationEvidence {
    watchdog_nonce: String,
    lease_digest: String,
}

struct ProcessWatchdogBackend {
    child: Child,
    stdin: Option<ChildStdin>,
    responses: Receiver<Result<String, String>>,
    stderr: Arc<Mutex<Vec<u8>>>,
    stdout_thread: Option<std::thread::JoinHandle<()>>,
    stderr_thread: Option<std::thread::JoinHandle<()>>,
    authorization: Option<WatchdogAuthorizationEvidence>,
    authorization_issued: bool,
}

impl ProcessWatchdogBackend {
    fn spawn(
        pane: &crate::tmux::PaneInfo,
        target: &CapturedWorktreeIdentity,
        operation_id: &str,
        request_path: &str,
        provider: &ManagedProviderIdentity,
    ) -> Result<Self, String> {
        let target_json = serde_json::to_string(target)
            .map_err(|error| format!("worktree containment identity failed: {error}"))?;
        let provider_json = serde_json::to_string(provider)
            .map_err(|error| format!("provider containment identity failed: {error}"))?;
        let mut command = containment_command();
        command.args([
            "/usr/bin/python3",
            "-c",
            ATOMIC_CONTAINMENT_INSPECTION_SCRIPT,
            &pane.pid.to_string(),
            &target_json,
            operation_id,
            request_path,
            &provider_json,
            CONTAINMENT_FREEZE_WATCHDOG_SCRIPT,
            &(provider_timeout().as_secs() + 30).to_string(),
        ]);
        command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut child = command
            .spawn()
            .map_err(|error| format!("could not start containment backend: {error}"))?;
        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| "containment backend input is unavailable".to_string())?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| "containment backend output is unavailable".to_string())?;
        let stderr = child
            .stderr
            .take()
            .ok_or_else(|| "containment backend errors are unavailable".to_string())?;
        let (sender, responses) = mpsc::channel();
        let stdout_thread = std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines() {
                if sender
                    .send(line.map_err(|error| error.to_string()))
                    .is_err()
                {
                    break;
                }
            }
        });
        let captured_stderr = Arc::new(Mutex::new(Vec::new()));
        let stderr_output = Arc::clone(&captured_stderr);
        let stderr_thread = std::thread::spawn(move || {
            let mut output = Vec::new();
            let _ = stderr
                .take((CONTAINMENT_EVIDENCE_LIMIT + 1) as u64)
                .read_to_end(&mut output);
            *stderr_output
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner()) = output;
        });
        Ok(Self {
            child,
            stdin: Some(stdin),
            responses,
            stderr: captured_stderr,
            stdout_thread: Some(stdout_thread),
            stderr_thread: Some(stderr_thread),
            authorization: None,
            authorization_issued: false,
        })
    }

    fn wait(&mut self, timeout: Duration) -> Result<std::process::ExitStatus, String> {
        let deadline = std::time::Instant::now() + timeout;
        loop {
            if let Some(status) = self
                .child
                .try_wait()
                .map_err(|error| format!("could not inspect containment backend: {error}"))?
            {
                if let Some(thread) = self.stdout_thread.take() {
                    let _ = thread.join();
                }
                if let Some(thread) = self.stderr_thread.take() {
                    let _ = thread.join();
                }
                return Ok(status);
            }
            if std::time::Instant::now() >= deadline {
                let _ = self.child.kill();
                return Err("containment backend did not stop within its deadline".into());
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    fn stderr(&self) -> String {
        String::from_utf8_lossy(
            &self
                .stderr
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner()),
        )
        .trim()
        .to_string()
    }
}

impl WatchdogBackend for ProcessWatchdogBackend {
    fn perform(&mut self, operation: WatchdogOperation) -> Result<(), String> {
        if operation == WatchdogOperation::AuthorizeProvider {
            self.authorization
                .as_ref()
                .filter(|authorization| {
                    authorization.watchdog_nonce.len() == 32
                        && authorization
                            .watchdog_nonce
                            .bytes()
                            .all(|byte| byte.is_ascii_hexdigit())
                        && authorization.lease_digest.len() == 64
                        && authorization
                            .lease_digest
                            .bytes()
                            .all(|byte| byte.is_ascii_hexdigit())
                })
                .ok_or_else(|| "watchdog provider authorization evidence is missing".to_string())?;
            if self.authorization_issued {
                return Err("watchdog provider authorization was already issued".into());
            }
            self.authorization_issued = true;
            return Ok(());
        }
        let stdin = self
            .stdin
            .as_mut()
            .ok_or_else(|| "containment backend is no longer writable".to_string())?;
        serde_json::to_writer(
            &mut *stdin,
            &WatchdogBackendCommand {
                operation: operation.name(),
            },
        )
        .map_err(|error| format!("could not encode containment command: {error}"))?;
        stdin
            .write_all(b"\n")
            .and_then(|_| stdin.flush())
            .map_err(|error| format!("could not send containment command: {error}"))?;
        let response = match self.responses.recv_timeout(Duration::from_secs(15)) {
            Ok(response) => {
                response.map_err(|error| format!("could not read containment response: {error}"))?
            }
            Err(error) => {
                let _ = self.wait(Duration::from_secs(15));
                let stderr = self.stderr();
                return Err(if stderr.is_empty() {
                    format!(
                        "containment backend did not complete '{}': {error}",
                        operation.name()
                    )
                } else {
                    format!(
                        "containment backend did not complete '{}': {stderr}",
                        operation.name()
                    )
                });
            }
        };
        let completion: WatchdogBackendCompletion = serde_json::from_str(&response)
            .map_err(|error| format!("containment response is invalid: {error}"))?;
        if completion.operation != operation.name() {
            return Err(format!(
                "containment backend completed '{}' while '{}' was required",
                completion.operation,
                operation.name()
            ));
        }
        if operation == WatchdogOperation::InspectEvidence {
            self.authorization = Some(completion.authorization.ok_or_else(|| {
                "watchdog inspection omitted provider authorization evidence".to_string()
            })?);
        } else if completion.authorization.is_some() {
            return Err("watchdog authorization evidence arrived out of state".into());
        }
        Ok(())
    }

    fn recover(&mut self) -> Result<(), String> {
        self.stdin.take();
        self.wait(Duration::from_secs(15)).map(|_| ())
    }

    fn finish(&mut self) -> Result<(), String> {
        self.stdin.take();
        let status = self.wait(Duration::from_secs(3))?;
        let stderr = self.stderr();
        if !status.success() || !stderr.is_empty() {
            return Err(if stderr.is_empty() {
                format!("containment backend exited with {status}")
            } else {
                format!("containment backend failed: {stderr}")
            });
        }
        Ok(())
    }

    fn take_authorization(&mut self) -> Result<WatchdogAuthorizationEvidence, String> {
        if !self.authorization_issued {
            return Err("watchdog provider authorization transition is incomplete".into());
        }
        self.authorization_issued = false;
        self.authorization
            .take()
            .ok_or_else(|| "watchdog provider authorization was already consumed".into())
    }
}

fn verify_process_containment(
    panes: &[crate::tmux::PaneInfo],
    target: &CapturedWorktreeIdentity,
    operation_id: &str,
    request_path: &str,
    provider: &ManagedProviderIdentity,
) -> Result<ProcessContainmentGuard, String> {
    let generation = uuid::Uuid::new_v4().simple().to_string();
    let mut guard = ProcessContainmentGuard {
        backends: Vec::with_capacity(panes.len()),
        evidence: ContainmentAuthorizationEvidence {
            generation,
            watchdogs: Vec::with_capacity(panes.len()),
        },
        released: false,
    };
    for pane in panes {
        if path_within(&pane.cwd, &target.path) {
            return Err(format!(
                "Cargo cleanup refuses live session '{}' in the target worktree",
                pane.session
            ));
        }
        let mut backend =
            ProcessWatchdogBackend::spawn(pane, target, operation_id, request_path, provider)
                .map_err(|error| {
                    format!(
                        "Cargo cleanup could not start session '{}' containment: {error}",
                        pane.session
                    )
                })?;
        let authorization = prepare_watchdog_lifecycle(&mut backend).map_err(|error| {
            format!(
                "Cargo cleanup refuses session '{}' containment: {error}",
                pane.session
            )
        })?;
        guard.evidence.watchdogs.push(authorization);
        guard.backends.push(backend);
    }
    Ok(guard)
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct ContainmentAuthorizationEvidence {
    generation: String,
    watchdogs: Vec<WatchdogAuthorizationEvidence>,
}

struct ProcessContainmentGuard {
    backends: Vec<ProcessWatchdogBackend>,
    evidence: ContainmentAuthorizationEvidence,
    released: bool,
}

impl ProcessContainmentGuard {
    fn release(&mut self) -> Result<(), String> {
        let mut first_error = None;
        for backend in &mut self.backends {
            if let Err(error) = release_watchdog_lifecycle(backend) {
                first_error.get_or_insert(error);
            }
        }
        self.released = first_error.is_none();
        match first_error {
            Some(error) => Err(error),
            None => Ok(()),
        }
    }
}

impl Drop for ProcessContainmentGuard {
    fn drop(&mut self) {
        if !self.released {
            for backend in &mut self.backends {
                let _ = backend.recover();
            }
        }
    }
}

#[cfg(not(windows))]
fn containment_command() -> Command {
    Command::new("/usr/bin/env")
}

#[cfg(windows)]
fn containment_command() -> Command {
    let mut command = Command::new("wsl.exe");
    command.args(["-d", &crate::files::host_distro(), "--cd", "~", "-e"]);
    command
}

fn configured_provider_command() -> Result<Vec<String>, String> {
    let configured = std::env::var("T_HUB_RUST_STORAGE_COMMAND")
        .map_err(|_| "T_HUB_RUST_STORAGE_COMMAND is not configured".to_string())?;
    let command = shell_words::split(&configured)
        .map_err(|error| format!("T_HUB_RUST_STORAGE_COMMAND is invalid: {error}"))?;
    if command.is_empty() {
        return Err("T_HUB_RUST_STORAGE_COMMAND must not be empty".into());
    }
    Ok(command)
}

fn provider_timeout() -> Duration {
    let seconds = std::env::var("T_HUB_RUST_STORAGE_TIMEOUT_SECS")
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .filter(|seconds| (60..=21_600).contains(seconds))
        .unwrap_or(7_200);
    Duration::from_secs(seconds)
}

impl PreparedProviderProcess {
    fn spawn(request_path: &str) -> Result<Self, String> {
        let mut configured = configured_provider_command()?;
        configured.extend(
            [
                "retirement-clean",
                "--request",
                request_path,
                "--apply",
                "--confirm",
                "--json",
            ]
            .map(str::to_string),
        );
        Self::spawn_command(configured)
    }

    fn spawn_command(configured: Vec<String>) -> Result<Self, String> {
        Self::spawn_command_with_timeout(configured, provider_timeout())
    }

    fn spawn_command_with_timeout(
        configured: Vec<String>,
        timeout: Duration,
    ) -> Result<Self, String> {
        let generation = uuid::Uuid::new_v4().simple().to_string();
        let unit = format!("t-hub-provider-{generation}.scope");
        let configured = serde_json::to_string(&configured)
            .map_err(|error| format!("provider command identity failed: {error}"))?;
        let mut command = containment_command();
        command.args([
            "/usr/bin/systemd-run",
            "--user",
            "--scope",
            &format!("--unit={unit}"),
            "--collect",
            "--quiet",
            "/usr/bin/python3",
            "-c",
            PROVIDER_SUPERVISOR_SCRIPT,
            &configured,
            &generation,
            &timeout.as_secs().to_string(),
            &unit,
            &PROVIDER_OUTPUT_LIMIT.to_string(),
            TERMINATE_PROVIDER_SCRIPT,
        ]);
        command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut child = command
            .spawn()
            .map_err(|error| format!("could not start provider supervisor: {error}"))?;
        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| "provider supervisor input is unavailable".to_string())?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| "provider supervisor output is unavailable".to_string())?;
        let stderr = child
            .stderr
            .take()
            .ok_or_else(|| "provider supervisor errors are unavailable".to_string())?;
        let (sender, messages) = mpsc::channel();
        let stdout_thread = std::thread::spawn(move || {
            let mut reader = BufReader::new(stdout);
            loop {
                let mut message = Vec::new();
                match reader.read_until(b'\n', &mut message) {
                    Ok(0) => break,
                    Ok(_) if message.len() <= PROVIDER_OUTPUT_LIMIT * 3 => {
                        if sender.send(Ok(message)).is_err() {
                            break;
                        }
                    }
                    Ok(_) => {
                        let _ = sender.send(Err(
                            "provider supervisor message exceeded its safe bound".into(),
                        ));
                        break;
                    }
                    Err(error) => {
                        let _ = sender.send(Err(error.to_string()));
                        break;
                    }
                }
            }
        });
        let captured_stderr = Arc::new(Mutex::new(Vec::new()));
        let stderr_output = Arc::clone(&captured_stderr);
        let stderr_thread = std::thread::spawn(move || {
            let mut output = Vec::new();
            let _ = stderr
                .take((CONTAINMENT_EVIDENCE_LIMIT + 1) as u64)
                .read_to_end(&mut output);
            *stderr_output
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner()) = output;
        });
        let ready = messages
            .recv_timeout(Duration::from_secs(10))
            .map_err(|error| format!("provider supervisor did not become ready: {error}"))?
            .map_err(|error| format!("provider supervisor readiness failed: {error}"))?;
        let ProviderSupervisorMessage::Ready { identity } =
            serde_json::from_slice::<ProviderSupervisorMessage>(&ready)
                .map_err(|error| format!("provider supervisor readiness is invalid: {error}"))?
        else {
            return Err("provider supervisor completed before authorization".into());
        };
        if identity.generation != generation
            || identity.unit != unit
            || !identity.cgroup_path.ends_with(&format!("/{unit}"))
        {
            return Err("provider supervisor readiness identity is mismatched".into());
        }
        Ok(Self {
            child,
            stdin: Some(stdin),
            messages,
            identity,
            stderr: captured_stderr,
            stdout_thread: Some(stdout_thread),
            stderr_thread: Some(stderr_thread),
            timeout,
            finished: false,
        })
    }

    fn run(&mut self) -> ManagedProviderCompletion {
        let result = self.run_inner();
        let termination = terminate_managed_provider(&self.identity);
        self.finish_transport();
        self.finished = termination.is_ok();
        match (result, termination) {
            (Ok(output), Ok(())) => ManagedProviderCompletion::Completed(output),
            (Err(error), Ok(())) => ManagedProviderCompletion::Terminated(error),
            (Ok(_), Err(error)) => ManagedProviderCompletion::Indeterminate(format!(
                "provider completed but exact generation termination is unproven: {error}"
            )),
            (Err(provider_error), Err(termination_error)) => {
                ManagedProviderCompletion::Indeterminate(format!(
                    "{provider_error}; exact provider generation termination is unproven: {termination_error}"
                ))
            }
        }
    }

    fn stop(&mut self, reason: String) -> ManagedProviderCompletion {
        let termination = terminate_managed_provider(&self.identity);
        self.finish_transport();
        self.finished = termination.is_ok();
        match termination {
            Ok(()) => ManagedProviderCompletion::Terminated(reason),
            Err(error) => ManagedProviderCompletion::Indeterminate(format!(
                "{reason}; exact provider generation termination is unproven: {error}"
            )),
        }
    }

    fn run_inner(&mut self) -> Result<std::process::Output, String> {
        self.stdin
            .as_mut()
            .ok_or_else(|| "provider supervisor authorization was already consumed".to_string())?
            .write_all(b"start\n")
            .and_then(|_| self.stdin.as_mut().unwrap().flush())
            .map_err(|error| format!("could not authorize provider supervisor: {error}"))?;
        let message = self
            .messages
            .recv_timeout(self.timeout + Duration::from_secs(10))
            .map_err(|error| {
                format!(
                    "provider supervisor completion was not received: {error}: {}",
                    String::from_utf8_lossy(
                        &self
                            .stderr
                            .lock()
                            .unwrap_or_else(|poisoned| poisoned.into_inner())
                    )
                    .trim()
                )
            })?
            .map_err(|error| format!("provider supervisor completion failed: {error}"))?;
        let message = serde_json::from_slice::<ProviderSupervisorMessage>(&message)
            .map_err(|error| format!("provider supervisor completion is invalid: {error}"))?;
        let (identity, exit_code, stdout, stderr, error) = match message {
            ProviderSupervisorMessage::Completed {
                identity,
                exit_code,
                stdout,
                stderr,
                error,
            } => (identity, exit_code, stdout, stderr, error),
            ProviderSupervisorMessage::Terminated {
                identity,
                exit_code,
                stdout,
                stderr,
                error,
            } => (identity, exit_code, stdout, stderr, error),
            ProviderSupervisorMessage::Ready { .. } => {
                return Err("provider supervisor repeated its readiness message".into())
            }
        };
        if identity != self.identity {
            return Err("provider supervisor completion identity is mismatched".into());
        }
        if let Some(error) = error {
            return Err(error);
        }
        let stdout = STANDARD
            .decode(stdout)
            .map_err(|error| format!("provider supervisor stdout is invalid: {error}"))?;
        let stderr = STANDARD
            .decode(stderr)
            .map_err(|error| format!("provider supervisor stderr is invalid: {error}"))?;
        if stdout.len() > PROVIDER_OUTPUT_LIMIT || stderr.len() > PROVIDER_OUTPUT_LIMIT {
            return Err("provider supervisor output exceeded its safe bound".into());
        }
        Ok(std::process::Output {
            status: exit_status(exit_code),
            stdout,
            stderr,
        })
    }

    fn finish_transport(&mut self) {
        self.stdin.take();
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while self.child.try_wait().ok().flatten().is_none() && std::time::Instant::now() < deadline
        {
            std::thread::sleep(Duration::from_millis(10));
        }
        if self.child.try_wait().ok().flatten().is_none() {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
        if let Some(thread) = self.stdout_thread.take() {
            let _ = thread.join();
        }
        if let Some(thread) = self.stderr_thread.take() {
            let _ = thread.join();
        }
    }
}

impl Drop for PreparedProviderProcess {
    fn drop(&mut self) {
        if !self.finished {
            self.stdin.take();
            let _ = terminate_managed_provider(&self.identity);
            self.finish_transport();
        }
    }
}

fn terminate_managed_provider(identity: &ManagedProviderIdentity) -> Result<(), String> {
    let identity = serde_json::to_string(identity)
        .map_err(|error| format!("provider termination identity failed: {error}"))?;
    let mut command = containment_command();
    command.args([
        "/usr/bin/python3",
        "-c",
        TERMINATE_PROVIDER_SCRIPT,
        &identity,
    ]);
    let output = crate::bounded_exec::output_with_timeout_and_limit(
        command,
        Duration::from_secs(10),
        CONTAINMENT_EVIDENCE_LIMIT,
    )
    .map_err(|error| format!("provider generation termination failed: {error}"))?;
    if !output.status.success() || !output.stderr.is_empty() {
        return Err(format!(
            "provider generation termination failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    Ok(())
}

#[cfg(test)]
fn managed_provider_stopped(identity: &ManagedProviderIdentity) -> Result<(), String> {
    let identity = serde_json::to_string(identity)
        .map_err(|error| format!("provider probe identity failed: {error}"))?;
    let mut command = containment_command();
    command.args([
        "/usr/bin/python3",
        "-c",
        PROBE_PROVIDER_STOPPED_SCRIPT,
        &identity,
    ]);
    let output = crate::bounded_exec::output_with_timeout_and_limit(
        command,
        Duration::from_secs(10),
        CONTAINMENT_EVIDENCE_LIMIT,
    )
    .map_err(|error| format!("provider generation probe failed: {error}"))?;
    if !output.status.success() || !output.stderr.is_empty() {
        return Err(format!(
            "provider generation remains active or ambiguous: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    Ok(())
}

#[cfg(unix)]
fn exit_status(code: i32) -> std::process::ExitStatus {
    use std::os::unix::process::ExitStatusExt;
    std::process::ExitStatus::from_raw(code << 8)
}

#[cfg(windows)]
fn exit_status(code: i32) -> std::process::ExitStatus {
    use std::os::windows::process::ExitStatusExt;
    std::process::ExitStatus::from_raw(code as u32)
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ProviderRequestIdentity {
    device: Option<u64>,
    inode: Option<u64>,
    links: Option<u64>,
    volume: Option<u64>,
    file: Option<u64>,
    length: u64,
    modified_nanos: u128,
    digest: [u8; 32],
    provider_namespace: ProviderNamespaceIdentity,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ProviderNamespaceIdentity {
    device: u64,
    inode: u64,
    links: u64,
    length: u64,
    modified_nanos: u128,
    digest: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct BoundProviderRequestIdentity {
    provider_path: String,
    identity: ProviderRequestIdentity,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct CapturedProviderRequest {
    native_path: String,
    provider_path: String,
    bytes: Vec<u8>,
    request: RetirementCleanupRequest,
    identity: ProviderRequestIdentity,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ManagedProviderIdentity {
    generation: String,
    unit: String,
    cgroup_path: String,
    cgroup_device: u64,
    cgroup_inode: u64,
}

#[derive(Deserialize)]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
enum ProviderSupervisorMessage {
    Ready {
        identity: ManagedProviderIdentity,
    },
    Completed {
        identity: ManagedProviderIdentity,
        exit_code: i32,
        stdout: String,
        stderr: String,
        error: Option<String>,
    },
    Terminated {
        identity: ManagedProviderIdentity,
        exit_code: i32,
        stdout: String,
        stderr: String,
        error: Option<String>,
    },
}

enum ManagedProviderCompletion {
    Completed(std::process::Output),
    Terminated(String),
    Indeterminate(String),
}

struct PreparedProviderProcess {
    child: Child,
    stdin: Option<ChildStdin>,
    messages: Receiver<Result<Vec<u8>, String>>,
    identity: ManagedProviderIdentity,
    stderr: Arc<Mutex<Vec<u8>>>,
    stdout_thread: Option<std::thread::JoinHandle<()>>,
    stderr_thread: Option<std::thread::JoinHandle<()>>,
    timeout: Duration,
    finished: bool,
}

struct ProviderAuthorizationGuard {
    guard_nonce: String,
    operation_id: String,
    reservation_updated_at: u64,
    target: CapturedWorktreeIdentity,
    request_path: String,
    request_identity: ProviderRequestIdentity,
    request_digest: [u8; 32],
    containment: ContainmentAuthorizationEvidence,
    consumed: bool,
}

impl ProviderAuthorizationGuard {
    fn issue(
        record: &WorktreeRetirement,
        target: &CapturedWorktreeIdentity,
        request: &CapturedProviderRequest,
        containment: ContainmentAuthorizationEvidence,
    ) -> Result<Self, String> {
        let request_digest = Sha256::digest(&request.bytes);
        let request_digest_hex = format!("{request_digest:x}");
        if record.state != RetirementState::Running
            || record.operation_id != request.request.operation_id
            || record.worktree_path != target.path
            || &request.request.worktree != target
            || record.request_sha256.as_deref() != Some(request_digest_hex.as_str())
            || containment.generation.len() != 32
            || !containment
                .generation
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit())
        {
            return Err("provider authorization binding is mismatched".into());
        }
        let watchdog_nonces = containment
            .watchdogs
            .iter()
            .map(|evidence| evidence.watchdog_nonce.as_str())
            .collect::<BTreeSet<_>>();
        if watchdog_nonces.len() != containment.watchdogs.len()
            || containment.watchdogs.iter().any(|evidence| {
                evidence.watchdog_nonce.len() != 32
                    || !evidence
                        .watchdog_nonce
                        .bytes()
                        .all(|byte| byte.is_ascii_hexdigit())
                    || evidence.lease_digest.len() != 64
                    || !evidence
                        .lease_digest
                        .bytes()
                        .all(|byte| byte.is_ascii_hexdigit())
            })
        {
            return Err("provider authorization has invalid watchdog evidence".into());
        }
        Ok(Self {
            guard_nonce: uuid::Uuid::new_v4().simple().to_string(),
            operation_id: record.operation_id.clone(),
            reservation_updated_at: record.updated_at,
            target: target.clone(),
            request_path: request.provider_path.clone(),
            request_identity: request.identity.clone(),
            request_digest: request_digest.into(),
            containment,
            consumed: false,
        })
    }

    fn launch(
        &mut self,
        record: &WorktreeRetirement,
        target: &CapturedWorktreeIdentity,
        request: &CapturedProviderRequest,
        provider: &mut PreparedProviderProcess,
    ) -> ManagedProviderCompletion {
        match self.launch_with(record, target, request, |_| Ok(provider.run())) {
            Ok(completion) => completion,
            Err(error) => provider.stop(error),
        }
    }

    fn launch_with<T>(
        &mut self,
        record: &WorktreeRetirement,
        target: &CapturedWorktreeIdentity,
        request: &CapturedProviderRequest,
        invoke: impl FnOnce(&str) -> Result<T, String>,
    ) -> Result<T, String> {
        if self.consumed {
            return Err("provider authorization was already consumed".into());
        }
        self.consumed = true;
        let digest: [u8; 32] = Sha256::digest(&request.bytes).into();
        if self.guard_nonce.len() != 32
            || self.operation_id != record.operation_id
            || self.reservation_updated_at != record.updated_at
            || record.state != RetirementState::Running
            || &self.target != target
            || self.request_path != request.provider_path
            || self.request_identity != request.identity
            || self.request_digest != digest
            || self.containment.generation.len() != 32
        {
            return Err("provider authorization is stale or mismatched".into());
        }
        request.revalidate()?;
        invoke(&self.request_path)
    }
}

impl CapturedProviderRequest {
    fn revalidate(&self) -> Result<(), String> {
        let provider_path = provider_request_path(&self.native_path)?;
        let (identity, bytes) =
            capture_provider_request_identity(&self.native_path, &provider_path)?;
        if provider_path != self.provider_path || identity != self.identity || bytes != self.bytes {
            return Err("provider request identity changed before invocation".into());
        }
        Ok(())
    }
}

fn capture_provider_request(
    record: &WorktreeRetirement,
) -> Result<CapturedProviderRequest, String> {
    let expected_digest = record
        .request_sha256
        .as_deref()
        .ok_or_else(|| "durable provider request identity is not bound".to_string())?;
    let provider_path = provider_request_path(&record.request_path)?;
    let (identity, bytes) =
        capture_provider_request_identity(&record.request_path, &provider_path)?;
    let bound_identity = record
        .request_identity
        .as_ref()
        .ok_or_else(|| "durable provider request path identity is not bound".to_string())?;
    if bound_identity.provider_path != provider_path
        || bound_identity.identity != identity
        || format!("{:x}", Sha256::digest(&bytes)) != expected_digest
    {
        return Err("durable provider request changed after reservation binding".into());
    }
    let request = parse_provider_request_bytes(record, &bytes)?;
    Ok(CapturedProviderRequest {
        native_path: record.request_path.clone(),
        provider_path,
        bytes,
        request,
        identity,
    })
}

fn capture_provider_request_identity(
    request_path: &str,
    provider_path: &str,
) -> Result<(ProviderRequestIdentity, Vec<u8>), String> {
    capture_provider_request_identity_with(request_path, provider_path, || {})
}

fn capture_provider_request_identity_with(
    request_path: &str,
    provider_path: &str,
    after_open: impl FnOnce(),
) -> Result<(ProviderRequestIdentity, Vec<u8>), String> {
    let symlink = std::fs::symlink_metadata(request_path)
        .map_err(|error| format!("could not inspect provider request path: {error}"))?;
    if symlink.file_type().is_symlink() || !symlink.file_type().is_file() {
        return Err("provider request path is not an exact regular file".into());
    }
    let initial_file = open_provider_request(request_path)?;
    let initial_metadata = initial_file
        .metadata()
        .map_err(|error| format!("could not inspect initial provider request file: {error}"))?;
    if initial_metadata.file_type().is_symlink() || !initial_metadata.file_type().is_file() {
        return Err("provider request path did not open as an exact regular file".into());
    }
    let initial_file_identity = provider_file_identity(&initial_file, &initial_metadata)?;
    let mut file = open_provider_request(request_path)?;
    let before = file
        .metadata()
        .map_err(|error| format!("could not inspect provider request file: {error}"))?;
    if before.file_type().is_symlink() || !before.file_type().is_file() {
        return Err("provider request path did not remain an exact regular file".into());
    }
    let before_file_identity = provider_file_identity(&file, &before)?;
    if initial_file_identity != before_file_identity {
        return Err("provider request changed while opening exact identity".into());
    }
    if before.len() > INSPECTION_OUTPUT_LIMIT as u64 {
        return Err("provider request exceeds the safe identity bound".into());
    }
    after_open();
    let mut contents = Vec::with_capacity(before.len() as usize);
    file.read_to_end(&mut contents)
        .map_err(|error| format!("could not hash provider request file: {error}"))?;
    let after = file
        .metadata()
        .map_err(|error| format!("could not recheck provider request file: {error}"))?;
    let (device, inode, links, volume, file_identity) = provider_file_identity(&file, &after)?;
    if before.len() != after.len()
        || before.modified().ok() != after.modified().ok()
        || before_file_identity != (device, inode, links, volume, file_identity)
    {
        return Err("provider request changed while capturing identity".into());
    }
    let modified_nanos = after
        .modified()
        .map_err(|error| format!("provider request modification time is unavailable: {error}"))?
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|_| "provider request modification time is invalid".to_string())?
        .as_nanos();
    let digest: [u8; 32] = Sha256::digest(&contents).into();
    let durable_after = std::fs::symlink_metadata(request_path)
        .map_err(|error| format!("could not recheck provider request path: {error}"))?;
    let durable_file = open_provider_request(request_path)?;
    let durable_metadata = durable_file
        .metadata()
        .map_err(|error| format!("could not recheck provider request file identity: {error}"))?;
    if durable_after.file_type().is_symlink()
        || !durable_after.file_type().is_file()
        || durable_metadata.file_type().is_symlink()
        || !durable_metadata.file_type().is_file()
        || provider_file_identity(&durable_file, &durable_metadata)?
            != (device, inode, links, volume, file_identity)
    {
        return Err("provider request changed while opening exact identity".into());
    }
    let provider_namespace = capture_provider_namespace_identity(provider_path)?;
    Ok((
        ProviderRequestIdentity {
            device,
            inode,
            links,
            volume,
            file: file_identity,
            length: after.len(),
            modified_nanos,
            digest,
            provider_namespace,
        },
        contents,
    ))
}

#[cfg(unix)]
fn open_provider_request(request_path: &str) -> Result<std::fs::File, String> {
    use std::os::unix::fs::OpenOptionsExt;
    let mut options = std::fs::OpenOptions::new();
    options
        .read(true)
        .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW);
    options
        .open(request_path)
        .map_err(|error| format!("could not open exact provider request path: {error}"))
}

#[cfg(windows)]
fn open_provider_request(request_path: &str) -> Result<std::fs::File, String> {
    std::fs::File::open(request_path)
        .map_err(|error| format!("could not open provider request path: {error}"))
}

#[cfg(unix)]
fn capture_provider_namespace_identity(
    provider_path: &str,
) -> Result<ProviderNamespaceIdentity, String> {
    use std::os::unix::fs::MetadataExt;
    let mut file = open_provider_request(provider_path)?;
    let metadata = file
        .metadata()
        .map_err(|error| format!("could not inspect provider namespace request: {error}"))?;
    let mut contents = Vec::with_capacity(metadata.len() as usize);
    file.read_to_end(&mut contents)
        .map_err(|error| format!("could not hash provider namespace request: {error}"))?;
    Ok(ProviderNamespaceIdentity {
        device: metadata.dev(),
        inode: metadata.ino(),
        links: metadata.nlink(),
        length: metadata.len(),
        modified_nanos: metadata.mtime() as u128 * 1_000_000_000 + metadata.mtime_nsec() as u128,
        digest: format!("{:x}", Sha256::digest(contents)),
    })
}

#[cfg(windows)]
fn capture_provider_namespace_identity(
    provider_path: &str,
) -> Result<ProviderNamespaceIdentity, String> {
    const SCRIPT: &str = r#"
import hashlib
import json
import os
import stat
import sys

path = sys.argv[1]
descriptor = os.open(path, os.O_RDONLY | os.O_CLOEXEC | os.O_NOFOLLOW)
try:
    before = os.fstat(descriptor)
    if not stat.S_ISREG(before.st_mode) or before.st_nlink < 1:
        raise RuntimeError("provider namespace request is not an exact regular file")
    body = b""
    while True:
        chunk = os.read(descriptor, 65536)
        if not chunk:
            break
        body += chunk
        if len(body) > 1048576:
            raise RuntimeError("provider namespace request exceeds the safe identity bound")
    after = os.fstat(descriptor)
    if (
        (before.st_dev, before.st_ino, before.st_nlink, before.st_size, before.st_mtime_ns)
        != (after.st_dev, after.st_ino, after.st_nlink, after.st_size, after.st_mtime_ns)
    ):
        raise RuntimeError("provider namespace request changed during identity capture")
    print(json.dumps({
        "device": after.st_dev,
        "inode": after.st_ino,
        "links": after.st_nlink,
        "length": after.st_size,
        "modifiedNanos": after.st_mtime_ns,
        "digest": hashlib.sha256(body).hexdigest(),
    }, separators=(",", ":")))
finally:
    os.close(descriptor)
"#;
    let mut command = containment_command();
    command.args(["/usr/bin/python3", "-c", SCRIPT, provider_path]);
    let output = crate::bounded_exec::output_with_timeout_and_limit(
        command,
        crate::bounded_exec::WSL_PROBE_TIMEOUT,
        4096,
    )
    .map_err(|error| format!("could not inspect provider namespace request: {error}"))?;
    if !output.status.success() || !output.stderr.is_empty() {
        return Err(format!(
            "could not inspect provider namespace request: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    serde_json::from_slice(&output.stdout)
        .map_err(|error| format!("provider namespace identity is invalid: {error}"))
}

#[cfg(unix)]
fn provider_file_identity(
    _file: &std::fs::File,
    metadata: &std::fs::Metadata,
) -> Result<
    (
        Option<u64>,
        Option<u64>,
        Option<u64>,
        Option<u64>,
        Option<u64>,
    ),
    String,
> {
    use std::os::unix::fs::MetadataExt;
    Ok((
        Some(metadata.dev()),
        Some(metadata.ino()),
        Some(metadata.nlink()),
        None,
        None,
    ))
}

#[cfg(windows)]
fn provider_file_identity(
    file: &std::fs::File,
    _metadata: &std::fs::Metadata,
) -> Result<
    (
        Option<u64>,
        Option<u64>,
        Option<u64>,
        Option<u64>,
        Option<u64>,
    ),
    String,
> {
    use std::os::windows::io::AsRawHandle;
    use windows::Win32::Foundation::HANDLE;
    use windows::Win32::Storage::FileSystem::{
        GetFileInformationByHandle, BY_HANDLE_FILE_INFORMATION,
    };

    let mut identity = BY_HANDLE_FILE_INFORMATION::default();
    unsafe {
        GetFileInformationByHandle(HANDLE(file.as_raw_handle()), &mut identity)
            .map_err(|error| format!("provider request file identity is unavailable: {error}"))?;
    }
    Ok((
        None,
        None,
        None,
        Some(identity.dwVolumeSerialNumber as u64),
        Some((u64::from(identity.nFileIndexHigh) << 32) | u64::from(identity.nFileIndexLow)),
    ))
}

#[cfg(not(windows))]
fn provider_request_path(request_path: &str) -> Result<String, String> {
    let canonical = std::fs::canonicalize(request_path)
        .map_err(|error| format!("could not canonicalize provider request path: {error}"))?;
    let path = canonical
        .to_str()
        .ok_or_else(|| "provider request path is not valid UTF-8".to_string())?;
    validate_posix_provider_request_path(path)?;
    Ok(path.to_string())
}

#[cfg(windows)]
fn provider_request_path(request_path: &str) -> Result<String, String> {
    translate_windows_provider_request_path(request_path, &WindowsProviderPathTransport)
}

#[cfg(any(windows, test))]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct ProviderPathIdentity {
    volume: u64,
    file: u64,
}

#[cfg(any(windows, test))]
trait ProviderPathTransport {
    fn canonical_native(&self, path: &str) -> Result<(String, ProviderPathIdentity), String>;
    fn wslpath(&self, arguments: &[&str]) -> Result<String, String>;
    fn readlink(&self, path: &str) -> Result<String, String>;
    fn native_identity(&self, path: &str) -> Result<ProviderPathIdentity, String>;
}

#[cfg(any(windows, test))]
fn translate_windows_provider_request_path(
    request_path: &str,
    transport: &impl ProviderPathTransport,
) -> Result<String, String> {
    validate_windows_provider_request_spelling(request_path)?;
    let (canonical_native, original_identity) = transport.canonical_native(request_path)?;
    let path = transport.wslpath(&["-a", "-u", request_path])?;
    validate_posix_provider_request_path(&path)?;
    if transport.readlink(&path)? != path {
        return Err("provider request path changes identity inside WSL".into());
    }
    let round_trip = transport.wslpath(&["-a", "-w", &path])?;
    if transport.native_identity(&round_trip)? != original_identity {
        return Err("provider request path round trip changed file identity".into());
    }
    if transport.native_identity(request_path)? != original_identity {
        return Err("provider request path identity changed during translation".into());
    }
    if canonical_native.is_empty() {
        return Err("provider request canonical identity is empty".into());
    }
    Ok(path)
}

#[cfg(any(windows, test))]
fn validate_windows_provider_request_spelling(request_path: &str) -> Result<(), String> {
    if request_path.trim() != request_path
        || request_path.contains('\0')
        || (request_path.contains('/') && request_path.contains('\\'))
        || request_path.starts_with("\\\\")
        || request_path.len() < 4
        || request_path.as_bytes()[1] != b':'
        || !request_path.as_bytes()[0].is_ascii_alphabetic()
        || request_path.as_bytes()[2] != b'\\'
        || request_path
            .split('\\')
            .any(|part| matches!(part, "." | ".."))
    {
        return Err("provider request path has ambiguous native spelling".into());
    }
    Ok(())
}

fn validate_posix_provider_request_path(path: &str) -> Result<(), String> {
    if !path.starts_with('/')
        || path.starts_with("//")
        || path.contains('\0')
        || path.contains('\n')
        || path.split('/').any(|part| matches!(part, "." | ".."))
    {
        return Err("provider request path is not an exact absolute POSIX path".into());
    }
    Ok(())
}

#[cfg(windows)]
struct WindowsProviderPathTransport;

#[cfg(windows)]
impl ProviderPathTransport for WindowsProviderPathTransport {
    fn canonical_native(&self, path: &str) -> Result<(String, ProviderPathIdentity), String> {
        let canonical = std::fs::canonicalize(path)
            .map_err(|error| format!("could not canonicalize provider request path: {error}"))?;
        let canonical = canonical
            .to_str()
            .ok_or_else(|| "provider request path is not valid UTF-8".to_string())?
            .to_string();
        Ok((canonical, self.native_identity(path)?))
    }

    fn wslpath(&self, arguments: &[&str]) -> Result<String, String> {
        let mut command = Command::new("wsl.exe");
        command.args([
            "-d",
            &crate::files::host_distro(),
            "--cd",
            "~",
            "-e",
            "wslpath",
        ]);
        command.args(arguments);
        bounded_path_output(
            command,
            "could not translate provider request path into WSL",
        )
    }

    fn readlink(&self, path: &str) -> Result<String, String> {
        let mut command = Command::new("wsl.exe");
        command.args([
            "-d",
            &crate::files::host_distro(),
            "--cd",
            "~",
            "-e",
            "readlink",
            "-f",
            "--",
            path,
        ]);
        bounded_path_output(
            command,
            "could not canonicalize provider request path in WSL",
        )
    }

    fn native_identity(&self, path: &str) -> Result<ProviderPathIdentity, String> {
        let file = open_provider_request(path)?;
        let metadata = file
            .metadata()
            .map_err(|error| format!("could not inspect provider request identity: {error}"))?;
        if metadata.file_type().is_symlink() || !metadata.file_type().is_file() {
            return Err("provider request path is not an exact regular file".into());
        }
        let (_, _, _, volume, file_identity) = provider_file_identity(&file, &metadata)?;
        Ok(ProviderPathIdentity {
            volume: volume
                .ok_or_else(|| "provider request volume identity is unavailable".to_string())?
                as u64,
            file: file_identity
                .ok_or_else(|| "provider request file identity is unavailable".to_string())?,
        })
    }
}

#[cfg(windows)]
fn bounded_path_output(command: Command, context: &str) -> Result<String, String> {
    let output = crate::bounded_exec::output_with_timeout_and_limit(
        command,
        crate::bounded_exec::WSL_PROBE_TIMEOUT,
        4096,
    )
    .map_err(|error| format!("{context}: {error}"))?;
    if !output.status.success() || !output.stderr.is_empty() {
        return Err(format!(
            "{context}: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    let path = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if path.is_empty() || path.contains('\0') || path.contains('\n') {
        return Err(format!("{context}: command returned malformed output"));
    }
    Ok(path)
}

fn default_store_path() -> PathBuf {
    if let Ok(path) = std::env::var("T_HUB_WORKTREE_RETIREMENTS_FILE") {
        return PathBuf::from(path);
    }
    let home = std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."));
    home.join(".t-hub").join("worktree-retirements.json")
}

fn write_atomic(path: &Path, snapshot: &WorktreeRetirementSnapshot) -> std::io::Result<()> {
    write_json_atomic(path, snapshot)
}

fn write_json_atomic(path: &Path, value: &impl Serialize) -> std::io::Result<()> {
    write_bytes_atomic(path, &serde_json::to_vec_pretty(value)?)
}

fn write_bytes_atomic(path: &Path, body: &[u8]) -> std::io::Result<()> {
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    std::fs::create_dir_all(parent)?;
    let temp = path.with_extension(format!(
        "json.tmp.{}.{}",
        std::process::id(),
        uuid::Uuid::new_v4().simple()
    ));
    let result = (|| {
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        let mut file = options.open(&temp)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            file.set_permissions(std::fs::Permissions::from_mode(0o600))?;
        }
        file.write_all(body)?;
        file.sync_all()?;
        drop(file);
        replace_file(&temp, path)?;
        #[cfg(unix)]
        {
            let _ = std::fs::File::open(parent).and_then(|directory| directory.sync_all());
        }
        Ok(())
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&temp);
    }
    result
}

#[cfg(not(windows))]
fn replace_file(source: &Path, destination: &Path) -> std::io::Result<()> {
    std::fs::rename(source, destination)
}

#[cfg(windows)]
fn replace_file(source: &Path, destination: &Path) -> std::io::Result<()> {
    use std::os::windows::ffi::OsStrExt;
    use windows::core::PCWSTR;
    use windows::Win32::Storage::FileSystem::{
        MoveFileExW, MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH,
    };

    let source: Vec<u16> = source.as_os_str().encode_wide().chain(Some(0)).collect();
    let destination: Vec<u16> = destination
        .as_os_str()
        .encode_wide()
        .chain(Some(0))
        .collect();
    unsafe {
        MoveFileExW(
            PCWSTR(source.as_ptr()),
            PCWSTR(destination.as_ptr()),
            MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
        )
    }
    .map_err(|_| std::io::Error::last_os_error())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn prepared_test_provider() -> PreparedProviderProcess {
        PreparedProviderProcess::spawn_command(vec!["/bin/true".into()]).unwrap()
    }

    fn git(root: &Path, args: &[&str]) {
        let output = Command::new("git")
            .arg("-C")
            .arg(root)
            .args(args)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "git {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr)
        );
    }

    #[test]
    fn reservation_is_durable_and_blocks_descendant_activity() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("worktree-retirements.json");
        let operation_id = {
            let coordinator = Arc::new(WorktreeCoordinator::load(path.clone()).unwrap());
            let record = coordinator
                .begin_retirement("/repo/worktrees/clean", "/requests/one.json")
                .unwrap();
            assert!(coordinator
                .admit_activity("/repo/worktrees/clean/apps/cli", "spawn_terminal")
                .is_err());
            record.operation_id
        };

        let coordinator = WorktreeCoordinator::load(path).unwrap();
        let reservation = coordinator
            .reservation_for("/repo/worktrees/clean")
            .unwrap();
        assert_eq!(reservation.operation_id, operation_id);
        assert_eq!(reservation.state, RetirementState::Reserved);
    }

    #[test]
    fn completed_reservation_no_longer_blocks_activity() {
        let coordinator = Arc::new(WorktreeCoordinator::ephemeral());
        let record = coordinator
            .begin_retirement("/repo/worktrees/clean", "/requests/one.json")
            .unwrap();
        coordinator
            .transition(&record.operation_id, RetirementState::Succeeded, None)
            .unwrap();

        assert!(coordinator
            .reservation_for("/repo/worktrees/clean")
            .is_none());
        coordinator
            .admit_activity("/repo/worktrees/clean/apps/cli", "start_agent")
            .unwrap();
    }

    #[test]
    fn admitted_activity_blocks_retirement_until_runtime_creation_finishes() {
        let coordinator = Arc::new(WorktreeCoordinator::ephemeral());
        let admission = coordinator
            .admit_activity("/repo/worktrees/clean/apps/cli", "spawn_terminal")
            .unwrap();

        assert!(matches!(
            coordinator.begin_retirement("/repo/worktrees/clean", "/requests/while-admitted.json"),
            Err(WorktreeCoordinatorError::Conflict(_))
        ));

        drop(admission);
        coordinator
            .begin_retirement("/repo/worktrees/clean", "/requests/after-admission.json")
            .unwrap();
    }

    #[test]
    fn live_activity_check_blocks_retirement_before_reservation() {
        let coordinator = Arc::new(WorktreeCoordinator::ephemeral());

        let error = coordinator
            .begin_retirement_if_idle("/repo/worktrees/clean", "/requests/while-live.json", |_| {
                Ok(vec!["th_live".into()])
            })
            .unwrap_err();

        assert!(matches!(error, WorktreeCoordinatorError::Conflict(_)));
        assert!(coordinator
            .reservation_for("/repo/worktrees/clean")
            .is_none());
    }

    #[test]
    fn path_matching_collapses_repeated_separators() {
        assert!(path_within(
            "/repo//worktrees/clean/apps/cli",
            "/repo/worktrees/clean"
        ));
    }

    #[cfg(unix)]
    #[test]
    fn path_matching_resolves_symlinked_worktrees() {
        let directory = tempfile::tempdir().unwrap();
        let worktree = directory.path().join("worktree");
        let alias = directory.path().join("worktree-link");
        std::fs::create_dir(&worktree).unwrap();
        std::os::unix::fs::symlink(&worktree, &alias).unwrap();

        assert!(path_within(
            alias.join("apps/cli").to_str().unwrap(),
            worktree.to_str().unwrap()
        ));
    }

    #[cfg(unix)]
    #[test]
    fn admission_resolves_symlinks_before_parent_components() {
        let directory = tempfile::tempdir().unwrap();
        let real = directory.path().join("real");
        let nested = real.join("nested");
        let worktree = real.join("worktree");
        let alias = directory.path().join("alias");
        std::fs::create_dir_all(&nested).unwrap();
        std::fs::create_dir(&worktree).unwrap();
        std::os::unix::fs::symlink(&nested, &alias).unwrap();
        let coordinator = Arc::new(WorktreeCoordinator::ephemeral());
        coordinator
            .begin_retirement(worktree.to_str().unwrap(), "/requests/one.json")
            .unwrap();

        let candidate = alias.join("../worktree");
        assert!(coordinator
            .admit_activity(candidate.to_str().unwrap(), "spawn_terminal")
            .is_err());
    }

    #[test]
    fn corrupt_state_fails_closed() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("worktree-retirements.json");
        std::fs::write(&path, br#"{"schemaVersion":99,"retirements":{}}"#).unwrap();

        assert!(matches!(
            WorktreeCoordinator::load(path),
            Err(WorktreeCoordinatorError::CorruptState(_))
        ));
    }

    #[test]
    fn inspection_captures_exact_linked_merged_cargo_targets() {
        let directory = tempfile::tempdir().unwrap();
        let repository = directory.path().join("repository");
        let worktree = directory.path().join("linked");
        std::fs::create_dir_all(repository.join("apps/cli")).unwrap();
        std::fs::create_dir_all(repository.join("apps/desktop/src-tauri")).unwrap();
        std::fs::write(repository.join(".gitignore"), b"target\ntarget-*\n").unwrap();
        std::fs::write(repository.join("apps/cli/Cargo.toml"), b"[workspace]\n").unwrap();
        std::fs::write(
            repository.join("apps/desktop/src-tauri/Cargo.toml"),
            b"[workspace]\n",
        )
        .unwrap();
        git(directory.path(), &["init", "-b", "main", "repository"]);
        git(&repository, &["config", "user.email", "test@example.com"]);
        git(&repository, &["config", "user.name", "Test User"]);
        git(&repository, &["add", "."]);
        git(&repository, &["commit", "-m", "initial"]);
        git(
            &repository,
            &[
                "worktree",
                "add",
                "-b",
                "feature",
                worktree.to_str().unwrap(),
            ],
        );
        git(
            &repository,
            &[
                "update-ref",
                "refs/remotes/origin/main",
                "refs/heads/feature",
            ],
        );
        std::fs::create_dir_all(worktree.join("apps/cli/target")).unwrap();
        std::fs::create_dir_all(worktree.join("apps/desktop/src-tauri/target-windows")).unwrap();

        let capture = inspect_cleanup_candidate(worktree.to_str().unwrap()).unwrap();

        assert!(capture.is_linked);
        assert!(capture.merged);
        assert!(!capture.dirty);
        assert_eq!(capture.targets.len(), 2);
        assert_eq!(capture.worktree.path, worktree.to_str().unwrap());
        assert!(capture.worktree.inode > 0);
        assert!(capture.targets.iter().all(|target| target.inode > 0));
    }

    #[test]
    fn provider_request_has_the_exact_rust_storage_schema() {
        let directory = tempfile::tempdir().unwrap();
        let coordinator =
            WorktreeCoordinator::load(directory.path().join("retirements.json")).unwrap();
        let request_path = directory.path().join("request.json");
        let record = coordinator
            .begin_retirement("/repo/worktree", request_path.to_str().unwrap())
            .unwrap();
        coordinator
            .write_provider_request(
                &record,
                RetirementCleanupCapture {
                    worktree: CapturedWorktreeIdentity {
                        path: "/repo/worktree".into(),
                        device: 7,
                        inode: 11,
                        head: "1234567890123456789012345678901234567890".into(),
                        branch: "feature".into(),
                    },
                    targets: vec![CapturedPathIdentity {
                        path: "/repo/worktree/apps/cli/target".into(),
                        device: 7,
                        inode: 12,
                    }],
                    dirty: false,
                    merged: true,
                    is_linked: true,
                },
            )
            .unwrap();
        let request: serde_json::Value =
            serde_json::from_slice(&std::fs::read(request_path).unwrap()).unwrap();

        assert_eq!(
            request
                .as_object()
                .unwrap()
                .keys()
                .cloned()
                .collect::<BTreeSet<_>>(),
            [
                "allowUnmerged",
                "inventoryComplete",
                "operationId",
                "project",
                "schemaVersion",
                "targets",
                "worktree",
            ]
            .into_iter()
            .map(str::to_string)
            .collect()
        );
        assert_eq!(request["operationId"], record.operation_id);
        assert_eq!(request["project"], "t-hub");
        assert_eq!(request["allowUnmerged"], false);
        assert_eq!(request["inventoryComplete"], true);
    }

    fn provider_output(
        success: bool,
        code: i32,
        report: serde_json::Value,
    ) -> std::process::Output {
        #[cfg(unix)]
        let status = {
            use std::os::unix::process::ExitStatusExt;
            std::process::ExitStatus::from_raw(if success { 0 } else { code << 8 })
        };
        #[cfg(windows)]
        let status = {
            use std::os::windows::process::ExitStatusExt;
            std::process::ExitStatus::from_raw(if success { 0 } else { code as u32 })
        };
        std::process::Output {
            status,
            stdout: serde_json::to_vec(&report).unwrap(),
            stderr: b"provider detail".to_vec(),
        }
    }

    #[test]
    fn clean_provider_refusal_releases_the_reservation_as_failed() {
        let target = CapturedPathIdentity {
            path: "/repo/worktree/apps/cli/target".into(),
            device: 7,
            inode: 12,
        };
        let output = provider_output(
            false,
            5,
            serde_json::json!({
                "complete": false,
                "actions": [{
                    "target": target.clone(),
                    "status": "refused",
                    "recoveryState": "original",
                    "quarantinePath": null,
                }],
            }),
        );

        assert!(matches!(
            classify_provider_output(&output, &[target]),
            ProviderCompletion::Failed(_)
        ));
    }

    #[test]
    fn provider_success_requires_every_action_to_be_completed_clean() {
        let target = CapturedPathIdentity {
            path: "/repo/worktree/apps/cli/target".into(),
            device: 7,
            inode: 12,
        };
        let exact = provider_output(
            true,
            0,
            serde_json::json!({
                "complete": true,
                "actions": [{
                    "target": target.clone(),
                    "status": "completed",
                    "recoveryState": "clean",
                    "quarantinePath": null,
                }],
            }),
        );
        assert_eq!(
            classify_provider_output(&exact, std::slice::from_ref(&target)),
            ProviderCompletion::Succeeded
        );

        for (status, recovery_state, quarantine_path) in [
            ("failed", "clean", serde_json::Value::Null),
            ("completed", "quarantined", serde_json::Value::Null),
            (
                "completed",
                "clean",
                serde_json::Value::String("/tmp/quarantine".into()),
            ),
        ] {
            let contradictory = provider_output(
                true,
                0,
                serde_json::json!({
                    "complete": true,
                    "actions": [{
                        "target": target.clone(),
                        "status": status,
                        "recoveryState": recovery_state,
                        "quarantinePath": quarantine_path,
                    }],
                }),
            );
            assert!(matches!(
                classify_provider_output(&contradictory, std::slice::from_ref(&target)),
                ProviderCompletion::RecoveryRequired(_)
            ));
        }
    }

    #[test]
    fn missing_unbound_request_is_a_pre_provider_failure() {
        let record = WorktreeCoordinator::ephemeral()
            .begin_retirement("/repo/worktree", "/missing/request.json")
            .unwrap();
        assert!(matches!(
            missing_request_completion(&record),
            ProviderCompletion::Failed(_)
        ));
    }

    #[test]
    fn restart_releases_unbound_reservation_without_invalid_recovery_state() {
        let directory = tempfile::tempdir().unwrap();
        let store = directory.path().join("retirements.json");
        let operation_id = {
            let coordinator = WorktreeCoordinator::load(store.clone()).unwrap();
            coordinator
                .begin_retirement("/repo/worktree", "/missing/request.json")
                .unwrap()
                .operation_id
        };
        let coordinator = Arc::new(WorktreeCoordinator::load(store.clone()).unwrap());
        assert!(coordinator
            .transition(
                &operation_id,
                RetirementState::RecoveryRequired,
                Some("invalid".into()),
            )
            .is_err());
        coordinator.recover_pending();

        let restarted = WorktreeCoordinator::load(store).unwrap();
        assert!(restarted.pending_retirements().is_empty());
        assert_eq!(
            restarted
                .lock()
                .retirements
                .get(&operation_id)
                .unwrap()
                .state,
            RetirementState::Failed
        );
    }

    #[test]
    fn recovery_requires_exact_operation_target_request_and_inventory() {
        let directory = tempfile::tempdir().unwrap();
        let request_path = directory.path().join("request.json");
        let coordinator =
            WorktreeCoordinator::load(directory.path().join("retirements.json")).unwrap();
        let record = coordinator
            .begin_retirement("/repo/worktree", request_path.to_str().unwrap())
            .unwrap();
        let capture = RetirementCleanupCapture {
            worktree: CapturedWorktreeIdentity {
                path: "/repo/worktree".into(),
                device: 7,
                inode: 11,
                head: "1234567890123456789012345678901234567890".into(),
                branch: "feature".into(),
            },
            targets: vec![CapturedPathIdentity {
                path: "/repo/worktree/apps/cli/target".into(),
                device: 7,
                inode: 12,
            }],
            dirty: false,
            merged: true,
            is_linked: true,
        };
        coordinator
            .write_provider_request(&record, capture.clone())
            .unwrap();
        coordinator
            .transition(
                &record.operation_id,
                RetirementState::RecoveryRequired,
                Some("ambiguous provider result".into()),
            )
            .unwrap();

        assert!(coordinator
            .recovery_record(&record.operation_id, "/repo/other")
            .is_err());
        let recovered = coordinator
            .recovery_record(&record.operation_id, "/repo/worktree")
            .unwrap();
        coordinator
            .validate_recovery_capture(&recovered, &capture)
            .unwrap();
        let mut changed = capture;
        changed.targets[0].inode += 1;
        assert!(coordinator
            .validate_recovery_capture(&recovered, &changed)
            .is_err());

        let restarted =
            WorktreeCoordinator::load(directory.path().join("retirements.json")).unwrap();
        assert_eq!(
            restarted
                .recovery_record(&record.operation_id, "/repo/worktree")
                .unwrap()
                .state,
            RetirementState::RecoveryRequired
        );
    }

    #[test]
    fn restart_preserves_interrupted_commit_for_explicit_recovery() {
        let directory = tempfile::tempdir().unwrap();
        let store = directory.path().join("retirements.json");
        let request = directory.path().join("request.json");
        let operation_id = {
            let coordinator = WorktreeCoordinator::load(store.clone()).unwrap();
            let record = coordinator
                .begin_retirement("/repo/worktree", request.to_str().unwrap())
                .unwrap();
            coordinator
                .write_provider_request(
                    &record,
                    RetirementCleanupCapture {
                        worktree: CapturedWorktreeIdentity {
                            path: "/repo/worktree".into(),
                            device: 7,
                            inode: 11,
                            head: "1234567890123456789012345678901234567890".into(),
                            branch: "feature".into(),
                        },
                        targets: vec![CapturedPathIdentity {
                            path: "/repo/worktree/apps/cli/target".into(),
                            device: 7,
                            inode: 12,
                        }],
                        dirty: false,
                        merged: true,
                        is_linked: true,
                    },
                )
                .unwrap();
            coordinator
                .transition(&record.operation_id, RetirementState::Running, None)
                .unwrap();
            record.operation_id
        };

        let restarted = Arc::new(WorktreeCoordinator::load(store).unwrap());
        restarted.recover_pending();

        let recovered = restarted
            .recovery_record(&operation_id, "/repo/worktree")
            .unwrap();
        assert_eq!(recovered.state, RetirementState::RecoveryRequired);
        assert!(recovered
            .last_error
            .as_deref()
            .is_some_and(|error| error.contains("interrupted")));
    }

    #[cfg(unix)]
    #[test]
    fn namespace_blocker_hides_real_target_from_shell_and_direct_chdir_aliases() {
        let directory = tempfile::tempdir().unwrap();
        let worktree = directory.path().join("worktree");
        let alias = directory.path().join("worktree-alias");
        let unrelated = directory.path().join("unrelated");
        let request_path = directory.path().join("request.json");
        std::fs::create_dir(&worktree).unwrap();
        std::fs::create_dir(&unrelated).unwrap();
        std::os::unix::fs::symlink(&worktree, &alias).unwrap();
        let stat = std::fs::metadata(&worktree).unwrap();
        use std::os::unix::fs::MetadataExt;
        let coordinator = Arc::new(WorktreeCoordinator::ephemeral());
        let record = coordinator
            .begin_retirement(worktree.to_str().unwrap(), request_path.to_str().unwrap())
            .unwrap();
        coordinator
            .write_provider_request(
                &record,
                RetirementCleanupCapture {
                    worktree: CapturedWorktreeIdentity {
                        path: worktree.to_str().unwrap().into(),
                        device: stat.dev(),
                        inode: stat.ino(),
                        head: "1234567890123456789012345678901234567890".into(),
                        branch: "feature".into(),
                    },
                    targets: vec![CapturedPathIdentity {
                        path: worktree.join("target").to_str().unwrap().into(),
                        device: stat.dev(),
                        inode: stat.ino() + 1,
                    }],
                    dirty: false,
                    merged: true,
                    is_linked: true,
                },
            )
            .unwrap();
        let admission = coordinator
            .admit_activity(unrelated.to_str().unwrap(), "spawn_terminal")
            .unwrap();
        let direct = format!(
            "import os; os.chdir({:?}); s=os.stat('.'); assert (s.st_dev,s.st_ino)!=({},{})",
            alias.to_str().unwrap(),
            stat.dev(),
            stat.ino()
        );
        let proc_probe = format!(
            "import os; p={:?}; expected=({},{}); \
             assert (os.stat('/proc/1/root'+p).st_dev,os.stat('/proc/1/root'+p).st_ino)!=expected; \
             assert (os.stat('/proc/self/root'+p).st_dev,os.stat('/proc/self/root'+p).st_ino)!=expected",
            worktree.to_str().unwrap(),
            stat.dev(),
            stat.ino(),
        );
        let command = format!(
            "cd {} && test \"$(stat -c %d:%i .)\" != {}:{} && python3 -c {} && python3 -c {}",
            shell_quote(alias.to_str().unwrap()),
            stat.dev(),
            stat.ino(),
            shell_quote(&direct),
            shell_quote(&proc_probe),
        );
        let (contained, _) = admission
            .contain_process(Some(&command), Vec::new())
            .unwrap();

        let status = Command::new("/bin/sh")
            .args(["-c", contained.as_deref().unwrap()])
            .status()
            .unwrap();
        assert!(status.success());

        let session = format!(
            "th_test_containment_{}",
            &uuid::Uuid::new_v4().simple().to_string()[..8]
        );
        let (contained, env) = admission
            .contain_process(
                Some(
                    "python3 -c 'import os,threading,time; \
                     threading.Thread(target=lambda: (os.fork() or time.sleep(30)), daemon=True).start(); \
                     time.sleep(30)'",
                ),
                Vec::new(),
            )
            .unwrap();
        crate::tmux::new_session_with_env(
            &session,
            unrelated.to_str().unwrap(),
            contained.as_deref(),
            &env,
        )
        .unwrap();
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        let pane = loop {
            if let Some(pane) = crate::tmux::pane_info()
                .unwrap()
                .into_iter()
                .find(|pane| pane.session == session)
            {
                break pane;
            }
            assert!(std::time::Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(20));
        };
        let mut provider = prepared_test_provider();
        let error = verify_process_containment(
            &[pane],
            admission.blockers.first().expect("one exact blocker"),
            "0123456789abcdef0123456789abcdef",
            "/tmp/test-request.json",
            &provider.identity,
        )
        .err()
        .unwrap();
        assert!(matches!(
            provider.stop("test complete".into()),
            ManagedProviderCompletion::Terminated(_)
        ));
        assert!(error.contains("exact managed cgroup-v2 freezer ownership is unavailable"));
        crate::tmux::kill_session_tree(&session).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn cleanup_review_preexisting_managed_runtime_uses_frozen_boundary() {
        use std::os::unix::fs::MetadataExt;
        let directory = tempfile::tempdir().unwrap();
        let worktree = directory.path().join("worktree");
        let unrelated = directory.path().join("unrelated");
        let request_path = directory.path().join("request.json");
        std::fs::create_dir(&worktree).unwrap();
        std::fs::create_dir(&unrelated).unwrap();
        let stat = std::fs::metadata(&worktree).unwrap();
        let coordinator = Arc::new(WorktreeCoordinator::ephemeral());
        let admission = coordinator
            .admit_activity(unrelated.to_str().unwrap(), "reconcile_cortana")
            .unwrap();
        let (contained, env) = admission
            .contain_process(
                Some(
                    "python3 -c 'import os,threading,time; \
                     threading.Thread(target=lambda: (os.fork() or time.sleep(30)), daemon=True).start(); \
                     time.sleep(30)'",
                ),
                Vec::new(),
            )
            .unwrap();
        let launch = || {
            let session = format!(
                "th_test_managed_containment_{}",
                &uuid::Uuid::new_v4().simple().to_string()[..8]
            );
            crate::tmux::new_managed_session_with_env(
                &session,
                unrelated.to_str().unwrap(),
                contained.as_deref(),
                &env,
            )
            .map(|owner| (session, owner))
        };
        // This test runs beside the process-heavy preview and agent suites.
        // Retry the complete production launch once with fresh tmux and systemd
        // identities so scheduler starvation cannot consume the first bounded
        // ownership-publication window.
        let (session, owner) = launch()
            .or_else(|first_error| launch().map_err(|_| first_error))
            .unwrap();
        drop(admission);
        let target = CapturedWorktreeIdentity {
            path: worktree.to_str().unwrap().into(),
            device: stat.dev(),
            inode: stat.ino(),
            head: "1234567890123456789012345678901234567890".into(),
            branch: "feature".into(),
        };
        let record = coordinator
            .begin_retirement(worktree.to_str().unwrap(), request_path.to_str().unwrap())
            .unwrap();
        coordinator
            .write_provider_request(
                &record,
                RetirementCleanupCapture {
                    worktree: target.clone(),
                    targets: vec![CapturedPathIdentity {
                        path: worktree.join("target").to_str().unwrap().into(),
                        device: stat.dev(),
                        inode: stat.ino() + 1,
                    }],
                    dirty: false,
                    merged: true,
                    is_linked: true,
                },
            )
            .unwrap();
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        let pane = loop {
            if let Some(pane) = crate::tmux::pane_info()
                .unwrap()
                .into_iter()
                .find(|pane| pane.session == session)
            {
                break pane;
            }
            assert!(std::time::Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(20));
        };
        let mut provider = prepared_test_provider();
        let mut verification = verify_process_containment(
            &[pane],
            &target,
            &record.operation_id,
            request_path.to_str().unwrap(),
            &provider.identity,
        );
        assert!(matches!(
            provider.stop("test complete".into()),
            ManagedProviderCompletion::Terminated(_)
        ));
        if let Ok(guard) = &mut verification {
            guard.release().unwrap();
        }
        let leaked_lease = std::fs::read_dir(directory.path())
            .unwrap()
            .filter_map(Result::ok)
            .any(|entry| entry.file_name().to_string_lossy().contains(".freeze-"));
        crate::tmux::retire_managed_runtime(&session, &owner).unwrap();
        verification.unwrap();
        assert!(!leaked_lease);
    }

    #[test]
    fn uncontained_managed_process_blocks_cleanup() {
        let target = CapturedWorktreeIdentity {
            path: "/repo/worktree".into(),
            device: 7,
            inode: 11,
            head: "1234567890123456789012345678901234567890".into(),
            branch: "feature".into(),
        };
        let pane = crate::tmux::PaneInfo {
            session: "th_uncontained".into(),
            command: "sh".into(),
            cwd: "/tmp".into(),
            pid: std::process::id(),
        };
        let mut provider = prepared_test_provider();
        assert!(verify_process_containment(
            &[pane],
            &target,
            "0123456789abcdef0123456789abcdef",
            "/tmp/test-request.json",
            &provider.identity,
        )
        .err()
        .unwrap()
        .contains("exact managed cgroup-v2 freezer ownership is unavailable"));
        assert!(matches!(
            provider.stop("test complete".into()),
            ManagedProviderCompletion::Terminated(_)
        ));
    }

    #[test]
    fn cleanup_review_watchdog_scripts_are_executable() {
        for script in [
            PROVIDER_SUPERVISOR_SCRIPT,
            TERMINATE_PROVIDER_SCRIPT,
            PROBE_PROVIDER_STOPPED_SCRIPT,
            ATOMIC_CONTAINMENT_INSPECTION_SCRIPT,
            CONTAINMENT_FREEZE_WATCHDOG_SCRIPT,
        ] {
            let status = Command::new("/usr/bin/python3")
                .args([
                    "-c",
                    "import sys; compile(sys.argv[1], '<containment>', 'exec')",
                    script,
                ])
                .status()
                .unwrap();
            assert!(status.success());
        }
    }

    #[test]
    fn cleanup_review_provider_supervisor_proves_completion() {
        let mut provider = PreparedProviderProcess::spawn_command_with_timeout(
            vec!["/bin/sh".into(), "-c".into(), "printf completed".into()],
            Duration::from_secs(5),
        )
        .unwrap();
        let ManagedProviderCompletion::Completed(output) = provider.run() else {
            panic!("provider completion was not proven");
        };
        assert!(output.status.success());
        assert_eq!(output.stdout, b"completed");
    }

    #[test]
    fn cleanup_review_provider_supervisor_terminates_timeout_and_descendants() {
        let mut timeout = PreparedProviderProcess::spawn_command_with_timeout(
            vec!["/bin/sleep".into(), "30".into()],
            Duration::from_secs(1),
        )
        .unwrap();
        assert!(matches!(
            timeout.run(),
            ManagedProviderCompletion::Terminated(error) if error.contains("timeout")
        ));

        let mut descendant = PreparedProviderProcess::spawn_command_with_timeout(
            vec!["/bin/sh".into(), "-c".into(), "sleep 30 & exit 0".into()],
            Duration::from_secs(5),
        )
        .unwrap();
        assert!(matches!(
            descendant.run(),
            ManagedProviderCompletion::Terminated(error) if error.contains("descendant")
        ));
    }

    #[test]
    fn cleanup_review_provider_supervisor_terminates_on_parent_loss() {
        let directory = tempfile::tempdir().unwrap();
        let escaped_pid_path = directory.path().join("escaped.pid");
        let script = format!(
            "import os,pathlib,subprocess; \
             child=subprocess.Popen(['/usr/bin/setsid','/bin/sleep','30']); \
             pathlib.Path({:?}).write_text(f'{{os.getpid()}},{{child.pid}}')",
            escaped_pid_path.to_str().unwrap()
        );
        let mut provider = PreparedProviderProcess::spawn_command_with_timeout(
            vec!["/usr/bin/python3".into(), "-c".into(), script],
            Duration::from_secs(30),
        )
        .unwrap();
        provider
            .stdin
            .as_mut()
            .unwrap()
            .write_all(b"start\n")
            .unwrap();
        provider.stdin.as_mut().unwrap().flush().unwrap();
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while !escaped_pid_path.is_file() {
            assert!(std::time::Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(10));
        }
        let identities = std::fs::read_to_string(&escaped_pid_path).unwrap();
        let (leader_pid, escaped_pid) = identities.trim().split_once(',').unwrap();
        while Path::new(&format!("/proc/{leader_pid}")).exists() {
            assert!(std::time::Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(Path::new(&format!("/proc/{escaped_pid}")).exists());
        provider.stdin.take();
        provider.finish_transport();
        managed_provider_stopped(&provider.identity).unwrap();
        assert!(!Path::new(&format!("/proc/{escaped_pid}")).exists());
        provider.finished = true;
    }

    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    enum TestLeaseState {
        Prepared,
        Armed,
        Thawed,
    }

    #[derive(Debug)]
    struct InjectedWatchdogOperations {
        fail_at: Option<WatchdogOperation>,
        operations: Vec<WatchdogOperation>,
        exact_frozen: bool,
        unrelated_frozen: bool,
        changed: bool,
        lease: Option<TestLeaseState>,
        provider_admissions: usize,
        authorization: Option<WatchdogAuthorizationEvidence>,
        authorization_issued: bool,
        terminal_preserved: bool,
        agent_preserved: bool,
        source_preserved: bool,
    }

    impl WatchdogBackend for InjectedWatchdogOperations {
        fn perform(&mut self, operation: WatchdogOperation) -> Result<(), String> {
            self.operations.push(operation);
            if self.fail_at == Some(operation) {
                return Err(format!("injected failure at {}", operation.name()));
            }
            match operation {
                WatchdogOperation::LaunchWatchdog => self.lease = Some(TestLeaseState::Prepared),
                WatchdogOperation::Arm => self.lease = Some(TestLeaseState::Armed),
                WatchdogOperation::Freeze => {
                    self.exact_frozen = true;
                    self.changed = true;
                }
                WatchdogOperation::InspectEvidence => {
                    self.authorization = Some(WatchdogAuthorizationEvidence {
                        watchdog_nonce: "a".repeat(32),
                        lease_digest: "b".repeat(64),
                    });
                }
                WatchdogOperation::Thaw => {
                    self.exact_frozen = false;
                    self.changed = false;
                    self.lease = Some(TestLeaseState::Thawed);
                }
                WatchdogOperation::Disarm => {
                    self.lease = None;
                }
                WatchdogOperation::AuthorizeProvider => {
                    if self.authorization.is_none() {
                        return Err("missing injected authorization".into());
                    }
                    self.provider_admissions += 1;
                    self.authorization_issued = true;
                }
                _ => {}
            }
            Ok(())
        }

        fn recover(&mut self) -> Result<(), String> {
            if self.changed {
                self.exact_frozen = false;
                self.changed = false;
            }
            self.lease = None;
            Ok(())
        }

        fn finish(&mut self) -> Result<(), String> {
            Ok(())
        }

        fn take_authorization(&mut self) -> Result<WatchdogAuthorizationEvidence, String> {
            if !self.authorization_issued {
                return Err("injected authorization transition is incomplete".into());
            }
            self.authorization_issued = false;
            self.authorization
                .take()
                .ok_or_else(|| "injected authorization was already consumed".into())
        }
    }

    fn injected_watchdog(fail_at: Option<WatchdogOperation>) -> InjectedWatchdogOperations {
        InjectedWatchdogOperations {
            fail_at,
            operations: Vec::new(),
            exact_frozen: false,
            unrelated_frozen: false,
            changed: false,
            lease: None,
            provider_admissions: 0,
            authorization: None,
            authorization_issued: false,
            terminal_preserved: true,
            agent_preserved: true,
            source_preserved: true,
        }
    }

    #[test]
    fn cleanup_review_production_watchdog_state_machine_injects_every_operation() {
        let failures = [
            ("setup", WatchdogOperation::CaptureCgroup),
            ("missing-ready", WatchdogOperation::VerifyReady),
            ("malformed-ready", WatchdogOperation::VerifyReady),
            ("parent-before-freeze", WatchdogOperation::Arm),
            ("parent-after-freeze", WatchdogOperation::InspectEvidence),
            ("wrapper-only-death", WatchdogOperation::InspectEvidence),
            ("watchdog-crash", WatchdogOperation::InspectEvidence),
            ("deadline", WatchdogOperation::InspectEvidence),
            ("inode-replacement", WatchdogOperation::Thaw),
            ("competing-owner", WatchdogOperation::CaptureCgroup),
            ("stale-disarm", WatchdogOperation::Disarm),
            ("mismatched-disarm", WatchdogOperation::Disarm),
            ("restart-recovery", WatchdogOperation::InspectEvidence),
            ("failed-authorize", WatchdogOperation::AuthorizeProvider),
        ];
        for (name, operation) in failures {
            let mut state = injected_watchdog(Some(operation));
            assert!(execute_watchdog_lifecycle(&mut state).is_err(), "{name}");
            assert!(!state.exact_frozen, "{name}");
            assert!(!state.unrelated_frozen, "{name}");
            assert_eq!(state.lease, None, "{name}");
            assert_eq!(
                state.provider_admissions,
                usize::from(WATCHDOG_RELEASE_LIFECYCLE.contains(&operation)),
                "{name}"
            );
            assert!(state.terminal_preserved, "{name}");
            assert!(state.agent_preserved, "{name}");
            assert!(state.source_preserved, "{name}");
        }

        let mut pre_frozen = injected_watchdog(Some(WatchdogOperation::CaptureCgroup));
        pre_frozen.exact_frozen = true;
        assert!(pre_frozen.exact_frozen);
        pre_frozen.changed = false;
        assert!(execute_watchdog_lifecycle(&mut pre_frozen).is_err());
        assert!(pre_frozen.exact_frozen);
        assert_eq!(pre_frozen.provider_admissions, 0);
        assert!(!pre_frozen.unrelated_frozen);

        let mut success = injected_watchdog(None);
        execute_watchdog_lifecycle(&mut success).unwrap();
        assert!(!success.exact_frozen);
        assert!(!success.unrelated_frozen);
        assert_eq!(success.lease, None);
        assert_eq!(success.provider_admissions, 1);
        assert!(success.terminal_preserved);
        assert!(success.agent_preserved);
        assert!(success.source_preserved);
        assert_eq!(
            success.operations,
            WATCHDOG_PREPARE_LIFECYCLE
                .into_iter()
                .chain(WATCHDOG_RELEASE_LIFECYCLE)
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn cleanup_review_watchdog_remains_frozen_until_provider_completion() {
        let mut state = injected_watchdog(None);
        let authorization = prepare_watchdog_lifecycle(&mut state).unwrap();
        assert!(state.exact_frozen);
        assert_eq!(state.lease, Some(TestLeaseState::Armed));
        assert_eq!(state.provider_admissions, 1);
        assert_eq!(authorization.watchdog_nonce.len(), 32);

        release_watchdog_lifecycle(&mut state).unwrap();
        assert!(!state.exact_frozen);
        assert_eq!(state.lease, None);
    }

    #[cfg(unix)]
    #[test]
    fn cleanup_review_provider_path_canonicalizes_aliases_and_preserves_quoting() {
        let directory = tempfile::tempdir().unwrap();
        let request = directory.path().join("request with 'quotes'.json");
        let alias = directory.path().join("request-alias.json");
        std::fs::write(&request, b"{}").unwrap();
        std::os::unix::fs::symlink(&request, &alias).unwrap();
        assert_eq!(
            provider_request_path(alias.to_str().unwrap()).unwrap(),
            request.canonicalize().unwrap().to_str().unwrap()
        );
        assert!(validate_posix_provider_request_path("/tmp/request with 'quotes'.json").is_ok());
        for invalid in [
            "tmp/request.json",
            "//tmp/request.json",
            "/tmp/../request.json",
            "/tmp/./request.json",
            "/tmp/request\n.json",
        ] {
            assert!(validate_posix_provider_request_path(invalid).is_err());
        }
    }

    #[cfg(unix)]
    fn captured_test_provider_request(
        directory: &tempfile::TempDir,
    ) -> (WorktreeRetirement, CapturedProviderRequest) {
        let request = directory.path().join("request.json");
        let coordinator = WorktreeCoordinator::ephemeral();
        let record = coordinator
            .begin_retirement("/repo/worktree", request.to_str().unwrap())
            .unwrap();
        let record = coordinator
            .write_provider_request(
                &record,
                RetirementCleanupCapture {
                    worktree: CapturedWorktreeIdentity {
                        path: "/repo/worktree".into(),
                        device: 7,
                        inode: 11,
                        head: "1234567890123456789012345678901234567890".into(),
                        branch: "feature".into(),
                    },
                    targets: vec![CapturedPathIdentity {
                        path: "/repo/worktree/apps/cli/target".into(),
                        device: 7,
                        inode: 12,
                    }],
                    dirty: false,
                    merged: true,
                    is_linked: true,
                },
            )
            .unwrap();
        let request = capture_provider_request(&record).unwrap();
        let mut running = record;
        running.state = RetirementState::Running;
        running.updated_at += 1;
        (running, request)
    }

    #[cfg(unix)]
    fn test_containment_authorization() -> ContainmentAuthorizationEvidence {
        let mut backend = injected_watchdog(None);
        let evidence = execute_watchdog_lifecycle(&mut backend).unwrap();
        ContainmentAuthorizationEvidence {
            generation: "c".repeat(32),
            watchdogs: vec![evidence],
        }
    }

    #[cfg(unix)]
    #[test]
    fn cleanup_review_provider_request_rejects_replacements_at_every_boundary() {
        for phase in [
            "before-capture",
            "during-read",
            "after-capture",
            "during-containment",
            "immediately-before-launch",
        ] {
            let directory = tempfile::tempdir().unwrap();
            let request_path = directory.path().join("request.json");
            let (record, capture) = captured_test_provider_request(&directory);
            let source = directory.path().join("source.txt");
            std::fs::write(&source, b"preserved").unwrap();
            let replacement = directory.path().join("replacement.json");
            std::fs::write(&replacement, &capture.bytes).unwrap();
            let invocations = std::sync::atomic::AtomicUsize::new(0);

            if phase == "before-capture" {
                std::fs::rename(&replacement, &request_path).unwrap();
                assert!(capture_provider_request(&record).is_err());
            } else if phase == "during-read" {
                let provider_path = provider_request_path(request_path.to_str().unwrap()).unwrap();
                assert!(capture_provider_request_identity_with(
                    request_path.to_str().unwrap(),
                    &provider_path,
                    || std::fs::rename(&replacement, &request_path).unwrap()
                )
                .is_err());
            } else {
                let mut guard = ProviderAuthorizationGuard::issue(
                    &record,
                    &capture.request.worktree,
                    &capture,
                    test_containment_authorization(),
                )
                .unwrap();
                std::fs::rename(&replacement, &request_path).unwrap();
                let result =
                    guard.launch_with(&record, &capture.request.worktree, &capture, |_| {
                        invocations.fetch_add(1, Ordering::SeqCst);
                        Ok(())
                    });
                assert!(result.is_err(), "{phase}");
            }
            assert_eq!(invocations.load(Ordering::SeqCst), 0, "{phase}");
            assert!(request_path.is_file(), "{phase}");
            assert_eq!(record.state, RetirementState::Running, "{phase}");
            assert_eq!(std::fs::read(&source).unwrap(), b"preserved", "{phase}");
        }
    }

    #[cfg(unix)]
    #[test]
    fn cleanup_review_provider_authorization_is_exact_and_single_use() {
        let directory = tempfile::tempdir().unwrap();
        let (record, capture) = captured_test_provider_request(&directory);
        let target = capture.request.worktree.clone();
        let invocations = std::sync::atomic::AtomicUsize::new(0);
        let mut valid = ProviderAuthorizationGuard::issue(
            &record,
            &target,
            &capture,
            test_containment_authorization(),
        )
        .unwrap();
        valid
            .launch_with(&record, &target, &capture, |_| {
                invocations.fetch_add(1, Ordering::SeqCst);
                Ok(())
            })
            .unwrap();
        assert!(valid
            .launch_with(&record, &target, &capture, |_| {
                invocations.fetch_add(1, Ordering::SeqCst);
                Ok(())
            })
            .is_err());

        let mut stale = ProviderAuthorizationGuard::issue(
            &record,
            &target,
            &capture,
            test_containment_authorization(),
        )
        .unwrap();
        let mut changed_record = record.clone();
        changed_record.updated_at += 1;
        assert!(stale
            .launch_with(&changed_record, &target, &capture, |_| {
                invocations.fetch_add(1, Ordering::SeqCst);
                Ok(())
            })
            .is_err());

        let mut mismatched = ProviderAuthorizationGuard::issue(
            &record,
            &target,
            &capture,
            test_containment_authorization(),
        )
        .unwrap();
        let mut changed_target = target.clone();
        changed_target.inode += 1;
        assert!(mismatched
            .launch_with(&record, &changed_target, &capture, |_| {
                invocations.fetch_add(1, Ordering::SeqCst);
                Ok(())
            })
            .is_err());

        assert!(ProviderAuthorizationGuard::issue(
            &record,
            &target,
            &capture,
            ContainmentAuthorizationEvidence {
                generation: String::new(),
                watchdogs: Vec::new(),
            },
        )
        .is_err());
        let duplicated = WatchdogAuthorizationEvidence {
            watchdog_nonce: "d".repeat(32),
            lease_digest: "e".repeat(64),
        };
        assert!(ProviderAuthorizationGuard::issue(
            &record,
            &target,
            &capture,
            ContainmentAuthorizationEvidence {
                generation: "f".repeat(32),
                watchdogs: vec![duplicated.clone(), duplicated],
            },
        )
        .is_err());
        assert_eq!(invocations.load(Ordering::SeqCst), 1);
    }

    #[cfg(unix)]
    #[test]
    fn cleanup_review_provider_request_rejects_link_and_digest_changes() {
        let directory = tempfile::tempdir().unwrap();
        let (record, capture) = captured_test_provider_request(&directory);
        let request = PathBuf::from(&capture.native_path);
        let alias = directory.path().join("request-link.json");
        std::fs::hard_link(&request, &alias).unwrap();
        assert!(capture.revalidate().is_err());
        std::fs::remove_file(&alias).unwrap();

        let capture = capture_provider_request(&record).unwrap();
        std::fs::write(&request, b"other-content").unwrap();
        assert!(capture.revalidate().is_err());
    }

    struct InjectedProviderPathTransport {
        canonical: Result<(String, ProviderPathIdentity), String>,
        translations: std::cell::RefCell<std::collections::VecDeque<Result<String, String>>>,
        canonical_wsl: Result<String, String>,
        identities:
            std::cell::RefCell<std::collections::VecDeque<Result<ProviderPathIdentity, String>>>,
        calls: std::cell::RefCell<Vec<Vec<String>>>,
    }

    impl InjectedProviderPathTransport {
        fn exact(native: &str, wsl: &str) -> Self {
            let identity = ProviderPathIdentity {
                volume: 7,
                file: 11,
            };
            Self {
                canonical: Ok((native.to_string(), identity)),
                translations: std::cell::RefCell::new(
                    [Ok(wsl.to_string()), Ok(native.to_string())].into(),
                ),
                canonical_wsl: Ok(wsl.to_string()),
                identities: std::cell::RefCell::new([Ok(identity), Ok(identity)].into()),
                calls: std::cell::RefCell::new(Vec::new()),
            }
        }
    }

    impl ProviderPathTransport for InjectedProviderPathTransport {
        fn canonical_native(&self, _path: &str) -> Result<(String, ProviderPathIdentity), String> {
            self.canonical.clone()
        }

        fn wslpath(&self, arguments: &[&str]) -> Result<String, String> {
            self.calls.borrow_mut().push(
                arguments
                    .iter()
                    .map(|argument| argument.to_string())
                    .collect(),
            );
            self.translations
                .borrow_mut()
                .pop_front()
                .expect("expected translation")
        }

        fn readlink(&self, _path: &str) -> Result<String, String> {
            self.canonical_wsl.clone()
        }

        fn native_identity(&self, _path: &str) -> Result<ProviderPathIdentity, String> {
            self.identities
                .borrow_mut()
                .pop_front()
                .expect("expected identity check")
        }
    }

    #[test]
    fn cleanup_review_windows_path_transport_enforces_exact_contract() {
        let native = r"C:\Users\natha\request with 'quotes' & $(literal).json";
        let wsl = "/mnt/c/Users/natha/request with 'quotes' & $(literal).json";
        let transport = InjectedProviderPathTransport::exact(native, wsl);
        assert_eq!(
            translate_windows_provider_request_path(native, &transport).unwrap(),
            wsl
        );
        assert_eq!(
            transport.calls.into_inner(),
            vec![vec!["-a", "-u", native], vec!["-a", "-w", wsl],]
        );

        let lower_drive = r"c:\Users\natha\request.json";
        assert!(translate_windows_provider_request_path(
            lower_drive,
            &InjectedProviderPathTransport::exact(lower_drive, "/mnt/c/Users/natha/request.json")
        )
        .is_ok());

        for invalid in [
            r"request.json",
            r"\\server\share\request.json",
            r"C:\Users/natha\request.json",
            r"C:\Users\natha\..\request.json",
            r"C:\Users\natha\.\request.json",
            "C:\\Users\\natha\\request.json\n",
        ] {
            assert!(
                translate_windows_provider_request_path(
                    invalid,
                    &InjectedProviderPathTransport::exact(
                        r"C:\Users\natha\request.json",
                        "/mnt/c/Users/natha/request.json"
                    )
                )
                .is_err(),
                "{invalid:?}"
            );
        }
    }

    #[test]
    fn cleanup_review_windows_path_transport_fails_closed_on_transport_faults() {
        let native = r"C:\Users\natha\request.json";
        let wsl = "/mnt/c/Users/natha/request.json";

        for translated in [
            "mnt/c/Users/natha/request.json",
            "/mnt/c/Users/natha/request.json\n/mnt/c/Users/natha/request.json",
            "/mnt/c/Users/natha/../request.json",
        ] {
            let mut transport = InjectedProviderPathTransport::exact(native, wsl);
            transport.translations = std::cell::RefCell::new(
                [Ok(translated.to_string()), Ok(native.to_string())].into(),
            );
            assert!(translate_windows_provider_request_path(native, &transport).is_err());
        }

        let mut alias = InjectedProviderPathTransport::exact(native, wsl);
        alias.canonical_wsl = Ok("/mnt/c/Users/natha/other.json".into());
        assert!(translate_windows_provider_request_path(native, &alias).is_err());

        let mut timeout = InjectedProviderPathTransport::exact(native, wsl);
        timeout.translations =
            std::cell::RefCell::new([Err("translation timed out".into())].into());
        assert!(translate_windows_provider_request_path(native, &timeout).is_err());

        let mut round_trip_mismatch = InjectedProviderPathTransport::exact(native, wsl);
        round_trip_mismatch.identities = std::cell::RefCell::new(
            [
                Ok(ProviderPathIdentity {
                    volume: 8,
                    file: 11,
                }),
                Ok(ProviderPathIdentity {
                    volume: 7,
                    file: 11,
                }),
            ]
            .into(),
        );
        assert!(translate_windows_provider_request_path(native, &round_trip_mismatch).is_err());

        let mut changed_during_use = InjectedProviderPathTransport::exact(native, wsl);
        changed_during_use.identities = std::cell::RefCell::new(
            [
                Ok(ProviderPathIdentity {
                    volume: 7,
                    file: 11,
                }),
                Ok(ProviderPathIdentity {
                    volume: 7,
                    file: 12,
                }),
            ]
            .into(),
        );
        assert!(translate_windows_provider_request_path(native, &changed_during_use).is_err());
    }

    #[cfg(windows)]
    #[test]
    fn cleanup_review_windows_provider_path_round_trips_exact_identity() {
        let directory = tempfile::tempdir().unwrap();
        let request = directory.path().join("request with 'quotes'.json");
        std::fs::write(&request, b"{}").unwrap();
        let translated = provider_request_path(request.to_str().unwrap()).unwrap();
        validate_posix_provider_request_path(&translated).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn multiple_targets_coordinate_without_holding_the_global_boundary() {
        use std::os::unix::fs::MetadataExt;
        let directory = tempfile::tempdir().unwrap();
        let coordinator = Arc::new(WorktreeCoordinator::ephemeral());
        for name in ["first", "second"] {
            let worktree = directory.path().join(name);
            let request = directory.path().join(format!("{name}.json"));
            std::fs::create_dir(&worktree).unwrap();
            let stat = std::fs::metadata(&worktree).unwrap();
            let record = coordinator
                .begin_retirement(worktree.to_str().unwrap(), request.to_str().unwrap())
                .unwrap();
            coordinator
                .write_provider_request(
                    &record,
                    RetirementCleanupCapture {
                        worktree: CapturedWorktreeIdentity {
                            path: worktree.to_str().unwrap().into(),
                            device: stat.dev(),
                            inode: stat.ino(),
                            head: "1234567890123456789012345678901234567890".into(),
                            branch: "feature".into(),
                        },
                        targets: vec![CapturedPathIdentity {
                            path: worktree.join("target").to_str().unwrap().into(),
                            device: stat.dev(),
                            inode: stat.ino() + 1,
                        }],
                        dirty: false,
                        merged: true,
                        is_linked: true,
                    },
                )
                .unwrap();
        }
        let unrelated = directory.path().join("unrelated");
        std::fs::create_dir(&unrelated).unwrap();
        let boundaries = coordinator
            .boundaries
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let first = Arc::clone(
            boundaries
                .get(directory.path().join("first").to_str().unwrap())
                .unwrap(),
        );
        let second = Arc::clone(
            boundaries
                .get(directory.path().join("second").to_str().unwrap())
                .unwrap(),
        );
        drop(boundaries);
        let _first_guard = first
            .coordination
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        assert!(second.coordination.try_lock().is_ok());
        let (sent, received) = std::sync::mpsc::channel();
        let admitting = Arc::clone(&coordinator);
        let unrelated = unrelated.to_str().unwrap().to_string();
        let worker = std::thread::spawn(move || {
            let admission = admitting
                .admit_activity(&unrelated, "spawn_terminal")
                .unwrap();
            sent.send(admission.blockers.len()).unwrap();
        });
        assert_eq!(received.recv_timeout(Duration::from_secs(2)).unwrap(), 2);
        worker.join().unwrap();
        drop(_first_guard);
        assert!(coordinator
            .admit_activity(
                directory.path().join("first").to_str().unwrap(),
                "spawn_terminal",
            )
            .is_err());
    }

    #[test]
    fn cleanup_review_retirement_checks_only_matching_inflight_admissions() {
        let coordinator = Arc::new(WorktreeCoordinator::ephemeral());
        let unrelated = coordinator
            .admit_activity("/repo/worktrees/second/nested", "spawn_terminal")
            .unwrap();
        coordinator
            .begin_retirement_if_idle("/repo/worktrees/first", "/requests/first.json", |_| {
                Ok(Vec::new())
            })
            .unwrap();
        drop(unrelated);

        let coordinator = Arc::new(WorktreeCoordinator::ephemeral());
        let matching = coordinator
            .admit_activity("/repo/worktrees/third/nested", "spawn_terminal")
            .unwrap();
        let error = coordinator
            .begin_retirement_if_idle("/repo/worktrees/third", "/requests/third.json", |_| {
                Ok(Vec::new())
            })
            .unwrap_err();
        assert!(error.to_string().contains("activity being admitted"));
        drop(matching);
    }

    /// Windows regression: a spawn with no explicit cwd hands the gate an empty
    /// candidate, which cannot be canonicalized. It used to fail the whole spawn
    /// with "could not resolve WSL path ''".
    #[test]
    fn admission_admits_an_unnameable_directory_when_no_cleanup_is_running() {
        let coordinator = Arc::new(WorktreeCoordinator::ephemeral());
        for candidate in ["", "   "] {
            let guard = coordinator
                .admit_activity(candidate, "spawn_terminal")
                .unwrap();
            assert_eq!(guard.canonical_path(), "");
            let (command, env) = guard
                .contain_process(Some("exec ${SHELL:-/bin/sh} -l"), Vec::new())
                .unwrap();
            assert_eq!(command.as_deref(), Some("exec ${SHELL:-/bin/sh} -l"));
            assert!(env.is_empty());
        }
    }

    /// The unscoped admission is conservative, not a hole: an unnameable candidate
    /// counts as possibly inside every active retirement.
    #[test]
    fn admission_refuses_an_unnameable_directory_while_a_cleanup_is_running() {
        let coordinator = Arc::new(WorktreeCoordinator::ephemeral());
        let record = coordinator
            .begin_retirement_if_idle("/repo/worktrees/first", "/requests/first.json", |_| {
                Ok(Vec::new())
            })
            .unwrap();

        let Err(error) = coordinator.admit_activity("", "spawn_terminal") else {
            panic!("an unnameable candidate must be refused while a cleanup is running");
        };
        assert!(error.contains("reserved for Cargo cleanup"), "{error}");
        assert!(error.contains(&record.operation_id), "{error}");
    }

    /// ...and it blocks a cleanup from starting underneath it, exactly as a named
    /// admission inside that worktree would.
    #[test]
    fn an_unnameable_admission_blocks_a_new_cleanup_from_starting() {
        let coordinator = Arc::new(WorktreeCoordinator::ephemeral());
        let unnameable = coordinator.admit_activity("", "spawn_terminal").unwrap();

        let error = coordinator
            .begin_retirement_if_idle("/repo/worktrees/first", "/requests/first.json", |_| {
                Ok(Vec::new())
            })
            .unwrap_err();
        assert!(error.to_string().contains("activity being admitted"));

        drop(unnameable);
        coordinator
            .begin_retirement_if_idle("/repo/worktrees/first", "/requests/first.json", |_| {
                Ok(Vec::new())
            })
            .unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn cleanup_review_retirement_matches_inflight_symlink_admission() {
        let directory = tempfile::tempdir().unwrap();
        let worktree = directory.path().join("worktree");
        let alias = directory.path().join("alias");
        std::fs::create_dir(&worktree).unwrap();
        std::os::unix::fs::symlink(&worktree, &alias).unwrap();
        let coordinator = Arc::new(WorktreeCoordinator::ephemeral());
        let matching = coordinator
            .admit_activity(alias.join("nested").to_str().unwrap(), "spawn_terminal")
            .unwrap();

        let error = coordinator
            .begin_retirement_if_idle(
                worktree.to_str().unwrap(),
                directory.path().join("request.json").to_str().unwrap(),
                |_| Ok(Vec::new()),
            )
            .unwrap_err();
        assert!(error.to_string().contains("activity being admitted"));
        drop(matching);
    }

    #[test]
    fn quarantined_provider_refusal_requires_recovery() {
        let target = CapturedPathIdentity {
            path: "/repo/worktree/apps/cli/target".into(),
            device: 7,
            inode: 12,
        };
        let output = provider_output(
            false,
            5,
            serde_json::json!({
                "complete": false,
                "actions": [{
                    "target": target.clone(),
                    "status": "refused",
                    "recoveryState": "quarantined",
                    "quarantinePath": "/repo/worktree/apps/cli/.target-quarantine",
                }],
            }),
        );

        assert!(matches!(
            classify_provider_output(&output, &[target]),
            ProviderCompletion::RecoveryRequired(_)
        ));
    }

    #[test]
    fn incomplete_provider_inventory_requires_recovery() {
        let first = CapturedPathIdentity {
            path: "/repo/worktree/apps/cli/target".into(),
            device: 7,
            inode: 12,
        };
        let second = CapturedPathIdentity {
            path: "/repo/worktree/apps/desktop/src-tauri/target".into(),
            device: 7,
            inode: 13,
        };
        let output = provider_output(
            false,
            5,
            serde_json::json!({
                "complete": false,
                "actions": [{
                    "target": first.clone(),
                    "status": "refused",
                    "recoveryState": "original",
                    "quarantinePath": null,
                }],
            }),
        );

        assert!(matches!(
            classify_provider_output(&output, &[first, second]),
            ProviderCompletion::RecoveryRequired(_)
        ));
    }
}
