import { useEffect, useState, type ReactNode } from "react";
import { useSearchParams } from "react-router-dom";
import { useTranslation } from "react-i18next";
import { Button } from "@/components/ui/button";
import { Card, CardContent } from "@/components/ui/card";
import { ScheduledExecutionResult } from "./ScheduledExecutionResult";
import { readSchedulePage, readExecutionPage, readSchedule, readScheduledExecution, isScheduleId,
  type ScheduleCursor, type ScheduledTask } from "@/api/scheduledTasks";

const pageIsVisible = () => typeof document === "undefined" || document.visibilityState !== "hidden";
function usePageVisibility() {
  const [visible, setVisible] = useState(pageIsVisible());
  useEffect(() => {
    if (typeof document === "undefined") return;
    const changed = () => setVisible(pageIsVisible());
    document.addEventListener("visibilitychange", changed);
    changed();
    return () => document.removeEventListener("visibilitychange", changed);
  }, []);
  return visible;
}

type Read<T> = { value: T | null; error: boolean; loading: boolean };
// Every read owns one navigation visit. A -> B -> A does not revive the first A.
function useRead<T>(key: string, enabled: boolean, load: (signal: AbortSignal) => Promise<T>, visible: boolean): Read<T> {
  const [result, setResult] = useState<{ key: string; value: T | null; error: boolean; loading: boolean } | null>(null);
  useEffect(() => {
    if (!enabled || !visible) return;
    const controller = new AbortController();
    let reading = false;
    async function read() {
      if (reading || controller.signal.aborted || !pageIsVisible()) return;
      reading = true;
      setResult(previous => ({ key, value: previous?.key === key ? previous.value : null, error: false, loading: true }));
      try {
        const value = await load(controller.signal);
        if (!controller.signal.aborted) setResult({ key, value, error: false, loading: false });
      } catch {
        if (!controller.signal.aborted) setResult({ key, value: null, error: true, loading: false });
      } finally { reading = false; }
    }
    void read();
    const timer = window.setInterval(() => { void read(); }, 15000);
    return () => { controller.abort(); window.clearInterval(timer); };
    // Load closes over the exact key's inputs; an inline callback is not a new visit.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [key, enabled, visible]);
  // Hidden-page cleanup aborts the old read lifetime. Retain same-visit receipts
  // without a false spinner; foregrounding creates one fresh read per resource.
  return enabled && result?.key === key ? (visible ? result : { ...result, loading: false })
    : { value: null, error: false, loading: enabled && visible };
}
function cursor(params: URLSearchParams, prefix: string): ScheduleCursor | null | false {
  const at = params.get(`${prefix}_at`); const id = params.get(`${prefix}_id`);
  if (at === null && id === null) return null;
  return at && id && Number.isFinite(Date.parse(at)) && isScheduleId(id) ? { before_at: at, before_id: id } : false;
}
export function ScheduledHistoryBrowser({ revision, renderActions }: {
  revision: number; renderActions?: (taskId: string, task: ScheduledTask | null, available: boolean) => ReactNode;
}) {
  const { t } = useTranslation();
  const visible = usePageVisibility();
  const [params, setParams] = useSearchParams();
  const [refresh, setRefresh] = useState(0);
  const selected = params.get("task"); const execution = params.get("execution");
  const taskId = selected && isScheduleId(selected) ? selected.toLowerCase() : null;
  const executionId = execution && isScheduleId(execution) ? execution.toLowerCase() : null;
  const taskCursor = cursor(params, "schedule_before"); const executionCursor = cursor(params, "execution_before");
  const version = `${revision}:${refresh}`;
  const list = useRead(`${version}:${JSON.stringify(taskCursor)}`, taskCursor !== false,
    signal => readSchedulePage(taskCursor || null, signal), visible);
  const detail = useRead(`${version}:${taskId}`, !!taskId, signal => readSchedule(taskId!, signal), visible);
  const runs = useRead(`${version}:${taskId}:${JSON.stringify(executionCursor)}`, !!taskId && executionCursor !== false,
    signal => readExecutionPage(taskId!, executionCursor || null, signal), visible);
  const exact = useRead(`${version}:${taskId}:${executionId}`, !!taskId && !!executionId,
    signal => readScheduledExecution(taskId!, executionId!, signal), visible);
  function move(prefix: string, next: ScheduleCursor | null) {
    setParams(current => { const value = new URLSearchParams(current);
      for (const name of ["at", "id"]) value.delete(`${prefix}_${name}`);
      if (next) { value.set(`${prefix}_at`, next.before_at); value.set(`${prefix}_id`, next.before_id); }
      return value;
    });
  }
  function select(id: string) {
    setParams(current => { const next = new URLSearchParams(current); next.set("tab", "scheduled"); next.set("task", id);
      next.delete("execution"); next.delete("execution_before_at"); next.delete("execution_before_id"); return next; });
  }
  const pageControls = (prefix: string, current: ScheduleCursor | null | false, next: ScheduleCursor | null | undefined, loading: boolean) =>
    <div className="flex flex-wrap gap-2">
      {current !== null && <Button variant="outline" disabled={loading} onClick={() => move(prefix, null)}>{t("tasks.scheduled.latest", "Latest")}</Button>}
      {next && <Button variant="outline" disabled={loading} onClick={() => move(prefix, next)}>{t("tasks.scheduled.older", "Older")}</Button>}
    </div>;
  const rows = list.value?.rows ?? [];
  const receipts = runs.value?.rows ?? [];
  const linked = exact.value && !receipts.some(row => row.execution_id === exact.value!.execution_id) ? [exact.value] : [];
  const refreshing = list.loading || detail.loading || runs.loading || exact.loading;
  const freshness = !visible ? "paused" : refreshing ? "refreshing"
    : list.error || detail.error || runs.error || exact.error ? "unavailable"
      : list.value || detail.value || runs.value || exact.value ? "observed" : null;
  return <section className="space-y-4">
    <div className="flex items-center justify-between"><h2 className="font-medium">{t("tasks.scheduled.mine", "Your schedules")}</h2><Button variant="outline" disabled={!visible || refreshing} onClick={() => setRefresh(value => value + 1)}>{t("tasks.scheduled.refresh", "Refresh")}</Button></div>
    <p className="text-sm text-muted-foreground">{t("tasks.scheduled.historyNotice", "Pages show the latest observed receipts, ordered by creation time. Reading or paging never starts a task. Browser Back restores the previous page.")}</p>
    {freshness && <p role="status" className="text-xs text-muted-foreground">{t(`tasks.scheduleVisibility.${freshness}`)}</p>}
    {(taskCursor === false || executionCursor === false || (selected && !taskId) || (execution && !executionId)) && <p role="alert">{t("tasks.scheduled.invalidHistoryLink", "This history link is invalid. Return to Latest or select a schedule.")}</p>}
    {list.loading && <p role="status">{t("tasks.scheduled.loadingSchedules", "Loading schedules…")}</p>}
    {list.error && <p role="alert">{t("tasks.scheduled.historyReadError", "Receipts could not be read. Refresh to retry this read; no task is rerun.")}</p>}
    {list.value && !rows.length && <p>{t("tasks.scheduled.emptyPage", "No schedules on this page.")}</p>}
    {list.value && !list.value.paginationAvailable && <p>{t("tasks.scheduled.legacyHistory", "This server does not provide older-page navigation. Only its returned receipts are shown.")}</p>}
    {rows.map(task => <Card key={task.task_id}><CardContent className="space-y-2 pt-4">
      <h3 className="font-medium">{task.title}</h3><p>{task.status} · {new Date(task.next_run_at).toLocaleString()} · {task.model_id} · {task.interval_seconds ? `${task.interval_seconds / 60} min` : t("tasks.scheduled.once", "Once")}</p>
      <Button size="sm" variant="outline" onClick={() => select(task.task_id)}>{t("tasks.scheduled.history", "Execution history")}</Button>
    </CardContent></Card>)}
    {pageControls("schedule_before", taskCursor, list.value?.next, list.loading)}
    {taskId && <Card><CardContent className="space-y-3 pt-4">
      <h2 className="font-medium">{t("tasks.scheduled.selectedSchedule", "Selected schedule")}</h2>
      <p className="break-all text-sm">{taskId}</p>
      {detail.loading && <p role="status">{t("tasks.scheduled.loadingSchedules", "Loading schedules…")}</p>}
      {detail.error && <p role="alert">{t("tasks.scheduled.exactUnavailable", "This receipt could not be read with your current access. Check the link, account and service, then refresh. It may require a newer server.")}</p>}
      {renderActions?.(taskId, detail.value, visible && !detail.loading && !detail.error && !!detail.value)}
      {detail.value && <>
        <h3>{detail.value.title}</h3><p>{detail.value.status} · {new Date(detail.value.next_run_at).toLocaleString()} · {detail.value.model_id} · {detail.value.interval_seconds ? `${detail.value.interval_seconds / 60} min` : t("tasks.scheduled.once", "Once")}</p>
        <p className="whitespace-pre-wrap text-sm">{detail.value.message}</p>
        {runs.loading && <p role="status">{t("tasks.scheduled.loadingExecutions", "Loading executions…")}</p>}
        {runs.error && <p role="alert">{t("tasks.scheduled.historyReadError", "Receipts could not be read. Refresh to retry this read; no task is rerun.")}</p>}
        {runs.value && !receipts.length && <p>{t("tasks.scheduled.noExecutions", "No executions yet.")}</p>}
        {runs.value && !runs.value.paginationAvailable && <p>{t("tasks.scheduled.legacyHistory", "This server does not provide older-page navigation. Only its returned receipts are shown.")}</p>}
        {executionId && exact.loading && <p role="status">{t("tasks.scheduled.loadingLinkedExecution", "Loading linked execution…")}</p>}
        {executionId && exact.error && <p role="alert">{t("tasks.scheduled.exactUnavailable", "This receipt could not be read with your current access. Check the link, account and service, then refresh. It may require a newer server.")}</p>}
        {linked.length > 0 && <p>{t("tasks.scheduled.linkedOutsidePage", "Linked execution, outside the current page")}</p>}
        {[...linked, ...receipts].map(run => <ScheduledExecutionResult key={run.execution_id} run={run} task={detail.value!} initiallyOpen={executionId === run.execution_id} />)}
        {pageControls("execution_before", executionCursor, runs.value?.next, runs.loading)}
      </>}
    </CardContent></Card>}
  </section>;
}
