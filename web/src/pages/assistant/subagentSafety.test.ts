import assert from "node:assert/strict";
import test from "node:test";

import { safeSubAgentText } from "./subagentSafety.ts";

test("subagent text redacts credential-shaped content before activity rendering", () => {
  assert.equal(
    safeSubAgentText("token=super-secret-value and done"),
    "[redacted] and done",
  );
});
