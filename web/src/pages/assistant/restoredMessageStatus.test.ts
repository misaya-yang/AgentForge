import assert from "node:assert/strict";
import { test } from "node:test";

import {
  restoredMessageStatus,
  restoredProcessStatus,
} from "./restoredMessageStatus.ts";

test("runtime restore preserves cancellation instead of converting it to failure", () => {
  assert.equal(restoredProcessStatus("cancelled"), "cancelled");
  assert.equal(restoredProcessStatus("succeeded"), "succeeded");
  assert.equal(restoredProcessStatus("running"), "running");
  assert.equal(restoredProcessStatus("queued"), "running");
  assert.equal(restoredProcessStatus("awaiting_approval"), "running");
  assert.equal(restoredProcessStatus("unknown"), "failed");
});

test("restored terminal status preserves cancelled and failed turns", () => {
  assert.equal(restoredMessageStatus("cancelled"), "cancelled");
  assert.equal(restoredMessageStatus("failed"), "failed");
});

test("restored active and successful summaries map to chat turn states", () => {
  assert.equal(restoredMessageStatus("running"), "streaming");
  assert.equal(restoredMessageStatus("blocked"), "streaming");
  assert.equal(restoredMessageStatus("succeeded"), "completed");
  assert.equal(restoredMessageStatus(undefined), "completed");
});
