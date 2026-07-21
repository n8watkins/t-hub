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
