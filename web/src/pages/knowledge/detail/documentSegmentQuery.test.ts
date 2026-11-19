// @ts-expect-error -- node built-ins are supplied by the node --test runtime.
import assert from "node:assert/strict";
// @ts-expect-error -- node built-ins are supplied by the node --test runtime.
import { test } from "node:test";
import { QueryClient, QueryObserver } from "@tanstack/react-query";
import type { Document } from "../../../types/knowledge.ts";
import { documentSegmentQueryKey, sameDocumentSegmentObservation } from "./documentSegmentQuery.ts";

const page = { limit: 100, offset: 0 };
function document(overrides: Partial<Document> = {}): Document {
  return { dataset_id: "dataset-a", document_id: "doc-a", title: "Source", status: "error", completed_at: "2026-10-12T12:00:00Z", ...overrides };
}
function key(doc: Document, offset = 0) {
  const value = documentSegmentQueryKey(doc.dataset_id, doc.document_id, "", { ...page, offset }, doc, "user-a");
  assert.ok(value);
  return value!;
}
function deferred<T>() {
  let resolve!: (value: T) => void;
  let reject!: (reason?: unknown) => void;
  const promise = new Promise<T>((yes, no) => { resolve = yes; reject = no; });
  return { promise, resolve, reject };
}
async function settle() { await new Promise((resolve) => setTimeout(resolve, 0)); }
function client() { return new QueryClient({ defaultOptions: { queries: { retry: false, gcTime: Infinity } } }); }

test("failed retry gets fresh segment keys for queue and terminal snapshots", () => {
  const failed = key(document());
  const waiting = key(document({ status: "waiting", completed_at: undefined }));
  const completed = key(document({ status: "completed", completed_at: "2026-10-12T12:01:00Z" }));
  assert.notDeepEqual(waiting, failed);
  assert.notDeepEqual(completed, waiting);
  assert.notDeepEqual(completed, failed);
});

test("stage/progress updates do not poll chunks, but repeated completion refreshes", () => {
  const parsing = key(document({ status: "parsing", completed_at: undefined }));
  assert.deepEqual(parsing, key(document({ status: "indexing", progress: 95, completed_at: undefined })));
  assert.notDeepEqual(key(document({ status: "completed" })), key(document({ status: "completed", completed_at: "2026-10-12T12:02:00Z" })));
  assert.notDeepEqual(key(document({ status: "error" })), key(document({ status: "completed" })));
});

test("pending lifecycle activation remains processing until its terminal observation", () => {
  const pending = document({ status: "waiting", display_status: "archived", metadata: { _document_lifecycle_reindex: { status: "pending" } } });
  assert.deepEqual(key(pending), key({ ...pending, status: "indexing" }));
  assert.notDeepEqual(key(pending), key(document({ status: "completed", completed_at: "2026-10-12T12:03:00Z" })));
});

test("mismatched dataset, document and missing observation never enable a segment read", () => {
  assert.equal(documentSegmentQueryKey("dataset-b", "doc-a", "", page, document(), "user-a"), null);
  assert.equal(documentSegmentQueryKey("dataset-a", "doc-b", "", page, document(), "user-a"), null);
  assert.equal(documentSegmentQueryKey("dataset-a", "doc-a", "", page, undefined, "user-a"), null);
  assert.equal(documentSegmentQueryKey(undefined, "doc-a", "", page, document(), "user-a"), null);
});

test("placeholder rows stay within one document observation while allowing pagination", () => {
  const original = key(document());
  assert.equal(sameDocumentSegmentObservation(original, key(document(), 100)), true);
  assert.equal(sameDocumentSegmentObservation(original, key(document({ document_id: "doc-b" }))), false);
  assert.equal(sameDocumentSegmentObservation(original, key(document({ dataset_id: "dataset-b" }))), false);
  assert.equal(sameDocumentSegmentObservation(original, key(document({ status: "completed" }))), false);
  assert.equal(sameDocumentSegmentObservation(original,
    documentSegmentQueryKey("dataset-a", "doc-a", "", page, document(), "user-b")), false);
  assert.equal(documentSegmentQueryKey("dataset-a", "doc-a", "", page, document(), undefined), null);
  assert.equal(sameDocumentSegmentObservation(original, null), false);
  assert.equal(sameDocumentSegmentObservation(undefined, original), false);
});

test("late initial prepublication GET cannot replace the completed panel result", async () => {
  const qc = client();
  const oldRead = deferred<string[]>();
  const newRead = deferred<string[]>();
  const before = key(document({ status: "indexing", completed_at: undefined }));
  const after = key(document({ status: "completed", completed_at: "2026-10-12T12:04:00Z" }));
  const observer = new QueryObserver(qc, { queryKey: before, queryFn: () => oldRead.promise });
  const stop = observer.subscribe(() => {});
  try {
    observer.setOptions({ queryKey: after, queryFn: () => newRead.promise });
    newRead.resolve(["new chunk"]);
    await settle();
    oldRead.resolve(["stale chunk"]); // Deliberately ignore cancellation to prove key isolation.
    await settle();
    assert.deepEqual(observer.getCurrentResult().data, ["new chunk"]);
    assert.deepEqual(qc.getQueryData(after), ["new chunk"]);
  } finally { stop(); qc.clear(); }
});

test("new terminal reads expose pending/error rather than previous generation placeholders", async () => {
  const qc = client();
  const before = key(document());
  const after = key(document({ status: "completed", completed_at: "2026-10-12T12:05:00Z" }));
  qc.setQueryData(before, ["old chunk"]);
  const read = deferred<string[]>();
  const observer = new QueryObserver(qc, { queryKey: before, queryFn: async () => ["old chunk"], staleTime: Infinity });
  const stop = observer.subscribe(() => {});
  try {
    observer.setOptions({ queryKey: after, queryFn: () => read.promise,
      placeholderData: (previousData, previousQuery) => sameDocumentSegmentObservation(previousQuery?.queryKey, after) ? previousData : undefined });
    assert.equal(observer.getCurrentResult().isPending, true);
    assert.equal(observer.getCurrentResult().data, undefined);
    read.reject(new Error("read unavailable"));
    await settle();
    assert.equal(observer.getCurrentResult().isError, true);
    assert.equal(observer.getCurrentResult().data, undefined);
  } finally { stop(); qc.clear(); }
});

test("observation change aborts the old GET when its query consumes the signal", async () => {
  const qc = client();
  let oldSignal: AbortSignal | undefined;
  const oldRead = deferred<string[]>();
  const observer = new QueryObserver(qc, { queryKey: key(document()), queryFn: ({ signal }) => { oldSignal = signal; return oldRead.promise; } });
  const stop = observer.subscribe(() => {});
  try {
    observer.setOptions({ queryKey: key(document({ status: "completed" })), queryFn: async () => ["current"] });
    assert.equal(oldSignal?.aborted, true);
    oldRead.resolve(["old"]);
    await settle();
    assert.deepEqual(observer.getCurrentResult().data, ["current"]);
  } finally { stop(); qc.clear(); }
});

test("existing document/search mutation prefixes invalidate every observed page", async () => {
  const qc = client();
  try {
    const oldKey = key(document());
    const newKey = key(document({ status: "completed" }), 100);
    const otherKey = key(document({ document_id: "other" }));
    for (const queryKey of [oldKey, newKey, otherKey]) qc.setQueryData(queryKey, ["chunk"]);
    await qc.invalidateQueries({ queryKey: ["kb-segments", "dataset-a", "doc-a", ""], refetchType: "none" });
    assert.equal(qc.getQueryState(oldKey)?.isInvalidated, true);
    assert.equal(qc.getQueryState(newKey)?.isInvalidated, true);
    assert.equal(qc.getQueryState(otherKey)?.isInvalidated, false);
  } finally { qc.clear(); }
});
