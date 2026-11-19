import {
  documentNeedsLifecyclePolling,
  type Document,
} from "../../../types/knowledge.ts";

type SegmentDocument = Pick<Document, "dataset_id" | "document_id" | "status" | "display_status" | "completed_at" | "metadata">;

type SegmentQueryKey = readonly ["kb-segments", string, string, string | undefined, number, number, string, string];

/**
 * The terminal timestamp is a cache-refresh observation, not a content revision.
 * Keep the existing mutation prefixes and isolate a late prepublication GET
 * from the query observing a later terminal write. Active stages share one key.
 */
export function documentSegmentQueryKey(
  datasetId: string | undefined,
  documentId: string | undefined,
  q: string | undefined,
  page: { limit: number; offset: number },
  document: SegmentDocument | null | undefined,
  userId: string | undefined,
): SegmentQueryKey | null {
  if (!userId || !datasetId || !documentId || document?.dataset_id !== datasetId
    || document.document_id !== documentId) return null;
  const phase = documentNeedsLifecyclePolling(document)
    ? "processing"
    : String(document.status).trim().toLowerCase();
  const observation = JSON.stringify([phase, document.completed_at ?? null]);
  return ["kb-segments", datasetId, documentId, q, page.limit, page.offset, observation, userId];
}

/** Pagination/search may retain rows only from the same observed document. */
export function sameDocumentSegmentObservation(
  previous: readonly unknown[] | undefined,
  next: SegmentQueryKey | null,
): boolean {
  return !!previous && !!next && previous.length === next.length
    && previous[0] === next[0] && previous[1] === next[1]
    && previous[2] === next[2] && previous[6] === next[6] && previous[7] === next[7];
}
