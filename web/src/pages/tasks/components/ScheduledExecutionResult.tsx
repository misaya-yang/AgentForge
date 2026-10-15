import { useEffect, useState } from "react";
import { Link } from "react-router-dom";
import { useTranslation } from "react-i18next";
import { getSessionHistory, type SessionHistoryResponse } from "@/api/assistant";
import type { ScheduledExecution, ScheduledTask } from "@/api/scheduledTasks";
import { Button } from "@/components/ui/button";

interface Props {
  run: ScheduledExecution;
  task: Pick<ScheduledTask, "task_id" | "status">;
  initiallyOpen: boolean;
  canonicalHref?: string;
}

/** Read-only receipt inspection: opening or refreshing never launches another turn. */
export function ScheduledExecutionResult({ run, task, initiallyOpen, canonicalHref }: Props) {
  const { t } = useTranslation();
  const [open, setOpen] = useState(initiallyOpen);
  const [result, setResult] = useState<SessionHistoryResponse | null>(null);
  const [failed, setFailed] = useState(false);
  const [loading, setLoading] = useState(false);
  const [revision, setRevision] = useState(0);
  useEffect(() => { if (initiallyOpen) setOpen(true); }, [initiallyOpen]);
  useEffect(() => {
    if (!open || !run.session_id || !run.run_id) return;
    let alive = true;
    setResult(null); setFailed(false); setLoading(true);
    void getSessionHistory(encodeURIComponent(run.session_id), 200).then((response) => {
      if (alive) setResult(response);
    }).catch(() => {
      if (alive) setFailed(true);
    }).finally(() => {
      if (alive) setLoading(false);
    });
    return () => { alive = false; };
  }, [open, run.session_id, run.run_id, run.status, revision]);

  const terminal = ["succeeded", "failed", "cancelled"].includes(run.status);
  const runMessages = result?.messages.filter((message) => message.metadata?.runtime_run_id === run.run_id) ?? [];
  const messages = runMessages.filter((message) => message.role === "assistant"
    && message.metadata?.source_access_revoked !== true
    && typeof message.content === "string" && message.content.trim());
  const revoked = runMessages.some((message) => message.metadata?.source_access_revoked === true);
  const href = canonicalHref ?? `/tasks?${new URLSearchParams({ tab: "scheduled", task: task.task_id, execution: run.execution_id })}`;

  return <div className="space-y-2 break-words rounded-md border p-3 text-sm">
    <p>{new Date(run.created_at).toLocaleString()} · {run.status}
      {run.cancel_requested && !terminal ? ` · ${t("tasks.scheduled.cancelPending", "Cancellation requested")}` : ""}
    </p>
    <p className="text-muted-foreground">{t("tasks.scheduled.executionId", "Execution ID")}: {run.execution_id}</p>
    {run.run_id && <p className="text-muted-foreground">{t("tasks.scheduled.run", "Run ID")}: {run.run_id}</p>}
    {run.finished_at && <p>{t("tasks.scheduled.finished", "Finished")}: {new Date(run.finished_at).toLocaleString()}</p>}
    {run.error_code && <p className="text-destructive">{run.error_code}</p>}
    {run.status === "uncertain" && <p role="status" className="text-amber-700 dark:text-amber-400">{t("tasks.scheduled.uncertainRecovery", "The launch outcome is unresolved. Do not create a replacement task yet: it could repeat tool actions. Refresh the receipt and give an administrator the execution and run IDs. Cancellation remains a request until the runtime confirms it.")}</p>}
    {run.status === "failed" && run.run_id && <p>{t("tasks.scheduled.failedRecovery", "This run failed after launch and may have performed actions. Review its output before creating another schedule. A recurring schedule may still launch its next occurrence; pause it if needed.")}</p>}
    {run.status === "failed" && !run.run_id && <p>{task.status === "paused"
      ? t("tasks.scheduled.preflightRecovery", "The schedule is paused after a pre-launch failure. Check the error and resolve access, model or service availability before resuming. Resume allows a new launch when due.")
      : t("tasks.scheduled.preflightReceipt", "This attempt failed before launch. Check the schedule status: a bounded pre-launch retry may still be pending.")}</p>}
    <div className="flex flex-wrap gap-2">
      <Button asChild size="sm" variant="link"><Link to={href}>{t("tasks.scheduled.executionLink", "Link to this execution")}</Link></Button>
      {run.run_id && <Button size="sm" variant="outline" aria-expanded={open} onClick={() => setOpen((value) => !value)}>{open ? t("tasks.scheduled.hideResult", "Hide output") : t("tasks.scheduled.viewResult", "View output")}</Button>}
    </div>
    {open && run.run_id && <div className="space-y-2 border-t pt-2" aria-busy={loading}>
      <p className="text-muted-foreground">{t("tasks.scheduled.resultScope", "Read-only assistant text from this execution's session. Output may be partial; the execution status determines whether the run succeeded.")}</p>
      <Button size="sm" variant="outline" disabled={loading} onClick={() => setRevision((value) => value + 1)}>{t("tasks.scheduled.refreshResult", "Refresh output")}</Button>
      {loading && <p role="status">{t("tasks.scheduled.loadingResult", "Loading output…")}</p>}
      {failed && <p role="alert" className="text-destructive">{t("tasks.scheduled.resultUnavailable", "Output could not be loaded. Your access may have changed or the service may be unavailable. Refreshing only reads the result; it does not rerun the task.")}</p>}
      {result && revoked && <p>{t("tasks.scheduled.resultRevoked", "Some output is hidden because access to its sources was revoked.")}</p>}
      {result && result.total > result.messages.length && <p>{t("tasks.scheduled.resultLimited", "Only the latest 200 history messages are available in this view.")}</p>}
      {result && !messages.length && <p>{t("tasks.scheduled.resultEmpty", "No readable assistant text is available yet. This does not mean the run succeeded or had no effects.")}</p>}
      {messages.map((message, index) => <p key={`${message.timestamp ?? "message"}-${index}`} className="whitespace-pre-wrap">{message.content}</p>)}
    </div>}
  </div>;
}
