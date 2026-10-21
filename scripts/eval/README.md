# Orchestration comparison protocol

Use measured task outcomes to evaluate single-agent execution, provider-native helpers, and durable T-Hub Crew.
This scorer does not launch agents or change Captain policy.
No comparative model results are bundled with it.

Run each mode from an isolated checkout of the same base commit with the same root model, reasoning effort, provider version, task prompt, acceptance tests, and resource limits.
Use at least three attempts per task and rotate mode order between attempts.
Record every attempt, including timeouts, refused operations, and failures.
Record child model settings and total child usage in the evidence; comparisons with different helper settings must be labeled as different experiments.
A reviewer should evaluate final patches without seeing the orchestration mode.
Include all child runtime, usage, and human intervention in each trial's totals.
Use measured billing cost or null, never an estimated zero for missing billing evidence.

The initial task set should cover these cases:

| Task | Acceptance evidence |
| --- | --- |
| Isolated frontend defect | Reproduce in Chromium, fix behavior, run the focused regression and typecheck. |
| Cross-layer runtime state | Reproduce with an isolated real tmux process, verify permission, working, completion, and unavailable states through control requests. |
| Lost acknowledgement recovery | Kill an isolated provider fixture after mutation and before its response, restart, and prove no duplicate mutation. |
| Independent multi-file changes | Verify disjoint ownership, integration, final acceptance tests, and any merge intervention. |
| Ambiguous investigation | Verify evidence quality, unsupported claims, unnecessary tool calls, and human clarification burden. |

Use archived prompts and acceptance scripts whose hashes appear in the evidence file.
Keep acceptance checks identical across modes and retain raw logs, final patches, provider usage, and reviewer decisions.
Never run recovery or interruption experiments against a user's live sessions.
The runtime fixtures in `scripts/runtime` and real-tmux control regressions provide reusable starting points for isolated acceptance checks.

Write one JSON object per trial to a JSONL file:

```json
{"task":"lost-acknowledgement","mode":"single","model":"provider-model-id","effort":"medium","harnessVersion":"codex-cli VERSION","baseCommit":"FULL_COMMIT_SHA","attempt":1,"passed":true,"wallSeconds":93.4,"inputTokens":10200,"outputTokens":1400,"costUsd":null,"humanInterventions":0,"unintendedWrites":0,"evidence":{"path":"trial-1.json","sha256":"SHA256_OF_EVIDENCE_FILE"}}
```

`mode` must be `single`, `native-helper`, or `durable-crew`.
Evidence paths are relative to a private evidence directory and must resolve inside it.
Evidence digests establish file integrity; the scorer does not certify the truth of a trial's reported outcome.

```sh
python3 scripts/eval/orchestration.py trials.jsonl --evidence-root ./evidence --repeats 3
python3 -m unittest discover -s scripts/eval -p 'test_*.py' -v
```

The scorer rejects missing modes, missing repetitions, duplicate attempts, configuration drift, invalid measurements, and missing or changed evidence.
It reports acceptance counts, median elapsed time, summed usage, cost, human intervention, and unintended writes for each matched task/configuration/mode.
It does not collapse quality and safety into a single score or automatically select a winner.
Prefer a simpler execution strategy only when acceptance quality and unintended-write results hold up across the representative task set.
Durable ownership and restart recovery may still justify Crew even when a smaller execution strategy is faster on short tasks.
