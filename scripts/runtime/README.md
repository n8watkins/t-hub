# Structured runtime pilot

This isolated, opt-in Codex app-server adapter tests structured delivery before production integration.
It does not migrate T-Hub terminals or enable automatic quota recovery.
Run on Linux or WSL with Python 3 and an installed Codex CLI.

```sh
python3 scripts/runtime/codex_runtime.py --experimental --state-dir /tmp/thub-runtime-check --check
python3 -m unittest discover -s scripts/runtime -p 'test_*.py' -v
```

The check initializes the installed provider and discovers its model catalog without starting a model turn.
Omit `--check` to accept JSONL requests on stdin and emit responses and provider events on stdout.
Each request has `requestId`, `method`, and `params` fields.
Use a unique private state directory per runtime, with mode 0700.

```json
{"requestId":"create-1","method":"thread/start","params":{"model":"MODEL_FROM_CATALOG","config":{"model_reasoning_effort":"medium"}}}
{"requestId":"inspect-1","method":"events/replay","params":{"cursor":0,"limit":128}}
```

Supported provider methods are `model/list`, `thread/start`, `thread/resume`, `thread/read`, `turn/start`, `turn/steer`, and `turn/interrupt`.
Thread configuration uses the provider's `config.model_reasoning_effort`; turn overrides use `effort`.
An effort override requires an explicit catalog model and a supported effort.
Steering requires `expectedTurnId`; interruption requires `turnId`.
Provider approval requests are forwarded with a boot-specific `approvalId`.
Only an explicit `approval/respond` request containing that ID and a provider `result` object answers an approval.
A sent approval response is not reported as acknowledged.

SQLite records intent before delivery and provider acknowledgements before returning success.
Repeating an acknowledged request ID with identical input returns its stored result without repeating the operation.
Reusing an ID with different input is refused.
A request whose response was lost remains uncertain across restart and is never automatically replayed.
Inspect the provider thread before deciding whether to issue a new request.
An acknowledgement confirms the provider accepted the operation, not that a turn completed successfully.
Lifecycle events report turn completion separately.

The metadata event journal records provider-confirmed thread model, reasoning effort, and installed CLI version.
It excludes prompts and tool arguments, but persisted RPC responses can contain transcript content.
The private state directory therefore contains sensitive session data.
There is no automatic retention pruning in this experiment: retain the directory while request IDs may be retried, then delete the entire directory after retiring the runtime.
Deleting it removes replay protection.
Event replay is paginated and live queues are bounded; overflow disconnects the provider instead of silently losing events.

Before production adoption, validate actual authenticated turns, approval UX, quota recovery, provider upgrades, and packaged Windows/WSL reconnects.
The subprocess tests cover protocol behavior, duplicate delivery, lost acknowledgements, and restart safety using a deterministic provider fixture.
They do not establish model quality or replace packaged application testing.
