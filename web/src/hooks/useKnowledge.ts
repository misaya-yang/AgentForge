import { useEffect, useRef, useState } from "react";
import { waitForSseReconnect } from "@/lib/sse";
import { useQuery, useQueryClient } from "@tanstack/react-query";

import {
  getDataset,
  getDocument,
  getDatasetSources,
  listDatasets,
  listDocuments,
  listSegments,
  streamDocumentProgress,
} from "@/api/knowledge";
import { documentNeedsLifecyclePolling, type Document } from "@/types/knowledge";
import {
  documentSegmentQueryKey,
  sameDocumentSegmentObservation,
} from "@/pages/knowledge/detail/documentSegmentQuery";
import {
  documentProgressReconnectDelay,
  shouldApplyDocumentProgressEvent,
} from "@/pages/knowledge/detail/documentProgress";

import { refreshSelectedDocument, selectedDocumentQueryOptions } from "@/pages/knowledge/detail/selectedDocumentQuery";

function waitForStreamReconnect(ms: number, signal: AbortSignal): Promise<void> {
  return waitForSseReconnect(signal, 0, ms, ms);
}

/**
 * Returns `value` once it has been stable for `delayMs`. Search inputs feed
 * through this so keystrokes do not fan out into server queries or large
 * client-side filters (C4: the codebase previously had no debounce).
 */
export function useDebouncedValue<T>(value: T, delayMs = 300): T {
  const [debounced, setDebounced] = useState(value);
  useEffect(() => {
    const timer = setTimeout(() => setDebounced(value), delayMs);
    return () => clearTimeout(timer);
  }, [value, delayMs]);
  return debounced;
}

/**
 * A 409 carrying Retry-After is a transient busy state (e.g. a concurrent
 * generation switch), not a failure the user can act on. Wait the requested
 * time (capped) and retry once so list reads never flash an error banner
 * for it.
 */
async function withConflictRetry<T>(fn: () => Promise<T>): Promise<T> {
  try {
    return await fn();
  } catch (error) {
    const response = (error as { response?: { status?: number; headers?: unknown } } | null)
      ?.response;
    if (response?.status !== 409) throw error;
    const headers = response.headers as
      | { get?: (name: string) => string | undefined; "retry-after"?: string }
      | undefined;
    const raw =
      typeof headers?.get === "function"
        ? headers.get("retry-after")
        : headers?.["retry-after"];
    const seconds = Number.parseFloat(raw ?? "");
    const waitMs = Number.isFinite(seconds)
      ? Math.min(Math.max(seconds, 0), 5) * 1000
      : 1000;
    await new Promise((resolve) => setTimeout(resolve, waitMs));
    return fn();
  }
}

export function useDatasets() {
  return useQuery({
    queryKey: ["kb-datasets"],
    queryFn: () => listDatasets(),
    staleTime: 30000, // 30秒内使用缓存
  });
}

export function useDataset(datasetId?: string) {
  return useQuery({
    queryKey: ["kb-dataset", datasetId],
    queryFn: () => getDataset(datasetId!),
    enabled: !!datasetId,
    staleTime: 30000,
  });
}

export function useDatasetSources(datasetId?: string) {
  return useQuery({
    queryKey: ["kb-dataset-sources", datasetId],
    queryFn: () => getDatasetSources(datasetId!),
    enabled: !!datasetId,
    staleTime: 5000,
  });
}

export function useDocuments(
  datasetId?: string,
  page: { limit: number; offset: number } = { limit: 50, offset: 0 }
) {
  return useQuery({
    queryKey: ["kb-documents", datasetId, page.limit, page.offset],
    queryFn: () => withConflictRetry(() => listDocuments(datasetId!, page)),
    enabled: !!datasetId,
    staleTime: 1000, // 1秒内使用缓存，配合轮询
    placeholderData: (previousData) => previousData,
    // Poll only while at least one row is still in flight; an idle dataset
    // costs zero requests (PRD §5-#4). This checks both the raw worker state
    // and the durable activation marker because enable/unarchive keep the old
    // enabled/archived fields until their queued rebuild completes.
    refetchInterval: (query) => {
      const active = (query.state.data?.items ?? []).some(documentNeedsLifecyclePolling);
      return active ? 2000 : false;
    },
  });
}

/**
 * Push document lifecycle changes through the durable SSE ledger. The list's
 * existing active-row interval remains enabled as a bounded fallback whenever
 * the stream is down, so a reconnect cannot make progress disappear.
 */
export function useDocumentProgressStream(
  datasetId: string | undefined,
  enabled = false,
  selectedDocumentId?: string,
  userId?: string,
): void {
  const queryClient = useQueryClient();
  const cursorRef = useRef<{ datasetId: string; eventId?: string }>({
    datasetId: "",
  });

  useEffect(() => {
    if (cursorRef.current.datasetId !== datasetId) {
      cursorRef.current = { datasetId: datasetId ?? "" };
    }
    if (!datasetId || !enabled) return;
    const streamDatasetId = datasetId;

    const controller = new AbortController();
    let stopped = false;
    let reconnectAttempt = 0;

    async function consume() {
      while (!stopped) {
        try {
          for await (const event of streamDocumentProgress(streamDatasetId, {
            signal: controller.signal,
            lastEventId: cursorRef.current.eventId,
          })) {
            if (stopped) return;
            if (
              !event.id ||
              !shouldApplyDocumentProgressEvent(
                streamDatasetId,
                cursorRef.current.eventId,
                event.id,
                event.event,
              )
            ) {
              continue;
            }
            cursorRef.current.eventId = event.id;
            reconnectAttempt = 0;
            if (["progress", "terminal", "deleted", "reset"].includes(event.event)) {
              void queryClient.invalidateQueries({
                queryKey: ["kb-documents", streamDatasetId],
              });
              if (event.event === "reset" || event.data?.document_id === selectedDocumentId) {
                void refreshSelectedDocument(queryClient, streamDatasetId, selectedDocumentId, userId);
              }
            }
          }
          if (stopped || controller.signal.aborted) return;
          throw new Error("document progress stream ended");
        } catch {
          if (stopped || controller.signal.aborted) return;
          const delay = documentProgressReconnectDelay(reconnectAttempt);
          reconnectAttempt += 1;
          await waitForStreamReconnect(delay, controller.signal);
        }
      }
    }

    void consume();
    return () => {
      stopped = true;
      controller.abort();
    };
  }, [datasetId, enabled, queryClient, selectedDocumentId, userId]);
}

export function useSelectedDocument(datasetId?: string, documentId?: string, userId?: string) {
  return useQuery(selectedDocumentQueryOptions(datasetId, documentId, userId,
    (dataset, document, options) => withConflictRetry(() => getDocument(dataset, document, options))));
}

export function useSegments(
  datasetId?: string,
  documentId?: string,
  q?: string,
  page: { limit: number; offset: number } = { limit: 100, offset: 0 },
  document?: Document,
  userId?: string,
) {
  const observedKey = documentSegmentQueryKey(datasetId, documentId, q, page, document, userId);
  return useQuery({
    queryKey: observedKey ?? ["kb-segments", datasetId, documentId, q, page.limit, page.offset, null, userId],
    queryFn: ({ signal }) => withConflictRetry(() => listSegments(datasetId!, {
      document_id: documentId,
      q,
      ...page,
    }, { signal })),
    enabled: observedKey !== null,
    staleTime: 2000,
    placeholderData: (previousData, previousQuery) =>
      sameDocumentSegmentObservation(previousQuery?.queryKey, observedKey)
        ? previousData
        : undefined,
    // Document SSE/polling changes the observation key after a terminal write.
    // No segment polling or mutation replay; existing invalidation prefixes stay valid.
  });
}
