# Agent-Session Smoke Procedure - 0.3.106

Run this procedure after local verification and before any release activation.

Back up the Windows registry, WSL CLI binaries, and Codex and Claude MCP configuration first.

1. Install the Windows 0.3.106 candidate and the matching WSL CLI from the same commit.
2. Start a fresh T-Hub control-capability session and a fresh Codex conversation.
3. Register the repository without Powder fields and commission one Captain.
4. Verify the MCP catalog contains `start_agent`, `agent_checkpoint`, and `agent_events`, and contains no Powder tools.
5. Start one bounded read-only Codex agent and one bounded read-only Claude agent.
6. Verify each agent has a durable record, an owning Captain, a directory, a provider, a running state, and a lifecycle event.
7. From each provider session, write a checkpoint and verify the event cursor advances exactly once.
8. Restart T-Hub and verify the records, stages, provider identities, and event cursors recover.
9. Close one agent safely and verify stopped history remains available through explicit pagination.
10. Inspect diagnostics and the network sentinel and confirm no Powder connection or mutation occurred.

Record the reviewed commit, installer hashes, installed component versions, test results, and residual risks before requesting push or merge authorization.
