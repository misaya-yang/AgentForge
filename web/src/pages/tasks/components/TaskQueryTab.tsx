/**
 * TaskQueryTab - Task ID query tab (migrated from original Tasks.tsx)
 */

import { useState, useCallback, useMemo } from "react";
import { Link, useSearchParams } from "react-router-dom";
import { useTranslation } from "react-i18next";
import {
  Search,
  Loader2,
  AlertCircle,
  CheckCircle2,
  Clock,
  XCircle,
  RefreshCw,
  FileSearch,
  ArrowRight,
  Copy,
  Check,
} from "lucide-react";
import { Input } from "@/components/ui/input";
import { Button } from "@/components/ui/button";
import { Badge } from "@/components/ui/badge";
import { Card, CardContent, CardHeader, CardTitle } from "@/components/ui/card";
import { useTask, useTaskResult } from "@/hooks/useTasks";
import { copyToClipboard } from "@/lib/clipboard";
import { taskDetailParams, taskInboxParams } from "../taskNavigation";
import { TaskTraceLink } from "./TaskTraceLink";
import { TaskDiagnostics } from "./TaskDiagnostics";
import { TaskOutputBrowser } from "./TaskOutputBrowser";
import { useAuthenticatedReadScope } from "@/hooks/useAuthenticatedReadScope";
import { TaskCancellation } from "./TaskCancellation";

const STATUS_CONFIG: Record<
  string,
  {
    labelKey: string;
    variant: "default" | "secondary" | "destructive" | "outline";
    icon: React.ReactNode;
    color: string;
  }
> = {
  completed: {
    labelKey: "tasks.status.completed",
    variant: "default",
    icon: <CheckCircle2 className="h-3.5 w-3.5" />,
    color: "bg-emerald-500/10 text-emerald-600 border-emerald-500/20",
  },
  processing: {
    labelKey: "tasks.status.processing",
    variant: "secondary",
    icon: <Loader2 className="h-3.5 w-3.5 animate-spin" />,
    color: "bg-blue-500/10 text-blue-600 border-blue-500/20",
  },
  pending: {
    labelKey: "tasks.status.pending",
    variant: "outline",
    icon: <Clock className="h-3.5 w-3.5" />,
    color: "bg-amber-500/10 text-amber-600 border-amber-500/20",
  },
  failed: {
    labelKey: "tasks.status.failed",
    variant: "destructive",
    icon: <XCircle className="h-3.5 w-3.5" />,
    color: "bg-red-500/10 text-red-600 border-red-500/20",
  },
  cancelled: {
    labelKey: "tasks.status.cancelled",
    variant: "outline",
    icon: <XCircle className="h-3.5 w-3.5" />,
    color: "bg-gray-500/10 text-gray-500 border-gray-500/20",
  },
};

function StatusBadge({ status }: { status: string }) {
  const { t } = useTranslation();
  const config = STATUS_CONFIG[status] || {
    labelKey: status,
    variant: "outline" as const,
    icon: <Clock className="h-3.5 w-3.5" />,
    color: "bg-gray-500/10 text-gray-500 border-gray-500/20",
  };

  return (
    <Badge className={`gap-1.5 ${config.color} border font-medium`}>
      {config.icon}
      {t(config.labelKey)}
    </Badge>
  );
}

export function TaskQueryTab() {
  const [searchParams, setSearchParams] = useSearchParams();
  const searchedId = searchParams.get("task_id")?.trim() || undefined;
  return <TaskQueryContent key={searchedId ?? ""} searchedId={searchedId}
    inboxHref={`?${taskInboxParams(searchParams)}`}
    onSearch={(taskId) => setSearchParams((current) => taskDetailParams(current, taskId))} />;
}

function TaskQueryContent({ searchedId, inboxHref, onSearch }: {
  searchedId?: string;
  inboxHref: string;
  onSearch: (taskId: string) => void;
}) {
  const { t, i18n } = useTranslation();
  const [inputValue, setInputValue] = useState(searchedId ?? "");
  const [copied, setCopied] = useState(false);
  const [showPartialResult, setShowPartialResult] = useState(false);
  const [showDetailResult, setShowDetailResult] = useState(false);

  const readScope = useAuthenticatedReadScope();
  const taskQuery = useTask(searchedId, readScope);
  // Read the typed result only after this exact receipt completes. Retained
  // failed/cancelled output uses the canonical public detail below.
  const completedTaskId = !taskQuery.isError && taskQuery.data?.task_id === searchedId
    && taskQuery.data?.status === "completed" ? searchedId : undefined;
  const resultQuery = useTaskResult(completedTaskId, readScope);

  const handleSearch = useCallback(() => {
    const trimmed = inputValue.trim();
    if (!trimmed) return;
    onSearch(trimmed);
  }, [inputValue, onSearch]);

  const handleKeyDown = useCallback(
    (e: React.KeyboardEvent) => {
      if (e.key === "Enter") {
        handleSearch();
      }
    },
    [handleSearch]
  );

  const handleCopyId = useCallback(() => {
    if (searchedId) {
      copyToClipboard(searchedId);
      setCopied(true);
      setTimeout(() => setCopied(false), 2000);
    }
  }, [searchedId]);

  const hasSearched = searchedId !== undefined;
  const isLoading = taskQuery.isLoading;
  const hasError = taskQuery.isError;
  const hasData = !!taskQuery.data && taskQuery.data.task_id === searchedId && !hasError;
  // Detail.result is the authorized public projection, not stored bookkeeping.
  // Failed/cancelled output may be partial and never implies successful execution.
  const hasPartialResult = hasData
    && ["failed", "cancelled"].includes(taskQuery.data!.status)
    && taskQuery.data!.result !== undefined && taskQuery.data!.result !== null;
  const partialResultText = useMemo(() => showPartialResult && hasPartialResult
    ? JSON.stringify(taskQuery.data?.result, null, 2) : null,
  [showPartialResult, hasPartialResult, taskQuery.data?.result]);
  // This snapshot is owned by the successful detail read, independently of /result.
  // Neither its shape nor a /result failure identifies the stored result type.
  const hasDetailResult = hasData && taskQuery.data!.status === "completed"
    && taskQuery.data!.result !== undefined && taskQuery.data!.result !== null;
  const detailResultText = useMemo(() => showDetailResult && hasDetailResult
    ? JSON.stringify(taskQuery.data?.result, null, 2) : null,
  [showDetailResult, hasDetailResult, taskQuery.data?.result]);
  const isEmpty = hasSearched && !isLoading && !hasError && !hasData;

  return (
    <div className="space-y-6">
      <Button asChild size="sm" variant="outline">
        <Link to={inboxHref}>{t("tasks.inbox.back")}</Link>
      </Button>
      {/* Search area */}
      <Card className="overflow-hidden">
        <CardContent className="p-4">
          <div className="flex gap-3">
            <div className="relative flex-1">
              <Search className="absolute left-3 top-1/2 -translate-y-1/2 h-4 w-4 text-muted-foreground/60" />
              <Input
                className="pl-9 h-10 bg-background"
                placeholder={t("tasks.searchPlaceholder")}
                value={inputValue}
                onChange={(e) => setInputValue(e.target.value)}
                onKeyDown={handleKeyDown}
              />
            </div>
            <Button
              onClick={handleSearch}
              disabled={!inputValue.trim() || isLoading}
              className="h-10 px-5"
            >
              {isLoading ? (
                <>
                  <Loader2 className="mr-2 h-4 w-4 animate-spin" />
                  {t("tasks.querying")}
                </>
              ) : (
                <>
                  {t("tasks.query")}
                  <ArrowRight className="ml-2 h-4 w-4" />
                </>
              )}
            </Button>
          </div>
        </CardContent>
      </Card>

      {/* Loading state */}
      {isLoading && (
        <Card className="border-dashed bg-card/40">
          <CardContent className="flex flex-col items-center justify-center py-16">
            <div className="rounded-xl bg-primary/10 p-4 border border-primary/20">
              <Loader2 className="h-7 w-7 text-primary animate-spin" />
            </div>
            <p className="mt-4 text-sm font-medium text-foreground">
              {t("tasks.states.loading")}
            </p>
            <p className="text-xs text-muted-foreground mt-1">
              Task ID: {searchedId}
            </p>
          </CardContent>
        </Card>
      )}

      {/* Error state */}
      {hasError && !isLoading && (
        <Card className="border-destructive/30 bg-destructive/5">
          <CardContent className="flex flex-col items-center justify-center py-12">
            <div className="rounded-full bg-destructive/10 p-4">
              <AlertCircle className="h-8 w-8 text-destructive" />
            </div>
            <p className="mt-4 text-sm font-medium text-destructive">
              {t("tasks.states.error")}
            </p>
            <p className="text-xs text-muted-foreground mt-1 max-w-md text-center">
              {taskQuery.error instanceof Error
                ? taskQuery.error.message
                : t("tasks.states.errorDesc")}
            </p>
            <Button
              variant="outline"
              size="sm"
              className="mt-4"
              onClick={() => taskQuery.refetch()}
            >
              <RefreshCw className="mr-2 h-3.5 w-3.5" />
              {t("tasks.states.retry")}
            </Button>
          </CardContent>
        </Card>
      )}

      {/* Empty state */}
      {isEmpty && (
        <Card className="border-dashed">
          <CardContent className="flex flex-col items-center justify-center py-16">
            <div className="rounded-full bg-muted p-4">
              <FileSearch className="h-8 w-8 text-muted-foreground/60" />
            </div>
            <p className="mt-4 text-sm font-medium text-foreground">
              {t("tasks.states.notFound")}
            </p>
            <p className="text-xs text-muted-foreground mt-1">
              {t("tasks.states.notFoundDesc", { id: searchedId })}
            </p>
          </CardContent>
        </Card>
      )}

      {/* Keep cancellation ownership across unavailable/loading receipt observations.
          Only an exact successful detail unlocks a new cancellation action. */}
      {searchedId && <TaskCancellation taskId={searchedId}
        status={hasData && !isLoading ? taskQuery.data!.status : undefined} />}

      {/* Task details */}
      {hasData && !isLoading && (
        <div className="space-y-4">
          {/* Task info card */}
          <Card>
            <CardHeader className="pb-3">
              <div className="flex items-start justify-between">
                <div className="space-y-1">
                  <CardTitle className="text-base">
                    {t("tasks.taskDetail")}
                  </CardTitle>
                  <div className="flex items-center gap-2 text-xs text-muted-foreground">
                    <span className="font-mono">{searchedId}</span>
                    <button
                      onClick={handleCopyId}
                      className="p-1 hover:bg-muted rounded transition-colors"
                      title={t("common.copy")}
                    >
                      {copied ? (
                        <Check className="h-3 w-3 text-green-500" />
                      ) : (
                        <Copy className="h-3 w-3" />
                      )}
                    </button>
                  </div>
                </div>
                <StatusBadge status={taskQuery.data.status} />
              </div>
            </CardHeader>
            <CardContent className="pt-0">
              <div className="grid grid-cols-2 gap-4">
                <div className="space-y-1">
                  <p className="text-xs text-muted-foreground">
                    {t("tasks.service")}
                  </p>
                  <p className="text-sm font-medium">
                    {taskQuery.data.service_id}
                  </p>
                </div>
                <div className="space-y-1">
                  <p className="text-xs text-muted-foreground">
                    {t("tasks.createdAt")}
                  </p>
                  <p className="text-sm font-medium">
                    {new Date(taskQuery.data.created_at).toLocaleString(
                      i18n.language
                    )}
                  </p>
                </div>
              </div>

              {/* Error info */}
              {taskQuery.data.error && (
                <div className="mt-4 p-3 rounded-lg bg-destructive/10 border border-destructive/20">
                  <div className="flex items-start gap-2">
                    <AlertCircle className="h-4 w-4 text-destructive mt-0.5 shrink-0" />
                    <div>
                      <p className="text-xs font-medium text-destructive">
                        {t("tasks.errorInfo")}
                      </p>
                      <p className="text-xs text-destructive/80 mt-1">
                        {taskQuery.data.error}
                      </p>
                    </div>
                  </div>
                </div>
              )}
            </CardContent>
          </Card>

          <TaskTraceLink key={`${readScope.id}:${searchedId}:${taskQuery.data.request_id}`} taskId={searchedId!}
            requestId={taskQuery.data.request_id} isCurrent={readScope.isCurrent} />
          <TaskDiagnostics
            key={JSON.stringify([taskQuery.data.task_id, taskQuery.data.status, resultQuery.isError, resultQuery.isLoading])}
            receipt={taskQuery.data}
            result={resultQuery.isError ? "unavailable" : resultQuery.isLoading ? "loading" : resultQuery.data != null ? "available" : "unknown"}
            refreshing={taskQuery.isFetching}
            refresh={() => { void taskQuery.refetch(); }}
          />

          {/* Partial results belong to the terminal receipt, not the success-only view. */}
          {hasPartialResult && (
            <Card>
              <CardHeader className="pb-3">
                <CardTitle className="text-base">{t("tasks.taskResult")}</CardTitle>
              </CardHeader>
              <CardContent className="pt-0">
                <p className="mb-3 text-sm text-muted-foreground">{t("tasks.partialResult")}</p>
                <Button variant="outline" size="sm" aria-expanded={showPartialResult}
                  onClick={() => setShowPartialResult((current) => !current)}>
                  {t(showPartialResult ? "tasks.hidePartialResult" : "tasks.viewPartialResult")}
                </Button>
                {showPartialResult && <pre className="mt-3 rounded-lg bg-muted/50 border p-4 text-xs font-mono whitespace-pre-wrap overflow-auto max-h-96">
                  {partialResultText}
                </pre>}
              </CardContent>
            </Card>
          )}

          {/* Independently observed detail output; never a successful /result fallback. */}
          {hasDetailResult && (
            <Card>
              <CardHeader className="pb-3">
                <CardTitle className="text-base">{t("tasks.detailResultSnapshot")}</CardTitle>
              </CardHeader>
              <CardContent className="pt-0">
                <p className="mb-3 text-sm text-muted-foreground">{t("tasks.detailResultProvenance")}</p>
                <Button variant="outline" size="sm" aria-expanded={showDetailResult}
                  onClick={() => setShowDetailResult((current) => !current)}>
                  {t(showDetailResult ? "tasks.hideDetailResult" : "tasks.viewDetailResult")}
                </Button>
                {showDetailResult && <pre className="mt-3 rounded-lg bg-muted/50 border p-4 text-xs font-mono whitespace-pre-wrap overflow-auto max-h-96">
                  {detailResultText}
                </pre>}
              </CardContent>
            </Card>
          )}

          {/* Task result */}
          {completedTaskId && resultQuery.isLoading && (
            <p role="status" className="text-sm text-muted-foreground">{t("tasks.states.loading")}</p>
          )}
          {completedTaskId && resultQuery.isError && (
            <div role="alert" className="flex items-center gap-3 text-sm text-destructive">
              <span>{t("tasks.states.error")}</span>
              <Button variant="outline" size="sm" disabled={resultQuery.isFetching} onClick={() => void resultQuery.refetch()}>
                {t("tasks.states.retry")}
              </Button>
            </div>
          )}
          {completedTaskId && !resultQuery.isError && resultQuery.data && (
            <Card>
              <CardHeader className="pb-3">
                <CardTitle className="text-base">
                  {t("tasks.taskResult")}
                </CardTitle>
              </CardHeader>
              <CardContent className="pt-0">
                <TaskOutputBrowser taskId={completedTaskId} requestId={taskQuery.data.request_id}
                  result={resultQuery.data} scope={readScope} />
              </CardContent>
            </Card>
          )}
        </div>
      )}

      {/* Initial state hint */}
      {!hasSearched && (
        <Card className="border-dashed">
          <CardContent className="flex flex-col items-center justify-center py-16">
            <div className="rounded-full bg-primary/5 p-4">
              <Search className="h-8 w-8 text-primary/40" />
            </div>
            <p className="mt-4 text-sm font-medium text-foreground">
              {t("tasks.states.initial")}
            </p>
            <p className="text-xs text-muted-foreground mt-1">
              {t("tasks.states.initialDesc")}
            </p>
          </CardContent>
        </Card>
      )}
    </div>
  );
}
