// The browser app type gate intentionally omits Node globals; these imports
// are supplied by the `node --test` runtime used for this file.
// @ts-expect-error -- node built-in types are outside tsconfig.app.json.
import assert from "node:assert/strict";
// @ts-expect-error -- node built-in types are outside tsconfig.app.json.
import { test } from "node:test";
import type { EvalExample } from "@/api/eval";
import {
  diffEvalCases,
  evalCaseToImportItem,
  exampleToEvalCase,
  extractRelevantSegmentIds,
  findKbEvalDataset,
  findKbEvalDatasetPaged,
  hashEvalCaseInput,
  hitTestEvalCaseId,
  knowledgeFailureToImportItem,
  sourceVersionsFromHits,
  HIT_TEST_EVAL_SOURCE,
  KB_FAILURE_EVAL_SPLIT,
  KB_EVAL_DATASET_SOURCE,
  KB_EVAL_SPLIT,
  kbEvalDatasetName,
} from "./evalCaseStore.ts";

function makeExample(overrides: Partial<EvalExample> = {}): EvalExample {
  return {
    example_id: "ex-1",
    dataset_id: "eval-ds-1",
    tenant_id: "tenant-1",
    split: "regression",
    input: { query: "员工手册在哪里？" },
    expected_output: { relevant_segment_ids: ["seg-a", "seg-b"] },
    metadata: { case_id: "case_1" },
    created_by: "user-1",
    ...overrides,
  };
}

test("extractRelevantSegmentIds keeps strings only, trims, dedupes, preserves order", () => {
  assert.deepEqual(
    extractRelevantSegmentIds(["seg-b", " seg-a ", "seg-b", "", "  ", 7, null, { id: "x" }]),
    ["seg-b", "seg-a"]
  );
});

test("extractRelevantSegmentIds returns [] for non-array input", () => {
  assert.deepEqual(extractRelevantSegmentIds(undefined), []);
  assert.deepEqual(extractRelevantSegmentIds("seg-a"), []);
  assert.deepEqual(extractRelevantSegmentIds({ ids: ["seg-a"] }), []);
});

test("exampleToEvalCase projects an example into a workbench case", () => {
  const result = exampleToEvalCase(makeExample());
  assert.deepEqual(result, {
    caseId: "case_1",
    exampleId: "ex-1",
    query: "员工手册在哪里？",
    relevantSegmentIds: ["seg-a", "seg-b"],
  });
});

test("exampleToEvalCase falls back to example_id when metadata.case_id is absent", () => {
  const result = exampleToEvalCase(makeExample({ metadata: {} }));
  assert.equal(result?.caseId, "ex-1");
});

test("exampleToEvalCase returns null without a usable input.query", () => {
  assert.equal(exampleToEvalCase(makeExample({ input: {} })), null);
  assert.equal(exampleToEvalCase(makeExample({ input: { query: "   " } })), null);
  assert.equal(exampleToEvalCase(makeExample({ input: { query: 42 } })), null);
});

test("pending failure observations stay out of the labelled retrieval workbench", () => {
  assert.equal(exampleToEvalCase(makeExample({ metadata: { source_kind: "kb_failure" } })), null);
});

test("failure import keeps expectation separate from observed hits and requires review", () => {
  const sourceHash = "a".repeat(64);
  const hit = {
    segment_id: "seg-1",
    document_id: "doc-1",
    source_version: 3,
    source_hash: sourceHash,
  };
  const item = knowledgeFailureToImportItem({
    kbDatasetId: "kb-1",
    query: "  年假规则？ ",
    expectedAnswer: "应提前五天申请",
    observedHits: [hit],
    source: HIT_TEST_EVAL_SOURCE,
    sourceTraceId: "trace-1",
    confirmedTraceId: "trace-1",
    queryFingerprint: "fp-1",
  });
  assert.equal(item.split, KB_FAILURE_EVAL_SPLIT);
  assert.deepEqual(item.input, { query: "年假规则？" });
  assert.deepEqual(item.expected_output, { answer: "应提前五天申请" });
  assert.equal(item.source_trace_id, "trace-1");
  assert.deepEqual(item.metadata?.kb_observed_segment_ids, ["seg-1"]);
  assert.deepEqual(item.metadata?.kb_source_versions, [{
    kb_dataset_id: "kb-1",
    document_id: "doc-1",
    segment_id: "seg-1",
    source_version: 3,
    source_hash: sourceHash,
  }]);
  assert.equal(item.metadata?.review_status, "pending");
  assert.equal(item.metadata?.behavior_confirmed, false);
  assert.equal(item.metadata?.source_kind, "kb_failure");
  const repeated = knowledgeFailureToImportItem({
    kbDatasetId: "kb-1", query: "年假规则？", expectedAnswer: "另一个答案",
    observedHits: [], source: HIT_TEST_EVAL_SOURCE, sourceTraceId: "trace-1",
  });
  assert.equal(repeated.case_id, item.case_id);
  assert.throws(() => knowledgeFailureToImportItem({
    kbDatasetId: "kb-1", query: "q", expectedAnswer: " ",
    observedHits: [], source: HIT_TEST_EVAL_SOURCE,
  }));
});

test("source references omit incomplete or unverified version identities", () => {
  const refs = sourceVersionsFromHits("kb-1", [
    { segment_id: "ok", document_id: "doc-1", source_version: 2, source_hash: "B".repeat(64) },
    { segment_id: "stale-metadata", document_id: "doc-5", metadata: {
      source_version: 7, source_hash: "c".repeat(64),
    } },
    { segment_id: "no-hash", document_id: "doc-2", source_version: 2 },
    { segment_id: "bad-hash", document_id: "doc-3", source_version: 2, source_hash: "guess" },
    { segment_id: "bad-version", document_id: "doc-4", source_version: 0, source_hash: "a".repeat(64) },
  ]);
  assert.deepEqual(refs, [{
    kb_dataset_id: "kb-1", document_id: "doc-1", segment_id: "ok",
    source_version: 2, source_hash: "b".repeat(64),
  }]);
});

test("exampleToEvalCase trims the query", () => {
  const result = exampleToEvalCase(makeExample({ input: { query: "  报销流程  " } }));
  assert.equal(result?.query, "报销流程");
});

test("evalCaseToImportItem carries every field validate_case requires", () => {
  const item = evalCaseToImportItem({
    caseId: "case_1",
    kbDatasetId: "kb-1",
    query: "员工手册在哪里？",
    relevantSegmentIds: ["seg-a", "seg-b"],
  });
  // All six validate_case-required fields are present (backend returns 422
  // when any of case_id/input/expected_output/expected_trajectory/assertions/
  // metadata is missing).
  assert.equal(item.case_id, "case_1");
  assert.deepEqual(item.input, { query: "员工手册在哪里？" });
  assert.deepEqual(item.expected_output, { relevant_segment_ids: ["seg-a", "seg-b"] });
  assert.deepEqual(item.expected_trajectory, {});
  assert.deepEqual(item.assertions, []);
  assert.deepEqual(item.metadata, {
    source: KB_EVAL_DATASET_SOURCE,
    kb_dataset_id: "kb-1",
  });
  assert.equal(item.split, KB_EVAL_SPLIT);
});

test("evalCaseToImportItem honours a custom source", () => {
  const item = evalCaseToImportItem({
    caseId: "case_1",
    kbDatasetId: "kb-1",
    query: "q",
    relevantSegmentIds: [],
    source: "kb-hit-test",
  });
  assert.equal(item.metadata?.source, "kb-hit-test");
});

test("evalCaseToImportItem links a confirmed backend RAG trace", () => {
  const item = evalCaseToImportItem({
    caseId: "case_trace",
    kbDatasetId: "kb-1",
    query: "q",
    relevantSegmentIds: ["seg-1"],
    sourceTraceId: "b3ccb99e-51a0-44f9-80a9-f2dce7b75c28",
  });
  assert.equal(item.source_trace_id, "b3ccb99e-51a0-44f9-80a9-f2dce7b75c28");
});

test("diffEvalCases splits new / changed / unchanged / removed", () => {
  const serverCases = [
    { caseId: "case_keep", exampleId: "ex-keep", query: "保持不变的用例", relevantSegmentIds: ["seg-1"] },
    { caseId: "case_edit", exampleId: "ex-edit", query: "将被修改的用例", relevantSegmentIds: ["seg-2"] },
    { caseId: "case_gone", exampleId: "ex-gone", query: "工作台里已删除", relevantSegmentIds: ["seg-3"] },
  ];
  const localCases = [
    { id: "case_keep", query: "保持不变的用例", relevantSegmentIds: ["seg-1"] },
    { id: "case_edit", query: "将被修改的用例（改）", relevantSegmentIds: ["seg-2"] },
    { id: "case_new", query: "新增用例", relevantSegmentIds: ["seg-9"] },
  ];

  const diff = diffEvalCases({ localCases, serverCases, kbDatasetId: "kb-1" });

  assert.equal(diff.toImport.length, 1);
  assert.equal(diff.toImport[0].case_id, "case_new");
  assert.equal(diff.toImport[0].metadata?.kb_dataset_id, "kb-1");

  assert.equal(diff.toUpdate.length, 1);
  assert.deepEqual(diff.toUpdate[0], {
    exampleId: "ex-edit",
    caseId: "case_edit",
    query: "将被修改的用例（改）",
    relevantSegmentIds: ["seg-2"],
  });

  assert.equal(diff.unchangedCount, 1);
  assert.deepEqual(diff.toDelete, [{ exampleId: "ex-gone", caseId: "case_gone" }]);
});

test("diffEvalCases deletes only explicit removals when the server changed concurrently", () => {
  const serverCases = [
    { caseId: "case_removed", exampleId: "ex-removed", query: "已删除", relevantSegmentIds: [] },
    { caseId: "case_remote", exampleId: "ex-remote", query: "其他客户端新增", relevantSegmentIds: [] },
  ];

  const diff = diffEvalCases({
    localCases: [],
    serverCases,
    kbDatasetId: "kb-1",
    removedExampleIds: ["ex-removed"],
  });

  assert.deepEqual(diff.toDelete, [
    { exampleId: "ex-removed", caseId: "case_removed" },
  ]);
});

test("diffEvalCases treats reordered segment ids and surrounding whitespace as unchanged", () => {
  const serverCases = [
    { caseId: "case_a", exampleId: "ex-a", query: "查询", relevantSegmentIds: ["seg-2", "seg-1"] },
  ];
  const diff = diffEvalCases({
    localCases: [{ id: "case_a", query: "  查询 ", relevantSegmentIds: ["seg-1", "seg-2"] }],
    serverCases,
    kbDatasetId: "kb-1",
  });
  assert.equal(diff.unchangedCount, 1);
  assert.equal(diff.toImport.length, 0);
  assert.equal(diff.toUpdate.length, 0);
});

test("diffEvalCases detects changed segment ids with the same query", () => {
  const serverCases = [
    { caseId: "case_a", exampleId: "ex-a", query: "查询", relevantSegmentIds: ["seg-1"] },
  ];
  const diff = diffEvalCases({
    localCases: [{ id: "case_a", query: "查询", relevantSegmentIds: ["seg-1", "seg-2"] }],
    serverCases,
    kbDatasetId: "kb-1",
  });
  assert.equal(diff.toUpdate.length, 1);
  assert.deepEqual(diff.toUpdate[0].relevantSegmentIds, ["seg-1", "seg-2"]);
});

test("hashEvalCaseInput is deterministic and whitespace-insensitive", () => {
  assert.equal(hashEvalCaseInput("报销 流程 是什么"), hashEvalCaseInput("  报销   流程 是什么  "));
  assert.match(hashEvalCaseInput("任意查询"), /^[0-9a-f]{8}$/);
  assert.notEqual(hashEvalCaseInput("查询一"), hashEvalCaseInput("查询二"));
});

test("hitTestEvalCaseId is stable across repeated sends", () => {
  const first = hitTestEvalCaseId("kb-1", "年假 怎么申请");
  const second = hitTestEvalCaseId("kb-1", "  年假   怎么申请 ");
  assert.equal(first, second);
  assert.equal(first.startsWith("kb-hit-kb-1-"), true);
  assert.notEqual(first, hitTestEvalCaseId("kb-2", "年假怎么申请"));
});

test("findKbEvalDataset matches by metadata.kb_dataset_id only", () => {
  const linked = {
    dataset_id: "eval-linked",
    metadata: { kb_dataset_id: "kb-1" },
  };
  const unrelated = {
    dataset_id: "eval-other",
    metadata: { kb_dataset_id: "kb-2" },
  };
  const bare = { dataset_id: "eval-bare", metadata: {} };
  // eslint-disable-next-line @typescript-eslint/no-explicit-any
  const datasets = [unrelated, bare, linked] as any;
  assert.equal(findKbEvalDataset(datasets, "kb-1")?.dataset_id, "eval-linked");
  assert.equal(findKbEvalDataset(datasets, "kb-missing"), undefined);
});

test("linked eval dataset lookup reaches pages after the first 200", async () => {
  const calls: number[] = [];
  const unrelated = Array.from({ length: 200 }, (_, index) => ({
    dataset_id: `eval-${index}`, metadata: { kb_dataset_id: `other-${index}` },
  }));
  const linked = { dataset_id: "eval-linked", metadata: { kb_dataset_id: "kb-1" } };
  const found = await findKbEvalDatasetPaged("kb-1", async (offset) => {
    calls.push(offset);
    // eslint-disable-next-line @typescript-eslint/no-explicit-any
    return { datasets: (offset === 0 ? unrelated : [linked]) as any, total: 201 };
  });
  assert.equal(found?.dataset_id, "eval-linked");
  assert.deepEqual(calls, [0, 200]);
});

test("kbEvalDatasetName is deterministic per KB dataset", () => {
  assert.equal(kbEvalDatasetName("kb-1"), "kb-retrieval-eval-kb-1");
});
