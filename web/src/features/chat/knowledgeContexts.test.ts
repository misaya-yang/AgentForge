import assert from "node:assert/strict";
import { test } from "node:test";

import { mergeKnowledgeContexts } from "./knowledgeContexts.ts";

test("repeated searches of one selected KB remain one dataset", () => {
  const selected = ["kb-selected"];
  const first = mergeKnowledgeContexts([], {
    query: "agent orchestration",
    chunks: [{
      dataset_id: "kb-selected",
      document_id: "doc-a",
      segment_id: "segment-a",
      content: "first result",
      score: 0.9,
    }],
  }, selected, { "kb-selected": "Agent research" });
  const second = mergeKnowledgeContexts(first, {
    query: "context engineering",
    chunks: [
      {
        metadata: { dataset_id: "kb-selected" },
        document_id: "doc-a",
        segment_id: "segment-a",
        content: "first result",
        score: 0.9,
      },
      {
        metadata: { dataset_id: "kb-selected" },
        document_id: "doc-b",
        segment_id: "segment-b",
        content: "second result",
        score: 0.8,
      },
    ],
  }, selected, { "kb-selected": "Agent research" });

  assert.equal(second.length, 1);
  assert.equal(second[0]?.dataset_id, "kb-selected");
  assert.equal(second[0]?.dataset_name, "Agent research");
  assert.deepEqual(second[0]?.chunks.map((chunk) => chunk.segment_id), ["segment-a", "segment-b"]);
});

test("projected chunks outside the selected Runtime dataset scope are ignored", () => {
  const contexts = mergeKnowledgeContexts([], {
    dataset_ids: ["kb-selected", "kb-foreign"],
    chunks: [
      { dataset_id: "kb-selected", content: "allowed", score: 0.9 },
      { dataset_id: "kb-foreign", content: "must not render", score: 0.8 },
    ],
  }, ["kb-selected"]);

  assert.equal(contexts.length, 1);
  assert.equal(contexts[0]?.chunks.length, 1);
  assert.equal(contexts[0]?.chunks[0]?.content, "allowed");
});
