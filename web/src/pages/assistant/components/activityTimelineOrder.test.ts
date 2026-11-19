import assert from "node:assert/strict";
import test from "node:test";

import { appendReasoningAfterActivity } from "./activityTimelineOrder.ts";
import type { TimelineStepData } from "./TimelineStep.tsx";

function tool(id: string): TimelineStepData {
  return {
    kind: "tool",
    id,
    icon: "other",
    title: id,
    body: "",
    status: "completed",
  };
}

function reasoning(body: string): TimelineStepData {
  return {
    kind: "thinking",
    id: "thinking-assistant-1",
    title: "Thinking",
    body,
    streaming: true,
  };
}

test("tool activity stays above the single streaming reasoning row", () => {
  const steps = appendReasoningAfterActivity(
    [tool("tool-1"), tool("tool-2")],
    reasoning("正在比较资料"),
  );

  assert.deepEqual(
    steps.map((step) => step.id),
    ["tool-1", "tool-2", "thinking-assistant-1"],
  );
  assert.deepEqual(
    steps.map((step) => step.kind),
    ["tool", "tool", "thinking"],
  );
});

test("reasoning growth preserves tool order and updates one stable row", () => {
  const before = appendReasoningAfterActivity(
    [tool("tool-1"), tool("tool-2")],
    reasoning("正在比较资料"),
  );
  const after = appendReasoningAfterActivity(
    [tool("tool-1"), tool("tool-2")],
    reasoning("正在比较资料，并整理证据分歧"),
  );

  assert.deepEqual(
    after.slice(0, 2).map((step) => step.id),
    before.slice(0, 2).map((step) => step.id),
  );
  assert.equal(after.length, before.length);
  assert.equal(after.filter((step) => step.kind === "thinking").length, 1);
  assert.equal(after.at(-1)?.id, before.at(-1)?.id);
  if (after.at(-1)?.kind === "thinking") {
    assert.equal(after.at(-1)?.body, "正在比较资料，并整理证据分歧");
  }
});
