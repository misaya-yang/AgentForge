import assert from "node:assert/strict";
import test from "node:test";

import {
  agentPreviewEventText,
  agentPreviewToolActivityId,
} from "./agentPreviewEvents.ts";

test("agent preview preserves primitive SSE text deltas", () => {
  assert.equal(
    agentPreviewEventText({ event_type: "text_delta", data: "Billing queue" }),
    "Billing queue",
  );
  assert.equal(
    agentPreviewEventText({
      event_type: "text_delta",
      data: { content: "Payment queue" },
    }),
    "Payment queue",
  );
});

test("agent preview gives one lifecycle identity to a tool call", () => {
  assert.equal(
    agentPreviewToolActivityId({
      event_type: "tool_call_start",
      data: { tool_call_id: "call-1" },
    }),
    "tool:call-1",
  );
  assert.equal(
    agentPreviewToolActivityId({
      event_type: "tool_result",
      tool_call_id: "call-1",
    }),
    "tool:call-1",
  );
});
