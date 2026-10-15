import importlib.util
from pathlib import Path
from types import SimpleNamespace
import unittest
spec=importlib.util.spec_from_file_location('sdk_usage',Path(__file__).resolve().parents[2]/'apps/sdk-agent-worker/sdk_agent_worker/usage.py')
u=importlib.util.module_from_spec(spec);spec.loader.exec_module(u)
class UsageTests(unittest.TestCase):
 def test_missing_not_zero(self):
  for value in [None,{}, {'input_tokens':5,'output_tokens':2}]:
   r=u.claude_usage(value,None);self.assertEqual(r['sdk_usage']['state'],'unavailable');self.assertNotIn('input_tokens',r)
 def test_explicit_zero(self):
  r=u.claude_usage(dict.fromkeys(['input_tokens','output_tokens','cache_read_input_tokens','cache_creation_input_tokens'],0),0)
  self.assertEqual(r['total_tokens'],0);self.assertEqual(r['sdk_usage']['state'],'reported');self.assertEqual(r['sdk_usage']['provider_cost_usd'],0)
 def test_cache_total(self):
  r=u.claude_usage({'input_tokens':4,'output_tokens':1,'cache_read_input_tokens':3,'cache_creation_input_tokens':2},0.125)
  self.assertEqual((r['input_tokens'],r['output_tokens'],r['total_tokens']),(9,1,10));self.assertEqual(r['sdk_usage']['provider_usage']['input_tokens'],4)
 def test_invalid_counts(self):
  for value in [-1,True,1.5,'5',2**63,None]:
   self.assertEqual(u.observation(value,2)['sdk_usage']['state'],'unavailable')
 def test_overflow(self):self.assertNotIn('total_tokens',u.observation(2**63-1,1))
 def test_invalid_cost(self):
  for cost in [float('nan'),float('inf'),-1,True,'1']:
   self.assertEqual(u.observation(1,2,cost=cost)['sdk_usage']['cost_state'],'unavailable')
 def test_partial_is_preserved(self):
  r=u.claude_usage({'input_tokens':5,'output_tokens':2},None)
  self.assertEqual(r['output_tokens'],2);self.assertNotIn('input_tokens',r);self.assertEqual(r['sdk_usage']['provider_usage']['input_tokens'],5)
 def test_cost_independent(self):
  r=u.claude_usage(None,1.25);self.assertEqual(r['sdk_usage']['state'],'unavailable');self.assertEqual(r['sdk_usage']['provider_cost_usd'],1.25)
 def test_openai_initialized_zero(self):
  self.assertNotIn('input_tokens',u.openai_usage([SimpleNamespace(raw_usage=None)]))
 def test_openai_reported(self):
  self.assertEqual(u.openai_usage([SimpleNamespace(raw_usage={'input_tokens':14,'output_tokens':6})])['total_tokens'],20)
 def test_cached_tokens_not_normal_rate_priced(self):
  r=u.claude_usage({'input_tokens':4,'output_tokens':1,'cache_read_input_tokens':3,'cache_creation_input_tokens':2},0.1)
  self.assertEqual(r['sdk_usage']['platform_pricing_state'],'reconciliation_required')
 def test_openai_missing_one_response_usage(self):
  r=u.openai_usage([SimpleNamespace(raw_usage={'input_tokens':5,'output_tokens':2}),SimpleNamespace(raw_usage=None)])
  self.assertEqual(r['sdk_usage']['state'],'unavailable');self.assertNotIn('input_tokens',r)
 def test_no_arbitrary_provider_metadata(self):
  r=u.claude_usage({'secret':'do not preserve'},None);self.assertEqual(r['sdk_usage']['provider_usage'],{})
if __name__=='__main__':unittest.main()
