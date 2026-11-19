import assert from "node:assert/strict";
import { test } from "node:test";

import {
  createStreamReducerContext,
  createStreamTurnState,
  markStreamFirstToken,
  reduceNormalizedStreamEvent,
} from "./reducer.ts";
import type { ChatTurnState } from "./types.ts";

for (const terminalStatus of ["completed", "failed", "cancelled"] as ChatTurnState[]) {
  test(`message_start does not reopen a ${terminalStatus} turn`, () => {
    const state = {
      ...createStreamTurnState(1),
      status: terminalStatus,
      content: "kept",
    };
    const result = reduceNormalizedStreamEvent(
      state,
      { type: "message_start", timestamp: 2 },
      createStreamReducerContext(),
      2,
    );

    assert.equal(result.state, state);
    assert.equal(result.state.status, terminalStatus);
    assert.equal(result.changed, false);
    assert.equal(result.terminal, false);
  });
}

test("message_start still opens an idle turn", () => {
  const result = reduceNormalizedStreamEvent(
    createStreamTurnState(1),
    { type: "message_start", timestamp: 2 },
    createStreamReducerContext(),
    2,
  );

  assert.equal(result.state.status, "streaming");
  assert.equal(result.changed, true);
});

test("first reasoning token remains TTFT when answer text arrives later", () => {
  const context = createStreamReducerContext();
  const thinkingState = markStreamFirstToken(createStreamTurnState(1_000), 4_500);
  const result = reduceNormalizedStreamEvent(
    thinkingState,
    { type: "message_delta", content: "200" },
    context,
    6_000,
  );

  assert.equal(result.state.firstTokenMs, 3_500);
  assert.equal(result.state.content, "200");
});
