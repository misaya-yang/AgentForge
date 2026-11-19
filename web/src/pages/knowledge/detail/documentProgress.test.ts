// @ts-expect-error -- node built-ins are supplied by the node --test runtime.
import assert from "node:assert/strict";
// @ts-expect-error -- node built-ins are supplied by the node --test runtime.
import { test } from "node:test";

import {
  documentProgressReconnectDelay,
  parseDocumentProgressEventSequence,
  shouldApplyDocumentProgressEvent,
} from "./documentProgress.ts";

test("document progress cursors are dataset-scoped and monotonic", () => {
  assert.equal(parseDocumentProgressEventSequence("dataset-a", "dataset-a:7"), 7);
  assert.equal(parseDocumentProgressEventSequence("dataset-a", "dataset-a:7:8"), null);
  assert.equal(parseDocumentProgressEventSequence("dataset-a", "dataset-b:8"), null);
  assert.equal(shouldApplyDocumentProgressEvent("dataset-a", undefined, "dataset-a:1"), true);
  assert.equal(shouldApplyDocumentProgressEvent("dataset-a", "dataset-a:8", "dataset-a:8"), false);
  assert.equal(shouldApplyDocumentProgressEvent("dataset-a", "dataset-a:8", "dataset-a:7"), false);
  assert.equal(shouldApplyDocumentProgressEvent("dataset-a", "dataset-a:8", "dataset-a:9"), true);
  assert.equal(
    shouldApplyDocumentProgressEvent("dataset-a", "dataset-a:50", "dataset-a:12", "reset"),
    true,
  );
});

test("document progress reconnect delay backs off and is bounded", () => {
  assert.deepEqual(
    [0, 1, 2, 3, 4, 8].map(documentProgressReconnectDelay),
    [1000, 2000, 4000, 8000, 10000, 10000],
  );
  assert.equal(documentProgressReconnectDelay(Number.NaN), 1000);
  assert.equal(documentProgressReconnectDelay(-3), 1000);
});
