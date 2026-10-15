import { describe, expect, it } from "vitest";

import { namespaceAlias, namespaceBindings, responsesToChat } from "./chat_request_adapter.js";

const functions = ["files", "records"].map(name => ({
  type: "namespace", name, tools: [{ type: "function", name: "lookup", parameters: { type: "object" } }],
}));
const request = { model: "fixture", stream: true, input: "hello", tools: functions };

describe("Codex target request compatibility", () => {
  it("selects the exact namespaced function and rejects unavailable identities", () => {
    const chat = responsesToChat({ ...request, tool_choice: { type: "function", namespace: "records", name: "lookup" } });
    expect(chat.tool_choice).toEqual({ type: "function", function: { name: namespaceAlias("records", "lookup") } });
    expect(() => responsesToChat({ ...request, tool_choice: { type: "function", name: "lookup" } })).toThrow("responses_tool_choice_unavailable");
    expect(() => responsesToChat({ ...request, tool_choice: { type: "function", namespace: "missing", name: "lookup" } })).toThrow("responses_tool_choice_unavailable");
  });

  it("keeps aliases consistent for the accepted nested function envelope", () => {
    const body = { ...request, tools: [{ type: "namespace", name: "files", tools: [{ type: "function", function: { name: "lookup" } }] }] };
    expect(namespaceBindings(body).get(namespaceAlias("files", "lookup"))).toEqual({ namespace: "files", name: "lookup" });
    expect((responsesToChat(body).tools as any[])[0].function.name).toBe(namespaceAlias("files", "lookup"));
  });

  it.each([null, {}, { effort: null, summary: null }])("accepts optional reasoning settings %j", reasoning => {
    expect(responsesToChat({ ...request, reasoning }).reasoning_effort).toBeUndefined();
  });

  it.each([
    { type: "additional_tools", role: "developer", tools: functions },
    { type: "configuration_update", reasoning_effort: "high" },
    { type: "function_call_output", namespace: "files", name: "lookup", output: "unpaired" },
    { type: "reasoning", summary: [], encrypted_content: "opaque" },
  ])("rejects target input without a lossless Chat representation: $type", item => {
    expect(() => responsesToChat({ ...request, input: [item] })).toThrow();
  });

  it("rejects Responses Lite reasoning context instead of silently dropping it", () => {
    expect(() => responsesToChat({ ...request, reasoning: { context: "all_turns" } })).toThrow("responses_reasoning_context_unsupported");
  });

  it("binds named output to its call and rejects incomplete parallel result batches", () => {
    const calls = ["files", "records"].map((namespace, index) => ({ type: "function_call", call_id: `call_${index}`, namespace, name: "lookup", arguments: "{}" }));
    const outputs = calls.map(call => ({ type: "function_call_output", call_id: call.call_id, namespace: call.namespace, name: call.name, output: "found" }));
    const chat = responsesToChat({ ...request, input: [...calls, ...outputs] });
    expect((chat.messages as any[]).map(message => message.role)).toEqual(["assistant", "tool", "tool"]);
    expect((chat.messages as any[])[0].tool_calls).toHaveLength(2);
    expect(() => responsesToChat({ ...request, input: [...calls, { ...outputs[0], namespace: "records" }, outputs[1]] })).toThrow("responses_function_output_identity_mismatch");
    expect(() => responsesToChat({ ...request, input: [...calls, outputs[0], { type: "message", role: "assistant", content: "premature" }, outputs[1]] })).toThrow("responses_function_output_missing");
    expect(() => responsesToChat({ ...request, input: [calls[0], outputs[0], calls[0], outputs[0]] })).toThrow("responses_function_call_duplicate");
  });
});
