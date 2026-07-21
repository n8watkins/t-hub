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
import stat
import sys
import tempfile
from typing import Any, Dict, Optional


AT_FDCWD = -100
RENAME_EXCHANGE = 2
LIBC = ctypes.CDLL(None, use_errno=True)
VERSION = 1


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
        "prepared", "exchanged", "mismatch", "restored", "verified", "committed"
    }:
        raise AtomicError("invalid exchange phase")
    return value


def set_phase(journal: str, intent: Dict[str, Any], phase: str) -> None:
    intent["phase"] = phase
    write_json(os.path.join(journal, "intent.json"), intent)


def crash(point: str) -> None:
    if os.environ.get("T_HUB_ATOMIC_CRASH_AT") == point:
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


def recover_delete(journal: str, intent: Dict[str, Any], cleanup: bool) -> str:
    target = intent["target"]
    recovery = intent["candidate"]
    expected = intent["expected"]["digest"]
    target_digest = inspect_digest(target)
    recovery_digest = inspect_digest(recovery)
    if target_digest is None and recovery_digest == expected:
        if intent["phase"] != "committed":
            set_phase(journal, intent, "committed")
        discard(recovery)
        if cleanup:
            remove_journal(journal)
        return "committed"
    if target_digest == expected and recovery_digest is None:
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
        "recovery": {"prestate": "absent"},
    }
    write_json(os.path.join(journal, "intent.json"), intent)
    crash("prepared")
    os.rename(candidate, target)
    fsync_file(target)
    fsync_directory(os.path.dirname(target))
    crash("renamed-before-phase")
    set_phase(journal, intent, "verified")
    crash("verified")
    set_phase(journal, intent, "committed")
    crash("committed")
    remove_journal(journal)


def delete(target: str, expected: str, journal: str) -> None:
    target = os.path.abspath(target)
    journal = os.path.abspath(journal)
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
        "recovery": {"displaced_prestate": "candidate-after-rename"},
    }
    write_json(os.path.join(journal, "intent.json"), intent)
    crash("prepared")
    os.rename(target, recovery)
    fsync_file(recovery)
    fsync_directory(os.path.dirname(target))
    crash("renamed-before-phase")
    set_phase(journal, intent, "verified")
    crash("verified")
    set_phase(journal, intent, "committed")
    crash("committed")
    discard(recovery)
    remove_journal(journal)


def exchange(target: str, candidate: str, expected: str, journal: str) -> None:
    target = os.path.abspath(target)
    candidate = os.path.abspath(candidate)
    journal = os.path.abspath(journal)
    if os.name != "posix" or not sys.platform.startswith("linux"):
        raise AtomicError("atomic exchange is supported only on Linux/WSL")
    if os.path.dirname(target) != os.path.dirname(candidate):
        raise AtomicError("target and candidate must share a directory")
    if os.path.commonpath([journal, target]) == target:
        raise AtomicError("journal cannot be nested below the target")
    target_metadata = require_regular(target)
    require_regular(candidate)
    before = description(target)
    before_digest = description_digest(before)
    # Backward compatibility for callers that supplied the old content-only
    # hash.  New callers publish the complete state digest.
    if expected not in {before_digest, before["content_sha256"]}:
        raise AtomicError("target prestate changed before prepare")
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
        "recovery": {"displaced_prestate": "candidate-after-exchange"},
    }
    write_json(os.path.join(journal, "intent.json"), intent)
    fsync_file(target)
    fsync_file(candidate)
    fsync_directory(os.path.dirname(target))
    crash("prepared")

    durable_exchange(target, candidate)
    crash("exchanged-before-phase")
    displaced = state_digest(candidate)
    if displaced != before_digest:
        set_phase(journal, intent, "mismatch")
        crash("mismatch-before-restore")
        durable_exchange(target, candidate)
        crash("restored-before-phase")
        set_phase(journal, intent, "restored")
        raise AtomicError("target prestate changed during exchange; concurrent state restored")
    set_phase(journal, intent, "exchanged")
    crash("exchanged")
    if state_digest(target) != desired_digest:
        set_phase(journal, intent, "mismatch")
        durable_exchange(target, candidate)
        set_phase(journal, intent, "restored")
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
        require_regular(source)
        descriptor_value = description(source)
        input_descriptor = os.open(source, os.O_RDONLY | os.O_CLOEXEC | os.O_NOFOLLOW)
        output_descriptor, temporary = tempfile.mkstemp(
            prefix=f".{os.path.basename(recovery)}.", dir=os.path.dirname(recovery)
        )
        try:
            os.fchmod(output_descriptor, 0o600)
            with os.fdopen(input_descriptor, "rb") as input_file, os.fdopen(
                output_descriptor, "wb"
            ) as output_file:
                for chunk in iter(lambda: input_file.read(1024 * 1024), b""):
                    output_file.write(chunk)
                output_file.flush()
                os.fsync(output_file.fileno())
            os.replace(temporary, recovery)
            fsync_directory(os.path.dirname(recovery))
            metadata_recovery = {
                "uid": os.lstat(source).st_uid,
                "gid": os.lstat(source).st_gid,
                "mode": stat.S_IMODE(os.lstat(source).st_mode),
                "xattrs": {
                    name: base64.b64encode(
                        os.getxattr(source, name, follow_symlinks=False)
                    ).decode("ascii")
                    for name in sorted(os.listxattr(source, follow_symlinks=False))
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


def discard(path: str) -> None:
    path = os.path.abspath(path)
    require_regular(path)
    descriptor = os.open(path, os.O_WRONLY | os.O_CLOEXEC | os.O_NOFOLLOW)
    try:
        os.ftruncate(descriptor, 0)
        os.fsync(descriptor)
    finally:
        os.close(descriptor)
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
    install_parser = subparsers.add_parser("install")
    install_parser.add_argument("--target", required=True)
    install_parser.add_argument("--candidate", required=True)
    install_parser.add_argument("--expected-digest", required=True)
    install_parser.add_argument("--journal", required=True)
    delete_parser = subparsers.add_parser("delete")
    delete_parser.add_argument("--target", required=True)
    delete_parser.add_argument("--expected-digest", required=True)
    delete_parser.add_argument("--journal", required=True)
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
    publish_parser = subparsers.add_parser("publish")
    publish_parser.add_argument("--path", required=True)
    publish_parser.add_argument("--value", required=True)
    discard_parser = subparsers.add_parser("discard")
    discard_parser.add_argument("--path", required=True)
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
                )
        elif arguments.command == "delete":
            delete(arguments.target, arguments.expected_digest, arguments.journal)
        elif arguments.command == "recover":
            print(recover_exchange(arguments.journal, not arguments.keep_journal))
        elif arguments.command == "describe":
            value = description(os.path.abspath(arguments.path))
            print(json.dumps({"digest": description_digest(value), "description": value}, sort_keys=True))
        elif arguments.command == "capture":
            print(json.dumps(capture(arguments.source, arguments.recovery), sort_keys=True))
        elif arguments.command == "materialize":
            materialize(arguments.recovery, arguments.candidate)
        elif arguments.command == "publish":
            publish(arguments.path, arguments.value)
        else:
            discard(arguments.path)
    except (OSError, AtomicError, ValueError, json.JSONDecodeError) as error:
        print(f"atomic-config: {error}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
