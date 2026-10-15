// @ts-expect-error -- node built-ins are supplied by the node --test runtime.
import assert from "node:assert/strict";
// @ts-expect-error -- node built-ins are supplied by the node --test runtime.
import { test } from "node:test";

import type { DocumentBatchOperation } from "@/api/knowledge";
import { remainingDocumentBatchIds, documentBatchItemOutcomes, hasBlockingUnknown, partitionIds, settleDocumentBatchAttempt, summarizeDocumentBatches } from "./batchOperations.ts";


function operation(
  total: number,
  succeeded: number,
  skipped: number,
  failed: number
): DocumentBatchOperation {
  return {
    operation_id: crypto.randomUUID(),
    tenant_id: "tenant-a",
    dataset_id: "dataset-a",
    operation: "reembed",
    status: skipped || failed ? "partial" : "completed",
    total_count: total,
    queued_count: succeeded,
    skipped_count: skipped,
    failed_count: failed,
    problem_items: [],
    problem_items_truncated: false,
  };
}


test("partitionIds never truncates selections beyond two hundred", () => {
  const ids = Array.from({ length: 251 }, (_, index) => `doc-${index}`);
  const chunks = partitionIds(ids, 100);

  assert.deepEqual(chunks.map((chunk) => chunk.length), [100, 100, 51]);
  assert.deepEqual(chunks.flat(), ids);
});


test("partitionIds de-duplicates without changing first-seen order", () => {
  assert.deepEqual(partitionIds(["b", "a", "b", "c"], 2), [["b", "a"], ["c"]]);
});


test("summarizeDocumentBatches preserves partial outcomes", () => {
  assert.deepEqual(
    summarizeDocumentBatches([
      operation(100, 97, 2, 1),
      operation(51, 50, 1, 0),
    ]),
    { total: 151, succeeded: 147, skipped: 3, failed: 1 }
  );
});

test("per-item receipt preserves success, failure, and unknown without guessing", () => {
  const partial = operation(3, 1, 1, 1);
  partial.problem_items = [
    { document_id: "b", status: "skipped" },
    { document_id: "c", status: "failed" },
  ];
  const attempt = { requestedIds: ["a", "b", "c"], operation: partial, outcomeUnknown: false };
  assert.deepEqual(Array.from(documentBatchItemOutcomes(attempt)), [
    ["a", "succeeded"], ["b", "skipped"], ["c", "failed"],
  ]);
  partial.problem_items_truncated = true;
  assert.deepEqual(Array.from(documentBatchItemOutcomes(attempt).values()), ["unknown", "unknown", "unknown"]);
  partial.problem_items_truncated = false;
  assert.deepEqual(Array.from(documentBatchItemOutcomes({ ...attempt, outcomeUnknown: true }).values()), [
    "unknown", "unknown", "unknown",
  ]);
  partial.problem_items[0].document_id = "unexpected";
  assert.deepEqual(Array.from(documentBatchItemOutcomes(attempt).values()), ["unknown", "unknown", "unknown"]);
});

test("POST accepted then poll timeout retries the same operation with GET only", async () => {
  let posts = 0;
  let acceptedCallbacks = 0;
  const accepted = operation(1, 0, 0, 0);
  accepted.status = "running";
  const submit = async () => { posts += 1; return accepted; };
  let polls = 0;
  const wait = async (id: string) => {
    assert.equal(id, accepted.operation_id);
    polls += 1;
    if (polls === 1) throw new Error("poll timeout");
    return { ...operation(1, 1, 0, 0), operation_id: accepted.operation_id };
  };
  const onAccepted = () => { acceptedCallbacks += 1; };
  const first = await settleDocumentBatchAttempt(["doc-1"], undefined, submit, wait, onAccepted);
  assert.equal(first.outcomeUnknown, true);
  assert.equal(first.operation?.operation_id, accepted.operation_id);
  const second = await settleDocumentBatchAttempt(["doc-1"], first, submit, wait, onAccepted);
  assert.equal(second.outcomeUnknown, false);
  assert.equal(posts, 1);
  assert.equal(acceptedCallbacks, 1);
  assert.equal(polls, 2);
});

test("unknown POST without operation ID cannot be blindly resubmitted", async () => {
  let posts = 0;
  const submit = async () => { posts += 1; throw new Error("request timed out"); };
  const wait = async () => operation(1, 1, 0, 0);
  const first = await settleDocumentBatchAttempt(["doc-1"], undefined, submit, wait);
  const second = await settleDocumentBatchAttempt(["doc-1"], first, submit, wait);
  assert.equal(first.outcomeUnknown, true);
  assert.equal(second.outcomeUnknown, true);
  assert.equal(posts, 1);
  assert.equal(hasBlockingUnknown([second]), true);
  assert.equal(hasBlockingUnknown([{ ...second, acknowledged: true }]), false);
});

const recoverable = () => ({ action: "reembed", requestedCount: 4, requestedIds: ["a", "b", "c", "d"],
  attempts: [{ requestedIds: ["a", "b", "c"], outcomeUnknown: false, operation: {
    ...operation(3, 1, 1, 1), problem_items: [{ document_id: "b", status: "skipped" }, { document_id: "c", status: "failed" }],
  } }] });

test("remaining selection preserves failed, skipped and unsubmitted identities, excluding queued success", () => {
  assert.deepEqual(remainingDocumentBatchIds(recoverable(), "dataset-a", "tenant-a"), ["b", "c", "d"]);
  const deleted = recoverable(); deleted.action = "delete"; deleted.attempts[0].operation.operation = "delete";
  assert.deepEqual(remainingDocumentBatchIds(deleted, "dataset-a", "tenant-a"), ["b", "c", "d"]);
});

test("remaining selection fails closed for malformed, uncertain, mismatched and truncated receipts", () => {
  const mutations: ((value: ReturnType<typeof recoverable> & { allDocuments?: boolean }) => void)[] = [
    (r) => { r.allDocuments = true; }, (r) => { r.requestedCount = 5; },
    (r) => { r.requestedIds[1] = "a"; }, (r) => { r.attempts.push(r.attempts[0]); },
    (r) => { r.attempts[0].outcomeUnknown = true; Object.assign(r.attempts[0], { acknowledged: true }); },
    (r) => { r.attempts[0].requestedIds[1] = "foreign"; }, (r) => { r.attempts[0].requestedIds[1] = "a"; },
    (r) => { r.attempts[0].operation.problem_items_truncated = true; },
    (r) => { Object.assign(r.attempts[0].operation, { problem_items: null }); },
    (r) => { r.attempts[0].operation.problem_items[1].document_id = "b"; },
    (r) => { r.attempts[0].operation.problem_items[1].status = "skipped"; },
    (r) => { r.attempts[0].operation.queued_count = -1; },
    (r) => { r.attempts[0].operation.status = "running"; },
    (r) => { Object.assign(r.attempts[0].operation, { status: { toString: null } }); },
    (r) => { r.attempts[0].operation.dataset_id = "elsewhere"; },
    (r) => { r.attempts[0].operation.tenant_id = "elsewhere"; },
    (r) => { r.attempts[0].operation.operation = "delete"; },
    (r) => { r.attempts[0].operation.operation_id = ""; },
  ];
  for (const mutate of mutations) { const value = recoverable(); mutate(value); assert.deepEqual(remainingDocumentBatchIds(value, "dataset-a", "tenant-a"), []); }
  for (const value of [null, [], {}, { attempts: [null] }]) assert.deepEqual(remainingDocumentBatchIds(value, "dataset-a", "tenant-a"), []);
  assert.deepEqual(remainingDocumentBatchIds(recoverable(), "dataset-a"), []);
});
