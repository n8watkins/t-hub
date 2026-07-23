#!/usr/bin/env python3
"""Crash-recoverable Linux config replacement primitives.

The exchange operation is deliberately Linux-only.  It writes and fsyncs a
restricted intent journal before invoking renameat2(RENAME_EXCHANGE), and its
recover operation resolves every durable phase without guessing ownership.
"""

import argparse
import base64
import ctypes
import errno
import hashlib
import json
import os
import selectors
import signal
import stat
import subprocess
import sys
import tempfile
import time
from typing import Any, Dict, Optional


AT_FDCWD = -100
RENAME_EXCHANGE = 2
LIBC = ctypes.CDLL(None, use_errno=True)
VERSION = 1
CATALOG_OUTPUT_LIMIT = 128 * 1024
CATALOG_TIMEOUT_SECONDS = 5.0


class AtomicError(RuntimeError):
    """A fail-closed atomic operation error."""


def fsync_directory(path: str) -> None:
    descriptor = os.open(path, os.O_RDONLY | os.O_DIRECTORY | os.O_CLOEXEC)
    try:
        os.fsync(descriptor)
    finally:
        os.close(descriptor)


def fsync_file(path: str) -> None:
    descriptor = os.open(path, os.O_RDONLY | os.O_CLOEXEC | os.O_NOFOLLOW)
    try:
        os.fsync(descriptor)
    finally:
        os.close(descriptor)


def rename_exchange(left: str, right: str) -> None:
    renameat2 = getattr(LIBC, "renameat2", None)
    if renameat2 is None:
        raise AtomicError("Linux renameat2 is unavailable")
    result = renameat2(
        AT_FDCWD,
        os.fsencode(left),
        AT_FDCWD,
        os.fsencode(right),
        RENAME_EXCHANGE,
    )
    if result != 0:
        error = ctypes.get_errno()
        if error in (errno.ENOSYS, errno.EINVAL, errno.EOPNOTSUPP):
            raise AtomicError("filesystem does not support atomic rename exchange")
        raise OSError(error, os.strerror(error))


def require_regular(path: str) -> os.stat_result:
    metadata = os.lstat(path)
    if not stat.S_ISREG(metadata.st_mode):
        raise AtomicError(f"refusing non-regular path: {path}")
    return metadata


def require_single_link(path: str) -> os.stat_result:
    metadata = require_regular(path)
    if metadata.st_nlink != 1:
        raise AtomicError(f"refusing hard-linked path: {path}")
    return metadata


def identity(path: str) -> Dict[str, int]:
    metadata = os.lstat(path)
    return {"device": metadata.st_dev, "inode": metadata.st_ino}


def xattrs(path: str) -> Dict[str, str]:
    result: Dict[str, str] = {}
    for name in sorted(os.listxattr(path, follow_symlinks=False)):
        result[name] = hashlib.sha256(
            os.getxattr(path, name, follow_symlinks=False)
        ).hexdigest()
    return result


def content_sha256(path: str) -> str:
    digest = hashlib.sha256()
    descriptor = os.open(path, os.O_RDONLY | os.O_CLOEXEC | os.O_NOFOLLOW)
    try:
        with os.fdopen(descriptor, "rb") as source:
            for chunk in iter(lambda: source.read(1024 * 1024), b""):
                digest.update(chunk)
    except BaseException:
        try:
            os.close(descriptor)
        except OSError:
            pass
        raise
    return digest.hexdigest()


def snapshot_executable(source: str, destination: str) -> Dict[str, Any]:
    """Copy a verified executable FD into a new private snapshot."""
    source = os.path.abspath(source)
    destination = os.path.abspath(destination)
    destination_directory = restricted_directory(os.path.dirname(destination), False)
    source_descriptor = -1
    destination_descriptor = -1
    destination_created = False
    try:
        source_descriptor = os.open(
            source,
            os.O_RDONLY | os.O_CLOEXEC | os.O_NOFOLLOW | os.O_NONBLOCK,
        )
        source_before = os.fstat(source_descriptor)
        source_mode = stat.S_IMODE(source_before.st_mode)
        if not stat.S_ISREG(source_before.st_mode):
            raise AtomicError(f"refusing non-regular executable: {source}")
        if source_before.st_uid != os.geteuid():
            raise AtomicError(f"executable must be owned by the current user: {source}")
        if not source_mode & stat.S_IXUSR:
            raise AtomicError(f"executable must have its owner execute bit set: {source}")
        if source_mode & 0o022:
            raise AtomicError(f"executable must not be writable by group or others: {source}")
        if source_mode & 0o7000:
            raise AtomicError(f"executable must not have special mode bits set: {source}")

        destination_descriptor = os.open(
            destination,
            os.O_WRONLY
            | os.O_CREAT
            | os.O_EXCL
            | os.O_CLOEXEC
            | os.O_NOFOLLOW,
            0o700,
        )
        destination_created = True
        os.fchmod(destination_descriptor, 0o700)
        digest = hashlib.sha256()
        size = 0
        while True:
            chunk = os.read(source_descriptor, 1024 * 1024)
            if not chunk:
                break
            digest.update(chunk)
            size += len(chunk)
            offset = 0
            while offset < len(chunk):
                offset += os.write(destination_descriptor, chunk[offset:])
        os.fsync(destination_descriptor)

        source_after = os.fstat(source_descriptor)
        stable_fields = (
            "st_dev",
            "st_ino",
            "st_mode",
            "st_uid",
            "st_gid",
            "st_size",
            "st_mtime_ns",
            "st_ctime_ns",
        )
        if any(
            getattr(source_before, field) != getattr(source_after, field)
            for field in stable_fields
        ):
            raise AtomicError(f"executable changed while it was read: {source}")
        if size != source_before.st_size:
            raise AtomicError(f"executable size changed while it was read: {source}")

        path_metadata = os.lstat(source)
        if (
            not stat.S_ISREG(path_metadata.st_mode)
            or path_metadata.st_dev != source_before.st_dev
            or path_metadata.st_ino != source_before.st_ino
        ):
            raise AtomicError(f"executable path changed while it was read: {source}")

        destination_metadata = os.fstat(destination_descriptor)
        snapshot_digest = digest.hexdigest()
        fsync_directory(destination_directory)
        return {
            "source": {
                "path": source,
                "device": source_before.st_dev,
                "inode": source_before.st_ino,
                "uid": source_before.st_uid,
                "gid": source_before.st_gid,
                "mode": source_mode,
                "size": size,
                "mtime_ns": source_before.st_mtime_ns,
                "ctime_ns": source_before.st_ctime_ns,
                "content_sha256": snapshot_digest,
            },
            "snapshot": {
                "path": destination,
                "device": destination_metadata.st_dev,
                "inode": destination_metadata.st_ino,
                "mode": stat.S_IMODE(destination_metadata.st_mode),
                "size": destination_metadata.st_size,
                "content_sha256": snapshot_digest,
            },
        }
    except BaseException:
        if destination_descriptor >= 0:
            os.close(destination_descriptor)
            destination_descriptor = -1
        if destination_created:
            try:
                os.unlink(destination)
                fsync_directory(destination_directory)
            except FileNotFoundError:
                pass
        raise
    finally:
        if destination_descriptor >= 0:
            os.close(destination_descriptor)
        if source_descriptor >= 0:
            os.close(source_descriptor)


def verify_executable(source: str, expected: Dict[str, Any]) -> Dict[str, Any]:
    """Verify a mutable executable path through one bounded, no-follow FD."""
    source = os.path.abspath(source)
    descriptor = os.open(
        source,
        os.O_RDONLY | os.O_CLOEXEC | os.O_NOFOLLOW | os.O_NONBLOCK,
    )
    try:
        before = os.fstat(descriptor)
        mode = stat.S_IMODE(before.st_mode)
        if not stat.S_ISREG(before.st_mode):
            raise AtomicError(f"refusing non-regular executable: {source}")
        if before.st_uid != os.geteuid():
            raise AtomicError(f"executable must be owned by the current user: {source}")
        if not mode & stat.S_IXUSR or mode & 0o7022:
            raise AtomicError(f"executable mode is unsafe: {source}")
        digest = hashlib.sha256()
        size = 0
        while True:
            chunk = os.read(descriptor, 1024 * 1024)
            if not chunk:
                break
            digest.update(chunk)
            size += len(chunk)
        after = os.fstat(descriptor)
        observed = {
            "device": before.st_dev,
            "inode": before.st_ino,
            "uid": before.st_uid,
            "gid": before.st_gid,
            "mode": mode,
            "size": size,
            "mtime_ns": before.st_mtime_ns,
            "ctime_ns": before.st_ctime_ns,
            "content_sha256": digest.hexdigest(),
        }
        stable_fields = (
            "st_dev",
            "st_ino",
            "st_mode",
            "st_uid",
            "st_gid",
            "st_size",
            "st_mtime_ns",
            "st_ctime_ns",
        )
        if any(getattr(before, field) != getattr(after, field) for field in stable_fields):
            raise AtomicError(f"executable changed while it was verified: {source}")
        path_metadata = os.lstat(source)
        if (
            not stat.S_ISREG(path_metadata.st_mode)
            or path_metadata.st_dev != before.st_dev
            or path_metadata.st_ino != before.st_ino
        ):
            raise AtomicError(f"executable path changed while it was verified: {source}")
        if observed != expected:
            raise AtomicError(f"executable no longer matches selected identity: {source}")
        return {"path": source, **observed}
    finally:
        os.close(descriptor)


def verify_cortana_catalog(executable: str) -> None:
    """Run a pinned executable FD with bounded time and output."""
    executable = os.path.abspath(executable)
    descriptor = os.open(
        executable,
        os.O_RDONLY | os.O_CLOEXEC | os.O_NOFOLLOW | os.O_NONBLOCK,
    )
    process: Optional[subprocess.Popen[bytes]] = None
    selector = selectors.DefaultSelector()
    try:
        metadata = os.fstat(descriptor)
        mode = stat.S_IMODE(metadata.st_mode)
        if (
            not stat.S_ISREG(metadata.st_mode)
            or metadata.st_uid != os.geteuid()
            or not mode & stat.S_IXUSR
            or mode & 0o7022
        ):
            raise AtomicError("catalog executable is not a trusted private snapshot")
        process = subprocess.Popen(
            [f"/proc/self/fd/{descriptor}", "--list-tools"],
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            pass_fds=(descriptor,),
            start_new_session=True,
        )
        if process.stdout is None or process.stderr is None:
            raise AtomicError("catalog executable output pipes are unavailable")
        os.set_blocking(process.stdout.fileno(), False)
        os.set_blocking(process.stderr.fileno(), False)
        selector.register(process.stdout, selectors.EVENT_READ, "stdout")
        selector.register(process.stderr, selectors.EVENT_READ, "stderr")
        deadline = time.monotonic() + CATALOG_TIMEOUT_SECONDS
        output = bytearray()
        total_output = 0
        while selector.get_map():
            remaining = deadline - time.monotonic()
            if remaining <= 0:
                raise AtomicError("catalog executable exceeded its time limit")
            events = selector.select(remaining)
            if not events:
                raise AtomicError("catalog executable exceeded its time limit")
            for key, _ in events:
                chunk = os.read(key.fileobj.fileno(), 64 * 1024)
                if not chunk:
                    selector.unregister(key.fileobj)
                    continue
                total_output += len(chunk)
                if total_output > CATALOG_OUTPUT_LIMIT:
                    raise AtomicError("catalog executable exceeded its output limit")
                if key.data == "stdout":
                    output.extend(chunk)
        remaining = deadline - time.monotonic()
        if remaining <= 0:
            raise AtomicError("catalog executable exceeded its time limit")
        try:
            return_code = process.wait(timeout=remaining)
        except subprocess.TimeoutExpired as error:
            raise AtomicError("catalog executable exceeded its time limit") from error
        if return_code != 0:
            raise AtomicError("catalog executable failed")
        try:
            catalog = json.loads(output)
        except (UnicodeDecodeError, json.JSONDecodeError) as error:
            raise AtomicError("catalog executable returned invalid JSON") from error
        tools = catalog.get("tools") if isinstance(catalog, dict) else catalog
        if not isinstance(tools, list):
            raise AtomicError("catalog executable returned an invalid tool list")
        matches = [
            tool
            for tool in tools
            if isinstance(tool, dict) and tool.get("name") == "cortana_bootstrap"
        ]
        expected_schema = {
            "type": "object",
            "properties": {},
            "additionalProperties": False,
        }
        expected_annotations = {
            "t-hubTier": "read",
            "confirmationRequired": False,
            "readOnlyHint": True,
            "destructiveHint": False,
            "idempotentHint": True,
            "openWorldHint": False,
        }
        if (
            len(matches) != 1
            or matches[0].get("inputSchema") != expected_schema
            or matches[0].get("annotations") != expected_annotations
        ):
            raise AtomicError("catalog executable lacks the exact cortana_bootstrap contract")
    finally:
        selector.close()
        if process is not None and process.poll() is None:
            try:
                os.killpg(process.pid, signal.SIGKILL)
            except ProcessLookupError:
                pass
            process.wait()
        if process is not None and process.stdout is not None:
            process.stdout.close()
        if process is not None and process.stderr is not None:
            process.stderr.close()
        os.close(descriptor)


def description(path: str) -> Dict[str, Any]:
    metadata = require_regular(path)
    return {
        "content_sha256": content_sha256(path),
        "uid": metadata.st_uid,
        "gid": metadata.st_gid,
        "mode": stat.S_IMODE(metadata.st_mode),
        "xattrs": xattrs(path),
    }


def description_digest(value: Dict[str, Any]) -> str:
    encoded = json.dumps(value, sort_keys=True, separators=(",", ":")).encode("utf-8")
    return hashlib.sha256(encoded).hexdigest()


def state_digest(path: str) -> str:
    return description_digest(description(path))


def copy_metadata(source: str, destination: str, metadata: os.stat_result) -> None:
    # chown can clear mode bits, so ownership is established before chmod.
    os.chown(destination, metadata.st_uid, metadata.st_gid, follow_symlinks=False)
    os.chmod(destination, stat.S_IMODE(metadata.st_mode), follow_symlinks=False)
    source_names = set(os.listxattr(source, follow_symlinks=False))
    for name in set(os.listxattr(destination, follow_symlinks=False)) - source_names:
        os.removexattr(destination, name, follow_symlinks=False)
    for name in source_names:
        os.setxattr(
            destination,
            name,
            os.getxattr(source, name, follow_symlinks=False),
            follow_symlinks=False,
        )


def restricted_directory(path: str, create: bool) -> str:
    path = os.path.abspath(path)
    if create:
        try:
            os.mkdir(path, 0o700)
            fsync_directory(os.path.dirname(path))
        except FileExistsError:
            pass
    metadata = os.lstat(path)
    if not stat.S_ISDIR(metadata.st_mode) or stat.S_ISLNK(metadata.st_mode):
        raise AtomicError(f"refusing non-directory journal: {path}")
    if metadata.st_uid != os.geteuid() or stat.S_IMODE(metadata.st_mode) != 0o700:
        raise AtomicError(f"journal must be owned by the current user with mode 0700: {path}")
    return path


def write_json(path: str, value: Dict[str, Any]) -> None:
    directory = os.path.dirname(path)
    descriptor, temporary = tempfile.mkstemp(prefix=".intent.", dir=directory)
    try:
        os.fchmod(descriptor, 0o600)
        with os.fdopen(descriptor, "wb") as output:
            output.write(json.dumps(value, sort_keys=True, separators=(",", ":")).encode("utf-8"))
            output.write(b"\n")
            output.flush()
            os.fsync(output.fileno())
        os.replace(temporary, path)
        fsync_directory(directory)
    except BaseException:
        try:
            os.unlink(temporary)
        except FileNotFoundError:
            pass
        raise


def read_intent(journal: str) -> Dict[str, Any]:
    journal = restricted_directory(journal, False)
    path = os.path.join(journal, "intent.json")
    metadata = require_regular(path)
    if metadata.st_uid != os.geteuid() or stat.S_IMODE(metadata.st_mode) != 0o600:
        raise AtomicError("intent must be owned by the current user with mode 0600")
    with open(path, "r", encoding="utf-8") as source:
        value = json.load(source)
    required = {
        "version", "operation", "target", "candidate", "expected",
        "desired", "phase", "recovery",
    }
    if set(value) != required or value["version"] != VERSION or value["operation"] not in {
        "exchange", "create", "delete"
    }:
        raise AtomicError("invalid atomic intent")
    if value["phase"] not in {
        "prepared", "exchanged", "mismatch", "restored", "verified", "committed", "cleanup"
    }:
        raise AtomicError("invalid exchange phase")
    return value


def set_phase(journal: str, intent: Dict[str, Any], phase: str) -> None:
    intent["phase"] = phase
    write_json(os.path.join(journal, "intent.json"), intent)


def crash(point: str) -> None:
    if os.environ.get("T_HUB_ATOMIC_CRASH_AT") == point:
        once_path = os.environ.get("T_HUB_ATOMIC_CRASH_ONCE_FILE")
        if once_path:
            try:
                descriptor = os.open(
                    once_path,
                    os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_CLOEXEC,
                    0o600,
                )
            except FileExistsError:
                return
            os.fsync(descriptor)
            os.close(descriptor)
            fsync_directory(os.path.dirname(os.path.abspath(once_path)))
        os._exit(89)


def remove_journal(journal: str) -> None:
    journal = restricted_directory(journal, False)
    for name in os.listdir(journal):
        path = os.path.join(journal, name)
        require_regular(path)
        # Truncation avoids retaining secret-bearing recovery descriptions in a
        # reusable inode.  Filesystem snapshots remain outside our guarantees.
        descriptor = os.open(path, os.O_WRONLY | os.O_CLOEXEC | os.O_NOFOLLOW)
        try:
            os.ftruncate(descriptor, 0)
            os.fsync(descriptor)
        finally:
            os.close(descriptor)
        os.unlink(path)
    fsync_directory(journal)
    os.rmdir(journal)
    fsync_directory(os.path.dirname(journal))


def inspect_digest(path: str) -> Optional[str]:
    try:
        return state_digest(path)
    except FileNotFoundError:
        return None


def durable_exchange(target: str, candidate: str) -> None:
    rename_exchange(target, candidate)
    fsync_file(target)
    fsync_file(candidate)
    fsync_directory(os.path.dirname(target))


def recover_exchange(journal: str, cleanup: bool = True) -> str:
    journal = os.path.abspath(journal)
    intent = read_intent(journal)
    if intent["operation"] == "create":
        return recover_create(journal, intent, cleanup)
    if intent["operation"] == "delete":
        return recover_delete(journal, intent, cleanup)
    target = intent["target"]
    candidate = intent["candidate"]
    if os.path.dirname(target) != os.path.dirname(candidate):
        raise AtomicError("journal paths do not share a directory")
    expected = intent["expected"]["digest"]
    desired = intent["desired"]["digest"]
    target_digest = inspect_digest(target)
    candidate_digest = inspect_digest(candidate)
    phase = intent["phase"]
    if phase == "prepared":
        live_identities = (identity(target), identity(candidate))
        original_identities = (
            intent["recovery"].get("target_identity"),
            intent["recovery"].get("candidate_identity"),
        )
        swapped_identities = (original_identities[1], original_identities[0])
        if live_identities != original_identities and live_identities != swapped_identities:
            raise AtomicError("prepared exchange path identity changed")

    if phase in {"verified", "committed"}:
        if target_digest != desired:
            raise AtomicError("committed target no longer matches the journal")
        if phase == "verified":
            set_phase(journal, intent, "committed")
        if cleanup:
            remove_journal(journal)
        return "committed"

    if phase == "restored":
        if target_digest is None:
            raise AtomicError("restored target is missing")
        if cleanup:
            remove_journal(journal)
        return "restored"

    # A crash may occur after rename exchange but before the phase write.  The
    # two exact fingerprints make that state distinguishable from prepared.
    if target_digest == desired and candidate_digest == expected:
        if phase == "mismatch":
            raise AtomicError("mismatch phase contradicts exact exchanged state")
        set_phase(journal, intent, "exchanged")
        set_phase(journal, intent, "verified")
        set_phase(journal, intent, "committed")
        if cleanup:
            remove_journal(journal)
        return "committed"

    if target_digest == expected and candidate_digest == desired:
        # Nothing was exchanged, or an interrupted mismatch restoration already
        # completed.  Both resolve to the exact prestate.
        set_phase(journal, intent, "restored")
        if cleanup:
            remove_journal(journal)
        return "restored"

    if phase == "mismatch" and candidate_digest == desired and target_digest is not None:
        # The restore rename completed before its phase record.  The target is
        # intentionally allowed to be a concurrent prestate unknown at prepare.
        set_phase(journal, intent, "restored")
        if cleanup:
            remove_journal(journal)
        return "restored"

    if target_digest == desired and candidate_digest is not None:
        # The exchange displaced a concurrently changed prestate.  Put those
        # exact bytes and metadata back; never reconstruct them from a snapshot.
        if phase != "mismatch":
            set_phase(journal, intent, "mismatch")
        crash("mismatch-before-restore")
        durable_exchange(target, candidate)
        crash("restored-before-phase")
        set_phase(journal, intent, "restored")
        if cleanup:
            remove_journal(journal)
        return "restored"

    raise AtomicError("live paths do not match a recoverable journal state")


def recover_create(journal: str, intent: Dict[str, Any], cleanup: bool) -> str:
    target = intent["target"]
    candidate = intent["candidate"]
    desired = intent["desired"]["digest"]
    target_digest = inspect_digest(target)
    candidate_digest = inspect_digest(candidate)
    if intent["phase"] == "prepared" and target_digest is None and (
        candidate_digest is None
        or identity(candidate) != intent["recovery"].get("candidate_identity")
    ):
        raise AtomicError("prepared create path identity changed")
    if target_digest == desired and candidate_digest is None:
        if intent["phase"] != "committed":
            set_phase(journal, intent, "committed")
        if cleanup:
            remove_journal(journal)
        return "committed"
    if target_digest is None and candidate_digest == desired:
        set_phase(journal, intent, "restored")
        if cleanup:
            remove_journal(journal)
        return "restored"
    raise AtomicError("live paths do not match a recoverable create journal")


def cleanup_deleted_recovery(intent: Dict[str, Any], recovery: str, expected: str) -> None:
    if intent["recovery"].get("cleanup") == "unlink":
        release(recovery, expected, intent["recovery"]["target_identity"])
    else:
        discard(recovery)


def restore_delete_mismatch(
    journal: str,
    intent: Dict[str, Any],
    cleanup: bool,
) -> str:
    target = intent["target"]
    recovery = intent["candidate"]
    if os.path.lexists(target):
        raise AtomicError("delete mismatch target was recreated concurrently")
    require_single_link(recovery)
    if identity(recovery) != intent["recovery"].get("target_identity"):
        raise AtomicError("delete mismatch recovery identity changed")
    if intent["phase"] != "mismatch":
        set_phase(journal, intent, "mismatch")
    crash("mismatch-before-restore")
    os.rename(recovery, target)
    fsync_file(target)
    fsync_directory(os.path.dirname(target))
    crash("restored-before-phase")
    set_phase(journal, intent, "restored")
    if cleanup:
        remove_journal(journal)
    return "restored"


def recover_delete(journal: str, intent: Dict[str, Any], cleanup: bool) -> str:
    target = intent["target"]
    recovery = intent["candidate"]
    expected = intent["expected"]["digest"]
    target_digest = inspect_digest(target)
    recovery_digest = inspect_digest(recovery)
    if intent["phase"] == "prepared" and recovery_digest is None and (
        target_digest is None
        or identity(target) != intent["recovery"].get("target_identity")
    ):
        raise AtomicError("prepared delete path identity changed")
    if target_digest is None and recovery_digest == expected:
        if intent["phase"] not in {"committed", "cleanup"}:
            set_phase(journal, intent, "committed")
        if cleanup:
            set_phase(journal, intent, "cleanup")
            if state_digest(recovery) != expected:
                return restore_delete_mismatch(journal, intent, cleanup)
            cleanup_deleted_recovery(intent, recovery, expected)
            remove_journal(journal)
        return "committed"
    if target_digest is None and recovery_digest is None \
        and intent["phase"] in {"committed", "cleanup"}:
        if cleanup:
            remove_journal(journal)
        return "committed"
    if target_digest is None and intent["phase"] in {"committed", "cleanup"} \
        and intent["recovery"].get("cleanup") != "unlink" \
        and identity(recovery) == intent["recovery"].get("target_identity") \
        and os.lstat(recovery).st_size == 0:
        if cleanup:
            discard(recovery)
            remove_journal(journal)
        return "committed"
    if target_digest is None and recovery_digest is not None:
        return restore_delete_mismatch(journal, intent, cleanup)
    if target_digest == expected and recovery_digest is None:
        set_phase(journal, intent, "restored")
        if cleanup:
            remove_journal(journal)
        return "restored"
    if recovery_digest is None and target_digest is not None \
        and intent["phase"] in {"mismatch", "restored"} \
        and identity(target) == intent["recovery"].get("target_identity"):
        if intent["phase"] != "restored":
            set_phase(journal, intent, "restored")
        if cleanup:
            remove_journal(journal)
        return "restored"
    raise AtomicError("live paths do not match a recoverable delete journal")


def create(target: str, candidate: str, journal: str) -> None:
    target = os.path.abspath(target)
    candidate = os.path.abspath(candidate)
    journal = os.path.abspath(journal)
    if os.path.dirname(target) != os.path.dirname(candidate):
        raise AtomicError("target and candidate must share a directory")
    if os.path.lexists(target):
        raise AtomicError("create target is not absent")
    require_single_link(candidate)
    candidate_identity = identity(candidate)
    desired_description = description(candidate)
    desired_digest = description_digest(desired_description)
    fsync_file(candidate)
    fsync_directory(os.path.dirname(target))
    journal = restricted_directory(journal, True)
    if os.listdir(journal):
        raise AtomicError("journal is not empty; recover it first")
    intent = {
        "version": VERSION,
        "operation": "create",
        "target": target,
        "candidate": candidate,
        "expected": {"digest": "absent"},
        "desired": {"digest": desired_digest, "description": desired_description},
        "phase": "prepared",
        "recovery": {"prestate": "absent", "candidate_identity": candidate_identity},
    }
    write_json(os.path.join(journal, "intent.json"), intent)
    crash("prepared")
    if os.path.lexists(target) or identity(candidate) != candidate_identity:
        raise AtomicError("create path identity changed after prepare")
    os.rename(candidate, target)
    fsync_file(target)
    fsync_directory(os.path.dirname(target))
    crash("renamed-before-phase")
    set_phase(journal, intent, "verified")
    crash("verified")
    set_phase(journal, intent, "committed")
    crash("committed")
    remove_journal(journal)


def delete(target: str, expected: str, journal: str, unlink_only: bool = False) -> None:
    target = os.path.abspath(target)
    journal = os.path.abspath(journal)
    require_single_link(target)
    target_identity = identity(target)
    actual = state_digest(target)
    if actual != expected:
        raise AtomicError("delete target prestate changed")
    recovery = f"{target}.t-hub-delete.{os.getpid()}"
    if os.path.lexists(recovery):
        raise AtomicError("delete recovery path already exists")
    fsync_file(target)
    fsync_directory(os.path.dirname(target))
    journal = restricted_directory(journal, True)
    if os.listdir(journal):
        raise AtomicError("journal is not empty; recover it first")
    intent = {
        "version": VERSION,
        "operation": "delete",
        "target": target,
        "candidate": recovery,
        "expected": {"digest": expected},
        "desired": {"digest": "absent"},
        "phase": "prepared",
        "recovery": {
            "displaced_prestate": "candidate-after-rename",
            "target_identity": target_identity,
            "cleanup": "unlink" if unlink_only else "scrub",
        },
    }
    write_json(os.path.join(journal, "intent.json"), intent)
    crash("prepared")
    if identity(target) != target_identity or os.path.lexists(recovery):
        raise AtomicError("delete path identity changed after prepare")
    os.rename(target, recovery)
    fsync_file(recovery)
    fsync_directory(os.path.dirname(target))
    crash("renamed-before-phase")
    if state_digest(recovery) != expected:
        restore_delete_mismatch(journal, intent, True)
        raise AtomicError("delete displaced state changed; concurrent state restored")
    set_phase(journal, intent, "verified")
    crash("verified")
    set_phase(journal, intent, "committed")
    crash("committed")
    set_phase(journal, intent, "cleanup")
    crash("cleanup")
    if state_digest(recovery) != expected:
        restore_delete_mismatch(journal, intent, True)
        raise AtomicError("delete cleanup state changed; concurrent state restored")
    cleanup_deleted_recovery(intent, recovery, expected)
    crash("cleaned-before-journal")
    remove_journal(journal)


def exchange(
    target: str,
    candidate: str,
    expected: str,
    journal: str,
    preserve_candidate_metadata: bool = False,
) -> None:
    target = os.path.abspath(target)
    candidate = os.path.abspath(candidate)
    journal = os.path.abspath(journal)
    if os.name != "posix" or not sys.platform.startswith("linux"):
        raise AtomicError("atomic exchange is supported only on Linux/WSL")
    if os.path.dirname(target) != os.path.dirname(candidate):
        raise AtomicError("target and candidate must share a directory")
    if os.path.commonpath([journal, target]) == target:
        raise AtomicError("journal cannot be nested below the target")
    target_metadata = require_single_link(target)
    require_single_link(candidate)
    target_identity = identity(target)
    candidate_identity = identity(candidate)
    before = description(target)
    before_digest = description_digest(before)
    # Backward compatibility for callers that supplied the old content-only
    # hash.  New callers publish the complete state digest.
    if expected not in {before_digest, before["content_sha256"]}:
        raise AtomicError("target prestate changed before prepare")
    if not preserve_candidate_metadata:
        copy_metadata(target, candidate, target_metadata)
    fsync_file(candidate)
    desired_description = description(candidate)
    desired_digest = description_digest(desired_description)
    journal = restricted_directory(journal, True)
    if os.listdir(journal):
        raise AtomicError("journal is not empty; recover it first")
    intent = {
        "version": VERSION,
        "operation": "exchange",
        "target": target,
        "candidate": candidate,
        "expected": {"digest": before_digest, "description": before},
        "desired": {"digest": desired_digest, "description": desired_description},
        "phase": "prepared",
        "recovery": {
            "displaced_prestate": "candidate-after-exchange",
            "target_identity": target_identity,
            "candidate_identity": candidate_identity,
        },
    }
    write_json(os.path.join(journal, "intent.json"), intent)
    fsync_file(target)
    fsync_file(candidate)
    fsync_directory(os.path.dirname(target))
    crash("prepared")

    if identity(target) != target_identity or identity(candidate) != candidate_identity:
        raise AtomicError("path identity changed after prepare")

    durable_exchange(target, candidate)
    crash("exchanged-before-phase")
    displaced = state_digest(candidate)
    if displaced != before_digest:
        set_phase(journal, intent, "mismatch")
        crash("mismatch-before-restore")
        durable_exchange(target, candidate)
        crash("restored-before-phase")
        set_phase(journal, intent, "restored")
        remove_journal(journal)
        raise AtomicError("target prestate changed during exchange; concurrent state restored")
    set_phase(journal, intent, "exchanged")
    crash("exchanged")
    if state_digest(target) != desired_digest:
        set_phase(journal, intent, "mismatch")
        durable_exchange(target, candidate)
        set_phase(journal, intent, "restored")
        remove_journal(journal)
        raise AtomicError("replacement verification failed; prestate restored")
    set_phase(journal, intent, "verified")
    crash("verified")
    set_phase(journal, intent, "committed")
    crash("committed")
    remove_journal(journal)


def default_journal(candidate: str) -> str:
    return f"{os.path.abspath(candidate)}.journal"


def publish(path: str, value: str) -> None:
    path = os.path.abspath(path)
    directory = restricted_directory(os.path.dirname(path), False)
    descriptor, temporary = tempfile.mkstemp(prefix=f".{os.path.basename(path)}.", dir=directory)
    try:
        os.fchmod(descriptor, 0o600)
        with os.fdopen(descriptor, "wb") as output:
            output.write(value.encode("utf-8"))
            output.flush()
            os.fsync(output.fileno())
        os.replace(temporary, path)
        fsync_directory(directory)
    except BaseException:
        try:
            os.unlink(temporary)
        except FileNotFoundError:
            pass
        raise


def capture(source: str, recovery: str) -> Dict[str, Any]:
    source = os.path.abspath(source)
    recovery = os.path.abspath(recovery)
    restricted_directory(os.path.dirname(recovery), False)
    if os.path.lexists(source):
        input_descriptor = os.open(source, os.O_RDONLY | os.O_CLOEXEC | os.O_NOFOLLOW)
        output_descriptor, temporary = tempfile.mkstemp(
            prefix=f".{os.path.basename(recovery)}.", dir=os.path.dirname(recovery)
        )
        try:
            opened = os.fstat(input_descriptor)
            if not stat.S_ISREG(opened.st_mode) or opened.st_nlink != 1:
                raise AtomicError(f"refusing non-regular or hard-linked capture source: {source}")
            source_xattrs = {
                name: os.getxattr(input_descriptor, name)
                for name in sorted(os.listxattr(input_descriptor))
            }
            digest = hashlib.sha256()
            os.fchmod(output_descriptor, 0o600)
            with os.fdopen(input_descriptor, "rb") as input_file, os.fdopen(
                output_descriptor, "wb"
            ) as output_file:
                for chunk in iter(lambda: input_file.read(1024 * 1024), b""):
                    digest.update(chunk)
                    output_file.write(chunk)
                output_file.flush()
                os.fsync(output_file.fileno())
            after = os.stat(source, follow_symlinks=False)
            if (
                after.st_dev != opened.st_dev
                or after.st_ino != opened.st_ino
                or after.st_nlink != 1
                or after.st_size != opened.st_size
                or after.st_mtime_ns != opened.st_mtime_ns
                or after.st_ctime_ns != opened.st_ctime_ns
            ):
                raise AtomicError("capture source changed while it was read")
            current_xattrs = {
                name: os.getxattr(source, name, follow_symlinks=False)
                for name in sorted(os.listxattr(source, follow_symlinks=False))
            }
            if current_xattrs != source_xattrs:
                raise AtomicError("capture source xattrs changed while it was read")
            descriptor_value = {
                "content_sha256": digest.hexdigest(),
                "uid": opened.st_uid,
                "gid": opened.st_gid,
                "mode": stat.S_IMODE(opened.st_mode),
                "xattrs": {
                    name: hashlib.sha256(value).hexdigest()
                    for name, value in source_xattrs.items()
                },
            }
            os.replace(temporary, recovery)
            fsync_directory(os.path.dirname(recovery))
            metadata_recovery = {
                "uid": opened.st_uid,
                "gid": opened.st_gid,
                "mode": stat.S_IMODE(opened.st_mode),
                "xattrs": {
                    name: base64.b64encode(value).decode("ascii")
                    for name, value in source_xattrs.items()
                },
            }
            write_json(f"{recovery}.metadata", metadata_recovery)
        except BaseException:
            try:
                os.close(input_descriptor)
            except OSError:
                pass
            try:
                os.close(output_descriptor)
            except OSError:
                pass
            try:
                os.unlink(temporary)
            except FileNotFoundError:
                pass
            raise
        return {
            "presence": "present",
            "digest": description_digest(descriptor_value),
            "description": descriptor_value,
            "recovery": os.path.basename(recovery),
        }
    if os.path.exists(recovery):
        discard(recovery)
    if os.path.exists(f"{recovery}.metadata"):
        discard(f"{recovery}.metadata")
    return {"presence": "absent", "digest": "absent", "recovery": None}


def materialize(recovery: str, candidate: str) -> None:
    recovery = os.path.abspath(recovery)
    candidate = os.path.abspath(candidate)
    require_regular(recovery)
    with open(f"{recovery}.metadata", "r", encoding="utf-8") as source:
        metadata = json.load(source)
    if set(metadata) != {"uid", "gid", "mode", "xattrs"}:
        raise AtomicError("invalid recovery metadata")
    input_descriptor = os.open(recovery, os.O_RDONLY | os.O_CLOEXEC | os.O_NOFOLLOW)
    output_descriptor = os.open(
        candidate, os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_CLOEXEC | os.O_NOFOLLOW, 0o600
    )
    try:
        with os.fdopen(input_descriptor, "rb") as input_file, os.fdopen(
            output_descriptor, "wb"
        ) as output_file:
            for chunk in iter(lambda: input_file.read(1024 * 1024), b""):
                output_file.write(chunk)
            output_file.flush()
            os.fsync(output_file.fileno())
        os.chown(candidate, metadata["uid"], metadata["gid"], follow_symlinks=False)
        os.chmod(candidate, metadata["mode"], follow_symlinks=False)
        for name, encoded in metadata["xattrs"].items():
            os.setxattr(candidate, name, base64.b64decode(encoded), follow_symlinks=False)
        fsync_file(candidate)
        fsync_directory(os.path.dirname(candidate))
    except BaseException:
        try:
            os.close(input_descriptor)
        except OSError:
            pass
        try:
            os.close(output_descriptor)
        except OSError:
            pass
        try:
            os.unlink(candidate)
        except FileNotFoundError:
            pass
        raise


def apply_recovery_metadata(recovery: str, candidate: str) -> None:
    require_regular(candidate)
    with open(f"{os.path.abspath(recovery)}.metadata", "r", encoding="utf-8") as source:
        metadata = json.load(source)
    if set(metadata) != {"uid", "gid", "mode", "xattrs"}:
        raise AtomicError("invalid recovery metadata")
    os.chown(candidate, metadata["uid"], metadata["gid"], follow_symlinks=False)
    os.chmod(candidate, metadata["mode"], follow_symlinks=False)
    desired_names = set(metadata["xattrs"])
    for name in set(os.listxattr(candidate, follow_symlinks=False)) - desired_names:
        os.removexattr(candidate, name, follow_symlinks=False)
    for name, encoded in metadata["xattrs"].items():
        os.setxattr(candidate, name, base64.b64decode(encoded), follow_symlinks=False)
    fsync_file(candidate)


def inherit_metadata(source: str, candidate: str) -> None:
    metadata = require_regular(source)
    require_regular(candidate)
    copy_metadata(source, candidate, metadata)
    fsync_file(candidate)
    fsync_directory(os.path.dirname(candidate))


def canonical_json_digest(value: Any) -> str:
    encoded = json.dumps(value, sort_keys=True, separators=(",", ":")).encode("utf-8") + b"\n"
    return hashlib.sha256(encoded).hexdigest()


def claude_rollback(target: str, state_path: str, recovery: str, journal: str) -> None:
    target = os.path.abspath(target)
    require_regular(target)
    with open(state_path, "r", encoding="utf-8") as source:
        state = json.load(source)
    with open(target, "r", encoding="utf-8") as source:
        current = json.load(source)
    if not isinstance(current, dict):
        raise AtomicError("refusing non-object Claude config")
    parent = current.get("mcpServers", None)
    if "mcpServers" not in current or not isinstance(parent, dict):
        raise AtomicError("Claude mcpServers ownership boundary disappeared")
    post_key = state["post_structure"]["key"]
    key_present = "t-hub" in parent
    if key_present != post_key["presence"]:
        raise AtomicError("Claude t-hub ownership changed after helper return")
    if key_present:
        current_digest = canonical_json_digest(parent["t-hub"])
        if current_digest != post_key["digest"]:
            raise AtomicError("Claude t-hub owner changed after helper return")

    before = state["before"]
    before_file = state["before_file"]
    before_document: Dict[str, Any] = {}
    if before_file["presence"] == "present":
        require_regular(recovery)
        with open(recovery, "r", encoding="utf-8") as source:
            before_document = json.load(source)
        if not isinstance(before_document, dict):
            raise AtomicError("recovery Claude config is not an object")
    if before["key"]["presence"]:
        before_parent = before_document.get("mcpServers")
        if not isinstance(before_parent, dict) or "t-hub" not in before_parent:
            raise AtomicError("Claude recovery key contradicts its descriptor")
        parent["t-hub"] = before_parent["t-hub"]
    else:
        parent.pop("t-hub", None)
        if not before["parent"]["presence"] and not parent:
            current.pop("mcpServers", None)

    live_description = description(target)
    expected = description_digest(live_description)
    if before_file["presence"] == "absent" and not current:
        delete(target, expected, journal)
        return
    candidate_descriptor, candidate = tempfile.mkstemp(
        prefix=f".{os.path.basename(target)}.t-hub-rollback.", dir=os.path.dirname(target)
    )
    try:
        with os.fdopen(candidate_descriptor, "w", encoding="utf-8") as output:
            json.dump(current, output, indent=2)
            output.write("\n")
            output.flush()
            os.fsync(output.fileno())
        metadata_fields = ("uid", "gid", "mode", "xattrs")
        post_description = state.get("post", {}).get("description", {})
        metadata_still_owned = all(
            live_description.get(field) == post_description.get(field)
            for field in metadata_fields
        )
        preserve_metadata = before_file["presence"] == "present" and metadata_still_owned
        if preserve_metadata:
            apply_recovery_metadata(recovery, candidate)
        exchange(
            target,
            candidate,
            expected,
            journal,
            preserve_candidate_metadata=preserve_metadata,
        )
    except BaseException:
        try:
            os.unlink(candidate)
        except FileNotFoundError:
            pass
        raise
    # After exchange this contains the displaced poststate.
    discard(candidate)


def purge(path: str) -> None:
    path = os.path.abspath(path)
    metadata = os.lstat(path)
    if stat.S_ISLNK(metadata.st_mode):
        raise AtomicError(f"refusing symlink during purge: {path}")
    if stat.S_ISREG(metadata.st_mode):
        discard(path)
        return
    if not stat.S_ISDIR(metadata.st_mode):
        raise AtomicError(f"refusing special path during purge: {path}")
    for name in os.listdir(path):
        purge(os.path.join(path, name))
    os.rmdir(path)
    fsync_directory(os.path.dirname(path))


def discard(path: str) -> None:
    path = os.path.abspath(path)
    require_regular(path)
    descriptor = os.open(path, os.O_WRONLY | os.O_CLOEXEC | os.O_NOFOLLOW)
    try:
        os.ftruncate(descriptor, 0)
        os.fsync(descriptor)
    finally:
        os.close(descriptor)
    crash("discard-truncated")
    os.unlink(path)
    fsync_directory(os.path.dirname(path))


def release(
    path: str,
    expected_digest: str,
    expected_identity: Optional[Dict[str, int]] = None,
) -> None:
    """Durably unlink a non-secret staging inode without opening it for writing."""
    path = os.path.abspath(path)
    require_single_link(path)
    if expected_identity is not None and identity(path) != expected_identity:
        raise AtomicError("release path identity changed")
    if state_digest(path) != expected_digest:
        raise AtomicError("release path digest changed")
    os.unlink(path)
    fsync_directory(os.path.dirname(path))


def main() -> int:
    parser = argparse.ArgumentParser()
    subparsers = parser.add_subparsers(dest="command", required=True)
    exchange_parser = subparsers.add_parser("exchange")
    exchange_parser.add_argument("--target", required=True)
    exchange_parser.add_argument("--candidate", required=True)
    exchange_parser.add_argument("--expected-sha")
    exchange_parser.add_argument("--expected-digest")
    exchange_parser.add_argument("--journal")
    exchange_parser.add_argument("--preserve-candidate-metadata", action="store_true")
    install_parser = subparsers.add_parser("install")
    install_parser.add_argument("--target", required=True)
    install_parser.add_argument("--candidate", required=True)
    install_parser.add_argument("--expected-digest", required=True)
    install_parser.add_argument("--journal", required=True)
    install_parser.add_argument("--preserve-candidate-metadata", action="store_true")
    delete_parser = subparsers.add_parser("delete")
    delete_parser.add_argument("--target", required=True)
    delete_parser.add_argument("--expected-digest", required=True)
    delete_parser.add_argument("--journal", required=True)
    delete_parser.add_argument("--unlink-only", action="store_true")
    recover_parser = subparsers.add_parser("recover")
    recover_parser.add_argument("--journal", required=True)
    recover_parser.add_argument("--keep-journal", action="store_true")
    describe_parser = subparsers.add_parser("describe")
    describe_parser.add_argument("--path", required=True)
    capture_parser = subparsers.add_parser("capture")
    capture_parser.add_argument("--source", required=True)
    capture_parser.add_argument("--recovery", required=True)
    materialize_parser = subparsers.add_parser("materialize")
    materialize_parser.add_argument("--recovery", required=True)
    materialize_parser.add_argument("--candidate", required=True)
    inherit_parser = subparsers.add_parser("inherit-metadata")
    inherit_parser.add_argument("--source", required=True)
    inherit_parser.add_argument("--candidate", required=True)
    claude_rollback_parser = subparsers.add_parser("claude-rollback")
    claude_rollback_parser.add_argument("--target", required=True)
    claude_rollback_parser.add_argument("--state", required=True)
    claude_rollback_parser.add_argument("--recovery", required=True)
    claude_rollback_parser.add_argument("--journal", required=True)
    purge_parser = subparsers.add_parser("purge")
    purge_parser.add_argument("--path", required=True)
    sync_parser = subparsers.add_parser("sync-directory")
    sync_parser.add_argument("--path", required=True)
    publish_parser = subparsers.add_parser("publish")
    publish_parser.add_argument("--path", required=True)
    publish_parser.add_argument("--value", required=True)
    discard_parser = subparsers.add_parser("discard")
    discard_parser.add_argument("--path", required=True)
    release_parser = subparsers.add_parser("release")
    release_parser.add_argument("--path", required=True)
    release_parser.add_argument("--expected-digest", required=True)
    release_parser.add_argument("--expected-device", type=int)
    release_parser.add_argument("--expected-inode", type=int)
    snapshot_parser = subparsers.add_parser("snapshot-executable")
    snapshot_parser.add_argument("--source", required=True)
    snapshot_parser.add_argument("--destination", required=True)
    verify_parser = subparsers.add_parser("verify-executable")
    verify_parser.add_argument("--source", required=True)
    verify_parser.add_argument("--expected-device", type=int, required=True)
    verify_parser.add_argument("--expected-inode", type=int, required=True)
    verify_parser.add_argument("--expected-uid", type=int, required=True)
    verify_parser.add_argument("--expected-gid", type=int, required=True)
    verify_parser.add_argument("--expected-mode", type=int, required=True)
    verify_parser.add_argument("--expected-size", type=int, required=True)
    verify_parser.add_argument("--expected-mtime-ns", type=int, required=True)
    verify_parser.add_argument("--expected-ctime-ns", type=int, required=True)
    verify_parser.add_argument("--expected-digest", required=True)
    catalog_parser = subparsers.add_parser("verify-cortana-catalog")
    catalog_parser.add_argument("--executable", required=True)
    arguments = parser.parse_args()
    try:
        if arguments.command == "exchange":
            expected = arguments.expected_digest or arguments.expected_sha
            if not expected:
                raise AtomicError("exchange requires --expected-digest")
            exchange(
                arguments.target,
                arguments.candidate,
                expected,
                arguments.journal or default_journal(arguments.candidate),
                arguments.preserve_candidate_metadata,
            )
        elif arguments.command == "install":
            if arguments.expected_digest == "absent":
                create(arguments.target, arguments.candidate, arguments.journal)
            else:
                exchange(
                    arguments.target,
                    arguments.candidate,
                    arguments.expected_digest,
                    arguments.journal,
                    arguments.preserve_candidate_metadata,
                )
        elif arguments.command == "delete":
            delete(
                arguments.target,
                arguments.expected_digest,
                arguments.journal,
                arguments.unlink_only,
            )
        elif arguments.command == "recover":
            print(recover_exchange(arguments.journal, not arguments.keep_journal))
        elif arguments.command == "describe":
            value = description(os.path.abspath(arguments.path))
            print(json.dumps({"digest": description_digest(value), "description": value}, sort_keys=True))
        elif arguments.command == "capture":
            print(json.dumps(capture(arguments.source, arguments.recovery), sort_keys=True))
        elif arguments.command == "materialize":
            materialize(arguments.recovery, arguments.candidate)
        elif arguments.command == "inherit-metadata":
            inherit_metadata(arguments.source, arguments.candidate)
        elif arguments.command == "claude-rollback":
            claude_rollback(
                arguments.target, arguments.state, arguments.recovery, arguments.journal
            )
        elif arguments.command == "purge":
            purge(arguments.path)
        elif arguments.command == "sync-directory":
            fsync_directory(os.path.abspath(arguments.path))
        elif arguments.command == "publish":
            publish(arguments.path, arguments.value)
        elif arguments.command == "release":
            expected_identity = None
            if arguments.expected_device is not None or arguments.expected_inode is not None:
                if arguments.expected_device is None or arguments.expected_inode is None:
                    raise AtomicError("release identity requires both device and inode")
                expected_identity = {
                    "device": arguments.expected_device,
                    "inode": arguments.expected_inode,
                }
            release(arguments.path, arguments.expected_digest, expected_identity)
        elif arguments.command == "snapshot-executable":
            print(
                json.dumps(
                    snapshot_executable(arguments.source, arguments.destination),
                    sort_keys=True,
                )
            )
        elif arguments.command == "verify-executable":
            expected = {
                "device": arguments.expected_device,
                "inode": arguments.expected_inode,
                "uid": arguments.expected_uid,
                "gid": arguments.expected_gid,
                "mode": arguments.expected_mode,
                "size": arguments.expected_size,
                "mtime_ns": arguments.expected_mtime_ns,
                "ctime_ns": arguments.expected_ctime_ns,
                "content_sha256": arguments.expected_digest,
            }
            print(json.dumps(verify_executable(arguments.source, expected), sort_keys=True))
        elif arguments.command == "verify-cortana-catalog":
            verify_cortana_catalog(arguments.executable)
        else:
            discard(arguments.path)
    except (OSError, AtomicError, ValueError, json.JSONDecodeError) as error:
        print(f"atomic-config: {error}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
