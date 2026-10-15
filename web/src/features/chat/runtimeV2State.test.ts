import assert from "node:assert/strict";
import { test } from "node:test";

import {
  createRuntimeV2RunSnapshot,
  isExpectedApprovalRejection,
  projectAgentV2Events,
  reduceRuntimeV2RunSnapshot,
  withAfterSequence,
  type AgentV2Event,
} from "./runtimeV2State.ts";

function event(sequence: number, eventType: string, data: Record<string, unknown>): AgentV2Event {
  return {
    schema_version: "agent-event/v2",
    thread_id: "thread-1",
    sequence,
    event: {
      id: `event-${sequence}`,
      key: `event-${sequence}`,
      type: eventType,
      item_id: null,
      turn_id: "run-1",
      status: null,
      payload: { event_type: eventType, data },
    },
    timestamp: "2026-08-30T12:00:00Z",
  };
}

test("durable replay keeps approval requests actionable and cleanly folds rejection", () => {
  const approval = event(4, "rollout/item", {
    type: "approval_request",
    approval_id: "approval-1",
    tool_call_id: "tool-1",
    tool_name: "mcp_docgen__generate_document",
  });
  assert.equal(projectAgentV2Events(approval)[0]?.event_type, "approval_required");

  let snapshot = reduceRuntimeV2RunSnapshot(createRuntimeV2RunSnapshot(), approval);
  assert.equal(snapshot.pendingApproval?.approvalId, "approval-1");
  snapshot = reduceRuntimeV2RunSnapshot(snapshot, event(5, "approval_result", {
    approval_id: "approval-1",
    approved: false,
    status: "rejected",
  }));
  assert.equal(snapshot.pendingApproval, undefined);
  assert.equal(snapshot.rejectedApproval, true);
  assert.equal(isExpectedApprovalRejection({}, snapshot.rejectedApproval), true);
});

test("replay cursor advances without dropping the turn filter", () => {
  assert.equal(
    withAfterSequence("/api/v2/agent/threads/t/events?after_sequence=2&turn_id=r", 7),
    "/api/v2/agent/threads/t/events?after_sequence=7&turn_id=r",
  );
});

test("durable office result projects a downloadable DOCX artifact", () => {
  const projected = projectAgentV2Events(event(8, "rollout/item", {
    type: "dynamicToolCall",
    tool: "mcp_docgen__generate_document",
    contentItems: [{
      type: "inputText",
      text: JSON.stringify({
        artifact_id: "art_docx",
        filename: "report.docx",
        download_url: "/api/v1/assistant/artifacts/art_docx/download",
      }),
    }],
  }));
  const artifact = projected.find((item) => item.event_type === "artifact_created");
  assert.equal((artifact?.data as Record<string, unknown>).format, "docx");
  assert.equal((artifact?.data as Record<string, unknown>).artifact_id, "art_docx");
});
