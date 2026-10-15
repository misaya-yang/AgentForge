// @ts-expect-error -- node built-ins are supplied by the node --test runtime.
import assert from "node:assert/strict";
// @ts-expect-error -- node built-ins are supplied by the node --test runtime.
import { test } from "node:test";

import type { DatasetSources } from "@/api/knowledge";
import { bindingPresentationState, canShowSuccessfulSyncCount, connectorPresentationState } from "./sourcePresentation.ts";

const sources: DatasetSources = {
  file_uploads: { count: 2 }, url_imports: { count: 1 }, confluence_bindings: [],
  connector_status: "not_configured", total_documents: 3,
};

test("connector load failure is never presented as no configured sync", () => {
  assert.equal(connectorPresentationState(null, true, false), "loading");
  assert.equal(connectorPresentationState(sources, false, true), "load_failed");
  assert.equal(connectorPresentationState({ ...sources, connector_status: "unavailable" }, false, false), "unavailable");
  assert.equal(connectorPresentationState(sources, false, false), "not_configured");
});

test("failed binding never turns zero pages into a successful sync", () => {
  const binding = {
    binding_id: "binding-1", space_name: "Space", page_count: 0, status: "error",
    last_success_at: null, has_problem: true,
  };
  assert.equal(bindingPresentationState(binding), "problem");
  assert.equal(canShowSuccessfulSyncCount(binding), false);
  const healthy = { ...binding, status: "active", has_problem: false };
  assert.equal(bindingPresentationState(healthy), "never_succeeded");
  assert.equal(canShowSuccessfulSyncCount(healthy), false);
  assert.equal(canShowSuccessfulSyncCount({ ...healthy, last_success_at: "2026-09-28T00:00:00Z" }), true);
  assert.equal(bindingPresentationState({ ...binding, has_problem: false, status: "failed" }), "problem");
  assert.equal(canShowSuccessfulSyncCount({
    ...binding, has_problem: false, status: "failed", last_success_at: "2026-09-28T00:00:00Z",
  }), false);
});
