import assert from "node:assert/strict";
import test from "node:test";
import { docirCitation } from "./docirCitation.ts";
const source = () => ({ tenant_id: "t", dataset_id: "d", document_id: "doc", document_version: "immutable-v", publication_generation: "4", source_sha256: "a".repeat(64), execution_id: "execution", parser_profile: "measured" });
const citation = () => ({ source: source(), page_number: 2, block_id: "block", region: null as unknown, coordinate_space: "unknown" });
test("DocIR uses enriched immutable identity and preserves unknown geometry", () => {
  const evidence = docirCitation({ citation: citation() }, "t", "d", "doc")!;
  assert.equal(evidence.page, 2); assert.equal(evidence.documentVersion, "immutable-v"); assert.equal(evidence.region, null);
  assert.equal(docirCitation({ source_reference: citation() }, "t", "d", "doc"), null);
});
test("cross-tenant/document/generation and unmeasured coordinates never become region evidence", () => {
  for (const key of ["tenant_id", "dataset_id", "document_id", "document_version", "publication_generation", "source_sha256", "execution_id", "parser_profile"]) {
    const raw = citation(); (raw.source as Record<string, unknown>)[key] = ["tenant_id", "dataset_id", "document_id"].includes(key) ? "other" : "";
    assert.equal(docirCitation({ citation: raw }, "t", "d", "doc"), null, key);
  }
  const raw = citation(); raw.region = { x0: 0, y0: 0, x1: 100, y1: 120 };
  assert.equal(docirCitation({ citation: raw }, "t", "d", "doc"), null);
  raw.coordinate_space = "page_points"; assert.deepEqual(docirCitation({ citation: raw }, "t", "d", "doc")?.region, raw.region);
  for (const region of [{ x0: -1, y0: 0, x1: 3, y1: 4 }, { x0: 3, y0: 0, x1: 1, y1: 4 }, { x0: 0, y0: 0, x1: Infinity, y1: 4 }]) {
    raw.region = region; assert.equal(docirCitation({ citation: raw }, "t", "d", "doc"), null);
  }
});
