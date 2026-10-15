/** Existing bindings only. Mount/navigation/rechecks never submit a sync. */
import { useRef, useState } from "react";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import { useTranslation } from "react-i18next";

import { startSourceSync, type DatasetSources } from "@/api/knowledge";
import { Button } from "@/components/ui/button";
import { useAuthStore } from "@/store/useAuthStore";
import { readSourceSyncRequest, sourceSyncActive, sourceSyncQueryOptions, sourceSyncStorageKey, sourceSyncNotAccepted, validateSourceSync } from "./sourceSyncQuery";

type Props = { datasetId: string; binding: DatasetSources["confluence_bindings"][number]; disabled: boolean };

export function SourceManualSync(props: Props) {
  const userId = useAuthStore((state) => state.user?.user_id);
  if (!userId) return null;
  return <SourceManualSyncSession key={JSON.stringify([userId, props.datasetId, props.binding.binding_id])} {...props} userId={userId} />;
}

export function SourceManualSyncSession({ datasetId, binding, disabled, userId }: Props & { userId: string }) {
  const { t } = useTranslation();
  const client = useQueryClient();
  const storageKey = sourceSyncStorageKey(userId, datasetId, binding.binding_id);
  const [initial] = useState(() => {
    try {
      const storageId = readSourceSyncRequest(storageKey);
      return { requestId: storageId ?? binding.current_sync_task?.task_id ?? null, storageId, storageFailed: false };
    } catch { return { requestId: null, storageId: null, storageFailed: true }; }
  });
  const [requestId, setRequestId] = useState(initial.requestId);
  // A restored/ambiguous receipt must be explicitly read before any new submission.
  const [uncertain, setUncertain] = useState(!!initial.requestId);
  const [storageFailed, setStorageFailed] = useState(initial.storageFailed);
  const [submitting, setSubmitting] = useState(false);
  const [rejected, setRejected] = useState(false);
  const busy = useRef(false);
  const dispatchIdentity = useRef(initial.requestId);
  const storageIdentity = useRef(initial.storageId);
  const options = sourceSyncQueryOptions(userId, datasetId, binding.binding_id, requestId);
  const query = useQuery({ ...options, enabled: !!requestId && !uncertain && !submitting });
  const task = !uncertain && !query.isError ? query.data : undefined;
  const terminal = task?.status === "completed" || task?.status === "failed";
  const canStart = binding.manual_sync_available === true && !disabled && !storageFailed
    && !submitting && !query.isFetching && (!requestId || terminal);

  async function start() {
    if (busy.current || !canStart || dispatchIdentity.current !== requestId) return;
    busy.current = true;
    setSubmitting(true);
    setRejected(false);
    let id: string;
    try {
      if (readSourceSyncRequest(storageKey) !== storageIdentity.current) {
        throw new Error("Another control owns source sync recovery");
      }
      id = crypto.randomUUID();
      // Persist identity before dispatch. If storage fails, do not send the write.
      sessionStorage.setItem(storageKey, id);
      storageIdentity.current = id;
    } catch {
      setStorageFailed(true); setSubmitting(false); busy.current = false; return;
    }
    dispatchIdentity.current = id;
    setRequestId(id);
    setUncertain(true);
    try {
      const receipt = validateSourceSync(await startSourceSync(datasetId, binding.binding_id, id), binding.binding_id, id);
      client.setQueryData(sourceSyncQueryOptions(userId, datasetId, binding.binding_id, id).queryKey, receipt);
      setUncertain(false);
      void client.invalidateQueries({ queryKey: ["kb-dataset-sources", datasetId] });
    } catch (error) {
      // Generic proxy/transport errors are ambiguous. Only a service-issued
      // pre-admission rejection proves it is safe to release this request guard.
      if (sourceSyncNotAccepted(error)) {
        try {
          // A late response from an unmounted/older producer cannot clear a
          // newer producer's receipt or reclaim its recovery ownership.
          if (readSourceSyncRequest(storageKey) !== id) throw new Error("Source sync recovery owner changed", { cause: error });
          sessionStorage.removeItem(storageKey);
          storageIdentity.current = null;
          dispatchIdentity.current = null;
          setRequestId(null); setUncertain(false); setRejected(true);
        } catch { setStorageFailed(true); }
      } else setUncertain(true);
    } finally { setSubmitting(false); busy.current = false; }
  }

  async function recheck() {
    if (busy.current || !requestId) return;
    busy.current = true;
    try {
      const result = await query.refetch({ cancelRefetch: false });
      if (result.isSuccess) {
        setUncertain(false);
        void client.invalidateQueries({ queryKey: ["kb-dataset-sources", datasetId] });
      }
    } finally { busy.current = false; }
  }

  if (!binding.manual_sync_available && !requestId && !storageFailed) return null;
  return (
    <div className="mt-3 space-y-2 border-t pt-2" data-testid="source-manual-sync">
      {binding.manual_sync_available && <p>{t("knowledge.sources.manual.warning")}</p>}
      {storageFailed && <p role="alert">{t("knowledge.sources.manual.storageFailed")}</p>}
      {submitting ? <p role="status">{t("knowledge.sources.manual.submitting")}</p> :
        uncertain ? <p role="alert">{t("knowledge.sources.manual.uncertain")}</p> : null}
      {rejected && <p role="alert">{t("knowledge.sources.manual.rejected")}</p>}
      {query.isError && <p role="alert">{t("knowledge.sources.manual.readFailed")}</p>}
      {requestId && <p className="break-all">{t("knowledge.sources.manual.taskId", { id: requestId })}</p>}
      {task && <div role="status">
        <p>{t(`knowledge.sources.manual.status.${task.status}`)}</p>
        {sourceSyncActive(task) && task.processed_items !== null && task.total_items !== null &&
          <p>{t("knowledge.sources.manual.progress", { processed: task.processed_items, total: task.total_items })}</p>}
        {task.synced_items !== null && <p>{t("knowledge.sources.manual.synced", { count: task.synced_items })}</p>}
        {task.status === "failed" && <p>{t(task.synced_items && task.synced_items > 0 ? "knowledge.sources.manual.partialFailure" : "knowledge.sources.manual.failed")}</p>}
        {task.failed_items !== null && task.failed_items > 0 && <p>{t("knowledge.sources.manual.failedItems", { count: task.failed_items })}</p>}
        {task.error_count !== null && task.error_count > 0 && <p>{t("knowledge.sources.manual.errors", { count: task.error_count })}</p>}
      </div>}
      {requestId && <Button size="sm" variant="outline" disabled={submitting || query.isFetching} onClick={() => void recheck()}>
        {t("knowledge.sources.manual.recheck")}
      </Button>}
      {binding.manual_sync_available === true && <Button size="sm" variant="outline" disabled={!canStart} onClick={() => void start()}>
        {t(task?.status === "failed" ? "knowledge.sources.manual.retry" : "knowledge.sources.manual.start")}
      </Button>}
    </div>
  );
}
