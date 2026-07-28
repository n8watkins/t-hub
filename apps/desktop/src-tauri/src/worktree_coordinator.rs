//! Durable coordination for artifact cleanup reservations.
//!
//! A reservation is intentionally separate from Git worktree removal.
//! It blocks new activity in one exact linked worktree while an external storage
//! provider reclaims Cargo artifacts.
//! Completed records remain durable for recovery and audit, but only active
//! records participate in admission decisions.

use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

const SCHEMA_VERSION: u32 = 1;
const PROVIDER_OUTPUT_LIMIT: usize = 16 * 1024 * 1024;
const INSPECTION_OUTPUT_LIMIT: usize = 1024 * 1024;
const LAST_ERROR_LIMIT: usize = 8192;
const CONTAINMENT_EVIDENCE_LIMIT: usize = 64 * 1024;

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
watchdog_script = sys.argv[5]
MAX_PROCESSES = 256
MAX_TASKS = 1024

def inspection_timeout(signum, frame):
    raise RuntimeError("managed containment inspection timed out")

signal.signal(signal.SIGALRM, inspection_timeout)
signal.alarm(12)

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
        or target not in evidence.get("blockers", [])
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

cgroup_directory = None
cgroup_identity = None
watchdog_listener = None
watchdog_connection = None
watchdog_lease = None
watchdog_secret = None
freeze_changed = False
watchdog_events = []
try:
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
    watchdog_nonce = secrets.token_hex(16)
    watchdog_secret = secrets.token_bytes(32)
    watchdog_unit = f"t-hub-freeze-watchdog-{watchdog_nonce}.service"
    watchdog_socket = f"t-hub-freeze-watchdog-{watchdog_nonce}"
    watchdog_deadline = time.monotonic() + 10
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
    }
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
            "--property=RuntimeMaxSec=12s",
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
    watchdog_events.append("unitCreated")
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
    watchdog_events.append("preparedReady")
    watchdog_connection.sendall(signed("arm", watchdog_lease, watchdog_secret))
    watchdog_events.append("armSent")
    receive(
        watchdog_connection,
        "frozen",
        watchdog_lease,
        watchdog_secret,
        min(watchdog_deadline, time.monotonic() + 6),
    )
    freeze_changed = True
    watchdog_events.append("frozen")
    if (
        cgroup_path(root_pid) != managed_path
        or os.fstat(cgroup_directory).st_ino != managed_inode
        or event_value(cgroup_directory, "frozen") != 1
    ):
        raise RuntimeError("managed cgroup identity changed during freeze")
    roots = cgroup_processes(cgroup_directory)
    identities, edges, task_identities = process_tree(roots)
    cgroup_identity = (managed_path, managed_inode, roots)

    supervisor_pid, namespace_supervisor = containment_pair(identities, edges)
    if (
        pathlib.Path(f"/proc/{supervisor_pid}").stat().st_uid != os.getuid()
        or pathlib.Path(f"/proc/{namespace_supervisor}").stat().st_uid != os.getuid()
    ):
        raise RuntimeError("containment supervisor ownership is mismatched")
    if not executable_matches(root_pid, "/usr/bin/systemd-run") and root_pid != supervisor_pid:
        raise RuntimeError("managed runtime root has an ambiguous executable identity")
    if any(cgroup_path(pid) != managed_path for pid in identities):
        raise RuntimeError("managed runtime crossed its exact cgroup")
    if cgroup_processes(cgroup_directory) != cgroup_identity[2]:
        raise RuntimeError("managed cgroup process set changed while frozen")

    workload_roots = edges.get(namespace_supervisor, [])
    if len(workload_roots) != 1:
        raise RuntimeError("containment supervisor has an ambiguous child set")
    workload = descendants(edges, workload_roots[0])
    permitted = workload | {supervisor_pid, namespace_supervisor, root_pid}
    if set(identities) != permitted:
        raise RuntimeError("managed runtime contains an uncontained sibling process")
    supervisor_descriptors = {}
    for pid in (supervisor_pid, namespace_supervisor):
        supervisor_cwd = os.readlink(f"/proc/{pid}/cwd")
        if supervisor_cwd == target["path"] or supervisor_cwd.startswith(target["path"] + "/"):
            raise RuntimeError("containment supervisor cwd reaches the target")
        supervisor_descriptors[pid] = descriptors(pid)
        if any(
            value == target["path"] or value.startswith(target["path"] + "/")
            for value in supervisor_descriptors[pid].values()
        ):
            raise RuntimeError("containment supervisor retains a target descriptor")

    expected_evidence = evidence_for(workload_roots[0])
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

    after_identities, after_edges, after_tasks = process_tree(
        cgroup_processes(cgroup_directory)
    )
    if (
        identities != after_identities
        or edges != after_edges
        or task_identities != after_tasks
        or supervisor_descriptors[supervisor_pid] != descriptors(supervisor_pid)
        or supervisor_descriptors[namespace_supervisor] != descriptors(namespace_supervisor)
    ):
        raise RuntimeError("managed process or task set changed during containment inspection")
    watchdog_events.append("containmentVerified")
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
            watchdog_events.append("thawed")
            receive(
                watchdog_connection,
                "thawed",
                watchdog_lease,
                watchdog_secret,
                min(watchdog_lease["deadline"], time.monotonic() + 6),
            )
            if (
                cgroup_path(root_pid) != managed_path
                or os.fstat(cgroup_directory).st_ino != managed_inode
                or event_value(cgroup_directory, "frozen") != 0
            ):
                raise RuntimeError("managed cgroup identity changed after unfreeze")
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
            freeze_changed = False
            watchdog_events.append("disarmed")
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
print(json.dumps({"version": 1, "events": watchdog_events}, separators=(",", ":")))
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
    ) -> Result<(), WorktreeCoordinatorError> {
        let request = RetirementCleanupRequest {
            schema_version: SCHEMA_VERSION,
            operation_id: record.operation_id.clone(),
            project: "t-hub".into(),
            worktree: capture.worktree,
            targets: capture.targets,
            allow_unmerged: false,
            inventory_complete: true,
        };
        write_json_atomic(Path::new(&record.request_path), &request).map_err(|error| {
            WorktreeCoordinatorError::Persistence(format!("{}: {error}", record.request_path))
        })
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
                    if let Err(error) = self.start_provider_worker(record.clone()) {
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
            let request = match read_provider_request(&record) {
                Ok(request) => request,
                Err(error) => return ProviderCompletion::RecoveryRequired(error),
            };
            if let Err(error) = self.transition(&operation_id, RetirementState::Running, None) {
                return ProviderCompletion::RecoveryRequired(format!(
                    "could not persist the running provider state: {error}"
                ));
            }
            match self.run_provider(&record, &request) {
                Ok(output) => classify_provider_output(&output, &request.targets),
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

    pub fn admit_activity(
        self: &Arc<Self>,
        candidate_path: &str,
        operation: &str,
    ) -> Result<WorktreeAdmissionGuard, String> {
        let candidate_path = crate::files::canonical_posix_path_allow_missing(candidate_path)
            .map(|path| normalize_path(&path))
            .map_err(|error| {
                format!("{operation}: could not resolve worktree activity: {error}")
            })?;
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
        request: &RetirementCleanupRequest,
    ) -> Result<std::process::Output, String> {
        let boundary = self.boundary_for(&record.worktree_path);
        let _boundary = boundary
            .coordination
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let panes = crate::tmux::pane_info()
            .map_err(|error| format!("Cargo cleanup containment inspection failed: {error}"))?;
        let request_path = provider_request_path(&record.request_path)?;
        verify_process_containment(
            &panes,
            &request.worktree,
            &record.operation_id,
            &request_path,
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
        drop(_boundary);
        run_provider(&request_path)
    }
}

fn missing_request_completion(record: &WorktreeRetirement) -> ProviderCompletion {
    ProviderCompletion::RecoveryRequired(format!(
        "durable provider request is missing: {}",
        record.request_path
    ))
}

fn read_provider_request(record: &WorktreeRetirement) -> Result<RetirementCleanupRequest, String> {
    let request: RetirementCleanupRequest = serde_json::from_slice(
        &std::fs::read(&record.request_path)
            .map_err(|error| format!("could not read durable provider request: {error}"))?,
    )
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
    let reported_inventory = report
        .get("actions")
        .and_then(serde_json::Value::as_array)
        .and_then(|actions| {
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
        return if report.get("complete").and_then(serde_json::Value::as_bool) == Some(true) {
            ProviderCompletion::Succeeded
        } else {
            ProviderCompletion::RecoveryRequired(
                "rust-storage returned success without a complete report".into(),
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

const WATCHDOG_LIFECYCLE_EVENTS: [&str; 8] = [
    "unitCreated",
    "preparedReady",
    "armSent",
    "frozen",
    "containmentVerified",
    "thawed",
    "disarmed",
    "providerAdmission",
];

trait WatchdogLifecycleOperations {
    fn execute(&mut self, operation: &'static str) -> Result<(), String>;
    fn recover(&mut self) -> Result<(), String>;
}

fn execute_watchdog_lifecycle(
    operations: &mut impl WatchdogLifecycleOperations,
) -> Result<(), String> {
    for operation in WATCHDOG_LIFECYCLE_EVENTS {
        if let Err(error) = operations.execute(operation) {
            operations.recover()?;
            return Err(error);
        }
    }
    Ok(())
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct WatchdogLifecycleReport {
    version: u32,
    events: Vec<String>,
}

struct ReportedWatchdogLifecycle {
    events: std::vec::IntoIter<String>,
}

impl WatchdogLifecycleOperations for ReportedWatchdogLifecycle {
    fn execute(&mut self, operation: &'static str) -> Result<(), String> {
        if operation == "providerAdmission" && self.events.len() == 0 {
            return Ok(());
        }
        match self.events.next() {
            Some(event) if event == operation => Ok(()),
            Some(event) => Err(format!(
                "freeze watchdog lifecycle expected '{operation}', received '{event}'"
            )),
            None => Err(format!(
                "freeze watchdog lifecycle ended before '{operation}'"
            )),
        }
    }

    fn recover(&mut self) -> Result<(), String> {
        Ok(())
    }
}

fn validate_watchdog_lifecycle_report(stdout: &[u8]) -> Result<(), String> {
    let report: WatchdogLifecycleReport = serde_json::from_slice(stdout)
        .map_err(|error| format!("freeze watchdog lifecycle report is invalid: {error}"))?;
    if report.version != 1 {
        return Err("freeze watchdog lifecycle report version is unsupported".into());
    }
    let mut operations = ReportedWatchdogLifecycle {
        events: report.events.into_iter(),
    };
    execute_watchdog_lifecycle(&mut operations)?;
    if operations.events.next().is_some() {
        return Err("freeze watchdog lifecycle report has duplicate operations".into());
    }
    Ok(())
}

fn verify_process_containment(
    panes: &[crate::tmux::PaneInfo],
    target: &CapturedWorktreeIdentity,
    operation_id: &str,
    request_path: &str,
) -> Result<(), String> {
    let target_json = serde_json::to_string(target)
        .map_err(|error| format!("worktree containment identity failed: {error}"))?;
    for pane in panes {
        if path_within(&pane.cwd, &target.path) {
            return Err(format!(
                "Cargo cleanup refuses live session '{}' in the target worktree",
                pane.session
            ));
        }
        let mut command = containment_command();
        command.args([
            "/usr/bin/python3",
            "-c",
            ATOMIC_CONTAINMENT_INSPECTION_SCRIPT,
            &pane.pid.to_string(),
            &target_json,
            operation_id,
            request_path,
            CONTAINMENT_FREEZE_WATCHDOG_SCRIPT,
        ]);
        let output = match crate::bounded_exec::output_with_timeout_and_limit(
            command,
            Duration::from_secs(15),
            CONTAINMENT_EVIDENCE_LIMIT,
        ) {
            Ok(output) => output,
            Err(error) => {
                return Err(format!(
                    "Cargo cleanup could not verify session '{}' containment: {error}",
                    pane.session
                ));
            }
        };
        if !output.status.success() || !output.stderr.is_empty() {
            return Err(format!(
                "Cargo cleanup refuses uncontained session '{}': {}",
                pane.session,
                String::from_utf8_lossy(&output.stderr).trim()
            ));
        }
        validate_watchdog_lifecycle_report(&output.stdout).map_err(|error| {
            format!(
                "Cargo cleanup refuses session '{}' watchdog evidence: {error}",
                pane.session
            )
        })?;
    }
    Ok(())
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

fn run_provider(request_path: &str) -> Result<std::process::Output, String> {
    let configured = configured_provider_command()?;
    let mut command = Command::new(&configured[0]);
    command.args(&configured[1..]);
    command.args([
        "retirement-clean",
        "--request",
        request_path,
        "--apply",
        "--confirm",
        "--json",
    ]);
    crate::bounded_exec::output_with_timeout_and_limit(
        command,
        provider_timeout(),
        PROVIDER_OUTPUT_LIMIT,
    )
    .map_err(|error| format!("rust-storage retirement-clean could not complete: {error}"))
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
    use std::os::windows::fs::MetadataExt;

    if request_path.trim() != request_path
        || request_path.contains('\0')
        || (request_path.contains('/') && request_path.contains('\\'))
        || !Path::new(request_path).is_absolute()
        || Path::new(request_path).components().any(|component| {
            matches!(
                component,
                std::path::Component::CurDir | std::path::Component::ParentDir
            )
        })
    {
        return Err("provider request path has ambiguous native spelling".into());
    }
    let canonical_native = std::fs::canonicalize(request_path)
        .map_err(|error| format!("could not canonicalize provider request path: {error}"))?;
    let canonical_native = canonical_native
        .to_str()
        .ok_or_else(|| "provider request path is not valid UTF-8".to_string())?;
    let path = run_wslpath(&["-a", "-u", request_path])?;
    validate_posix_provider_request_path(&path)?;
    let canonical_wsl = run_wsl_readlink(&path)?;
    if canonical_wsl != path {
        return Err("provider request path changes identity inside WSL".into());
    }
    let round_trip = run_wslpath(&["-a", "-w", &path])?;
    let round_trip = std::fs::canonicalize(&round_trip)
        .map_err(|error| format!("could not validate provider request round trip: {error}"))?;
    let original = std::fs::metadata(canonical_native)
        .map_err(|error| format!("could not inspect provider request identity: {error}"))?;
    let translated = std::fs::metadata(&round_trip)
        .map_err(|error| format!("could not inspect translated request identity: {error}"))?;
    if original.volume_serial_number() != translated.volume_serial_number()
        || original.file_index() != translated.file_index()
    {
        return Err("provider request path round trip changed file identity".into());
    }
    Ok(path)
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
fn run_wslpath(arguments: &[&str]) -> Result<String, String> {
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
    let output = crate::bounded_exec::output_with_timeout_and_limit(
        command,
        crate::bounded_exec::WSL_PROBE_TIMEOUT,
        4096,
    )
    .map_err(|error| format!("could not translate provider request path into WSL: {error}"))?;
    if !output.status.success() {
        return Err(format!(
            "could not translate provider request path into WSL: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    let path = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if path.is_empty() || path.contains('\0') || path.contains('\n') {
        return Err("wslpath returned an invalid provider request path".into());
    }
    Ok(path)
}

#[cfg(windows)]
fn run_wsl_readlink(path: &str) -> Result<String, String> {
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
    let output = crate::bounded_exec::output_with_timeout_and_limit(
        command,
        crate::bounded_exec::WSL_PROBE_TIMEOUT,
        4096,
    )
    .map_err(|error| format!("could not canonicalize provider request path in WSL: {error}"))?;
    if !output.status.success() || !output.stderr.is_empty() {
        return Err(format!(
            "could not canonicalize provider request path in WSL: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    let path = String::from_utf8_lossy(&output.stdout).trim().to_string();
    validate_posix_provider_request_path(&path)?;
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
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    std::fs::create_dir_all(parent)?;
    let body = serde_json::to_vec_pretty(value)?;
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
        file.write_all(&body)?;
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
    fn missing_reserved_request_requires_recovery() {
        let record = WorktreeCoordinator::ephemeral()
            .begin_retirement("/repo/worktree", "/missing/request.json")
            .unwrap();
        assert!(matches!(
            missing_request_completion(&record),
            ProviderCompletion::RecoveryRequired(_)
        ));
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
        let operation_id = {
            let coordinator = WorktreeCoordinator::load(store.clone()).unwrap();
            let record = coordinator
                .begin_retirement("/repo/worktree", "/requests/one.json")
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
        let error = verify_process_containment(
            &[pane],
            admission.blockers.first().expect("one exact blocker"),
            "0123456789abcdef0123456789abcdef",
            "/tmp/test-request.json",
        )
        .unwrap_err();
        assert!(error.contains("exact managed cgroup-v2 freezer ownership is unavailable"));
        crate::tmux::kill_session_tree(&session).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn cleanup_review_managed_runtime_uses_supervised_freeze_watchdog() {
        use std::os::unix::fs::MetadataExt;
        let directory = tempfile::tempdir().unwrap();
        let worktree = directory.path().join("worktree");
        let unrelated = directory.path().join("unrelated");
        let request_path = directory.path().join("request.json");
        std::fs::create_dir(&worktree).unwrap();
        std::fs::create_dir(&unrelated).unwrap();
        let stat = std::fs::metadata(&worktree).unwrap();
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
        let session = format!(
            "th_test_managed_containment_{}",
            &uuid::Uuid::new_v4().simple().to_string()[..8]
        );
        let launch = crate::tmux::prepare_managed_runtime_launch().unwrap();
        let owner = crate::tmux::new_prepared_managed_session_with_env(
            &session,
            unrelated.to_str().unwrap(),
            contained.as_deref(),
            &env,
            &launch,
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
        let verification = verify_process_containment(
            &[pane],
            admission.blockers.first().expect("one exact blocker"),
            &record.operation_id,
            request_path.to_str().unwrap(),
        );
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
        assert!(verify_process_containment(
            &[pane],
            &target,
            "0123456789abcdef0123456789abcdef",
            "/tmp/test-request.json"
        )
        .unwrap_err()
        .contains("uncontained"));
    }

    #[test]
    fn cleanup_review_watchdog_scripts_are_executable() {
        for script in [
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

    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    enum TestLeaseState {
        Prepared,
        Armed,
        Thawed,
    }

    #[derive(Debug)]
    struct InjectedWatchdogOperations {
        fail_at: Option<&'static str>,
        operations: Vec<&'static str>,
        exact_frozen: bool,
        unrelated_frozen: bool,
        changed: bool,
        lease: Option<TestLeaseState>,
        provider_admissions: usize,
        terminal_preserved: bool,
        agent_preserved: bool,
        source_preserved: bool,
    }

    impl WatchdogLifecycleOperations for InjectedWatchdogOperations {
        fn execute(&mut self, operation: &'static str) -> Result<(), String> {
            self.operations.push(operation);
            if self.fail_at == Some(operation) {
                return Err(format!("injected failure at {operation}"));
            }
            match operation {
                "unitCreated" => self.lease = Some(TestLeaseState::Prepared),
                "armSent" => self.lease = Some(TestLeaseState::Armed),
                "frozen" => {
                    self.exact_frozen = true;
                    self.changed = true;
                }
                "thawed" => {
                    self.exact_frozen = false;
                    self.changed = false;
                    self.lease = Some(TestLeaseState::Thawed);
                }
                "disarmed" => self.lease = None,
                "providerAdmission" => self.provider_admissions += 1,
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
    }

    fn injected_watchdog(fail_at: Option<&'static str>) -> InjectedWatchdogOperations {
        InjectedWatchdogOperations {
            fail_at,
            operations: Vec::new(),
            exact_frozen: false,
            unrelated_frozen: false,
            changed: false,
            lease: None,
            provider_admissions: 0,
            terminal_preserved: true,
            agent_preserved: true,
            source_preserved: true,
        }
    }

    #[test]
    fn cleanup_review_production_watchdog_state_machine_injects_every_operation() {
        let failures = [
            ("setup", "unitCreated"),
            ("missing-ready", "preparedReady"),
            ("malformed-ready", "preparedReady"),
            ("parent-before-freeze", "armSent"),
            ("parent-after-freeze", "containmentVerified"),
            ("wrapper-only-death", "containmentVerified"),
            ("watchdog-crash", "containmentVerified"),
            ("deadline", "containmentVerified"),
            ("inode-replacement", "thawed"),
            ("competing-owner", "preparedReady"),
            ("stale-disarm", "disarmed"),
            ("mismatched-disarm", "disarmed"),
            ("restart-recovery", "containmentVerified"),
        ];
        for (name, operation) in failures {
            let mut state = injected_watchdog(Some(operation));
            assert!(execute_watchdog_lifecycle(&mut state).is_err(), "{name}");
            assert!(!state.exact_frozen, "{name}");
            assert!(!state.unrelated_frozen, "{name}");
            assert_eq!(state.lease, None, "{name}");
            assert_eq!(state.provider_admissions, 0, "{name}");
            assert!(state.terminal_preserved, "{name}");
            assert!(state.agent_preserved, "{name}");
            assert!(state.source_preserved, "{name}");
        }

        let mut pre_frozen = injected_watchdog(Some("unitCreated"));
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

        let duplicate = serde_json::json!({
            "version": 1,
            "events": [
                "unitCreated", "preparedReady", "armSent", "frozen",
                "containmentVerified", "thawed", "disarmed", "disarmed"
            ]
        });
        assert!(
            validate_watchdog_lifecycle_report(&serde_json::to_vec(&duplicate).unwrap()).is_err()
        );
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
