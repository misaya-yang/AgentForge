// @ts-expect-error -- Node types are outside tsconfig.app.json.
import assert from "node:assert/strict";
// @ts-expect-error -- Node types are outside tsconfig.app.json.
import { test } from "node:test";
import type { EvalExample } from "@/api/eval";
import { canApproveReviewExample, kbDeepLinkNavigationKey, latestReviewCandidates } from "./reviewQueue.ts";

function example(id: string, revision: number, status: string, traceId: string | null = null): EvalExample {
  return {
    example_id: id,
    dataset_id: "eval-1",
    tenant_id: "tenant-1",
    split: status === "approved" ? "regression" : "review",
    input: { query: "Question" },
    expected_output: { answer: "Expected" },
    metadata: {
      case_id: "kb-case-1",
      source_kind: "kb_failure",
      case_revision: revision,
      review_status: status,
    },
    source_trace_id: traceId,
    created_by: "user-1",
  };
}

test("review queue shows only latest pending KB revision", () => {
  const first = example("r1", 1, "pending");
  const second = example("r2", 2, "pending");
  assert.deepEqual(latestReviewCandidates([first, second]).map((item) => item.example_id), ["r2"]);
  second.metadata.review_status = "approved";
  assert.deepEqual(latestReviewCandidates([first, second]), []);
});

test("review queue retains candidates past the former eight and 200 row caps", () => {
  const ordinary = Array.from({ length: 210 }, (_, index) => ({
    ...example(`other-${index}`, 1, "pending"),
    metadata: { case_id: `other-${index}`, review_status: "pending" },
  }));
  assert.equal(latestReviewCandidates(ordinary).length, 210);
});

test("KB approval needs a confirmed source trace and a written expectation", () => {
  const pending = example("r1", 1, "pending");
  assert.equal(canApproveReviewExample(pending), false);
  pending.metadata.kb_trace_id = "unconfirmed-trace";
  assert.equal(canApproveReviewExample(pending), false);
  pending.source_trace_id = "confirmed-trace";
  assert.equal(canApproveReviewExample(pending), true);
  pending.expected_output = { answer: " " };
  assert.equal(canApproveReviewExample(pending), false);
});

test("returning to the same KB deep link after manual selection gets a new navigation identity", () => {
  const firstVisit = kbDeepLinkNavigationKey("location-1", "kb-A");
  const manualSelection = kbDeepLinkNavigationKey("location-1", "kb-A");
  const returnVisit = kbDeepLinkNavigationKey("location-2", "kb-A");
  assert.equal(manualSelection, firstVisit);
  assert.notEqual(returnVisit, firstVisit);
});
