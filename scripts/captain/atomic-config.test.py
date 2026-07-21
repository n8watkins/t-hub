#!/usr/bin/env python3

import hashlib
import os
import pathlib
import stat
import subprocess
import tempfile
import unittest


HELPER = pathlib.Path(__file__).with_name("atomic-config.py")


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
            subprocess.run(
                [str(HELPER), "exchange", "--target", str(target), "--candidate", str(candidate), "--expected-sha", digest(b"before\n")],
                check=True,
            )
            self.assertEqual(target.read_bytes(), b"after\n")
            self.assertEqual(candidate.read_bytes(), b"before\n")
            self.assertEqual(stat.S_IMODE(target.stat().st_mode), 0o640)

    def test_mismatched_prestate_is_restored_without_loss(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = pathlib.Path(directory)
            target = root / "config"
            candidate = root / ".candidate"
            target.write_bytes(b"concurrent-writer\n")
            candidate.write_bytes(b"migration\n")
            result = subprocess.run(
                [str(HELPER), "exchange", "--target", str(target), "--candidate", str(candidate), "--expected-sha", digest(b"old-prestate\n")],
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
                [str(HELPER), "exchange", "--target", str(target), "--candidate", str(candidate), "--expected-sha", digest(b"real\n")],
                check=False,
            )
            self.assertNotEqual(result.returncode, 0)
            self.assertEqual(real.read_bytes(), b"real\n")

    def test_publish_is_restricted_and_complete(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            target = pathlib.Path(directory) / "state"
            subprocess.run([str(HELPER), "publish", "--path", str(target), "--value", "hash\nnode\n"], check=True)
            self.assertEqual(target.read_text(), "hash\nnode\n")
            self.assertEqual(stat.S_IMODE(target.stat().st_mode), 0o600)

    def test_discard_removes_regular_file(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            target = pathlib.Path(directory) / "state"
            target.write_text("value")
            subprocess.run([str(HELPER), "discard", "--path", str(target)], check=True)
            self.assertFalse(target.exists())


if __name__ == "__main__":
    unittest.main()
