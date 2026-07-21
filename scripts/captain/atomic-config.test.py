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
        for phase in ("prepared", "renamed-before-phase", "verified", "committed"):
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
