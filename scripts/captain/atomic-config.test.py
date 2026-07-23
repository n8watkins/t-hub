#!/usr/bin/env python3

import hashlib
import importlib.util
import json
import os
import pathlib
import stat
import subprocess
import sys
import tempfile
import threading
import time
import unittest


HELPER = pathlib.Path(__file__).with_name("atomic-config.py")


def load_helper():
    spec = importlib.util.spec_from_file_location("atomic_config", HELPER)
    assert spec is not None and spec.loader is not None
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def digest(value: bytes) -> str:
    return hashlib.sha256(value).hexdigest()


class AtomicConfigTest(unittest.TestCase):
    def test_snapshot_executable_copies_verified_fd_privately(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = pathlib.Path(directory)
            source = root / "source"
            destination = root / "snapshot"
            source.write_bytes(b"verified executable\n")
            source.chmod(0o750)
            result = json.loads(
                subprocess.check_output(
                    [
                        sys.executable,
                        str(HELPER),
                        "snapshot-executable",
                        "--source",
                        str(source),
                        "--destination",
                        str(destination),
                    ],
                    text=True,
                )
            )
            self.assertEqual(destination.read_bytes(), source.read_bytes())
            self.assertEqual(stat.S_IMODE(destination.stat().st_mode), 0o700)
            self.assertEqual(result["source"]["inode"], source.stat().st_ino)
            self.assertEqual(result["snapshot"]["inode"], destination.stat().st_ino)
            self.assertEqual(
                result["source"]["content_sha256"],
                digest(b"verified executable\n"),
            )

    def test_snapshot_executable_rejects_nonregular_paths_without_blocking(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = pathlib.Path(directory)
            real = root / "real"
            real.write_bytes(b"executable\n")
            real.chmod(0o700)
            symlink = root / "symlink"
            symlink.symlink_to(real.name)
            fifo = root / "fifo"
            os.mkfifo(fifo, 0o700)
            acquired_type_mismatch = root / "directory"
            acquired_type_mismatch.mkdir(mode=0o700)
            for source in (symlink, fifo, acquired_type_mismatch):
                with self.subTest(source=source.name):
                    destination = root / f"{source.name}.snapshot"
                    result = subprocess.run(
                        [
                            sys.executable,
                            str(HELPER),
                            "snapshot-executable",
                            "--source",
                            str(source),
                            "--destination",
                            str(destination),
                        ],
                        check=False,
                        timeout=2,
                    )
                    self.assertNotEqual(result.returncode, 0)
                    self.assertFalse(destination.exists())

    def test_snapshot_executable_rejects_unsafe_modes_and_existing_destination(
        self,
    ) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = pathlib.Path(directory)
            source = root / "source"
            destination = root / "snapshot"
            source.write_bytes(b"executable\n")
            for mode in (0o600, 0o722, 0o4700):
                with self.subTest(mode=oct(mode)):
                    source.chmod(mode)
                    result = subprocess.run(
                        [
                            sys.executable,
                            str(HELPER),
                            "snapshot-executable",
                            "--source",
                            str(source),
                            "--destination",
                            str(destination),
                        ],
                        check=False,
                    )
                    self.assertNotEqual(result.returncode, 0)
                    self.assertFalse(destination.exists())
            source.chmod(0o700)
            destination.write_bytes(b"known good\n")
            destination.chmod(0o700)
            result = subprocess.run(
                [
                    sys.executable,
                    str(HELPER),
                    "snapshot-executable",
                    "--source",
                    str(source),
                    "--destination",
                    str(destination),
                ],
                check=False,
            )
            self.assertNotEqual(result.returncode, 0)
            self.assertEqual(destination.read_bytes(), b"known good\n")

    def test_verify_executable_is_nonblocking_and_exact(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = pathlib.Path(directory)
            source = root / "source"
            snapshot = root / "snapshot"
            source.write_bytes(b"verified executable\n")
            source.chmod(0o700)
            selected = json.loads(
                subprocess.check_output(
                    [
                        sys.executable,
                        str(HELPER),
                        "snapshot-executable",
                        "--source",
                        str(source),
                        "--destination",
                        str(snapshot),
                    ],
                    text=True,
                )
            )["source"]
            snapshot.unlink()

            command = [
                sys.executable,
                str(HELPER),
                "verify-executable",
                "--source",
                str(source),
            ]
            for field, option in (
                ("device", "--expected-device"),
                ("inode", "--expected-inode"),
                ("uid", "--expected-uid"),
                ("gid", "--expected-gid"),
                ("mode", "--expected-mode"),
                ("size", "--expected-size"),
                ("mtime_ns", "--expected-mtime-ns"),
                ("ctime_ns", "--expected-ctime-ns"),
                ("content_sha256", "--expected-digest"),
            ):
                command.extend((option, str(selected[field])))
            subprocess.run(command, check=True)

            original = root / "original"
            source.rename(original)
            for raced_type in ("fifo", "symlink"):
                with self.subTest(raced_type=raced_type):
                    if raced_type == "fifo":
                        os.mkfifo(source, 0o700)
                    else:
                        source.symlink_to(original.name)
                    result = subprocess.run(command, check=False, timeout=2)
                    self.assertNotEqual(result.returncode, 0)
                    source.unlink()

    def test_catalog_verification_is_exact_bounded_and_pinned(self) -> None:
        helper = load_helper()
        helper.CATALOG_TIMEOUT_SECONDS = 0.2
        with tempfile.TemporaryDirectory() as directory:
            root = pathlib.Path(directory)
            valid = root / "valid"
            valid.write_text(
                "#!/bin/sh\n"
                "printf '%s\\n' "
                "'{\"tools\":[{\"name\":\"cortana_bootstrap\","
                "\"inputSchema\":{\"type\":\"object\",\"properties\":{},"
                "\"additionalProperties\":false},\"annotations\":{"
                "\"t-hubTier\":\"read\",\"confirmationRequired\":false,"
                "\"readOnlyHint\":true,\"destructiveHint\":false,"
                "\"idempotentHint\":true,\"openWorldHint\":false}}]}'\n"
            )
            valid.chmod(0o700)
            helper.verify_cortana_catalog(str(valid))

            def assert_descendant_gone(pid_file: pathlib.Path) -> None:
                pid = int(pid_file.read_text())
                for _ in range(100):
                    try:
                        state = pathlib.Path(f"/proc/{pid}/stat").read_text().split()[2]
                    except (FileNotFoundError, ProcessLookupError):
                        return
                    if state == "Z":
                        return
                    time.sleep(0.01)
                self.fail(f"catalog descendant remained alive: {pid}")

            valid_fork = root / "valid-fork"
            valid_fork_pid = root / "valid-fork.pid"
            valid_fork.write_text(
                "#!/bin/sh\n"
                "sleep 30 >/dev/null 2>&1 &\n"
                f"printf '%s\\n' \"$!\" > '{valid_fork_pid}'\n"
                f"{valid.read_text().splitlines()[1]}\n"
            )
            valid_fork.chmod(0o700)
            helper.verify_cortana_catalog(str(valid_fork))
            assert_descendant_gone(valid_fork_pid)

            held_pipe = root / "held-pipe"
            held_pipe_pid = root / "held-pipe.pid"
            held_pipe.write_text(
                "#!/bin/sh\n"
                "sleep 30 &\n"
                f"printf '%s\\n' \"$!\" > '{held_pipe_pid}'\n"
            )
            held_pipe.chmod(0o700)
            with self.assertRaises(helper.AtomicError):
                helper.verify_cortana_catalog(str(held_pipe))
            assert_descendant_gone(held_pipe_pid)

            for name, body in (
                ("hang", "#!/bin/sh\nsleep 30\n"),
                ("flood", "#!/bin/sh\nyes x\n"),
                ("stderr-flood", "#!/bin/sh\nyes x >&2\n"),
                (
                    "dirty-exit",
                    f"#!/bin/sh\n{valid.read_text().splitlines()[1]}\nexit 7\n",
                ),
            ):
                with self.subTest(name=name):
                    executable = root / name
                    executable.write_text(body)
                    executable.chmod(0o700)
                    started = time.monotonic()
                    with self.assertRaises(helper.AtomicError):
                        helper.verify_cortana_catalog(str(executable))
                    self.assertLess(time.monotonic() - started, 2)

    def test_snapshot_executable_rejects_continuous_same_inode_growth_promptly(
        self,
    ) -> None:
        helper = load_helper()
        with tempfile.TemporaryDirectory() as directory:
            root = pathlib.Path(directory)
            source = root / "source"
            destination = root / "snapshot"
            source.write_bytes(b"x" * (1024 * 1024))
            source.chmod(0o700)
            original_inode = source.stat().st_ino
            stop = threading.Event()
            started = threading.Event()

            def append_continuously() -> None:
                with source.open("ab", buffering=0) as output:
                    started.set()
                    while not stop.is_set():
                        output.write(b"y" * 4096)

            writer = threading.Thread(target=append_continuously)
            writer.start()
            started.wait(timeout=1)
            began = time.monotonic()
            try:
                with self.assertRaises(helper.AtomicError):
                    helper.snapshot_executable(str(source), str(destination))
            finally:
                stop.set()
                writer.join(timeout=2)
            self.assertLess(time.monotonic() - began, 2)
            self.assertEqual(source.stat().st_ino, original_inode)
            self.assertFalse(destination.exists())

    def test_release_unlinks_running_executable_without_truncation(self) -> None:
        helper = load_helper()
        with tempfile.TemporaryDirectory() as directory:
            executable = pathlib.Path(directory) / "running"
            executable.write_bytes(pathlib.Path("/bin/sleep").read_bytes())
            executable.chmod(0o700)
            process = subprocess.Popen([str(executable), "120"])
            try:
                metadata = executable.stat()
                expected_digest = helper.state_digest(str(executable))
                helper.release(
                    str(executable),
                    expected_digest,
                    {"device": metadata.st_dev, "inode": metadata.st_ino},
                )
                self.assertFalse(executable.exists())
                self.assertIsNone(process.poll())
            finally:
                process.terminate()
                process.wait(timeout=5)

    def test_unlink_only_delete_recovers_running_executable_after_crash(self) -> None:
        helper = load_helper()
        with tempfile.TemporaryDirectory() as directory:
            root = pathlib.Path(directory)
            executable = root / "running"
            journal = root / "journal"
            executable.write_bytes(pathlib.Path("/bin/sleep").read_bytes())
            executable.chmod(0o700)
            process = subprocess.Popen([str(executable), "120"])
            try:
                expected_digest = helper.state_digest(str(executable))
                environment = os.environ.copy()
                environment["T_HUB_ATOMIC_CRASH_AT"] = "renamed-before-phase"
                result = subprocess.run(
                    [sys.executable, str(HELPER), "delete", "--target", str(executable),
                     "--expected-digest", expected_digest, "--journal", str(journal),
                     "--unlink-only"], env=environment, check=False,
                )
                self.assertEqual(result.returncode, 89)
                intent = json.loads((journal / "intent.json").read_text())
                recovery = pathlib.Path(intent["candidate"])
                self.assertTrue(recovery.exists())
                held = subprocess.check_output(
                    [sys.executable, str(HELPER), "recover", "--journal", str(journal),
                     "--keep-journal"], text=True,
                ).strip()
                self.assertEqual(held, "committed")
                self.assertTrue(recovery.exists())
                self.assertTrue(journal.exists())
                recovered = subprocess.check_output(
                    [sys.executable, str(HELPER), "recover", "--journal", str(journal)],
                    text=True,
                ).strip()
                self.assertEqual(recovered, "committed")
                self.assertFalse(recovery.exists())
                self.assertFalse(journal.exists())
                self.assertIsNone(process.poll())
            finally:
                process.terminate()
                process.wait(timeout=5)

    def test_release_refuses_digest_and_inode_replacement_races(self) -> None:
        helper = load_helper()
        with tempfile.TemporaryDirectory() as directory:
            root = pathlib.Path(directory)
            target = root / "stage"
            replacement = root / "replacement"
            target.write_bytes(b"owned\n")
            expected_digest = helper.state_digest(str(target))
            metadata = target.stat()
            expected_identity = {"device": metadata.st_dev, "inode": metadata.st_ino}
            target.write_bytes(b"changed\n")
            with self.assertRaises(helper.AtomicError):
                helper.release(str(target), expected_digest, expected_identity)
            self.assertEqual(target.read_bytes(), b"changed\n")
            target.write_bytes(b"owned\n")
            replacement.write_bytes(b"owned\n")
            os.replace(replacement, target)
            with self.assertRaises(helper.AtomicError):
                helper.release(str(target), expected_digest, expected_identity)
            self.assertEqual(target.read_bytes(), b"owned\n")

    def test_exchange_preserves_displaced_bytes_and_metadata(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = pathlib.Path(directory)
            target = root / "config"
            candidate = root / ".candidate"
            target.write_bytes(b"before\n")
            candidate.write_bytes(b"after\n")
            target.chmod(0o640)
            os.setxattr(target, "user.t-hub-test", b"label")
            subprocess.run(
                [sys.executable, str(HELPER), "exchange", "--target", str(target), "--candidate", str(candidate), "--expected-sha", digest(b"before\n")],
                check=True,
            )
            self.assertEqual(target.read_bytes(), b"after\n")
            self.assertEqual(candidate.read_bytes(), b"before\n")
            self.assertEqual(stat.S_IMODE(target.stat().st_mode), 0o640)
            self.assertEqual(target.stat().st_uid, candidate.stat().st_uid)
            self.assertEqual(target.stat().st_gid, candidate.stat().st_gid)
            self.assertEqual(os.getxattr(target, "user.t-hub-test"), b"label")
            self.assertFalse(pathlib.Path(f"{candidate}.journal").exists())

    def test_describe_detects_metadata_only_changes(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            target = pathlib.Path(directory) / "config"
            target.write_bytes(b"same bytes\n")
            before = json.loads(subprocess.check_output(
                [sys.executable, str(HELPER), "describe", "--path", str(target)], text=True
            ))["digest"]
            target.chmod(0o600)
            after = json.loads(subprocess.check_output(
                [sys.executable, str(HELPER), "describe", "--path", str(target)], text=True
            ))["digest"]
            self.assertNotEqual(before, after)

    def test_crash_recovery_at_each_committable_phase(self) -> None:
        for phase in ("exchanged-before-phase", "exchanged", "verified", "committed"):
            with self.subTest(phase=phase), tempfile.TemporaryDirectory() as directory:
                root = pathlib.Path(directory)
                target = root / "config"
                candidate = root / ".candidate"
                journal = root / "journal"
                target.write_bytes(b"before\n")
                candidate.write_bytes(b"after\n")
                environment = os.environ.copy()
                environment["T_HUB_ATOMIC_CRASH_AT"] = phase
                result = subprocess.run(
                    [sys.executable, str(HELPER), "exchange", "--target", str(target),
                     "--candidate", str(candidate), "--expected-sha", digest(b"before\n"),
                     "--journal", str(journal)], env=environment, check=False,
                )
                self.assertEqual(result.returncode, 89)
                recovered = subprocess.check_output(
                    [sys.executable, str(HELPER), "recover", "--journal", str(journal)], text=True
                ).strip()
                self.assertEqual(recovered, "committed")
                self.assertEqual(target.read_bytes(), b"after\n")
                self.assertFalse(journal.exists())

    def test_prepared_recovery_aborts_to_exact_prestate(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = pathlib.Path(directory)
            target = root / "config"
            candidate = root / ".candidate"
            journal = root / "journal"
            target.write_bytes(b"before\n")
            candidate.write_bytes(b"after\n")
            environment = os.environ.copy()
            environment["T_HUB_ATOMIC_CRASH_AT"] = "prepared"
            result = subprocess.run(
                [sys.executable, str(HELPER), "exchange", "--target", str(target),
                 "--candidate", str(candidate), "--expected-sha", digest(b"before\n"),
                 "--journal", str(journal)], env=environment, check=False,
            )
            self.assertEqual(result.returncode, 89)
            recovered = subprocess.check_output(
                [sys.executable, str(HELPER), "recover", "--journal", str(journal)], text=True
            ).strip()
            self.assertEqual(recovered, "restored")
            self.assertEqual(target.read_bytes(), b"before\n")

    def test_prepared_metadata_race_is_refused_without_loss(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = pathlib.Path(directory)
            target = root / "config"
            candidate = root / ".candidate"
            journal = root / "journal"
            target.write_bytes(b"before\n")
            candidate.write_bytes(b"after\n")
            environment = os.environ.copy()
            environment["T_HUB_ATOMIC_CRASH_AT"] = "prepared"
            subprocess.run(
                [sys.executable, str(HELPER), "exchange", "--target", str(target),
                 "--candidate", str(candidate), "--expected-sha", digest(b"before\n"),
                 "--journal", str(journal)], env=environment, check=False,
            )
            target.chmod(0o600)
            result = subprocess.run(
                [sys.executable, str(HELPER), "recover", "--journal", str(journal)], check=False
            )
            self.assertNotEqual(result.returncode, 0)
            self.assertEqual(target.read_bytes(), b"before\n")
            self.assertEqual(stat.S_IMODE(target.stat().st_mode), 0o600)
            self.assertTrue(journal.exists())

    def test_mismatch_before_restore_and_after_restore_are_recovered(self) -> None:
        helper = load_helper()
        for crash_phase in ("mismatch-before-restore", "restored-before-phase"):
            with self.subTest(phase=crash_phase), tempfile.TemporaryDirectory() as directory:
                root = pathlib.Path(directory)
                target = root / "config"
                candidate = root / ".candidate"
                journal = root / "journal"
                target.write_bytes(b"before\n")
                candidate.write_bytes(b"after\n")
                environment = os.environ.copy()
                environment["T_HUB_ATOMIC_CRASH_AT"] = "prepared"
                subprocess.run(
                    [sys.executable, str(HELPER), "exchange", "--target", str(target),
                     "--candidate", str(candidate), "--expected-sha", digest(b"before\n"),
                     "--journal", str(journal)], env=environment, check=False,
                )
                # Simulate a metadata-only concurrent change in the interval
                # between prepare and rename, then the already-issued exchange.
                target.chmod(0o600)
                helper.durable_exchange(str(target), str(candidate))
                intent_path = journal / "intent.json"
                intent = json.loads(intent_path.read_text())
                intent["phase"] = "mismatch"
                intent_path.write_text(json.dumps(intent, sort_keys=True, separators=(",", ":")) + "\n")
                intent_path.chmod(0o600)
                environment["T_HUB_ATOMIC_CRASH_AT"] = crash_phase
                result = subprocess.run(
                    [sys.executable, str(HELPER), "recover", "--journal", str(journal)],
                    env=environment, check=False,
                )
                self.assertEqual(result.returncode, 89)
                recovered = subprocess.check_output(
                    [sys.executable, str(HELPER), "recover", "--journal", str(journal)], text=True
                ).strip()
                self.assertEqual(recovered, "restored")
                self.assertEqual(target.read_bytes(), b"before\n")
                self.assertEqual(stat.S_IMODE(target.stat().st_mode), 0o600)

    def test_journal_permissions_and_no_content_values(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = pathlib.Path(directory)
            target = root / "config"
            candidate = root / ".candidate"
            journal = root / "journal"
            secret = b"do-not-publish-this-value\n"
            target.write_bytes(secret)
            candidate.write_bytes(b"after\n")
            environment = os.environ.copy()
            environment["T_HUB_ATOMIC_CRASH_AT"] = "prepared"
            subprocess.run(
                [sys.executable, str(HELPER), "exchange", "--target", str(target),
                 "--candidate", str(candidate), "--expected-sha", digest(secret),
                 "--journal", str(journal)], env=environment, check=False,
            )
            intent = journal / "intent.json"
            self.assertEqual(stat.S_IMODE(journal.stat().st_mode), 0o700)
            self.assertEqual(stat.S_IMODE(intent.stat().st_mode), 0o600)
            self.assertNotIn(secret.rstrip(), intent.read_bytes())

    def test_candidate_metadata_failure_never_changes_target(self) -> None:
        helper = load_helper()
        with tempfile.TemporaryDirectory() as directory:
            root = pathlib.Path(directory)
            target = root / "config"
            candidate = root / ".candidate"
            journal = root / "journal"
            target.write_bytes(b"before\n")
            candidate.write_bytes(b"after\n")
            original_chown = helper.os.chown
            try:
                helper.os.chown = lambda *args, **kwargs: (_ for _ in ()).throw(PermissionError("denied"))
                with self.assertRaises(PermissionError):
                    helper.exchange(str(target), str(candidate), digest(b"before\n"), str(journal))
            finally:
                helper.os.chown = original_chown
            self.assertEqual(target.read_bytes(), b"before\n")
            self.assertFalse(journal.exists())

    def test_absent_target_create_is_durable_at_every_phase(self) -> None:
        for phase in ("prepared", "renamed-before-phase", "verified", "committed"):
            with self.subTest(phase=phase), tempfile.TemporaryDirectory() as directory:
                root = pathlib.Path(directory)
                target = root / "new-config"
                candidate = root / ".candidate"
                journal = root / "journal"
                candidate.write_bytes(b"created\n")
                environment = os.environ.copy()
                environment["T_HUB_ATOMIC_CRASH_AT"] = phase
                result = subprocess.run(
                    [sys.executable, str(HELPER), "install", "--target", str(target),
                     "--candidate", str(candidate), "--expected-digest", "absent",
                     "--journal", str(journal)], env=environment, check=False,
                )
                self.assertEqual(result.returncode, 89)
                outcome = subprocess.check_output(
                    [sys.executable, str(HELPER), "recover", "--journal", str(journal)], text=True
                ).strip()
                if phase == "prepared":
                    self.assertEqual(outcome, "restored")
                    self.assertFalse(target.exists())
                    self.assertEqual(candidate.read_bytes(), b"created\n")
                else:
                    self.assertEqual(outcome, "committed")
                    self.assertEqual(target.read_bytes(), b"created\n")

    def test_delete_is_durable_at_every_phase(self) -> None:
        helper = load_helper()
        for phase in (
            "prepared", "renamed-before-phase", "verified", "committed",
            "cleanup", "discard-truncated", "cleaned-before-journal",
        ):
            with self.subTest(phase=phase), tempfile.TemporaryDirectory() as directory:
                root = pathlib.Path(directory)
                target = root / "config"
                journal = root / "journal"
                target.write_bytes(b"delete me\n")
                expected = helper.state_digest(str(target))
                environment = os.environ.copy()
                environment["T_HUB_ATOMIC_CRASH_AT"] = phase
                result = subprocess.run(
                    [sys.executable, str(HELPER), "delete", "--target", str(target),
                     "--expected-digest", expected, "--journal", str(journal)],
                    env=environment, check=False,
                )
                self.assertEqual(result.returncode, 89)
                outcome = subprocess.check_output(
                    [sys.executable, str(HELPER), "recover", "--journal", str(journal)], text=True
                ).strip()
                if phase == "prepared":
                    self.assertEqual(outcome, "restored")
                    self.assertEqual(target.read_bytes(), b"delete me\n")
                else:
                    self.assertEqual(outcome, "committed")
                    self.assertFalse(target.exists())
                    self.assertEqual(list(root.glob("config.t-hub-delete.*")), [])

    def test_normal_unlink_only_delete_completes_for_running_executable(self) -> None:
        helper = load_helper()
        with tempfile.TemporaryDirectory() as directory:
            root = pathlib.Path(directory)
            target = root / "running"
            journal = root / "journal"
            target.write_bytes(pathlib.Path("/bin/sleep").read_bytes())
            target.chmod(0o700)
            process = subprocess.Popen([str(target), "120"])
            try:
                result = subprocess.run(
                    [sys.executable, str(HELPER), "delete", "--target", str(target),
                     "--expected-digest", helper.state_digest(str(target)),
                     "--journal", str(journal), "--unlink-only"],
                    check=False,
                )
                self.assertEqual(result.returncode, 0)
                self.assertFalse(target.exists())
                self.assertEqual(list(root.glob("running.t-hub-delete.*")), [])
                self.assertFalse(journal.exists())
                self.assertIsNone(process.poll())
            finally:
                process.terminate()
                process.wait(timeout=5)

    def test_normal_secure_delete_scrubs_secret_and_removes_evidence(self) -> None:
        helper = load_helper()
        with tempfile.TemporaryDirectory() as directory:
            root = pathlib.Path(directory)
            target = root / "config"
            journal = root / "journal"
            secret = b"normal-delete-secret-value"
            target.write_bytes(secret + b"\n")
            result = subprocess.run(
                [sys.executable, str(HELPER), "delete", "--target", str(target),
                 "--expected-digest", helper.state_digest(str(target)),
                 "--journal", str(journal)],
                check=False,
            )
            self.assertEqual(result.returncode, 0)
            self.assertFalse(target.exists())
            self.assertEqual(list(root.glob("config.t-hub-delete.*")), [])
            self.assertFalse(journal.exists())
            for path in root.rglob("*"):
                if path.is_file():
                    self.assertNotIn(secret, path.read_bytes())

    def test_secure_delete_truncates_recovery_before_unlink(self) -> None:
        helper = load_helper()
        with tempfile.TemporaryDirectory() as directory:
            root = pathlib.Path(directory)
            target = root / "config"
            journal = root / "journal"
            target.write_bytes(b"secret recovery bytes\n")
            expected = helper.state_digest(str(target))
            real_unlink = helper.os.unlink
            observed_recovery = False

            def verifying_unlink(path, *args, **kwargs):
                nonlocal observed_recovery
                if ".t-hub-delete." in os.fspath(path):
                    observed_recovery = True
                    self.assertEqual(pathlib.Path(path).read_bytes(), b"")
                return real_unlink(path, *args, **kwargs)

            helper.os.unlink = verifying_unlink
            try:
                helper.delete(str(target), expected, str(journal))
            finally:
                helper.os.unlink = real_unlink
            self.assertTrue(observed_recovery)
            self.assertFalse(target.exists())
            self.assertFalse(journal.exists())

    def test_delete_restores_same_inode_content_and_metadata_races(self) -> None:
        helper = load_helper()
        for unlink_only in (False, True):
            for mutation in ("content", "metadata"):
                with self.subTest(unlink_only=unlink_only, mutation=mutation), \
                    tempfile.TemporaryDirectory() as directory:
                    root = pathlib.Path(directory)
                    target = root / "target"
                    journal = root / "journal"
                    target.write_bytes(b"expected\n")
                    target.chmod(0o640)
                    expected = helper.state_digest(str(target))
                    original_identity = helper.identity(str(target))
                    real_rename = helper.os.rename
                    raced = False

                    def racing_rename(source, destination, *args, **kwargs):
                        nonlocal raced
                        if os.fspath(source) == str(target) and not raced:
                            raced = True
                            if mutation == "content":
                                target.write_bytes(b"concurrent content\n")
                            else:
                                target.chmod(0o600)
                        return real_rename(source, destination, *args, **kwargs)

                    helper.os.rename = racing_rename
                    try:
                        with self.assertRaises(helper.AtomicError):
                            helper.delete(
                                str(target), expected, str(journal), unlink_only=unlink_only
                            )
                    finally:
                        helper.os.rename = real_rename
                    self.assertEqual(helper.identity(str(target)), original_identity)
                    if mutation == "content":
                        self.assertEqual(target.read_bytes(), b"concurrent content\n")
                    else:
                        self.assertEqual(stat.S_IMODE(target.stat().st_mode), 0o600)
                    self.assertFalse(journal.exists())
                    self.assertEqual(list(root.glob("target.t-hub-delete.*")), [])

    def test_delete_mismatch_restoration_is_crash_recoverable_for_both_policies(self) -> None:
        helper = load_helper()
        for unlink_only in (False, True):
            for crash_point in ("mismatch-before-restore", "restored-before-phase"):
                with self.subTest(unlink_only=unlink_only, crash_point=crash_point), \
                    tempfile.TemporaryDirectory() as directory:
                    root = pathlib.Path(directory)
                    target = root / "target"
                    journal = root / "journal"
                    target.write_bytes(b"expected\n")
                    target.chmod(0o640)
                    expected_identity = helper.identity(str(target))
                    command = [
                        sys.executable, str(HELPER), "delete", "--target", str(target),
                        "--expected-digest", helper.state_digest(str(target)),
                        "--journal", str(journal),
                    ]
                    if unlink_only:
                        command.append("--unlink-only")
                    environment = os.environ.copy()
                    environment["T_HUB_ATOMIC_CRASH_AT"] = "renamed-before-phase"
                    result = subprocess.run(command, env=environment, check=False)
                    self.assertEqual(result.returncode, 89)
                    intent = json.loads((journal / "intent.json").read_text())
                    recovery = pathlib.Path(intent["candidate"])
                    if crash_point == "mismatch-before-restore":
                        recovery.write_bytes(b"concurrent content\n")
                    else:
                        recovery.chmod(0o600)
                    environment["T_HUB_ATOMIC_CRASH_AT"] = crash_point
                    result = subprocess.run(
                        [sys.executable, str(HELPER), "recover", "--journal", str(journal)],
                        env=environment, check=False,
                    )
                    self.assertEqual(result.returncode, 89)
                    if crash_point == "mismatch-before-restore":
                        self.assertFalse(target.exists())
                        self.assertTrue(recovery.exists())
                    else:
                        self.assertTrue(target.exists())
                        self.assertFalse(recovery.exists())
                    outcome = subprocess.check_output(
                        [sys.executable, str(HELPER), "recover", "--journal", str(journal)],
                        text=True,
                    ).strip()
                    self.assertEqual(outcome, "restored")
                    self.assertEqual(helper.identity(str(target)), expected_identity)
                    if crash_point == "mismatch-before-restore":
                        self.assertEqual(target.read_bytes(), b"concurrent content\n")
                    else:
                        self.assertEqual(stat.S_IMODE(target.stat().st_mode), 0o600)
                    self.assertFalse(journal.exists())
                    self.assertEqual(list(root.glob("target.t-hub-delete.*")), [])

    def test_capture_materialize_preserves_exact_metadata_in_restricted_recovery(self) -> None:
        helper = load_helper()
        with tempfile.TemporaryDirectory() as directory:
            root = pathlib.Path(directory)
            state = root / "state"
            state.mkdir(mode=0o700)
            source = root / "source"
            recovery = state / "before.bin"
            candidate = root / "candidate"
            source.write_bytes(b"secret bytes\n")
            source.chmod(0o640)
            os.setxattr(source, "user.t-hub-test", b"metadata-secret")
            subprocess.run(
                [sys.executable, str(HELPER), "capture", "--source", str(source),
                 "--recovery", str(recovery)], check=True, stdout=subprocess.DEVNULL,
            )
            subprocess.run(
                [sys.executable, str(HELPER), "materialize", "--recovery", str(recovery),
                 "--candidate", str(candidate)], check=True,
            )
            self.assertEqual(helper.state_digest(str(source)), helper.state_digest(str(candidate)))
            self.assertEqual(stat.S_IMODE(recovery.stat().st_mode), 0o600)
            self.assertEqual(stat.S_IMODE(pathlib.Path(f"{recovery}.metadata").stat().st_mode), 0o600)

    def test_hard_links_are_refused_without_mutation(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = pathlib.Path(directory)
            target = root / "config"
            linked = root / "linked"
            candidate = root / "candidate"
            target.write_bytes(b"before\n")
            os.link(target, linked)
            candidate.write_bytes(b"after\n")
            result = subprocess.run(
                [sys.executable, str(HELPER), "exchange", "--target", str(target),
                 "--candidate", str(candidate), "--expected-sha", digest(b"before\n")],
                check=False,
            )
            self.assertNotEqual(result.returncode, 0)
            self.assertEqual(target.read_bytes(), b"before\n")
            self.assertEqual(linked.read_bytes(), b"before\n")

    def test_cross_directory_candidate_is_refused(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = pathlib.Path(directory)
            other = root / "other"
            other.mkdir()
            target = root / "config"
            candidate = other / "candidate"
            target.write_bytes(b"before\n")
            candidate.write_bytes(b"after\n")
            result = subprocess.run(
                [sys.executable, str(HELPER), "exchange", "--target", str(target),
                 "--candidate", str(candidate), "--expected-sha", digest(b"before\n")],
                check=False,
            )
            self.assertNotEqual(result.returncode, 0)
            self.assertEqual(target.read_bytes(), b"before\n")
            self.assertEqual(candidate.read_bytes(), b"after\n")

    def test_same_digest_path_swap_after_prepare_is_refused(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = pathlib.Path(directory)
            target = root / "config"
            replacement = root / "replacement"
            candidate = root / "candidate"
            journal = root / "journal"
            target.write_bytes(b"same\n")
            replacement.write_bytes(b"same\n")
            candidate.write_bytes(b"after\n")
            environment = os.environ.copy()
            environment["T_HUB_ATOMIC_CRASH_AT"] = "prepared"
            subprocess.run(
                [sys.executable, str(HELPER), "exchange", "--target", str(target),
                 "--candidate", str(candidate), "--expected-sha", digest(b"same\n"),
                 "--journal", str(journal)], env=environment, check=False,
            )
            os.replace(replacement, target)
            result = subprocess.run(
                [sys.executable, str(HELPER), "recover", "--journal", str(journal)], check=False
            )
            self.assertNotEqual(result.returncode, 0)
            self.assertEqual(target.read_bytes(), b"same\n")
            self.assertTrue(journal.exists())

    def test_claude_rollback_preserves_presence_semantics_and_siblings(self) -> None:
        helper = load_helper()
        cases = (
            ("absent-parent", {}, False, False, "absent"),
            ("empty-parent", {"mcpServers": {}}, True, False, "absent"),
            ("key-absent", {"mcpServers": {"other": {"keep": True}}}, True, False, "absent"),
            ("explicit-null", {"mcpServers": {"t-hub": None}}, True, True, "null"),
        )
        for name, before_document, parent_present, key_present, key_type in cases:
            with self.subTest(name=name), tempfile.TemporaryDirectory() as directory:
                root = pathlib.Path(directory)
                recovery_dir = root / "recovery"
                recovery_dir.mkdir(mode=0o700)
                before_path = root / "before.json"
                recovery = recovery_dir / "claude-before.bin"
                before_path.write_text(json.dumps(before_document) + "\n")
                before_path.chmod(0o640)
                os.setxattr(before_path, "user.t-hub-test", b"before-metadata")
                before_file = helper.capture(str(before_path), str(recovery))
                target = root / "claude.json"
                current = {
                    "cachedMetadata": {"concurrent": "preserved"},
                    "mcpServers": {
                        "t-hub": {"type": "stdio", "command": "/new", "args": [], "env": {}},
                        "concurrent-sibling": {"keep": True},
                    },
                }
                target.write_text(json.dumps(current) + "\n")
                target.chmod(0o600)
                os.setxattr(target, "user.t-hub-test", b"post-metadata")
                node = current["mcpServers"]["t-hub"]
                state = {
                    "before_file": before_file,
                    "before": {
                        "file_presence": "present",
                        "parent": {"presence": parent_present, "type": "object" if parent_present else "absent"},
                        "key": {"presence": key_present, "type": key_type, "digest": "unused"},
                    },
                    "post_structure": {
                        "key": {
                            "presence": True,
                            "type": "object",
                            "digest": helper.canonical_json_digest(node),
                        }
                    },
                    "post": {
                        "presence": "present",
                        "digest": helper.state_digest(str(target)),
                        "description": helper.description(str(target)),
                    },
                }
                state_path = recovery_dir / "claude-state.json"
                state_path.write_text(json.dumps(state) + "\n")
                state_path.chmod(0o600)
                subprocess.run(
                    [sys.executable, str(HELPER), "claude-rollback", "--target", str(target),
                     "--state", str(state_path), "--recovery", str(recovery),
                     "--journal", str(root / "journal")], check=True,
                )
                restored = json.loads(target.read_text())
                self.assertEqual(stat.S_IMODE(target.stat().st_mode), 0o640)
                self.assertEqual(os.getxattr(target, "user.t-hub-test"), b"before-metadata")
                self.assertEqual(restored["cachedMetadata"], {"concurrent": "preserved"})
                self.assertEqual(restored["mcpServers"]["concurrent-sibling"], {"keep": True})
                if key_present:
                    self.assertIn("t-hub", restored["mcpServers"])
                    self.assertIsNone(restored["mcpServers"]["t-hub"])
                else:
                    self.assertNotIn("t-hub", restored["mcpServers"])
                    if parent_present and not before_document["mcpServers"]:
                        # The concurrent sibling keeps the parent non-empty; its
                        # bytes are never removed to recreate an empty snapshot.
                        self.assertIn("mcpServers", restored)

    def test_claude_rollback_preserves_concurrent_metadata_drift(self) -> None:
        helper = load_helper()
        with tempfile.TemporaryDirectory() as directory:
            root = pathlib.Path(directory)
            recovery_dir = root / "recovery"
            recovery_dir.mkdir(mode=0o700)
            before = root / "before.json"
            before.write_text('{"mcpServers":{"t-hub":{"command":"/before"}}}\n')
            before.chmod(0o640)
            recovery = recovery_dir / "before.bin"
            before_file = helper.capture(str(before), str(recovery))
            target = root / "claude.json"
            current = {
                "cachedMetadata": {"concurrent": "preserved"},
                "mcpServers": {
                    "t-hub": {"command": "/owned"},
                    "concurrent-sibling": {"keep": True},
                },
            }
            target.write_text(json.dumps(current) + "\n")
            target.chmod(0o600)
            node = current["mcpServers"]["t-hub"]
            post_description = helper.description(str(target))
            state = {
                "before_file": before_file,
                "before": {
                    "file_presence": "present",
                    "parent": {"presence": True, "type": "object"},
                    "key": {"presence": True, "type": "object", "digest": "unused"},
                },
                "post": {
                    "presence": "present",
                    "digest": helper.description_digest(post_description),
                    "description": post_description,
                },
                "post_structure": {"key": {
                    "presence": True,
                    "type": "object",
                    "digest": helper.canonical_json_digest(node),
                }},
            }
            state_path = recovery_dir / "state.json"
            state_path.write_text(json.dumps(state) + "\n")
            state_path.chmod(0o600)
            target.chmod(0o620)
            os.setxattr(target, "user.t-hub-concurrent", b"metadata-owner")
            subprocess.run(
                [sys.executable, str(HELPER), "claude-rollback", "--target", str(target),
                 "--state", str(state_path), "--recovery", str(recovery),
                 "--journal", str(root / "journal")], check=True,
            )
            restored = json.loads(target.read_text())
            self.assertEqual(stat.S_IMODE(target.stat().st_mode), 0o620)
            self.assertEqual(os.getxattr(target, "user.t-hub-concurrent"), b"metadata-owner")
            self.assertEqual(restored["cachedMetadata"], {"concurrent": "preserved"})
            self.assertEqual(restored["mcpServers"]["concurrent-sibling"], {"keep": True})
            self.assertEqual(restored["mcpServers"]["t-hub"], {"command": "/before"})

    def test_claude_rollback_refuses_changed_owner_and_malformed_parent(self) -> None:
        helper = load_helper()
        with tempfile.TemporaryDirectory() as directory:
            root = pathlib.Path(directory)
            recovery_dir = root / "recovery"
            recovery_dir.mkdir(mode=0o700)
            before = root / "before.json"
            before.write_text("{}\n")
            recovery = recovery_dir / "before.bin"
            before_file = helper.capture(str(before), str(recovery))
            expected_node = {"command": "/expected"}
            state = {
                "before_file": before_file,
                "before": {
                    "file_presence": "present",
                    "parent": {"presence": False, "type": "absent"},
                    "key": {"presence": False, "type": "absent", "digest": "absent"},
                },
                "post_structure": {"key": {
                    "presence": True, "type": "object",
                    "digest": helper.canonical_json_digest(expected_node),
                }},
            }
            state_path = recovery_dir / "state.json"
            state_path.write_text(json.dumps(state) + "\n")
            state_path.chmod(0o600)
            for value in (
                {"mcpServers": {"t-hub": {"command": "/concurrent-owner"}}},
                {"mcpServers": None},
            ):
                target = root / "claude.json"
                target.write_text(json.dumps(value) + "\n")
                snapshot = target.read_bytes()
                result = subprocess.run(
                    [sys.executable, str(HELPER), "claude-rollback", "--target", str(target),
                     "--state", str(state_path), "--recovery", str(recovery),
                     "--journal", str(root / "journal")], check=False,
                )
                self.assertNotEqual(result.returncode, 0)
                self.assertEqual(target.read_bytes(), snapshot)

    def test_capture_refuses_same_content_path_swap_after_open(self) -> None:
        helper = load_helper()
        with tempfile.TemporaryDirectory() as directory:
            root = pathlib.Path(directory)
            state = root / "state"
            state.mkdir(mode=0o700)
            source = root / "source"
            replacement = root / "replacement"
            recovery = state / "before.bin"
            source.write_bytes(b"same content\n")
            replacement.write_bytes(b"same content\n")
            real_open = helper.os.open
            raced = False

            def racing_open(path, flags, *args, **kwargs):
                nonlocal raced
                descriptor = real_open(path, flags, *args, **kwargs)
                if os.fspath(path) == str(source) and not raced:
                    raced = True
                    os.replace(replacement, source)
                return descriptor

            helper.os.open = racing_open
            try:
                with self.assertRaises(helper.AtomicError):
                    helper.capture(str(source), str(recovery))
            finally:
                helper.os.open = real_open
            self.assertFalse(recovery.exists())
            self.assertFalse(pathlib.Path(f"{recovery}.metadata").exists())

    def test_mismatched_prestate_is_restored_without_loss(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = pathlib.Path(directory)
            target = root / "config"
            candidate = root / ".candidate"
            target.write_bytes(b"concurrent-writer\n")
            candidate.write_bytes(b"migration\n")
            result = subprocess.run(
                [sys.executable, str(HELPER), "exchange", "--target", str(target), "--candidate", str(candidate), "--expected-sha", digest(b"old-prestate\n")],
                check=False,
            )
            self.assertNotEqual(result.returncode, 0)
            self.assertEqual(target.read_bytes(), b"concurrent-writer\n")
            self.assertEqual(candidate.read_bytes(), b"migration\n")

    def test_symlink_target_is_refused(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = pathlib.Path(directory)
            real = root / "real"
            target = root / "config"
            candidate = root / ".candidate"
            real.write_bytes(b"real\n")
            candidate.write_bytes(b"candidate\n")
            target.symlink_to(real.name)
            result = subprocess.run(
                [sys.executable, str(HELPER), "exchange", "--target", str(target), "--candidate", str(candidate), "--expected-sha", digest(b"real\n")],
                check=False,
            )
            self.assertNotEqual(result.returncode, 0)
            self.assertEqual(real.read_bytes(), b"real\n")

    def test_publish_is_restricted_and_complete(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            target = pathlib.Path(directory) / "state"
            subprocess.run([sys.executable, str(HELPER), "publish", "--path", str(target), "--value", "hash\nnode\n"], check=True)
            self.assertEqual(target.read_text(), "hash\nnode\n")
            self.assertEqual(stat.S_IMODE(target.stat().st_mode), 0o600)

    def test_discard_removes_regular_file(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            target = pathlib.Path(directory) / "state"
            target.write_text("value")
            subprocess.run([sys.executable, str(HELPER), "discard", "--path", str(target)], check=True)
            self.assertFalse(target.exists())


if __name__ == "__main__":
    unittest.main()
