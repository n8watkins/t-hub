#!/usr/bin/env python3
"""Opt-in structured Codex runtime pilot. No PTY input or implicit approvals.

The local JSONL interface accepts {requestId, method, params}. Methods are the
allowlisted provider operations below plus approval/respond. Responses and
notifications are JSONL. Uncertain mutations are never automatically replayed.
"""
from __future__ import annotations

import argparse
import asyncio
import fcntl
import hashlib
import json
import os
from pathlib import Path
import sqlite3
import sys
import time
import uuid

MAX_FRAME = 4 * 1024 * 1024
METHODS = {"model/list", "thread/start", "thread/resume", "thread/read",
           "turn/start", "turn/steer", "turn/interrupt"}
LIFECYCLE = {"thread/started", "thread/status/changed", "turn/started", "turn/completed"}


class RuntimeError(Exception):
    """A refused operation or unavailable runtime, never a successful delivery."""


class CodexRuntime:
    def __init__(self, state_dir: Path, command: list[str], version: str):
        state_dir.mkdir(mode=0o700, parents=True, exist_ok=True)
        if state_dir.is_symlink() or state_dir.stat().st_uid != os.getuid():
            raise RuntimeError("state directory must be owned by the current user and not a symlink")
        if state_dir.stat().st_mode & 0o077:
            raise RuntimeError("state directory must have mode 0700")
        lock_fd = os.open(state_dir / "runtime.lock", os.O_CREAT | os.O_RDWR | os.O_NOFOLLOW, 0o600)
        self.lock = os.fdopen(lock_fd, "r+")
        try:
            fcntl.flock(self.lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
        except OSError:
            self.lock.close()
            raise RuntimeError("another runtime owns this state directory") from None
        db_path = state_dir / "runtime.sqlite"
        fd = os.open(db_path, os.O_CREAT | os.O_RDWR | os.O_NOFOLLOW, 0o600)
        os.close(fd)
        self.db = sqlite3.connect(db_path)
        self.db.execute("PRAGMA journal_mode=WAL")
        self.db.execute("PRAGMA synchronous=FULL")
        self.db.executescript("""
          CREATE TABLE IF NOT EXISTS requests (
            id TEXT PRIMARY KEY, digest TEXT NOT NULL, method TEXT NOT NULL,
            state TEXT NOT NULL, response TEXT, created REAL NOT NULL);
          CREATE TABLE IF NOT EXISTS events (
            cursor INTEGER PRIMARY KEY AUTOINCREMENT, kind TEXT NOT NULL,
            metadata TEXT NOT NULL, created REAL NOT NULL);
        """)
        self.command, self.version = command, version
        self.pending: dict[str, asyncio.Future] = {}
        self.approvals: dict[str, dict] = {}
        self.events: asyncio.Queue = asyncio.Queue(maxsize=256)
        self.models: dict[str, dict] = {}
        self.process = None
        self.reader = None
        self.write_lock = asyncio.Lock()
        self.boot = uuid.uuid4().hex
        self.failure = None

    def record_event(self, kind: str, metadata: dict):
        # Metadata only: no prompt text, generated output, credentials or tool arguments.
        with self.db:
            self.db.execute("INSERT INTO events(kind, metadata, created) VALUES(?,?,?)",
                            (kind, json.dumps(metadata), time.time()))

    async def start(self):
        self.process = await asyncio.create_subprocess_exec(*self.command,
            stdin=asyncio.subprocess.PIPE, stdout=asyncio.subprocess.PIPE,
            stderr=sys.stderr, limit=MAX_FRAME)
        self.reader = asyncio.create_task(self.read_messages())
        await self.rpc("initialize", {"clientInfo": {"name": "t_hub_runtime_pilot", "version": "1"},
                                      "capabilities": {"experimentalApi": False}})
        await self.send({"method": "initialized"})
        cursor = None
        for _ in range(100):
            result = await self.rpc("model/list", {"limit": 100, "cursor": cursor})
            for model in result["data"]:
                self.models[model["model"]] = model
            cursor = result.get("nextCursor")
            if not cursor:
                break
        else:
            raise RuntimeError("model catalog exceeded its pagination bound")
        self.record_event("runtime/connected", {"boot": self.boot, "harnessVersion": self.version})

    async def send(self, value: dict):
        if self.failure or not self.process or self.process.returncode is not None:
            raise RuntimeError("provider disconnected; inspect persisted request state before retrying")
        encoded = json.dumps(value, separators=(",", ":")).encode() + b"\n"
        if len(encoded) > MAX_FRAME:
            raise RuntimeError("request exceeds frame limit")
        async with self.write_lock:
            self.process.stdin.write(encoded)
            await self.process.stdin.drain()

    async def rpc(self, method: str, params: dict):
        rpc_id = uuid.uuid4().hex
        future = asyncio.get_running_loop().create_future()
        self.pending[rpc_id] = future
        try:
            await self.send({"id": rpc_id, "method": method, "params": params})
            return await asyncio.wait_for(future, timeout=30)
        finally:
            self.pending.pop(rpc_id, None)

    async def read_messages(self):
        try:
            while raw := await self.process.stdout.readline():
                if len(raw) > MAX_FRAME:
                    raise RuntimeError("provider frame exceeds limit")
                message = json.loads(raw)
                if not isinstance(message, dict):
                    raise RuntimeError("invalid provider envelope")
                if "method" not in message and "id" in message:
                    future = self.pending.get(message["id"])
                    if future and not future.done():
                        if "error" in message:
                            future.set_exception(RuntimeError(f"provider refused request: {message['error']}"))
                        else:
                            future.set_result(message.get("result"))
                    continue
                kind = message.get("method", "unknown")
                params = message.get("params") or {}
                if "id" in message:
                    # The caller must explicitly answer this exact boot's server request.
                    key = f"{self.boot}:{message['id']}"
                    self.approvals[key] = message
                    message = {**message, "approvalId": key}
                if kind in LIFECYCLE or "id" in message:
                    thread = params.get("thread") or {}
                    turn = params.get("turn") or {}
                    self.record_event(kind, {"threadId": params.get("threadId", thread.get("id")),
                        "turnId": params.get("turnId", turn.get("id")), "status": turn.get("status"),
                        "approvalId": message.get("approvalId")})
                self.events.put_nowait(message)
            raise RuntimeError("provider stream ended")
        except (Exception, asyncio.CancelledError) as error:
            self.failure = str(error) or "provider reader stopped"
            for future in self.pending.values():
                if not future.done():
                    future.set_exception(RuntimeError(self.failure))
            if self.process.returncode is None:
                self.process.terminate()

    def validate_configuration(self, method: str, params: dict):
        if method not in {"thread/start", "thread/resume", "turn/start"}:
            return
        model = params.get("model")
        if model is not None and model not in self.models:
            raise RuntimeError("requested model is absent from the provider catalog")
        config = params.get("config") or {}
        if not isinstance(config, dict):
            raise RuntimeError("config must be an object")
        if method != "turn/start" and "effort" in params:
            raise RuntimeError("thread reasoning effort belongs in config.model_reasoning_effort")
        effort = params.get("effort") if method == "turn/start" else config.get("model_reasoning_effort")
        if effort is not None:
            if model is None:
                raise RuntimeError("an explicit model is required when overriding reasoning effort")
            supported = {item["reasoningEffort"] for item in self.models[model].get("supportedReasoningEfforts", [])}
            if effort not in supported:
                raise RuntimeError("reasoning effort is unsupported by the selected model")

    async def request(self, request_id: str, method: str, params: dict):
        if not isinstance(request_id, str) or not request_id or len(request_id) > 128:
            raise RuntimeError("requestId must be a nonempty string of at most 128 characters")
        if not isinstance(params, dict) or method not in METHODS | {"approval/respond"}:
            raise RuntimeError("unsupported runtime operation")
        if method == "turn/steer" and not params.get("expectedTurnId"):
            raise RuntimeError("steering requires expectedTurnId")
        if method == "turn/interrupt" and not params.get("turnId"):
            raise RuntimeError("interruption requires turnId")
        digest = hashlib.sha256(json.dumps([method, params], sort_keys=True, separators=(",", ":")).encode()).hexdigest()
        previous = self.db.execute("SELECT digest,state,response FROM requests WHERE id=?", (request_id,)).fetchone()
        if previous:
            if previous[0] != digest:
                raise RuntimeError("requestId is already bound to different input")
            if previous[1] == "acknowledged":
                return json.loads(previous[2])
            raise RuntimeError("delivery is uncertain; inspect thread state instead of replaying the mutation")
        self.validate_configuration(method, params)
        with self.db:
            self.db.execute("INSERT INTO requests VALUES(?,?,?,?,?,?)",
                            (request_id, digest, method, "uncertain", None, time.time()))
        if method == "approval/respond":
            approval = self.approvals.get(params.get("approvalId"))
            if approval is None or not isinstance(params.get("result"), dict):
                raise RuntimeError("unknown or expired approval request")
            await self.send({"id": approval["id"], "result": params["result"]})
            del self.approvals[params["approvalId"]]
            # A write is not a provider acknowledgement. Leave this operation
            # uncertain until lifecycle events show the resulting provider state.
            return {"sent": True, "acknowledged": False}
        result = await self.rpc(method, params)
        if method in {"thread/start", "thread/resume"}:
            self.record_event("execution/configuration", {
                "threadId": result.get("thread", {}).get("id"), "requestedModel": params.get("model"),
                "effectiveModel": result.get("model"), "effectiveReasoningEffort": result.get("reasoningEffort"),
                "harnessVersion": self.version, "source": "provider-response"})
        with self.db:
            self.db.execute("UPDATE requests SET state='acknowledged',response=? WHERE id=?",
                            (json.dumps(result), request_id))
        return result

    def replay(self, cursor: int = 0, limit: int = 128):
        if cursor < 0 or not 1 <= limit <= 128:
            raise RuntimeError("invalid event cursor or limit")
        return [{"cursor": row[0], "kind": row[1], "metadata": json.loads(row[2]), "createdAt": row[3]}
                for row in self.db.execute("SELECT cursor,kind,metadata,created FROM events WHERE cursor>? ORDER BY cursor LIMIT ?", (cursor, limit))]

    async def close(self):
        if self.process and self.process.returncode is None:
            self.process.terminate()
            try:
                await asyncio.wait_for(self.process.wait(), timeout=5)
            except asyncio.TimeoutError:
                self.process.kill()
                await self.process.wait()
        if self.reader:
            # A descendant can retain stdout after the direct child exits.
            # Never let an inherited pipe prevent shutdown and lock release.
            try:
                await asyncio.wait_for(self.reader, timeout=5)
            except asyncio.TimeoutError:
                pass
        self.db.close()
        self.lock.close()


async def serve(args):
    version = await asyncio.create_subprocess_exec(args.codex, "--version", stdout=asyncio.subprocess.PIPE)
    stdout, _ = await asyncio.wait_for(version.communicate(), timeout=10)
    if version.returncode:
        raise RuntimeError("cannot determine Codex version")
    runtime = CodexRuntime(args.state_dir, [args.codex, "app-server", "--listen", "stdio://"], stdout.decode().strip())
    def emit(value):
        print(json.dumps(value), flush=True)
    async def relay():
        while True:
            emit({"event": await runtime.events.get()})
    relay_task = None
    try:
        await runtime.start()
        if args.check:
            emit({"ok": True, "harnessVersion": runtime.version, "models": list(runtime.models)})
            return
        relay_task = asyncio.create_task(relay())
        # Thread input leaves the provider event reader active while waiting.
        while line := await asyncio.to_thread(sys.stdin.readline, MAX_FRAME + 1):
            request = {}
            try:
                if len(line.encode()) > MAX_FRAME:
                    raise RuntimeError("input exceeds frame limit")
                request = json.loads(line)
                if not isinstance(request, dict):
                    raise RuntimeError("input must be a JSON object")
                if request.get("method") == "events/replay":
                    result = runtime.replay(**request.get("params", {}))
                else:
                    result = await runtime.request(request.get("requestId"), request.get("method"), request.get("params", {}))
                emit({"requestId": request.get("requestId"), "result": result})
            except (RuntimeError, ValueError, TypeError, asyncio.TimeoutError) as error:
                emit({"requestId": request.get("requestId") if isinstance(request, dict) else None, "error": str(error)})
    finally:
        if relay_task:
            relay_task.cancel()
            await asyncio.gather(relay_task, return_exceptions=True)
        await runtime.close()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--experimental", action="store_true", help="explicitly enable the structured runtime pilot")
    parser.add_argument("--state-dir", required=True, type=Path)
    parser.add_argument("--codex", default="codex")
    parser.add_argument("--check", action="store_true", help="verify handshake and model discovery without starting a turn")
    args = parser.parse_args()
    if not args.experimental:
        parser.error("the pilot requires --experimental; existing T-Hub terminals are not migrated")
    os.umask(0o077)
    asyncio.run(serve(args))


if __name__ == "__main__":
    main()
