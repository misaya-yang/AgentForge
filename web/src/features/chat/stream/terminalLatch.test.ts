import assert from "node:assert/strict";
import { test } from "node:test";

import { createStreamTerminalLatch } from "./terminalLatch.ts";

test("stream terminal latch accepts only the first outcome", () => {
  const latch = createStreamTerminalLatch();

  assert.equal(latch.accept("cancelled"), true);
  assert.equal(latch.accept("succeeded"), false);
  assert.equal(latch.accept("failed"), false);
  assert.equal(latch.current(), "cancelled");
});
