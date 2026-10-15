import { Link, useSearchParams } from "react-router-dom";
import { useTranslation } from "react-i18next";
import {
  AlertCircle,
  CheckCircle2,
  Clock3,
  Inbox,
  Loader2,
  RefreshCw,
  XCircle,
} from "lucide-react";

import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Card, CardContent } from "@/components/ui/card";
import { useTasks } from "@/hooks/useTasks";
import { useAuthenticatedReadScope } from "@/hooks/useAuthenticatedReadScope";
import type { Task } from "@/types/gateway";
import { TASK_FILTERS, TASK_PAGE_SIZE, taskDetailParams, taskInboxLocation, taskInboxParams, taskPageWindow } from "../taskNavigation";


function TaskStatusBadge({ status }: { status: string }) {
  const { t } = useTranslation();
  const icon = status === "completed"
    ? <CheckCircle2 className="h-3.5 w-3.5" />
    : status === "failed"
      ? <AlertCircle className="h-3.5 w-3.5" />
      : status === "cancelled"
        ? <XCircle className="h-3.5 w-3.5" />
        : status === "processing"
          ? <Loader2 className="h-3.5 w-3.5 animate-spin" />
          : <Clock3 className="h-3.5 w-3.5" />;
  return (
    <Badge variant="outline" className="gap-1.5">
      {icon}
      {t(`tasks.status.${status}`, status)}
    </Badge>
  );
}

function TaskRow({ task, detailHref, isCurrent }: { task: Task; detailHref: string; isCurrent: () => boolean }) {
  const { t, i18n } = useTranslation();
  // Preserve the existing numeric display convention, without coercing missing
  // or malformed observations into a definite percentage. Status stays separate.
  const rawProgress = task.progress;
  const progress = typeof rawProgress === "number" && Number.isFinite(rawProgress)
    && rawProgress >= 0 && rawProgress <= 100
    ? Math.round(rawProgress <= 1 ? rawProgress * 100 : rawProgress)
    : null;
  return (
    <article className="grid gap-3 rounded-xl border border-border/70 bg-card p-4 sm:grid-cols-[minmax(0,1fr)_auto] sm:items-center">
      <div className="min-w-0">
        <div className="flex flex-wrap items-center gap-2">
          <strong className="truncate text-sm">{task.service_id}</strong>
          <TaskStatusBadge status={task.status} />
        </div>
        <p className="mt-1 truncate font-mono text-xs text-muted-foreground">{task.task_id}</p>
        <p className="mt-1 text-xs text-muted-foreground">
          {t("tasks.createdAt")} · {new Date(task.created_at).toLocaleString(i18n.language)}
        </p>
      </div>
      <div className="text-left sm:text-right">
        <span className="text-xs text-muted-foreground">{t("tasks.progress")}</span>
        <div className="mt-1 font-mono text-sm tabular-nums">{progress === null ? t("tasks.progressUnavailable") : `${progress}%`}</div>
        <Button asChild size="sm" variant="outline" className="mt-2">
          <Link to={detailHref} onClick={(event) => { if (!isCurrent()) event.preventDefault(); }}>{t("tasks.inbox.openTask")}</Link>
        </Button>
      </div>
    </article>
  );
}

export function TaskInboxTab() {
  const { t } = useTranslation();
  const [searchParams, setSearchParams] = useSearchParams();
  const { filter, offset } = taskInboxLocation(searchParams);
  const status = filter === "all" ? undefined : filter;
  const readScope = useAuthenticatedReadScope();
  const query = useTasks(status, offset, readScope);
  const page = taskPageWindow(query.data ?? [], offset);
  const tasks = page.rows;
  if (!readScope.isCurrent()) return null;

  return (
    <div className="space-y-4">
      <div className="flex flex-wrap items-center justify-between gap-3">
        <div className="flex flex-wrap gap-2" role="group" aria-label={t("tasks.inbox.filterLabel")}>
          {TASK_FILTERS.map((value) => (
            <Button
              key={value}
              size="sm"
              variant={filter === value ? "default" : "outline"}
              aria-pressed={filter === value}
              onClick={() => { if (readScope.isCurrent()) setSearchParams((current) => taskInboxParams(current, value)); }}
            >
              {t(`tasks.inbox.filters.${value}`)}
            </Button>
          ))}
        </div>
        <Button size="sm" variant="ghost" onClick={() => { if (readScope.isCurrent()) void query.refetch(); }} disabled={query.isFetching}>
          <RefreshCw className={`mr-2 h-3.5 w-3.5 ${query.isFetching ? "animate-spin" : ""}`} />
          {t("common.refresh")}
        </Button>
      </div>

      {query.isLoading ? (
        <Card><CardContent className="flex items-center justify-center gap-2 py-14 text-sm text-muted-foreground">
          <Loader2 className="h-4 w-4 animate-spin" />{t("tasks.states.loading")}
        </CardContent></Card>
      ) : query.isError ? (
        <Card className="border-destructive/30"><CardContent className="flex flex-col items-center py-12 text-center">
          <AlertCircle className="h-7 w-7 text-destructive" />
          <p className="mt-3 text-sm font-medium">{t("tasks.inbox.loadFailed")}</p>
          <Button className="mt-3" size="sm" variant="outline" onClick={() => { if (readScope.isCurrent()) void query.refetch(); }}>{t("tasks.states.retry")}</Button>
        </CardContent></Card>
      ) : tasks.length === 0 ? (
        <Card className="border-dashed"><CardContent className="flex flex-col items-center py-14 text-center">
          <Inbox className="h-8 w-8 text-muted-foreground/60" />
          <p className="mt-3 text-sm font-medium">{t("tasks.inbox.empty")}</p>
          <p className="mt-1 max-w-md text-xs text-muted-foreground">{t("tasks.inbox.emptyDescription")}</p>
        </CardContent></Card>
      ) : (
        <div className="space-y-3">{tasks.map((task) => <TaskRow key={task.task_id} task={task} isCurrent={readScope.isCurrent} detailHref={`?${taskDetailParams(searchParams, task.task_id)}`} />)}</div>
      )}
      <div className="flex flex-wrap items-center justify-between gap-3" aria-label={t("tasks.inbox.pagination")}>
        <p className="text-xs text-muted-foreground" aria-live="polite">
          {!query.isLoading && !query.isError && t("tasks.inbox.loadedWindow", { start: page.start, end: page.end, count: tasks.length })}
        </p>
        <div className="flex gap-2">
          <Button size="sm" variant="outline" disabled={!page.hasPrevious || query.isFetching}
            onClick={() => { if (readScope.isCurrent()) setSearchParams((current) => taskInboxParams(current, undefined, Math.max(0, offset - TASK_PAGE_SIZE))); }}>
            {t("tasks.inbox.previousPage")}
          </Button>
          <Button size="sm" variant="outline" disabled={!page.hasNext || query.isFetching || query.isError}
            onClick={() => { if (readScope.isCurrent()) setSearchParams((current) => taskInboxParams(current, undefined, offset + TASK_PAGE_SIZE)); }}>
            {t("tasks.inbox.nextPage")}
          </Button>
        </div>
      </div>
    </div>
  );
}
