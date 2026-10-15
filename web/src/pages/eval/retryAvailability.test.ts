import assert from "node:assert/strict";
import test from "node:test";

import { canSelectEvalCaseForRetry, isFailedEvalCase } from "./retryAvailability.ts";

test("safe native judge failure is selectable while quality remains unscored", () => {
  const item = { status: "unscored", execution_status: "succeeded", retry_available: true } as const;
  assert.equal(isFailedEvalCase(item), true);
  assert.equal(canSelectEvalCaseForRetry(item), true);
  assert.equal(item.status, "unscored");
});

test("native denial excludes unsafe judge and execution or behavior failures", () => {
  for (const item of [
    { status: "unscored", execution_status: "succeeded" },
    { status: "unscored", execution_status: "failed" },
    { status: "failed", execution_status: "succeeded" },
  ] as const) {
    assert.equal(canSelectEvalCaseForRetry({ ...item, retry_available: false }), false);
  }
});

test("legacy servers do not newly expose unscored completed cases for replay", () => {
  assert.equal(isFailedEvalCase({ status: "unscored", execution_status: "succeeded" }), true);
  assert.equal(canSelectEvalCaseForRetry({ status: "unscored", execution_status: "succeeded" }), false);
  assert.equal(canSelectEvalCaseForRetry({ status: "unscored" }), false);
  assert.equal(canSelectEvalCaseForRetry({ status: "failed" }), true);
  assert.equal(canSelectEvalCaseForRetry({ status: "unscored", execution_status: "failed" }), true);
});

test("pending and cancelled or skipped cases cannot be selected even with stale hints", () => {
  for (const execution_status of ["queued", "running", "cancelled", "skipped"] as const) {
    assert.equal(canSelectEvalCaseForRetry({ status: "unscored", execution_status, retry_available: true }), false);
    assert.equal(canSelectEvalCaseForRetry({ status: "failed", execution_status }), false);
  }
});

test("passed and review cases are not failures or retry choices", () => {
  for (const status of ["passed", "review"] as const) {
    assert.equal(isFailedEvalCase({ status, execution_status: "succeeded" }), false);
    assert.equal(canSelectEvalCaseForRetry({ status, execution_status: "succeeded", retry_available: true }), false);
  }
});

test("existing safely replayable behavior and model failures stay selectable", () => {
  assert.equal(canSelectEvalCaseForRetry({ status: "failed", execution_status: "succeeded", retry_available: true }), true);
  assert.equal(canSelectEvalCaseForRetry({ status: "unscored", execution_status: "failed", retry_available: true }), true);
});
