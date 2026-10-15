import assert from 'node:assert/strict';
import test from 'node:test';
import { readFileSync } from 'node:fs';
import { extractToolResult, convertToolCallsFromMetadata, extractRunWaitToolCalls, mergeFallbackToolCalls } from './langgraph.ts';
import { createStreamTurnState, createStreamReducerContext, reduceLegacyStreamChunk } from '../../../features/chat/stream/reducer.ts';
type FixtureCase = {
  name: string;
  status: string;
  messages: Array<Record<string, unknown>>;
  chunks: Array<Parameters<typeof reduceLegacyStreamChunk>[1]>;
  session: { history: Array<{role: string; content: string; metadata: {tool_calls: Parameters<typeof convertToolCallsFromMetadata>[0]}}> };
};
const cases: FixtureCase[] = JSON.parse(readFileSync(process.env.LANGCHAIN_GATEWAY_RESPONSES || new URL("../../../../../tests/fixtures/adapters/langchain_tool_roundtrip_gateway.json",import.meta.url),'utf8'));

for(const c of cases) {
 test(`LangChain tool roundtrip ${c.name}`, () => {
 const resultMessage=c.messages.find((m)=>m.type==='tool');
 assert.ok(resultMessage);
 const expected=c.status==='error'?'error':'completed';
 let state=createStreamTurnState(0); const context=createStreamReducerContext();
 for(const chunk of c.chunks) state=reduceLegacyStreamChunk(state,chunk,context,1).state;
 const assistant=c.session.history.find((m)=>m.role==='assistant');
 assert.ok(assistant,c.name+' saved assistant');
 const replay=convertToolCallsFromMetadata(assistant.metadata.tool_calls)!;
 const raw=extractToolResult(resultMessage)!;
 assert.equal(raw.status,expected); assert.equal(state.toolCalls[0].status,expected);
 assert.equal(replay[0].toolCall.status,expected); assert.equal(state.status,'completed');
 assert.equal(state.toolCalls[0].result,resultMessage.content,c.name+' result presence');
 assert.equal(replay[0].result,resultMessage.content,c.name+' replay result presence');
 const direct=reduceLegacyStreamChunk(createStreamTurnState(0),{event_type:'tool_result',chunk_index:0,
  tool_call:{tool_call_id:raw.id,name:raw.name,arguments:'{}',status:raw.status},content:{type:'tool_result',data:raw.content}},createStreamReducerContext(),1).state;
 assert.equal(direct.toolCalls[0].status,expected); assert.equal(direct.toolCalls[0].result,resultMessage.content);
 const wait=extractRunWaitToolCalls({messages:c.messages})!;
 assert.equal(wait[0].toolCall.status,expected);
 const merged=mergeFallbackToolCalls(createStreamTurnState(0),wait);
 assert.equal(merged.toolCalls[0].status,expected); assert.equal(merged.toolCalls[0].result,resultMessage.content);
 assert.notEqual(merged.toolCalls[0].endedAt,undefined);
 if(c.name.startsWith('tool_only') || c.name.startsWith('result_only')) assert.equal(assistant.content,'');
 });
}
test("Legacy tool metadata remains readable", () => {
// Legacy metadata stays readable; known non-terminal status is not fabricated as complete.
assert.equal(convertToolCallsFromMetadata([{tool_call_id:'legacy',name:'tool',arguments:'{}'}])![0].toolCall.status,'completed');
assert.equal(convertToolCallsFromMetadata([{tool_call_id:'pending',name:'tool',arguments:'{}',status:'running'}])![0].toolCall.status,'running');

});

test("Missing result stays absent and explicit failure metadata stays authoritative", () => {
  const initial = createStreamTurnState(0);
  const chunk: Parameters<typeof reduceLegacyStreamChunk>[1] = { event_type: "tool_result", chunk_index: 0,
    tool_call: { tool_call_id: "missing", name: "tool", arguments: "{}", status: "pending" },
    metadata: { status: "error" }, content: { type: "tool_result", data: undefined } };
  const state = reduceLegacyStreamChunk(initial, chunk, createStreamReducerContext(), 1).state;
  assert.equal(state.toolCalls[0].status, "error");
  assert.equal(state.toolCalls[0].result, undefined);
  assert.equal(convertToolCallsFromMetadata([{tool_call_id:"missing",name:"tool",arguments:"{}",result:null,status:"error"}])![0].result, undefined);
});
