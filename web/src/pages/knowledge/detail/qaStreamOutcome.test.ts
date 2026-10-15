import assert from "node:assert/strict";
import test from "node:test";
import { finishQAStream, isQAResponse, qaStreamEvent, type QAStreamOutcome } from "./qaStreamOutcome.ts";
const response = () => ({ query: "question", answer: "answer", model: "fixture", context_segments: [], retrieval_metadata: {}, timing: { retrieval_ms: 1, llm_ms: 2, total_ms: 3 } });
const receiving = (): QAStreamOutcome => ({ phase: "receiving", text: "" });
const done = (result: unknown) => ({ event: "done", data: { result } });

test("valid service terminal preserves empty answer and no-context result", () => {
  const result = { ...response(), answer: "" };
  const state = qaStreamEvent({ phase: "receiving", text: "discarded provisional text" }, done(result), "question");
  assert.equal(state.phase, "completed"); assert.equal(state.text, "");
  assert.strictEqual(finishQAStream(state), state);
});
test("error and EOF retain partial text without completion", () => {
  const partial = qaStreamEvent(receiving(), { event: "delta", data: { content: "partial 中文" } }, "question");
  for (const interrupted of [finishQAStream(partial), qaStreamEvent(partial, { event: "error", data: { message: "provider error" } }, "question")]) {
    assert.deepEqual(interrupted, { phase: "interrupted", text: "partial 中文" });
    assert.strictEqual(qaStreamEvent(interrupted, done(response()), "question"), interrupted);
  }
  assert.deepEqual(finishQAStream(receiving()), { phase: "interrupted", text: "" });
});
test("malformed or mismatched terminal is never a successful history record", () => {
  for (const result of [null, {}, { ...response(), query: "other" }, { ...response(), answer: null }, { ...response(), timing: {} }, { ...response(), context_segments: [{ score: "1" }] }, { ...response(), retrieval_metadata: null }, { ...response(), tokens_used: Infinity }]) {
    assert.equal(isQAResponse(result, "question"), false);
    assert.equal(qaStreamEvent(receiving(), done(result), "question").phase, "interrupted");
  }
});
test("receipt allows Python/native optional nulls and valid citations without rewriting provenance", () => {
  const result = { ...response(), tokens_used: null, trace_id: null, query_fingerprint: null, context_segments: [{ segment_id: "segment", document_id: "document", text: "evidence", score: 0.7, source_version: 2, source_hash: "a".repeat(64) }] };
  assert.equal(isQAResponse(result, "question"), true);
  const state = qaStreamEvent(receiving(), done(result), "question");
  assert.equal(state.phase, "completed");
  if (state.phase === "completed") assert.strictEqual(state.response, result);
});
test("retrieval observation is not completion; malformed/unknown events fail closed", () => {
  const state = receiving(); assert.strictEqual(qaStreamEvent(state, { event: "retrieval", data: {} }, "question"), state);
  for (const event of [null, {}, { event: "delta", data: { content: 4 } }, { event: "unknown" }]) assert.equal(qaStreamEvent(state, event, "question").phase, "interrupted");
});
