#!/usr/bin/env python3
"""Explicitly opt in to one authenticated, read-only provider turn."""
import argparse
import asyncio
import hashlib
import json
from pathlib import Path
import subprocess
import tempfile
import time

from codex_runtime import CodexRuntime, RuntimeError


async def check(codex, model=None, timeout=120):
    version = subprocess.check_output([codex, "--version"], text=True, timeout=10).strip()
    with tempfile.TemporaryDirectory(prefix="thub-live-runtime-") as directory:
        root = Path(directory)
        work = root / "work"
        work.mkdir()
        runtime = CodexRuntime(root / "state", [codex, "app-server", "--listen", "stdio://"], version)
        started = time.monotonic()
        try:
            await runtime.start()
            selected = model or next((m["model"] for m in runtime.models.values() if m.get("isDefault")), None)
            if selected is None:
                raise RuntimeError("provider reported no default model; supply --model from its catalog")
            thread = await runtime.request("smoke-thread", "thread/start", {
                "model": selected, "cwd": str(work), "ephemeral": True,
                "sandbox": "read-only", "approvalPolicy": "untrusted",
                "developerInstructions": "This is a transport acceptance test. Do not use any tools. Reply only with the exact marker requested by the user.",
            })
            thread_id = thread["thread"]["id"]
            marker = "T_HUB_RUNTIME_OK"
            params = {"threadId": thread_id, "input": [{"type": "text", "text": "Reply exactly: " + marker}]}
            accepted = await runtime.request("smoke-turn", "turn/start", params)
            turn_id = accepted["turn"]["id"]
            messages = []
            event_count = 0
            async with asyncio.timeout(timeout):
                while True:
                    event = await runtime.events.get()
                    event_count += 1
                    if "approvalId" in event:
                        raise RuntimeError("unexpected provider approval; no approval was granted")
                    kind, payload = event.get("method"), event.get("params", {})
                    if payload.get("threadId") != thread_id or payload.get("turnId", payload.get("turn", {}).get("id")) != turn_id:
                        continue
                    if kind == "item/completed":
                        item = payload.get("item", {})
                        if item.get("type") == "agentMessage":
                            messages.append(item.get("text", ""))
                        elif item.get("type") not in {"reasoning", "userMessage"}:
                            raise RuntimeError("provider used tools during the no-tool smoke test")
                    if kind == "turn/completed" and payload.get("turn", {}).get("id") == turn_id:
                        if payload["turn"]["status"] != "completed":
                            raise RuntimeError("provider turn did not complete: " + str(payload["turn"].get("error")))
                        break
            if "".join(messages).strip() != marker:
                raise RuntimeError("provider response did not match the acceptance marker")
            if await runtime.request("smoke-turn", "turn/start", params) != accepted:
                raise RuntimeError("acknowledged delivery did not replay identically")
            if list(work.iterdir()):
                raise RuntimeError("read-only test workspace was modified")
            return {"ok": True, "harnessVersion": version, "effectiveModel": thread.get("model"),
                    "effectiveReasoningEffort": thread.get("reasoningEffort"),
                    "turnStatus": "completed", "duplicateAcknowledgementReplayed": True,
                    "responseSha256": hashlib.sha256(marker.encode()).hexdigest(),
                    "eventCount": event_count, "wallSeconds": round(time.monotonic() - started, 3)}
        finally:
            await runtime.close()


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--live", action="store_true", help="allow one authenticated model turn, which may incur usage")
    parser.add_argument("--codex", default="codex")
    parser.add_argument("--model")
    args = parser.parse_args()
    if not args.live:
        parser.error("--live is required; fixture tests do not use model quota")
    print(json.dumps(asyncio.run(check(args.codex, args.model)), indent=2))
