import assert from "node:assert/strict";
import test from "node:test";
import { planAssistantFileSelection } from "./fileSelection.ts";

test("only available attachment slots are accepted for upload", () => {
  const incoming = ["a", "b", "c"];
  assert.deepEqual(planAssistantFileSelection(incoming, 3), {
    accepted: ["a", "b"],
    exceedsLimit: true,
  });
  assert.deepEqual(planAssistantFileSelection(incoming, 5), {
    accepted: [],
    exceedsLimit: true,
  });
  assert.deepEqual(planAssistantFileSelection(incoming, 1), {
    accepted: incoming,
    exceedsLimit: false,
  });
});
