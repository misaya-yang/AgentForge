import { useEffect, useMemo, useRef, useState } from "react";
import { AlertTriangle, Loader2, RefreshCcw, SearchX } from "lucide-react";
import { useTranslation } from "react-i18next";

import { listDatasetQueries, listQueryFeedback } from "@/api/knowledge";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Card } from "@/components/ui/card";
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select";
import type { QueryFeedback, QueryHistoryItem } from "@/types/knowledge";
import { useAuthStore, usePermission } from "@/store/useAuthStore";
import { sdkAuthorityScope } from "@/lib/sdkAuthorityScope";
import { queryDiagnosticHandoff, queryDiagnosticTraceUrl, type QueryDiagnosticHandoff } from "./queryDiagnosticHandoff";

interface QueryObservabilityTabProps {
  datasetId?: string;
  permission?: string;
  onAccessChange?: (available: boolean) => void;
  onTestQuestion: (observation: QueryDiagnosticHandoff) => void;
  onInspect: (tab: "documents" | "settings") => void;
}

type ResultFilter = "all" | "zero" | "hits";
type ObservationScope = { datasetId?: string; authority: string; token: string | null; canRead: boolean; denied: boolean; retired: boolean };
type ObservationPage<T> = { rows: T[]; cursor: string | null; more: boolean };
type ReadTicket = { scope: ObservationScope; key: string; pending: boolean };

/** Retired reads and failed refreshes never leave actionable historical rows. */
function useObservationPages<T extends { dataset_id: string }>(
  scope: ObservationScope,
  key: string,
  read: (cursor?: string) => Promise<ObservationPage<T>>,
  onAccessChange: (available: boolean) => void,
) {
  const latest = useRef({ scope, key });
  latest.current = { scope, key };
  const active = useRef<ReadTicket | null>(null);
  const [state, setState] = useState<ObservationPage<T> & {
    ticket: ReadTicket | null; loading: boolean; error: boolean;
  }>({ rows: [], cursor: null, more: false, ticket: null, loading: false, error: false });
  const isCurrent = (ticket: ReadTicket | null) => !!ticket && !ticket.scope.denied && !ticket.scope.retired && active.current === ticket
    && latest.current.scope === ticket.scope && latest.current.key === ticket.key
    && useAuthStore.getState().token === ticket.scope.token
    && sdkAuthorityScope(useAuthStore.getState().user) === ticket.scope.authority;

  async function load(append = false) {
    if (!scope.datasetId || !scope.token || !scope.canRead || scope.denied || scope.retired
      || useAuthStore.getState().token !== scope.token
      || sdkAuthorityScope(useAuthStore.getState().user) !== scope.authority) return;
    if (active.current?.pending && isCurrent(active.current)) return;
    const cursor = append && isCurrent(state.ticket) ? state.cursor : undefined;
    if (append && !cursor) return;
    const ticket = { scope, key, pending: true };
    active.current = ticket;
    setState(current => ({ rows: append ? current.rows : [], cursor: null, more: false,
      ticket, loading: true, error: false }));
    try {
      const page = await read(cursor || undefined);
      if (!isCurrent(ticket)) return;
      if (page.rows.some(row => row.dataset_id !== scope.datasetId)
        || (page.more && (!page.cursor || page.cursor === cursor))) throw new Error("Invalid observation page");
      onAccessChange(true);
      setState(current => ({ ...page, rows: append ? [...current.rows, ...page.rows] : page.rows,
        ticket, loading: false, error: false }));
    } catch (error) {
      if (isCurrent(ticket)) {
        const status = (error as { response?: { status?: number } } | null)?.response?.status;
        if (status === 401 || status === 403 || status === 404) onAccessChange(false);
        setState({ rows: [], cursor: null, more: false, ticket, loading: false, error: true });
      }
    } finally {
      ticket.pending = false;
    }
  }

  useEffect(() => {
    void load();
    return () => { active.current = null; };
    // The key includes every read filter; scope includes the current session/authority.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [scope, key]);
  const current = isCurrent(state.ticket);
  return {
    rows: current && !state.error ? state.rows : [], loading: current && state.loading,
    error: current && state.error, more: current && !state.error && state.more,
    canAct: () => isCurrent(state.ticket) && !state.loading && !state.error, load,
  };
}

export function QueryObservabilityTab({ datasetId, permission, onTestQuestion, onInspect, onAccessChange }: QueryObservabilityTabProps) {
  const { t } = useTranslation();
  const authority = useAuthStore(state => sdkAuthorityScope(state.user));
  const token = useAuthStore(state => state.token);
  const canRead = permission === "owner" || permission === "editor";
  const canViewTrace = usePermission("console:eval:view");
  const [accessCheck, setAccessCheck] = useState(0);
  const [deniedScope, setDeniedScope] = useState<ObservationScope | null>(null);
  const scope = useMemo(() => ({ datasetId, authority, token, canRead, denied: false, retired: false, accessCheck }), [datasetId, authority, token, canRead, accessCheck]);
  useEffect(() => useAuthStore.subscribe(() => {
    const current = useAuthStore.getState();
    if (current.token !== scope.token || sdkAuthorityScope(current.user) !== scope.authority) {
      // Latch before React commits; a restored store snapshot cannot revive old reads.
      scope.retired = true;
      setAccessCheck(value => value + 1);
    }
  }), [scope]);
  function accessChanged(available: boolean) {
    if (!available) { scope.denied = true; setDeniedScope(scope); }
    onAccessChange?.(available);
  }
  const [resultFilter, setResultFilter] = useState<ResultFilter>("all");
  const [mode, setMode] = useState("all");
  const history = useObservationPages<QueryHistoryItem>(scope, JSON.stringify([resultFilter, mode]), async cursor => {
    const page = await listDatasetQueries(datasetId!, { limit: 50,
      zero_results: resultFilter === "all" ? undefined : resultFilter === "zero",
      mode: mode === "all" ? undefined : mode, cursor });
    return { rows: page.queries, cursor: page.next_cursor || null, more: page.has_more };
  }, accessChanged);
  const feedback = useObservationPages<QueryFeedback>(scope, "negative", async cursor => {
    const page = await listQueryFeedback(datasetId!, { limit: 50, rating: "negative", cursor });
    return { rows: page.feedback, cursor: page.next_cursor || null, more: page.has_more };
  }, accessChanged);
  const queries = history.rows, queryLoading = history.loading, queryError = history.error, queryHasMore = history.more;
  const negativeFeedback = feedback.rows, feedbackLoading = feedback.loading, feedbackError = feedback.error, feedbackHasMore = feedback.more;
  const loadQueries = history.load, loadNegativeFeedback = feedback.load;
  const feedbackReadCanAct = feedback.canAct;

  function actions(row: QueryHistoryItem | QueryFeedback, canAct: () => boolean) {
    const observation = queryDiagnosticHandoff(datasetId, row);
    if (!observation) return null;
    const traceUrl = queryDiagnosticTraceUrl(observation);
    return <div className="space-y-1.5">
      <div className="flex flex-wrap gap-2">
        <Button size="sm" variant="outline" disabled={!canAct() || !observation.question}
          data-testid={`query-test-${observation.observationId}`}
          onClick={() => { if (canAct() && observation.question) onTestQuestion(observation); }}>
          {t("knowledge.observability.testQuestion")}
        </Button>
        {traceUrl && canViewTrace && canAct() ? <a className="inline-flex items-center text-xs underline" href={traceUrl}
          data-testid={`query-trace-${observation.observationId}`}
          onClick={event => { if (!canAct()) event.preventDefault(); }}>{t("knowledge.observability.openTrace")}</a> : null}
      </div>
      {!observation.question && <p className="text-xs text-muted-foreground">{t("knowledge.observability.questionMissing")}</p>}
      {traceUrl && !canViewTrace && <p className="text-xs text-muted-foreground">{t("knowledge.observability.tracePermission")}</p>}
      {!traceUrl && <p className="text-xs text-muted-foreground">{t("knowledge.observability.traceMissing")}</p>}
    </div>;
  }
  if (deniedScope === scope) return <div role="alert" className="space-y-2 text-sm">
    <p>{t("knowledge.observability.accessUnavailable")}</p>
    <Button variant="outline" onClick={() => setAccessCheck(value => value + 1)}>{t("knowledge.observability.recheckAccess")}</Button>
  </div>;
  if (!canRead) return <p role="note" className="text-sm text-muted-foreground">{t("knowledge.observability.historyPermission")}</p>;

  return (
    <div className="grid gap-6 xl:grid-cols-[minmax(0,2fr)_minmax(320px,1fr)]">
      <Card className="overflow-hidden">
        <div className="flex flex-wrap items-center justify-between gap-3 border-b border-border/60 p-4">
          <div>
            <h3 className="font-semibold">{t("knowledge.observability.queryTitle")}</h3>
            <p className="text-xs text-muted-foreground">
              {t("knowledge.observability.queryHint")}
            </p>
          </div>
          <div className="flex flex-wrap items-center gap-2">
            <Select value={resultFilter} onValueChange={(value) => setResultFilter(value as ResultFilter)}>
              <SelectTrigger className="w-36" data-testid="query-result-filter">
                <SelectValue />
              </SelectTrigger>
              <SelectContent>
                <SelectItem value="all">{t("knowledge.observability.allQueries")}</SelectItem>
                <SelectItem value="zero">{t("knowledge.observability.zeroResults")}</SelectItem>
                <SelectItem value="hits">{t("knowledge.observability.withHits")}</SelectItem>
              </SelectContent>
            </Select>
            <Select value={mode} onValueChange={setMode}>
              <SelectTrigger className="w-32" data-testid="query-mode-filter">
                <SelectValue />
              </SelectTrigger>
              <SelectContent>
                <SelectItem value="all">{t("knowledge.observability.allModes")}</SelectItem>
                <SelectItem value="hybrid">Hybrid</SelectItem>
                <SelectItem value="dense">Dense</SelectItem>
                <SelectItem value="bm25">BM25</SelectItem>
              </SelectContent>
            </Select>
            <Button size="icon" variant="outline" disabled={queryLoading} aria-label={t("knowledge.observability.refreshQueries")} onClick={() => void loadQueries(false)}>
              <RefreshCcw className={queryLoading ? "h-4 w-4 animate-spin" : "h-4 w-4"} />
            </Button>
          </div>
        </div>

        {queryError ? (
          <div role="alert" className="m-4 rounded-lg border border-destructive/30 bg-destructive/5 p-3 text-sm text-destructive">
            {t("knowledge.observability.readUnavailable")}
          </div>
        ) : null}

        <div className="space-y-2 border-b border-border/60 px-4 py-3">
          <p className="text-xs text-muted-foreground">{t("knowledge.observability.handoffHint")}</p>
          <div className="flex flex-wrap gap-2">
            <Button size="sm" variant="outline" onClick={() => onInspect("documents")}>{t("knowledge.observability.inspectDocuments")}</Button>
            <Button size="sm" variant="outline" onClick={() => onInspect("settings")}>{t("knowledge.observability.inspectSettings")}</Button>
          </div>
        </div>
        <div className="divide-y divide-border/60" data-testid="query-log-list">
          {queries.map((query) => {
            const totalMs = Number(query.stage_timings?.total_ms ?? 0);
            const hitCount = typeof query.hit_count === "number" && Number.isSafeInteger(query.hit_count) && query.hit_count >= 0 ? query.hit_count : null;
            return (
              <article key={query.id} className="space-y-2 p-4">
                <div className="flex flex-wrap items-start justify-between gap-2">
                  <p className="min-w-0 flex-1 break-words text-sm font-medium">{query.content}</p>
                  <div className="flex items-center gap-2">
                    <Badge variant="outline">{query.mode || "-"}</Badge>
                    <Badge className={hitCount === null ? "bg-muted text-muted-foreground" : hitCount === 0 ? "bg-rose-500/10 text-rose-600" : "bg-emerald-500/10 text-emerald-600"}>
                      {hitCount === null ? t("knowledge.observability.hitsUnknown") : t("knowledge.observability.hitCount", { count: hitCount })}
                    </Badge>
                  </div>
                </div>
                <div className="flex flex-wrap gap-x-4 gap-y-1 text-xs text-muted-foreground">
                  <span>{new Date(query.created_at).toLocaleString()}</span>
                  <span>TopK {query.top_k ?? "-"}</span>
                  <span>{totalMs > 0 ? `${totalMs.toFixed(1)} ms` : "-"}</span>
                  <span>{query.source}</span>
                  {query.trace_id ? <span className="font-mono">{query.trace_id.slice(0, 8)}</span> : null}
                </div>
                {actions(query, history.canAct)}
              </article>
            );
          })}
          {!queryLoading && !queryError && queries.length === 0 ? (
            <div className="flex flex-col items-center gap-2 p-12 text-center text-muted-foreground">
              <SearchX className="h-8 w-8" />
              <p className="text-sm">{t("knowledge.observability.noQueries")}</p>
            </div>
          ) : null}
        </div>
        {queryHasMore ? (
          <div className="border-t border-border/60 p-3 text-center">
            <Button variant="outline" onClick={() => void loadQueries(true)} disabled={queryLoading}>
              {queryLoading && <Loader2 className="mr-2 h-4 w-4 animate-spin" />}
              {t("knowledge.observability.loadMore")}
            </Button>
          </div>
        ) : null}
      </Card>

      <Card className="overflow-hidden">
        <div className="flex items-center justify-between border-b border-border/60 p-4">
          <div>
            <h3 className="flex items-center gap-2 font-semibold">
              <AlertTriangle className="h-4 w-4 text-amber-600" />
              {t("knowledge.observability.negativeTitle")}
            </h3>
            <p className="text-xs text-muted-foreground">
              {t("knowledge.observability.negativeHint")}
            </p>
          </div>
          <Button size="icon" variant="outline" disabled={feedbackLoading} aria-label={t("knowledge.observability.refreshFeedback")} onClick={() => void loadNegativeFeedback(false)}>
            <RefreshCcw className={feedbackLoading ? "h-4 w-4 animate-spin" : "h-4 w-4"} />
          </Button>
        </div>
        {feedbackError ? (
          <div role="alert" className="m-4 rounded-lg border border-destructive/30 bg-destructive/5 p-3 text-sm text-destructive">
            {t("knowledge.observability.readUnavailable")}
          </div>
        ) : null}
        <div className="divide-y divide-border/60" data-testid="negative-feedback-list">
          {negativeFeedback.map((feedback) => (
            <article key={feedback.feedback_id} className="space-y-2 p-4">
              <div className="flex items-center justify-between gap-2">
                <Badge className="bg-rose-500/10 text-rose-600">
                  {t(`knowledge.feedback.reasons.${feedback.reason_code}`)}
                </Badge>
                <span className="text-xs text-muted-foreground">
                  {new Date(feedback.created_at).toLocaleString()}
                </span>
              </div>
              <p className="line-clamp-3 text-sm">
                {feedback.query_content || t("knowledge.observability.queryUnavailable")}
              </p>
              {feedback.comment ? (
                <p className="rounded-md bg-muted/50 p-2 text-xs text-muted-foreground">
                  {feedback.comment}
                </p>
              ) : null}
              <p className="text-xs font-mono text-muted-foreground">
                {feedback.target_type} · {feedback.target_id.slice(0, 12)}
              </p>
              {actions(feedback, feedbackReadCanAct)}
            </article>
          ))}
          {!feedbackLoading && !feedbackError && negativeFeedback.length === 0 ? (
            <p className="p-10 text-center text-sm text-muted-foreground">
              {t("knowledge.observability.noNegative")}
            </p>
          ) : null}
        </div>
        {feedbackHasMore ? (
          <div className="border-t border-border/60 p-3 text-center">
            <Button variant="outline" onClick={() => void loadNegativeFeedback(true)} disabled={feedbackLoading}>
              {feedbackLoading && <Loader2 className="mr-2 h-4 w-4 animate-spin" />}
              {t("knowledge.observability.loadMore")}
            </Button>
          </div>
        ) : null}
      </Card>
    </div>
  );
}
