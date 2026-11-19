import assert from "node:assert/strict";
import { test } from "node:test";

import { assistantOutcome, imageFailureReceipt } from "./assistantOutcome.ts";
import type { ChatMessage } from "./types.ts";

const runId = "11111111-2222-4333-8444-555555555555";

function message(overrides: Partial<ChatMessage> = {}): ChatMessage {
  return { id: "assistant-1", role: "assistant", content: "", ...overrides };
}

test("a failed turn remains failed with no text or with partial text", () => {
  for (const content of ["", "Partial answer"]) {
    assert.deepEqual(
      assistantOutcome(message({
        content,
        status: "failed",
        processSummary: { collapsed: true, runId, status: "failed", steps: [], tools: [] },
      })),
      { kind: "failed", diagnosticId: runId },
    );
  }
});

test("cancelled, unknown, empty success, normal success and running remain distinct", () => {
  assert.equal(assistantOutcome(message({ status: "cancelled" })).kind, "cancelled");
  assert.equal(assistantOutcome(message({ status: "failed", outcomeUncertain: true })).kind, "unknown");
  assert.equal(assistantOutcome(message({ status: "completed" })).kind, "empty");
  assert.equal(assistantOutcome(message({ status: "completed", content: "Done" })).kind, "succeeded");
  assert.equal(assistantOutcome(message({ status: "streaming", isStreaming: true })).kind, "running");
});

test("restart-orphaned approval is distinct from a user cancellation", () => {
  assert.deepEqual(assistantOutcome(message({
    status: "cancelled",
    content: "Earlier output remains visible",
    processSummary: {
      collapsed: false, runId, status: "cancelled", terminalReason: "runtime_restart_interrupted",
      steps: [], tools: [],
    },
  })), { kind: "restart_interrupted", diagnosticId: runId });
});

test("only a valid existing run ID is exposed as a diagnostic ID", () => {
  assert.equal(assistantOutcome(message({
    status: "failed",
    processSummary: { collapsed: true, runId: "raw error with credentials", status: "failed", steps: [], tools: [] },
  })).diagnosticId, undefined);
});

test("image generation stays running, and a failed image task exposes only its safe ID", () => {
  assert.equal(assistantOutcome(message({ isGeneratingImage: true, status: "running" })).kind, "running");
  assert.deepEqual(assistantOutcome(message({
    content: "The image result is uncertain",
    status: "failed",
    outcomeUncertain: true,
    diagnosticId: "imt_aaaaaaaaaaaaaaaaaaaa",
  })), { kind: "unknown", diagnosticId: "imt_aaaaaaaaaaaaaaaaaaaa" });
  assert.equal(assistantOutcome(message({
    status: "failed", diagnosticId: "raw error with credentials",
  })).diagnosticId, undefined);
});

test("an image transport loss after dispatch stays unknown, while an explicit provider failure does not", () => {
  assert.deepEqual(imageFailureReceipt(new Error("connection reset"), true), { uncertain: true });
  assert.deepEqual(imageFailureReceipt({ response: { data: { detail: {
    error_code: "outcome_unknown", task_id: "imt_aaaaaaaaaaaaaaaaaaaa",
  } } } }, true), { uncertain: true, diagnosticId: "imt_aaaaaaaaaaaaaaaaaaaa" });
  assert.deepEqual(imageFailureReceipt({ response: { data: { detail: {
    error_code: "provider_failed", task_id: "not-safe",
  } } } }, true), { uncertain: false });
  assert.deepEqual(imageFailureReceipt(new Error("user message not saved"), false), { uncertain: false });
});

test("source restriction keeps the public cancelled or failed outcome", () => {
  const restricted = { id: "source", role: "assistant", content: "", sourceAccessRevoked: true } as ChatMessage;
  assert.equal(assistantOutcome({ ...restricted, status: "cancelled" }).kind, "cancelled");
  assert.equal(assistantOutcome({ ...restricted, status: "failed" }).kind, "failed");
  assert.equal(assistantOutcome({ ...restricted, status: "completed" }).kind, "succeeded");
  assert.equal(assistantOutcome(restricted).kind, "source_revoked");
});
