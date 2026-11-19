// The browser app type gate intentionally omits Node globals; these imports
// are supplied by the `node --test` runtime used for this file.
// @ts-expect-error -- node built-in types are outside tsconfig.app.json.
import assert from "node:assert/strict";
// @ts-expect-error -- node built-in types are outside tsconfig.app.json.
import { test } from "node:test";

import {
  canRetryEmbeddingActionJob,
  clearEmbeddingActionJobPointer,
  embeddingActionJobPointerKey,
  embeddingActionJobPollDelay,
  isEmbeddingActionName,
  mergeEmbeddingActionJob,
  normalizeEmbeddingActionJobState,
  persistEmbeddingActionJobPointer,
  readEmbeddingActionJobPointer,
  selectServerEmbeddingActionJob,
  shouldPollEmbeddingActionJob,
  type EmbeddingActionJobLike,
} from "./embeddingMigrationJobs.ts";

function makeJob(overrides: Partial<EmbeddingActionJobLike> = {}): EmbeddingActionJobLike {
  return {
    job_id: "job-1",
    migration_id: "migration-1",
    dataset_id: "dataset-1",
    action: "backfill",
    state: "queued",
    error: null,
    poll_after_ms: 500,
    ...overrides,
  };
}

function memoryStorage() {
  const values = new Map<string, string>();
  return {
    getItem(key: string) {
      return values.get(key) ?? null;
    },
    setItem(key: string, value: string) {
      values.set(key, value);
    },
    removeItem(key: string) {
      values.delete(key);
    },
  };
}

test("normalizes queued/running/succeeded/failed/cancelled states", () => {
  assert.equal(normalizeEmbeddingActionJobState(makeJob({ state: "queued" })), "queued");
  assert.equal(normalizeEmbeddingActionJobState(makeJob({ state: "running" })), "running");
  assert.equal(normalizeEmbeddingActionJobState(makeJob({ state: "succeeded" })), "succeeded");
  assert.equal(normalizeEmbeddingActionJobState(makeJob({ state: "failed" })), "failed");
  assert.equal(normalizeEmbeddingActionJobState(makeJob({ state: "cancelled" })), "cancelled");
  assert.equal(normalizeEmbeddingActionJobState(makeJob({ state: "canceled" })), "cancelled");
  assert.equal(
    normalizeEmbeddingActionJobState(
      makeJob({ state: "failed", error: "cancelled by migration abort" })
    ),
    "cancelled"
  );
});

test("polling has no elapsed-time cutoff after thirty seconds", () => {
  const job = makeJob({ state: "running", poll_after_ms: 500 });
  let elapsedMs = 0;
  while (elapsedMs <= 31_000) {
    assert.equal(shouldPollEmbeddingActionJob(job), true);
    elapsedMs += embeddingActionJobPollDelay(job);
  }
  assert.equal(canRetryEmbeddingActionJob(job), false);
  assert.equal(canRetryEmbeddingActionJob(makeJob({ state: "failed" })), true);
});

test("poll delay obeys bounded server hints", () => {
  assert.equal(embeddingActionJobPollDelay(makeJob({ poll_after_ms: 1 })), 250);
  assert.equal(embeddingActionJobPollDelay(makeJob({ poll_after_ms: 800 })), 800);
  assert.equal(embeddingActionJobPollDelay(makeJob({ poll_after_ms: 60_000 })), 5_000);
  assert.equal(embeddingActionJobPollDelay(makeJob({ poll_after_ms: Number.NaN })), 1_000);
});

test("action validation and response merging preserve enqueue-only hints", () => {
  assert.equal(isEmbeddingActionName("gate"), true);
  assert.equal(isEmbeddingActionName("cutover"), false);
  const previous = makeJob({ reused: true, poll_after_ms: 500 });
  const polled = makeJob({ state: "running", reused: undefined, poll_after_ms: undefined });
  assert.deepEqual(mergeEmbeddingActionJob(previous, polled), {
    ...polled,
    reused: true,
    poll_after_ms: 500,
  });
  const replacement = makeJob({ job_id: "job-2", poll_after_ms: 800 });
  assert.equal(mergeEmbeddingActionJob(previous, replacement), replacement);
});

test("job pointer round-trips, scopes to migration, and clears by identity", () => {
  const storage = memoryStorage();
  const job = makeJob();
  persistEmbeddingActionJobPointer(storage, "dataset/1", job);
  assert.equal(
    embeddingActionJobPointerKey("dataset/1"),
    "kb-embedding-action-job:v1:dataset%2F1"
  );
  assert.deepEqual(readEmbeddingActionJobPointer(storage, "dataset/1", "migration-1"), {
    version: 1,
    jobId: "job-1",
    migrationId: "migration-1",
    action: "backfill",
    pollAfterMs: 500,
  });
  assert.equal(readEmbeddingActionJobPointer(storage, "dataset/1", "migration-other"), null);
  persistEmbeddingActionJobPointer(storage, "dataset/1", job);
  clearEmbeddingActionJobPointer(storage, "dataset/1", "job-other");
  assert.notEqual(storage.getItem(embeddingActionJobPointerKey("dataset/1")), null);
  clearEmbeddingActionJobPointer(storage, "dataset/1", "job-1");
  assert.equal(storage.getItem(embeddingActionJobPointerKey("dataset/1")), null);
});

test("server active job is authoritative over terminal history", () => {
  const active = makeJob({ job_id: "job-active", state: "running" });
  const recent = makeJob({ job_id: "job-recent", state: "failed" });
  assert.equal(
    selectServerEmbeddingActionJob(active, [recent], "migration-1")?.job_id,
    "job-active"
  );
  assert.equal(
    selectServerEmbeddingActionJob(null, [recent], "migration-1")?.job_id,
    "job-recent"
  );
  assert.equal(
    selectServerEmbeddingActionJob(active, [recent], "migration-other")?.job_id,
    "job-active"
  );
  assert.equal(selectServerEmbeddingActionJob(null, [recent], "migration-other"), null);
});
