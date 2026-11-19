import assert from "node:assert/strict";
import test from "node:test";
import {
  currentTraceExport, selectedTraceFromNavigation, traceFamilyFromParam, traceNavigationParams,
  type BoundTraceExport, type TraceExportBinding,
} from "./traceNavigation.ts";

test("explicit trace IDs survive missing, filtered, empty and unrelated list pages", () => {
  for (const rows of [[], [{ trace_id: "newer" }], [{ trace_id: "target" }]]) {
    assert.equal(selectedTraceFromNavigation("target", rows), "target");
  }
  assert.equal(selectedTraceFromNavigation(undefined, [{ trace_id: "newer" }]), "newer");
  assert.equal(selectedTraceFromNavigation(undefined, []), undefined);
});

test("trace navigation preserves source query parameters and atomically changes family and ID", () => {
  const original = new URLSearchParams("tab=runs&family=assistant&trace_id=old&dataset_id=kb&session_id=session&offset=100&custom=keep");
  const next = traceNavigationParams(original, "rag", "new/trace", "traces");
  assert.equal(next.get("trace_id"), "new/trace");
  assert.equal(next.get("family"), "rag");
  assert.equal(next.get("tab"), "traces");
  for (const key of ["dataset_id", "session_id", "offset", "custom"]) assert.equal(next.get(key), original.get(key));
  assert.equal(original.get("trace_id"), "old");
  const changedFamily = traceNavigationParams(next, "langgraph_proxy");
  assert.equal(changedFamily.has("trace_id"), false);
  assert.equal(changedFamily.get("dataset_id"), "kb");
  assert.equal(traceFamilyFromParam(null), "assistant");
  assert.equal(traceFamilyFromParam("unknown"), "assistant");
  assert.equal(traceFamilyFromParam("rag"), "rag");
});

test("export payload is displayed only for its exact source, navigation and authenticated scope", () => {
  const binding: TraceExportBinding = { accessKey: "access", navigationEpoch: "navigation", family: "assistant", traceId: "trace" };
  const observed: BoundTraceExport = { binding, result: { trace_id: "trace", format: "openinference", redaction_policy: {}, payload: { safe: "preview" } } };
  assert.equal(currentTraceExport(observed, binding), observed.result);
  for (const changed of [
    { accessKey: "other-account-or-session" }, { navigationEpoch: "return-to-same-url" },
    { family: "rag" as const }, { traceId: "different-trace" }, { traceId: undefined },
  ]) assert.equal(currentTraceExport(observed, { ...binding, ...changed }), null);
  assert.equal(currentTraceExport({ ...observed, result: { ...observed.result, trace_id: "wrong-response" } }, binding), null);
  assert.equal(currentTraceExport(null, binding), null);
});
