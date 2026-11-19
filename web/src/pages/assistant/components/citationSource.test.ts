import assert from "node:assert/strict";
import test from "node:test";
import { citationSource } from "./citationSource.ts";
import type { RetrievedChunk } from "../types.ts";

const hash = "a".repeat(64);
const chunk = (overrides: Partial<RetrievedChunk>): RetrievedChunk => ({
  content: "saved excerpt", score: 0.9, ...overrides,
});

test("top-level event identity opens the exact saved version", () => {
  const source = citationSource("dataset-a", chunk({
    document_id: "doc-a", source_version: 3, source_hash: hash,
  }));
  assert.deepEqual(source, {
    datasetId: "dataset-a", documentId: "doc-a", excerpt: "saved excerpt",
    provenance: "versioned", sourceVersion: 3, sourceHash: hash,
  });
});

test("metadata is a fallback for projected history and old events remain excerpts", () => {
  assert.equal(citationSource("dataset-a", chunk({
    metadata: { document_id: "doc-a", source_version: 2, source_hash: hash },
  })).provenance, "versioned");
  assert.equal(citationSource("dataset-a", chunk({ document_id: "doc-a" })).provenance, "legacy");
  assert.equal(citationSource("dataset-a", chunk({
    document_id: "doc-a", metadata: { source_version: null, source_hash: null },
  })).provenance, "legacy");
  assert.equal(citationSource("dataset-a", chunk({
    document_id: null, source_version: null, source_hash: null,
    metadata: { document_id: "doc-a", source_version: 2, source_hash: hash },
  } as unknown as Partial<RetrievedChunk>)).provenance, "versioned");
});

test("missing or conflicting identities never authorize a direct URL fallback", () => {
  for (const source of [
    citationSource("dataset-a", chunk({ source_url: "https://example.test/new", image_url: "https://example.test/new.png" })),
    citationSource("dataset-a", chunk({ document_id: "doc-a", source_version: 3 })),
    citationSource("dataset-a", chunk({ document_id: "doc-a", metadata: { document_id: "doc-b" } })),
    citationSource("dataset-a", chunk({ document_id: "doc-a", metadata: { dataset_id: "dataset-b" } })),
    citationSource("dataset-a", chunk({ document_id: "doc-a", source_version: 3, source_hash: hash,
      metadata: { source_version: 4 } })),
    citationSource("dataset-a", chunk({ document_id: "doc-a", source_version: 3, source_hash: hash,
      metadata: { source_version: 3, source_hash: null } })),
    citationSource("dataset-a", chunk({ document_id: "doc-a", source_version: 3, source_hash: null,
      metadata: { source_version: 3, source_hash: hash } } as unknown as Partial<RetrievedChunk>)),
  ]) {
    assert.equal(source.provenance, "invalid");
  }
});
