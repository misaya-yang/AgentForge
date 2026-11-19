// @ts-expect-error -- node built-ins are supplied by the node --test runtime.
import assert from "node:assert/strict";
// @ts-expect-error -- node built-ins are supplied by the node --test runtime.
import { test } from "node:test";
import { QueryClient, QueryObserver } from "@tanstack/react-query";
import type { Document } from "../../../types/knowledge.ts";
import { documentSegmentQueryKey } from "./documentSegmentQuery.ts";
import { refreshSelectedDocument, selectedDocumentPollInterval, selectedDocumentQueryOptions } from "./selectedDocumentQuery.ts";

const page = { limit: 100, offset: 0 };
function document(overrides: Partial<Document> = {}): Document {
  return { dataset_id: "dataset-a", document_id: "doc-off-page", title: "Linked source", status: "waiting", ...overrides };
}
function deferred<T>() {
  let resolve!: (value: T) => void;
  const promise = new Promise<T>((yes) => { resolve = yes; });
  return { promise, resolve };
}
async function settle() { await new Promise((resolve) => setTimeout(resolve, 0)); }
function client() { return new QueryClient({ defaultOptions: { queries: { retry: false, gcTime: Infinity } } }); }

test("off-page selected document reaches completion while the idle list remains unchanged", async () => {
  const qc = client();
  const idlePage = [document({ document_id: "unrelated", status: "completed" })];
  qc.setQueryData(["kb-documents", "dataset-a", 50, 0], idlePage);
  let current = document();
  let reads = 0;
  const options = selectedDocumentQueryOptions("dataset-a", "doc-off-page", "user-a", async () => { reads += 1; return current; });
  const observer = new QueryObserver(qc, options);
  const stop = observer.subscribe(() => {});
  try {
    await settle();
    const waiting = observer.getCurrentResult().data;
    assert.equal(selectedDocumentPollInterval(waiting), 2000);
    const before = documentSegmentQueryKey("dataset-a", "doc-off-page", "", page, waiting, "user-a");
    current = document({ status: "completed", completed_at: "2026-10-12T12:10:00Z" });
    // Controlled interval tick: no browser timer or current-page update is needed.
    await observer.refetch();
    const completed = observer.getCurrentResult().data;
    assert.equal(selectedDocumentPollInterval(completed), false);
    const after = documentSegmentQueryKey("dataset-a", "doc-off-page", "", page, completed, "user-a");
    assert.notDeepEqual(after, before);
    assert.equal(reads, 2);
    assert.deepEqual(qc.getQueryData(["kb-documents", "dataset-a", 50, 0]), idlePage);
  } finally { stop(); qc.clear(); }
});

test("document polling stops at error, completion, missing data and failed reads", () => {
  for (const status of ["waiting", "parsing", "splitting", "indexing"] as const) {
    assert.equal(selectedDocumentPollInterval(document({ status })), 2000);
  }
  assert.equal(selectedDocumentPollInterval(document({ status: "error" })), false);
  assert.equal(selectedDocumentPollInterval(document({ status: "completed" })), false);
  assert.equal(selectedDocumentPollInterval(undefined), false);
  assert.equal(selectedDocumentPollInterval(document(), true), false);
});

for (const status of [403, 404]) {
  test(`detail ${status} stays an error despite stale data and disables further interval reads`, async () => {
    const qc = client();
    let denied = false;
    const options = selectedDocumentQueryOptions("dataset-a", "doc-off-page", "user-a", async () => {
      if (denied) throw Object.assign(new Error("Document unavailable"), { status });
      return document();
    });
    const observer = new QueryObserver(qc, options);
    const stop = observer.subscribe(() => {});
    try {
      await settle();
      assert.ok(observer.getCurrentResult().data);
      denied = true;
      await observer.refetch();
      const result = observer.getCurrentResult();
      assert.equal(result.isError, true);
      assert.equal(selectedDocumentPollInterval(result.data, result.isError), false);
      // This is the panel's fail-closed selection rule; it must not reuse cached data.
      const selected = result.isError ? undefined : result.data;
      assert.equal(documentSegmentQueryKey("dataset-a", "doc-off-page", "", page, selected, "user-a"), null);
    } finally { stop(); qc.clear(); }
  });
}

test("selection and account changes cancel old detail reads without carrying old rows", async () => {
  const qc = client();
  const pending = deferred<Document>();
  let signal: AbortSignal | undefined;
  const observer = new QueryObserver(qc, selectedDocumentQueryOptions("dataset-a", "doc-off-page", "user-a", async (_dataset, _document, options) => {
    signal = options.signal;
    return pending.promise;
  }));
  const stop = observer.subscribe(() => {});
  try {
    const next = deferred<Document>();
    observer.setOptions(selectedDocumentQueryOptions("dataset-a", "doc-b", "user-b", async () => next.promise));
    assert.equal(signal?.aborted, true);
    assert.equal(observer.getCurrentResult().data, undefined);
    next.resolve(document({ document_id: "doc-b", status: "completed" }));
    await settle();
    pending.resolve(document());
    await settle();
    assert.equal(observer.getCurrentResult().data?.document_id, "doc-b");
  } finally { stop(); qc.clear(); }
});

test("wrong dataset or document payload is rejected before becoming selected data", async () => {
  for (const mismatch of [{ dataset_id: "dataset-b" }, { document_id: "doc-b" }]) {
    const qc = client();
    try {
      await assert.rejects(qc.fetchQuery(selectedDocumentQueryOptions("dataset-a", "doc-off-page", "user-a", async () => document(mismatch))), /does not match/);
    } finally { qc.clear(); }
  }
});

test("missing account or selection disables the selected detail query", () => {
  const read = async () => document();
  assert.equal(selectedDocumentQueryOptions("dataset-a", "doc-off-page", undefined, read).enabled, false);
  assert.equal(selectedDocumentQueryOptions("dataset-a", undefined, "user-a", read).enabled, false);
  assert.equal(selectedDocumentQueryOptions(undefined, "doc-off-page", "user-a", read).enabled, false);
});

test("terminal lifecycle refresh supersedes a pending first detail GET even with no cached data", async () => {
  const qc = client();
  const staleRead = deferred<Document>();
  let reads = 0;
  let staleSignal: AbortSignal | undefined;
  const options = selectedDocumentQueryOptions("dataset-a", "doc-off-page", "user-a", async (_dataset, _document, { signal }) => {
    reads += 1;
    if (reads === 1) { staleSignal = signal; return staleRead.promise; }
    return document({ status: "completed", completed_at: "2026-10-12T12:15:00Z" });
  });
  const observer = new QueryObserver(qc, options);
  const stop = observer.subscribe(() => {});
  try {
    assert.equal(observer.getCurrentResult().data, undefined);
    await refreshSelectedDocument(qc, "dataset-a", "doc-off-page", "user-a");
    assert.equal(staleSignal?.aborted, true);
    staleRead.resolve(document({ status: "error", completed_at: "2026-10-12T12:00:00Z" }));
    await settle();
    assert.equal(reads, 2);
    assert.equal(observer.getCurrentResult().data?.completed_at, "2026-10-12T12:15:00Z");
    assert.equal(selectedDocumentPollInterval(observer.getCurrentResult().data), false);
  } finally { stop(); qc.clear(); }
});

test("submitted-document refresh stays exact after selection/account changes and never restarts inactive reads", async () => {
  const qc = client();
  let oldReads = 0;
  let currentReads = 0;
  const oldOptions = selectedDocumentQueryOptions("dataset-a", "doc-off-page", "user-a", async () => { oldReads += 1; return document({ status: "completed" }); });
  const observer = new QueryObserver(qc, oldOptions);
  const stop = observer.subscribe(() => {});
  try {
    await settle();
    observer.setOptions(selectedDocumentQueryOptions("dataset-a", "doc-b", "user-b", async () => { currentReads += 1; return document({ document_id: "doc-b", status: "completed" }); }));
    await settle();
    await refreshSelectedDocument(qc, "dataset-a", "doc-off-page", "user-a");
    assert.equal(oldReads, 1);
    assert.equal(currentReads, 1);
    assert.equal(qc.getQueryState(oldOptions.queryKey)?.isInvalidated, true);
    assert.equal(observer.getCurrentResult().data?.document_id, "doc-b");
  } finally { stop(); qc.clear(); }
});
