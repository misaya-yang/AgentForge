import { AlertTriangle, Loader2, RefreshCw, RotateCcw } from "lucide-react";
import { useTranslation } from "react-i18next";

import type { EmbeddingMigrationActionJob } from "@/api/knowledge";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";

import {
  canRetryEmbeddingActionJob,
  normalizeEmbeddingActionJobState,
  shouldPollEmbeddingActionJob,
} from "./embeddingMigrationJobs";

interface EmbeddingActionJobCardProps {
  job: EmbeddingMigrationActionJob;
  pollError: string | null;
  retrying: boolean;
  onPollNow: () => void;
  onRetry: () => void;
}

function stateClass(state: string): string {
  if (state === "succeeded") {
    return "border-emerald-500/30 bg-emerald-500/10 text-emerald-700 dark:text-emerald-300";
  }
  if (state === "failed") {
    return "border-destructive/30 bg-destructive/10 text-destructive";
  }
  if (state === "queued") {
    return "border-amber-500/30 bg-amber-500/10 text-amber-800 dark:text-amber-200";
  }
  if (state === "running") {
    return "border-primary/30 bg-primary/10 text-primary";
  }
  return "border-border bg-muted/60 text-muted-foreground";
}

function summarizeResult(result: Record<string, unknown> | null): string {
  if (!result) return "";
  return Object.entries(result)
    .filter(([, value]) => ["string", "number", "boolean"].includes(typeof value))
    .slice(0, 5)
    .map(([key, value]) => `${key}=${String(value)}`)
    .join(" · ");
}

export function EmbeddingActionJobCard({
  job,
  pollError,
  retrying,
  onPollNow,
  onRetry,
}: EmbeddingActionJobCardProps) {
  const { t } = useTranslation();
  const state = normalizeEmbeddingActionJobState(job);
  const pending = shouldPollEmbeddingActionJob(job);
  const retryable = canRetryEmbeddingActionJob(job);
  const resultSummary = summarizeResult(job.result);

  return (
    <div
      className="rounded-xl border border-border/60 bg-muted/20 p-3"
      data-testid="embedding-action-job"
    >
      <div className="flex flex-wrap items-start justify-between gap-2">
        <div>
          <p className="text-sm font-semibold text-foreground">
            {t("knowledge.detail.embeddingMigration.actionJob.title", {
              action: t(`knowledge.detail.embeddingMigration.actions.${job.action}`),
            })}
          </p>
          <p className="mt-1 font-mono text-[11px] text-muted-foreground">
            {job.job_id}
          </p>
          <p className="mt-0.5 font-mono text-[11px] text-muted-foreground">
            migration {job.migration_id}
          </p>
        </div>
        <div className="flex flex-wrap items-center gap-2">
          {job.reused ? (
            <Badge variant="outline" data-testid="embedding-action-job-reused">
              {t("knowledge.detail.embeddingMigration.actionJob.reused")}
            </Badge>
          ) : null}
          <Badge
            variant="outline"
            className={stateClass(state)}
            data-testid="embedding-action-job-state"
          >
            {t(`knowledge.detail.embeddingMigration.actionJob.states.${state}`)}
          </Badge>
        </div>
      </div>

      <div className="mt-2 flex flex-wrap gap-x-4 gap-y-1 text-xs text-muted-foreground">
        <span>
          {t("knowledge.detail.embeddingMigration.actionJob.attempts", {
            count: job.attempt_count,
          })}
        </span>
        {job.request_hash ? (
          <span className="font-mono" title={job.request_hash}>
            request {job.request_hash.slice(0, 12)}
          </span>
        ) : null}
      </div>

      {pending ? (
        <div className="mt-3 flex items-start gap-2 text-xs text-primary">
          <Loader2 className="mt-0.5 h-3.5 w-3.5 shrink-0 animate-spin" />
          <p>{t("knowledge.detail.embeddingMigration.actionJob.durablePolling")}</p>
        </div>
      ) : null}
      {job.error ? (
        <p className="mt-3 text-xs text-destructive" data-testid="embedding-action-job-error">
          {job.error}
        </p>
      ) : null}
      {resultSummary ? (
        <p className="mt-3 break-words font-mono text-xs text-muted-foreground">
          {resultSummary}
        </p>
      ) : null}

      {pollError ? (
        <div
          className="mt-3 flex flex-wrap items-center justify-between gap-2 rounded-lg border border-destructive/20 bg-destructive/5 p-2"
          role="alert"
          data-testid="embedding-action-job-poll-error"
        >
          <span className="flex items-start gap-1.5 text-xs text-destructive">
            <AlertTriangle className="mt-0.5 h-3.5 w-3.5 shrink-0" />
            {pollError}
          </span>
          <Button variant="outline" size="sm" onClick={onPollNow}>
            <RefreshCw className="mr-1.5 h-3.5 w-3.5" />
            {t("knowledge.detail.embeddingMigration.actionJob.retryPoll")}
          </Button>
        </div>
      ) : null}

      {retryable ? (
        <Button
          className="mt-3"
          variant="outline"
          size="sm"
          onClick={onRetry}
          disabled={retrying}
          data-testid="embedding-action-job-retry"
        >
          {retrying ? (
            <Loader2 className="mr-1.5 h-3.5 w-3.5 animate-spin" />
          ) : (
            <RotateCcw className="mr-1.5 h-3.5 w-3.5" />
          )}
          {t("knowledge.detail.embeddingMigration.actionJob.retrySameJob")}
        </Button>
      ) : null}
    </div>
  );
}
