// The browser app type gate intentionally omits Node globals; these imports
// are supplied by the `node --test` runtime used for this file.
// @ts-expect-error -- node built-in types are outside tsconfig.app.json.
import assert from "node:assert/strict";
// @ts-expect-error -- node built-in types are outside tsconfig.app.json.
import { test } from "node:test";

import {
  CHUNKING_CONFIG_API_FIELDS,
  DEFAULT_CHUNKING_CONFIG,
  DEFAULT_RETRIEVAL_CONFIG,
  DOCUMENT_DISPLAY_STATUS_VOCABULARY,
  DOCUMENT_LIFECYCLE_MARKER_KEY,
  SAFE_CHUNK_HEADING_PATTERNS,
  deriveDisplayStatus,
  documentNeedsLifecyclePolling,
  hasPendingDocumentActivation,
  isSafeHeadingPatterns,
  parseSegmentKeywords,
  resolveDisplayStatus,
} from "./knowledge.ts";
import type { DocumentDisplayStatus } from "./knowledge.ts";

// deriveDisplayStatus is the client-side mirror of the backend's
// derive_document_display_status (knowledge-service document_service,
// PRD T1.1). These tests pin the exact precedence contract so the two
// implementations cannot drift.

test("deriveDisplayStatus: archived wins over every other state", () => {
  assert.equal(deriveDisplayStatus({ archived: true, status: "failed" }), "archived");
  assert.equal(deriveDisplayStatus({ archived: true, status: "completed" }), "archived");
  assert.equal(
    deriveDisplayStatus({ archived: true, status: "completed", enabled: false }),
    "archived"
  );
  assert.equal(deriveDisplayStatus({ archived: true, status: "indexing" }), "archived");
});

test("deriveDisplayStatus: error and failed collapse to error", () => {
  assert.equal(deriveDisplayStatus({ status: "error" }), "error");
  assert.equal(deriveDisplayStatus({ status: "failed" }), "error");
  // The backend normalises case and surrounding whitespace.
  assert.equal(deriveDisplayStatus({ status: " Failed " }), "error");
  assert.equal(deriveDisplayStatus({ status: "ERROR" }), "error");
});

test("deriveDisplayStatus: paused maps straight through", () => {
  assert.equal(deriveDisplayStatus({ status: "paused" }), "paused");
});

test("deriveDisplayStatus: completed splits on enabled flag", () => {
  assert.equal(deriveDisplayStatus({ status: "completed", enabled: false }), "disabled");
  assert.equal(deriveDisplayStatus({ status: "completed", enabled: true }), "available");
  // Missing enabled defaults to enabled (backend: doc.get("enabled", True)).
  assert.equal(deriveDisplayStatus({ status: "completed" }), "available");
  assert.equal(deriveDisplayStatus({ status: "completed", enabled: null }), "available");
});

test("deriveDisplayStatus: waiting is the only queuing state", () => {
  assert.equal(deriveDisplayStatus({ status: "waiting" }), "queuing");
  assert.equal(deriveDisplayStatus({ status: " waiting " }), "queuing");
});

test("deriveDisplayStatus: active pipeline states map to indexing", () => {
  for (const status of [
    "parsing",
    "splitting",
    "indexing",
    "syncing",
    "uploading",
    "uploading_images",
    "embedding_images",
    "segmenting",
    "embedding",
    "queued",
  ]) {
    assert.equal(deriveDisplayStatus({ status }), "indexing", `status=${status}`);
  }
});

test("deriveDisplayStatus: unknown and empty states fail closed to indexing", () => {
  assert.equal(deriveDisplayStatus({ status: "something-new" }), "indexing");
  assert.equal(deriveDisplayStatus({ status: "" }), "indexing");
  assert.equal(deriveDisplayStatus({ status: null }), "indexing");
  assert.equal(deriveDisplayStatus({}), "indexing");
  assert.equal(deriveDisplayStatus(null), "indexing");
  assert.equal(deriveDisplayStatus(undefined), "indexing");
});

test("deriveDisplayStatus: precedence order archived > error > paused > completed", () => {
  // archived beats error.
  assert.equal(deriveDisplayStatus({ archived: true, status: "error" }), "archived");
  // error beats paused-ish/completed inputs.
  assert.equal(deriveDisplayStatus({ status: "failed", enabled: true }), "error");
  // paused beats queuing/indexing buckets.
  assert.equal(deriveDisplayStatus({ status: "paused", enabled: false }), "paused");
});

test("resolveDisplayStatus: prefers a valid backend-stamped display_status", () => {
  assert.equal(
    resolveDisplayStatus({ display_status: "available", status: "indexing" }),
    "available"
  );
  assert.equal(
    resolveDisplayStatus({ display_status: "archived", status: "completed", enabled: true }),
    "archived"
  );
});

test("resolveDisplayStatus: falls back to derivation when stamp is missing or invalid", () => {
  assert.equal(resolveDisplayStatus({ status: "completed" }), "available");
  assert.equal(resolveDisplayStatus({ display_status: null, status: "waiting" }), "queuing");
  // An unexpected stamp value must fail closed through derivation, never leak.
  assert.equal(
    resolveDisplayStatus({ display_status: "weird-new-state", status: "completed" }),
    "available"
  );
  assert.equal(resolveDisplayStatus(null), "indexing");
});

test("DOCUMENT_DISPLAY_STATUS_VOCABULARY: fixed 7-value contract", () => {
  const expected: readonly DocumentDisplayStatus[] = [
    "queuing",
    "indexing",
    "paused",
    "error",
    "available",
    "disabled",
    "archived",
  ];
  assert.deepEqual([...DOCUMENT_DISPLAY_STATUS_VOCABULARY], [...expected]);
});

test("activation polling: raw waiting wins over an old archived display state", () => {
  const pendingRestore = {
    status: "waiting",
    display_status: "archived" as const,
    enabled: true,
    archived: true,
    metadata: {
      [DOCUMENT_LIFECYCLE_MARKER_KEY]: {
        status: "pending",
        desired_enabled: true,
        desired_archived: false,
      },
    },
  };

  assert.equal(resolveDisplayStatus(pendingRestore), "archived");
  assert.equal(hasPendingDocumentActivation(pendingRestore), true);
  assert.equal(documentNeedsLifecyclePolling(pendingRestore), true);
});

test("activation polling: enabled stays false until a queued rebuild completes", () => {
  const pendingEnable = {
    status: "waiting",
    enabled: false,
    archived: false,
    metadata: {
      [DOCUMENT_LIFECYCLE_MARKER_KEY]: { status: "pending" },
    },
  };
  assert.equal(documentNeedsLifecyclePolling(pendingEnable), true);

  const completedEnable = {
    ...pendingEnable,
    status: "completed",
    enabled: true,
    metadata: {},
  };
  assert.equal(hasPendingDocumentActivation(completedEnable), false);
  assert.equal(documentNeedsLifecyclePolling(completedEnable), false);
});

test("activation polling: malformed or deactivating markers are not worker-owned", () => {
  assert.equal(hasPendingDocumentActivation({ metadata: null }), false);
  assert.equal(
    hasPendingDocumentActivation({
      metadata: { [DOCUMENT_LIFECYCLE_MARKER_KEY]: [] },
    }),
    false
  );
  assert.equal(
    hasPendingDocumentActivation({
      metadata: { [DOCUMENT_LIFECYCLE_MARKER_KEY]: { status: "deactivating" } },
    }),
    false
  );
});

test("default configs: product baseline values stay frozen", () => {
  // Baseline-first discipline: these are the majority values the running UI
  // already uses. Changing them is a tuning decision that waits for T0
  // metrics, so any edit here must be deliberate.
  assert.equal(DEFAULT_CHUNKING_CONFIG.mode, "automatic");
  assert.equal(DEFAULT_CHUNKING_CONFIG.chunk_size, 500);
  assert.equal(DEFAULT_CHUNKING_CONFIG.chunk_overlap, 50);

  assert.equal(DEFAULT_RETRIEVAL_CONFIG.mode, "hybrid");
  assert.equal(DEFAULT_RETRIEVAL_CONFIG.top_k, 5);
  assert.equal(DEFAULT_RETRIEVAL_CONFIG.score_threshold, 0.3);
  assert.equal(DEFAULT_RETRIEVAL_CONFIG.fusion.strategy, "rrf");
  assert.equal(DEFAULT_RETRIEVAL_CONFIG.fusion.rrf_k, 60);
  assert.equal(DEFAULT_RETRIEVAL_CONFIG.fusion.alpha, 0.7);
  assert.equal(DEFAULT_RETRIEVAL_CONFIG.rerank.enabled, false);
  assert.equal(DEFAULT_RETRIEVAL_CONFIG.rerank.model, "gte-rerank");
  assert.equal(DEFAULT_RETRIEVAL_CONFIG.mmr.enabled, false);
  assert.equal(DEFAULT_RETRIEVAL_CONFIG.mmr.lambda, 0.5);
});

// CHUNKING_CONFIG_API_FIELDS mirrors the backend ChunkingConfigSchema field
// set (extra="forbid"). regex_pattern must stay out: the schema validator
// rejects it even when mode is not "regex".
test("chunking API allow-list: mirrors schema, excludes regex_pattern", () => {
  // Widen the literal tuple: the negative probes below check strings that
  // are deliberately NOT members.
  const fields = CHUNKING_CONFIG_API_FIELDS as readonly string[];
  assert.ok(fields.includes("mode"));
  assert.ok(fields.includes("chunk_size"));
  assert.ok(fields.includes("chunk_overlap"));
  assert.ok(fields.includes("use_token_count"));
  assert.ok(fields.includes("separator"));
  assert.ok(fields.includes("heading_patterns"));
  assert.ok(!fields.includes("regex_pattern"));
  assert.ok(fields.includes("extract_metadata"));
  assert.ok(fields.includes("metadata_fields"));
  assert.ok(fields.includes("normalize_whitespace"));
  assert.ok(fields.includes("strip_html"));
  assert.ok(fields.includes("page_marker"));
  assert.ok(fields.includes("segmentation"));
});

test("parseSegmentKeywords: splits on ASCII/full-width commas and CJK enumeration comma", () => {
  assert.deepEqual(parseSegmentKeywords("报销,发票，审批、流程"), [
    "报销",
    "发票",
    "审批",
    "流程",
  ]);
  assert.deepEqual(parseSegmentKeywords("  spaced ,  items  "), ["spaced", "items"]);
});

test("parseSegmentKeywords: drops empties and de-duplicates keeping first order", () => {
  assert.deepEqual(parseSegmentKeywords(""), []);
  assert.deepEqual(parseSegmentKeywords(",,，、 ,"), []);
  assert.deepEqual(parseSegmentKeywords("a, b, a, c,,b"), ["a", "b", "c"]);
});

test("isSafeHeadingPatterns: only the exact backend triple passes", () => {
  assert.equal(isSafeHeadingPatterns([...SAFE_CHUNK_HEADING_PATTERNS]), true);
  assert.equal(isSafeHeadingPatterns([]), false);
  assert.equal(isSafeHeadingPatterns(SAFE_CHUNK_HEADING_PATTERNS.slice(0, 2)), false);
  assert.equal(isSafeHeadingPatterns([...SAFE_CHUNK_HEADING_PATTERNS, "^extra$"]), false);
  assert.equal(
    isSafeHeadingPatterns(["^hacked$", SAFE_CHUNK_HEADING_PATTERNS[1], SAFE_CHUNK_HEADING_PATTERNS[2]]),
    false
  );
  assert.equal(isSafeHeadingPatterns("not-an-array"), false);
  assert.equal(isSafeHeadingPatterns(undefined), false);
});
