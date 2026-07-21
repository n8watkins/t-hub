#!/usr/bin/env python3
"""Durable Linux same-directory config exchange and state publication."""

import argparse
import ctypes
import hashlib
import os
import stat
import sys
import tempfile


AT_FDCWD = -100
RENAME_EXCHANGE = 2
LIBC = ctypes.CDLL(None, use_errno=True)


def sha256(path: str) -> str:
    digest = hashlib.sha256()
    with open(path, "rb") as source:
        for chunk in iter(lambda: source.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def fsync_directory(path: str) -> None:
    descriptor = os.open(path, os.O_RDONLY | os.O_DIRECTORY)
    try:
        os.fsync(descriptor)
    finally:
        os.close(descriptor)


def rename_exchange(left: str, right: str) -> None:
    renameat2 = getattr(LIBC, "renameat2", None)
    if renameat2 is None:
        raise RuntimeError("Linux renameat2 is unavailable")
    result = renameat2(
        AT_FDCWD,
        os.fsencode(left),
        AT_FDCWD,
        os.fsencode(right),
        RENAME_EXCHANGE,
    )
    if result != 0:
        error = ctypes.get_errno()
        raise OSError(error, os.strerror(error))


def require_regular(path: str) -> os.stat_result:
    metadata = os.lstat(path)
    if not stat.S_ISREG(metadata.st_mode):
        raise RuntimeError(f"refusing non-regular path: {path}")
    return metadata


def copy_metadata(source: str, destination: str, metadata: os.stat_result) -> None:
    os.chmod(destination, stat.S_IMODE(metadata.st_mode), follow_symlinks=False)
    os.chown(destination, metadata.st_uid, metadata.st_gid, follow_symlinks=False)
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


def exchange(target: str, candidate: str, expected_sha: str) -> None:
    target = os.path.abspath(target)
    candidate = os.path.abspath(candidate)
    if os.path.dirname(target) != os.path.dirname(candidate):
        raise RuntimeError("target and candidate must share a directory")
    target_metadata = require_regular(target)
    require_regular(candidate)
    copy_metadata(target, candidate, target_metadata)
    with open(candidate, "rb") as staged:
        os.fsync(staged.fileno())
    rename_exchange(target, candidate)
    fsync_directory(os.path.dirname(target))
    if sha256(candidate) != expected_sha:
        rename_exchange(target, candidate)
        fsync_directory(os.path.dirname(target))
        raise RuntimeError("target prestate changed; concurrent bytes restored")


def publish(path: str, value: str) -> None:
    path = os.path.abspath(path)
    directory = os.path.dirname(path)
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


def discard(path: str) -> None:
    path = os.path.abspath(path)
    require_regular(path)
    os.unlink(path)
    fsync_directory(os.path.dirname(path))


def main() -> int:
    parser = argparse.ArgumentParser()
    subparsers = parser.add_subparsers(dest="command", required=True)
    exchange_parser = subparsers.add_parser("exchange")
    exchange_parser.add_argument("--target", required=True)
    exchange_parser.add_argument("--candidate", required=True)
    exchange_parser.add_argument("--expected-sha", required=True)
    publish_parser = subparsers.add_parser("publish")
    publish_parser.add_argument("--path", required=True)
    publish_parser.add_argument("--value", required=True)
    discard_parser = subparsers.add_parser("discard")
    discard_parser.add_argument("--path", required=True)
    arguments = parser.parse_args()
    try:
        if arguments.command == "exchange":
            exchange(arguments.target, arguments.candidate, arguments.expected_sha)
        elif arguments.command == "publish":
            publish(arguments.path, arguments.value)
        else:
            discard(arguments.path)
    except (OSError, RuntimeError) as error:
        print(f"atomic-config: {error}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
