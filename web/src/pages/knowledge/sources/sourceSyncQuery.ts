import { getSourceSync, type SourceSyncTask } from "@/api/knowledge";

export function sourceSyncActive(task?: SourceSyncTask): boolean {
  return task?.status === "pending" || task?.status === "processing";
}

export function validateSourceSync(task: SourceSyncTask, bindingId: string, requestId: string): SourceSyncTask {
  if (!task || task.task_id !== requestId || task.binding_id !== bindingId
    || !["pending", "processing", "completed", "failed"].includes(task.status)) {
    throw new Error("Mismatched source sync receipt");
  }
  return task;
}

export function sourceSyncQueryOptions(userId: string, datasetId: string, bindingId: string, requestId: string | null) {
  return {
    queryKey: ["kb-source-sync", userId, datasetId, bindingId, requestId],
    queryFn: async () => validateSourceSync(await getSourceSync(datasetId, bindingId, requestId!), bindingId, requestId!),
    staleTime: Infinity,
    retry: false as const,
    refetchOnWindowFocus: false as const,
    refetchOnReconnect: false as const,
    refetchOnMount: false as const,
    refetchInterval: (query: { state: { data?: SourceSyncTask; error: unknown } }) =>
      query.state.error ? false as const : sourceSyncActive(query.state.data) ? 2000 : false as const,
  };
}

export function sourceSyncStorageKey(userId: string, datasetId: string, bindingId: string): string {
  return `kb-source-sync:${JSON.stringify([userId, datasetId, bindingId])}`;
}

export function readSourceSyncRequest(key: string): string | null {
  const value = sessionStorage.getItem(key);
  if (value !== null && !/^[0-9a-f]{8}-[0-9a-f]{4}-[1-8][0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/i.test(value)) {
    throw new Error("Invalid source sync receipt");
  }
  return value;
}


/** Only the service's explicit pre-admission receipt proves a write was rejected. */
export function sourceSyncNotAccepted(error: unknown): boolean {
  const response = (error as { response?: { status?: number; data?: { detail?: { code?: string; accepted?: boolean } } } } | null)?.response;
  return !!response && [400, 401, 403, 404, 409, 422, 503].includes(response.status ?? 0)
    && response.data?.detail?.code === "CONFLUENCE_SYNC_NOT_ACCEPTED"
    && response.data.detail.accepted === false;
}
