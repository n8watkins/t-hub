import copy
import hashlib
from pathlib import Path
import tempfile
import unittest
from orchestration import MODES, summarize


class EvaluationTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        (self.root / "evidence.json").write_text('{"fixture":true}')
        self.rows = [dict(task="recovery", mode=mode, model="fixture-model", effort="medium",
                          harnessVersion="fixture 1", baseCommit="fixture-commit", attempt=attempt,
                          passed=attempt != 2, wallSeconds=attempt * 10, inputTokens=10,
                          outputTokens=5, costUsd=None, humanInterventions=0, unintendedWrites=0,
                          evidence={"path":"evidence.json", "sha256":hashlib.sha256((self.root / "evidence.json").read_bytes()).hexdigest()})
                     for mode in sorted(MODES) for attempt in range(1, 4)]

    def test_summary_keeps_failures_and_unknown_cost(self):
        result = summarize(self.rows, self.root)
        self.assertEqual(len(result), 3)
        self.assertEqual(result[0]["passed"], 2)
        self.assertEqual(result[0]["medianWallSeconds"], 20)
        self.assertIsNone(result[0]["costUsd"])

    def test_missing_or_duplicate_trials_are_rejected(self):
        for rows in (self.rows[:-1], self.rows + [self.rows[0]], [r for r in self.rows if r["attempt"] != 2]):
            with self.assertRaises(ValueError):
                summarize(rows, self.root)

    def test_configuration_drift_is_not_a_matched_comparison(self):
        rows = copy.deepcopy(self.rows)
        rows[0]["model"] = "different-model"
        with self.assertRaisesRegex(ValueError, "identical configuration"):
            summarize(rows, self.root)

    def test_evidence_and_measurements_are_required(self):
        for field, value in [("wallSeconds", float("nan")), ("unintendedWrites", -1),
                             ("evidence", {"path":"missing"}), ("passed", "yes")]:
            rows = copy.deepcopy(self.rows)
            rows[0][field] = value
            with self.assertRaises(ValueError):
                summarize(rows, self.root)
        (self.root / "evidence.json").write_text("changed")
        with self.assertRaisesRegex(ValueError, "digest"):
            summarize(self.rows, self.root)
