from pathlib import Path
import sys
import tempfile
import unittest

from live_smoke import check
from codex_runtime import RuntimeError

SERVER = '''import json, sys
if '--version' in sys.argv:
    print('fixture 1'); sys.exit(0)
def emit(value): print(json.dumps(value), flush=True)
for line in sys.stdin:
    request = json.loads(line)
    method = request.get('method')
    if method == 'initialized': continue
    result = {}
    if method == 'model/list':
        result = {'data':[{'model':'fixture-model','isDefault':True}], 'nextCursor':None}
    elif method == 'thread/start':
        assert request['params']['ephemeral'] is True
        assert request['params']['sandbox'] == 'read-only'
        assert request['params']['approvalPolicy'] == 'untrusted'
        result = {'thread':{'id':'thread'},'model':'fixture-model','reasoningEffort':'medium'}
    elif method == 'turn/start':
        for event in EVENTS: emit(event)
        result = {'turn':{'id':'turn'}}
    emit({'id':request['id'],'result':result})
'''


class LiveSmokeTests(unittest.IsolatedAsyncioTestCase):
    async def run_fixture(self, events):
        with tempfile.TemporaryDirectory() as directory:
            executable = Path(directory) / 'codex'
            executable.write_text('#!' + sys.executable + '\nEVENTS = ' + repr(events) + '\n' + SERVER)
            executable.chmod(0o700)
            return await check(str(executable), timeout=2)

    @staticmethod
    def completion(text='T_HUB_RUNTIME_OK', status='completed'):
        return [
            {'method':'item/completed','params':{'threadId':'thread','turnId':'turn','item':{'type':'agentMessage','text':text}}},
            {'method':'turn/completed','params':{'threadId':'thread','turn':{'id':'turn','status':status}}},
        ]

    async def test_completed_response_and_replayed_acknowledgement(self):
        result = await self.run_fixture(self.completion())
        self.assertTrue(result['ok'])
        self.assertTrue(result['duplicateAcknowledgementReplayed'])
        self.assertEqual(result['effectiveModel'], 'fixture-model')

    async def test_invalid_completion_is_rejected(self):
        for events in [self.completion('wrong'), self.completion(status='failed')]:
            with self.assertRaises(RuntimeError):
                await self.run_fixture(events)

    async def test_approval_and_tool_use_are_not_accepted(self):
        for event in [
            {'id':1,'method':'item/commandExecution/requestApproval','params':{}},
            {'method':'item/completed','params':{'threadId':'thread','turnId':'turn','item':{'type':'commandExecution'}}},
        ]:
            with self.assertRaises(RuntimeError):
                await self.run_fixture([event] + self.completion())
