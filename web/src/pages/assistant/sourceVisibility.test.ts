import assert from "node:assert/strict";
import test from "node:test";
import { redactRestrictedSourceMessages, redactSourceMessage } from "./sourceVisibility.ts";
import type { ChatMessage } from "./types.ts";

test("source revocation clears content and private activity while retaining pending decision identity", () => {
  const message: ChatMessage = {
    id: "message-a", role: "assistant", content: "PRIVATE_SOURCE",
    thinkingContent: "PRIVATE_SOURCE", meta: { private: "PRIVATE_SOURCE" },
    generatedArtifacts: [{ id: "art-a", type: "file", format: "txt", title: "PRIVATE_SOURCE", url: "https://example.test/private" }],
    processSummary: {
      collapsed: false, runId: "run-a", runtimeThreadId: "thread-a", status: "blocked",
      currentStep: "PRIVATE_SOURCE", steps: [{ id: "step-a", title: "PRIVATE_SOURCE", status: "running" }],
      tools: [{ id: "call-a", name: "generate_quiz", status: "approval_required", approvalId: "approval-a", summary: "PRIVATE_SOURCE", error: "PRIVATE_SOURCE" }],
    },
  };
  const redacted = redactSourceMessage(message);
  assert.ok(redacted.sourceAccessRevoked);
  assert.equal(redacted.processSummary?.runId, "run-a");
  assert.equal(redacted.processSummary?.status, "blocked");
  assert.equal(redacted.processSummary?.tools[0].approvalId, "approval-a");
  assert.equal(JSON.stringify(redacted).includes("PRIVATE_SOURCE"), false);
  assert.equal(message.content, "PRIVATE_SOURCE");
});

test("uploaded user messages stay unchanged", () => {
  const message: ChatMessage = { id: "user-a", role: "user", content: "User original" };
  assert.equal(redactSourceMessage(message), message);
});

test("all messages of a restricted run are hidden, including an earlier tool prelude", () => {
  const messages: ChatMessage[] = [
    { id: "public", role: "assistant", content: "ordinary", runtimeRunId: "public" },
    { id: "prelude", role: "assistant", content: "private excerpt", runtimeRunId: "private" },
    { id: "answer", role: "assistant", content: "private answer", processSummary: {
      collapsed: true, runId: "private", status: "succeeded", steps: [], tools: [],
    } },
  ];
  const result = redactRestrictedSourceMessages(messages, new Set(["private"]));
  assert.equal(result[0], messages[0]);
  assert.deepEqual(result.slice(1).map((m) => [m.content, m.sourceAccessRevoked]), [["", true], ["", true]]);
});
