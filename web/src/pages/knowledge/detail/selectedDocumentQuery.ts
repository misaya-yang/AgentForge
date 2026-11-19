import { queryOptions, type QueryClient } from "@tanstack/react-query";
import { documentNeedsLifecyclePolling, type Document } from "../../../types/knowledge.ts";

type ReadDocument = (datasetId: string, documentId: string, options: { signal: AbortSignal }) => Promise<Document>;

/** Only a currently observed in-flight document needs continued read polling. */
export function selectedDocumentPollInterval(document: Document | undefined, failed = false): number | false {
  return !failed && document && documentNeedsLifecyclePolling(document) ? 2000 : false;
}

function selectedDocumentQueryKey(datasetId?: string, documentId?: string, userId?: string) {
  return ["kb-document", datasetId, documentId, userId] as const;
}

/** Supersede even a pending first read after an authoritative lifecycle change. */
export async function refreshSelectedDocument(
  client: QueryClient,
  datasetId: string | undefined,
  documentId: string | undefined,
  userId: string | undefined,
): Promise<void> {
  if (!datasetId || !documentId || !userId) return;
  const filters = { queryKey: selectedDocumentQueryKey(datasetId, documentId, userId), exact: true };
  await client.cancelQueries(filters);
  // Inactive old selections are marked stale without starting a new request.
  await client.invalidateQueries(filters);
}

/** Selection/account changes detach and cancel the old read; no placeholder fallback. */
export function selectedDocumentQueryOptions(
  datasetId: string | undefined,
  documentId: string | undefined,
  userId: string | undefined,
  read: ReadDocument,
) {
  return queryOptions({
    queryKey: selectedDocumentQueryKey(datasetId, documentId, userId),
    enabled: !!datasetId && !!documentId && !!userId,
    queryFn: async ({ signal }) => {
      const document = await read(datasetId!, documentId!, { signal });
      if (document.dataset_id !== datasetId || document.document_id !== documentId) {
        throw new Error("Document response does not match the selected source");
      }
      return document;
    },
    retry: false,
    staleTime: 0,
    refetchInterval: (query) => selectedDocumentPollInterval(query.state.data, query.state.status === "error"),
  });
}
