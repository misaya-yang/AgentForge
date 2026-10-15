"""Offline SDK lifecycle bridge checks; no provider or database is contacted."""
import ast
import asyncio
import copy
import json
from pathlib import Path
import sys
import tempfile
import unittest
from unittest.mock import patch

ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT / 'apps/sdk-agent-worker'))
from sdk_agent_worker.contract import Launch
from sdk_agent_worker.capabilities import GovernedTools, validate_bridge, ensure_access, _request
from sdk_agent_worker.knowledge import ensure_profile, tools_for, ensure_access as knowledge_access


def body(stream=False):
    return dict(protocol='ai-platform/sdk-agent/v1', framework='openai_agents',
                tenant_id='tenant', user_id='user', service_id='service', request_id='request',
                session_id='session', agent=dict(model='model', instructions='Help', max_turns=2),
                input='Hello', stream=stream,
                capability_bridge=dict(grant='a'*64,run_id='00000000-0000-4000-8000-000000000001',tools=[],timeout_seconds=300))


class SdkRunLineage(unittest.TestCase):
    def test_pure_sync_and_stream_have_identity_without_tools(self):
        for stream in (False, True):
            launch=Launch.parse(body(stream),'x'*32)
            self.assertEqual(launch.capability_bridge['timeout_seconds'],300)
            self.assertEqual(GovernedTools(launch,None).tools,{})
            self.assertNotIn('knowledge_retrieve',tools_for(launch))
    def test_empty_tools_cannot_dispatch(self):
        launch=Launch.parse(body(),'x'*32)
        with patch.dict('os.environ',{'SDK_AGENT_GATEWAY_URL':'https://gateway.example'}):
            with self.assertRaisesRegex(PermissionError,'not_enabled'):
                _request(launch,'execute',{})
        with self.assertRaisesRegex(PermissionError,'identity_required'):
            asyncio.run(GovernedTools(launch,None).invoke('platform_capability_0',{},'real-call'))
    def test_empty_bridge_still_checks_durable_admission(self):
        launch=Launch.parse(body(),'x'*32)
        with patch('sdk_agent_worker.capabilities._request',return_value={'authorized':False}) as request:
            with self.assertRaises(PermissionError):asyncio.run(ensure_access(launch))
            request.assert_called_once_with(launch,'admit',{})
    def test_terminal_authority_check_does_not_spend_exhausted_kb_quota(self):
        candidate=body();candidate['read_bridge']={'grant':'a'*64,'dataset_ids':['00000000-0000-4000-8000-000000000002']}
        launch=Launch.parse(candidate,'x'*32)
        calls=0
        def knowledge_request(launch,operation,args=None):
            nonlocal calls
            cost=1 if operation=='admit' else 2 # Gateway checks before/after retrieval.
            if calls+cost>32:raise PermissionError('sdk_read_not_allowed')
            calls+=cost
            return {'authorized':True} if operation=='admit' else {'passages':[]}
        async def run():
            await knowledge_access(launch) # Server admission.
            await knowledge_access(launch) # Child admission.
            retrieve=tools_for(launch)['knowledge_retrieve'][2]
            for _ in range(15):
                await retrieve({'dataset_id':candidate['read_bridge']['dataset_ids'][0],'query':'query','top_k':1})
            self.assertEqual(calls,32)
            await ensure_access(launch) # The exact non-consuming terminal authority check.
            self.assertEqual(calls,32)
            with self.assertRaises(PermissionError):await knowledge_access(launch)
        with patch('sdk_agent_worker.knowledge._request',side_effect=knowledge_request), patch('sdk_agent_worker.capabilities._request',return_value={'authorized':True}):
            asyncio.run(run())
        source=(ROOT/'apps/sdk-agent-worker/sdk_agent_worker/server.py').read_text()
        self.assertIn('from .capabilities import ensure_access as ensure_execution_active',source)
        # Match the completed branch structurally: cancellation must be fenced
        # after revalidation and before beginning the durable receipt commit.
        expected = ast.parse("""await ensure_execution_active(launch)
if owner.stopping or owner.finished:
    raise ValueError("sdk_execution_cancelled")
owner.commit_started = True
atomic_json(receipt, {"fingerprint": fingerprint, "result": value})
""").body
        matches = []
        for node in ast.walk(ast.parse(source)):
            block = getattr(node, "body", None)
            if not isinstance(block, list):
                continue
            for index, statement in enumerate(block):
                if ast.dump(statement) == ast.dump(expected[0]):
                    matches.append(block[index:index + len(expected)])
        self.assertEqual(len(matches), 1)
        self.assertEqual([ast.dump(node) for node in matches[0]],
                         [ast.dump(node) for node in expected])
        with patch('sdk_agent_worker.capabilities._request',return_value={'authorized':False}):
            with self.assertRaises(PermissionError):asyncio.run(ensure_access(launch))

    def test_malformed_empty_bridge_rejected(self):
        for update in ({'tools':None},{'tools':{}},{'timeout_seconds':1800},{'timeout_seconds':True}):
            candidate=body()['capability_bridge'];candidate.update(update)
            with self.assertRaises(ValueError):validate_bridge(candidate)
        candidate=body()['capability_bridge'];del candidate['tools']
        with self.assertRaises(ValueError):validate_bridge(candidate)
    def test_pure_profile_remains_compatible(self):
        with tempfile.TemporaryDirectory() as temp:
            state=Path(temp)
            (state/'sdk-profile.json').write_text(json.dumps({'schema':'sdk-session-tools/v1','dataset_ids':[]}))
            ensure_profile(state,Launch.parse(body(),'x'*32))
    def test_kb_profile_remains_independently_scoped(self):
        candidate=body();candidate['read_bridge']={'grant':'a'*64,'dataset_ids':['00000000-0000-4000-8000-000000000002']}
        launch=Launch.parse(candidate,'x'*32)
        self.assertIn('knowledge_retrieve',tools_for(launch))
        self.assertEqual(GovernedTools(launch,None).tools,{})
        with tempfile.TemporaryDirectory() as temp:
            state=Path(temp);ensure_profile(state,launch)
            with self.assertRaisesRegex(PermissionError,'profile_changed'):ensure_profile(state,Launch.parse(body(),'x'*32))

if __name__=='__main__':unittest.main()
