// @ts-expect-error -- node built-ins are supplied by the node --test runtime.
import assert from "node:assert/strict";
// @ts-expect-error -- node built-ins are supplied by the node --test runtime.
import { test } from "node:test";

import { parseSseEventPart } from "./sseEventParser.ts";

test("SSE parser retains event IDs and joins multiline data", () => {
  assert.deepEqual(
    parseSseEventPart<{ ok: boolean }>(
      'id: dataset-a:8\nevent: terminal\ndata: {"ok":\ndata: true}',
    ),
    { id: "dataset-a:8", event: "terminal", data: { ok: true } },
  );
});

test("SSE parser ignores comments and DONE frames", () => {
  assert.equal(parseSseEventPart(": keep-alive"), null);
  assert.equal(parseSseEventPart("data: [DONE]"), null);
  assert.deepEqual(parseSseEventPart("data: plain"), {
    event: "",
    data: "plain",
  });
});

for (const part of ["event: end", "event: error", "event: error|worker:fixture"]) {
  test(`SSE parser preserves named control frame only when opted in: ${part}`, () => {
    assert.equal(parseSseEventPart(part), null);
    assert.deepEqual(parseSseEventPart(part, { preserveControlFrames: true }), { event: part.slice(7), data: undefined });
  });
}
test("SSE parser opt-in preserves raw DONE without inventing a protocol event", () => {
  assert.equal(parseSseEventPart("data: [DONE]"), null);
  assert.deepEqual(parseSseEventPart("data: [DONE]", { preserveControlFrames: true }), { event: "", data: "[DONE]" });
  assert.equal(parseSseEventPart(": keep-alive", { preserveControlFrames: true }), null);
  assert.equal(parseSseEventPart("data:", { preserveControlFrames: true }), null);
});


test("SSE cursor reset is retained and NUL IDs are ignored", () => {
  assert.deepEqual(parseSseEventPart('id:\ndata: {}'), { event: "", data: {}, id: "" });
  assert.deepEqual(parseSseEventPart('id: unsafe\0cursor\ndata: {}'), { event: "", data: {} });
});
