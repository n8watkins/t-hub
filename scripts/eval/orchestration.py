#!/usr/bin/env python3
"""Validate matched orchestration trials and summarize measured outcomes."""
import argparse
import hashlib
import json
from pathlib import Path
import statistics

MODES = {"single", "native-helper", "durable-crew"}
METRICS = ("wallSeconds", "inputTokens", "outputTokens", "costUsd", "humanInterventions", "unintendedWrites")


def summarize(records, evidence_root, repeats=3):
    if repeats < 1 or not records:
        raise ValueError("nonempty trials and positive repeats are required")
    matched = {}
    seen = set()
    for row in records:
        for field in ("task", "mode", "model", "effort", "harnessVersion", "baseCommit"):
            if not isinstance(row.get(field), str) or not row[field]:
                raise ValueError(f"missing trial identity: {field}")
        if row["mode"] not in MODES or type(row.get("passed")) is not bool:
            raise ValueError("invalid mode or acceptance outcome")
        attempt = row.get("attempt")
        if type(attempt) is not int or not 1 <= attempt <= repeats:
            raise ValueError("attempt is outside the required repeat range")
        key = (row["task"], row["model"], row["effort"], row["harnessVersion"], row["baseCommit"], attempt)
        identity = (*key, row["mode"])
        if identity in seen:
            raise ValueError("duplicate trial; failed attempts must not be overwritten")
        seen.add(identity)
        matched.setdefault(key, set()).add(row["mode"])
        for metric in METRICS:
            value = row.get(metric)
            if metric == "costUsd" and value is None:
                continue  # Missing billing evidence is not zero cost.
            if type(value) not in (int, float) or not 0 <= value < float("inf"):
                raise ValueError(f"invalid or missing measurement: {metric}")
        artifact = row.get("evidence", {})
        root = evidence_root.resolve()
        path = (root / artifact.get("path", "")).resolve()
        if not path.is_relative_to(root) or not path.is_file():
            raise ValueError("trial requires an evidence file inside the evidence directory")
        if hashlib.sha256(path.read_bytes()).hexdigest() != artifact.get("sha256"):
            raise ValueError("trial evidence digest mismatch")
    if any(modes != MODES for modes in matched.values()):
        raise ValueError("each trial must compare all three modes with identical configuration")
    groups = {key[:-1] for key in matched}
    if any({key[-1] for key in matched if key[:-1] == group} != set(range(1, repeats + 1)) for group in groups):
        raise ValueError("all repeat attempts must be recorded, including failures")
    output = []
    for group in sorted(groups):
        for mode in sorted(MODES):
            rows = [row for row in records if (row["task"], row["model"], row["effort"], row["harnessVersion"], row["baseCommit"]) == group and row["mode"] == mode]
            output.append({
                "task": group[0], "model": group[1], "effort": group[2],
                "harnessVersion": group[3], "baseCommit": group[4], "mode": mode,
                "trials": len(rows), "passed": sum(row["passed"] for row in rows),
                "medianWallSeconds": statistics.median(row["wallSeconds"] for row in rows),
                **{metric: None if any(row.get(metric) is None for row in rows) else sum(row[metric] for row in rows) for metric in METRICS if metric != "wallSeconds"},
            })
    return output


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("trials", type=Path)
    parser.add_argument("--evidence-root", type=Path, required=True)
    parser.add_argument("--repeats", type=int, default=3)
    args = parser.parse_args()
    rows = [json.loads(line) for line in args.trials.read_text().splitlines() if line.strip()]
    print(json.dumps(summarize(rows, args.evidence_root, args.repeats), indent=2))
