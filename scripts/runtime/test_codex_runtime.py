import asyncio
import json
from pathlib import Path
import sys
import tempfile
import unittest

from codex_runtime import CodexRuntime, RuntimeError

FAKE = r'''
import json, sys
from pathlib import Path
counter = Path(sys.argv[1])
def send(value): print(json.dumps(value), flush=True)
for line in sys.stdin:
    m = json.loads(line); method = m.get("method"); p = m.get("params", {})
    if method == "initialized": continue
    if method is None:
        send({"method":"turn/completed","params":{"threadId":"thread-1","turn":{"id":"turn-1","status":"completed"}}})
        continue
    result = {}
    if method == "model/list": result = {"data":[{"model":"fixture-model", "supportedReasoningEfforts":[{"reasoningEffort":"medium"}]}],"nextCursor":None}
    if method in ("thread/start", "thread/resume"):
        counter.write_text(str(int(counter.read_text()) + 1))
        if p.get("cwd") == "crash": sys.exit(0)
        result = {"thread":{"id":"thread-1"},"model":"fixture-model","reasoningEffort":"medium"}
    if method == "turn/start":
        send({"id":77,"method":"item/commandExecution/requestApproval","params":{"threadId":"thread-1","turnId":"turn-1"}})
        result = {"turn":{"id":"turn-1","status":"inProgress"}}
    if method == "turn/steer": result = {"turnId":p["expectedTurnId"]}
    send({"id":m["id"],"result":result})
'''


class RuntimeTests(unittest.IsolatedAsyncioTestCase):
    async def asyncSetUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.root = Path(self.temp.name)
        self.server = self.root / "provider.py"
        self.server.write_text(FAKE)
        self.counter = self.root / "count"
        self.counter.write_text("0")
        self.runtime = await self.open_runtime()

    async def open_runtime(self):
        runtime = CodexRuntime(self.root / "state", [sys.executable, str(self.server), str(self.counter)], "fixture 1")
        await runtime.start()
        return runtime

    async def asyncTearDown(self):
        await self.runtime.close()
        self.temp.cleanup()

    async def test_provider_response_is_durable_and_duplicate_request_never_restarts_thread(self):
        result = await self.runtime.request("create-1", "thread/start", {"model":"fixture-model"})
        self.assertEqual(result["thread"]["id"], "thread-1")
        await self.runtime.close()
        self.runtime = await self.open_runtime()
        replay = await self.runtime.request("create-1", "thread/start", {"model":"fixture-model"})
        self.assertEqual(replay, result)
        self.assertEqual(self.counter.read_text(), "1")
        configs = [e for e in self.runtime.replay() if e["kind"] == "execution/configuration"]
        self.assertEqual(configs[0]["metadata"]["effectiveModel"], "fixture-model")
        self.assertEqual(configs[0]["metadata"]["harnessVersion"], "fixture 1")
        with self.assertRaisesRegex(RuntimeError, "different input"):
            await self.runtime.request("create-1", "thread/start", {})

    async def test_lost_response_does_not_repeat_mutation_after_restart(self):
        with self.assertRaises(RuntimeError):
            await self.runtime.request("create-crash", "thread/start", {"cwd":"crash"})
        await self.runtime.close()
        self.runtime = await self.open_runtime()
        with self.assertRaisesRegex(RuntimeError, "uncertain"):
            await self.runtime.request("create-crash", "thread/start", {"cwd":"crash"})
        self.assertEqual(self.counter.read_text(), "1")

    async def test_approval_requires_exact_pending_id_and_write_is_not_acknowledgement(self):
        await self.runtime.request("turn-1", "turn/start", {"threadId":"thread-1","input":[]})
        event = await asyncio.wait_for(self.runtime.events.get(), 1)
        self.assertEqual(event["method"], "item/commandExecution/requestApproval")
        with self.assertRaisesRegex(RuntimeError, "expired"):
            await self.runtime.request("foreign", "approval/respond", {"approvalId":"other:77","result":{"decision":"accept"}})
        response = await self.runtime.request("decision-1", "approval/respond", {"approvalId":event["approvalId"],"result":{"decision":"decline"}})
        self.assertEqual(response, {"sent":True,"acknowledged":False})
        complete = await asyncio.wait_for(self.runtime.events.get(), 1)
        self.assertEqual(complete["method"], "turn/completed")
        self.assertTrue(any(e["kind"] == "turn/completed" for e in self.runtime.replay()))

    async def test_steering_and_configuration_fail_before_provider_mutation(self):
        for method, params in [("turn/steer", {"threadId":"thread-1"}),
                               ("turn/interrupt", {"threadId":"thread-1"}),
                               ("thread/start", {"model":"not-in-catalog"}),
                               ("thread/start", {"model":"fixture-model","effort":"medium"}),
                               ("thread/start", {"model":"fixture-model","config":{"model_reasoning_effort":"invalid"}}),
                               ("turn/start", {"model":"fixture-model","effort":"invalid"})]:
            with self.assertRaises(RuntimeError):
                await self.runtime.request("bad", method, params)
        self.assertEqual(self.counter.read_text(), "0")
        result = await self.runtime.request("steer", "turn/steer", {"threadId":"thread-1","expectedTurnId":"turn-1","input":[]})
        self.assertEqual(result, {"turnId":"turn-1"})

    async def test_exclusive_state_and_metadata_cursor(self):
        with self.assertRaisesRegex(RuntimeError, "another runtime"):
            CodexRuntime(self.root / "state", [], "fixture")
        events = self.runtime.replay(limit=1)
        self.assertEqual(len(events), 1)
        self.assertEqual(self.runtime.replay(cursor=events[-1]["cursor"]), [])
        with self.assertRaises(RuntimeError):
            self.runtime.replay(limit=1000)


if __name__ == "__main__":
    unittest.main()
