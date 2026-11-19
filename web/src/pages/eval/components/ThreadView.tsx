import { Alert, Button, Empty, Spin, Tag } from "antd";
import { ArrowRight, MessagesSquare } from "lucide-react";
import { useMemo } from "react";
import { useTranslation } from "react-i18next";
import { useQuery } from "@tanstack/react-query";

import { getTraceThread, type AgentTraceSummary } from "@/api/eval";

import { formatDate, formatDuration, locatorText, traceLocator, traceTurn } from "./tracePresentation";

interface ThreadViewProps {
  traceAccessKey: string;
  threadId?: string | null;
  fallbackTraces: AgentTraceSummary[];
  selectedTraceId?: string;
  onSelectTrace: (traceId: string) => void;
  onOpenRun?: (traceId: string) => void;
}

function statusClass(status: AgentTraceSummary["status"]) {
  if (status === "succeeded") return "is-success";
  if (status === "failed" || status === "timeout") return "is-danger";
  if (status === "cancelled") return "is-warning";
  return "is-running";
}

export function ThreadView({
  traceAccessKey,
  threadId,
  fallbackTraces,
  selectedTraceId,
  onSelectTrace,
  onOpenRun,
}: ThreadViewProps) {
  const { t, i18n } = useTranslation();
  const threadQuery = useQuery({
    queryKey: ["eval", "thread", traceAccessKey, threadId],
    queryFn: () => getTraceThread(threadId || ""),
    enabled: Boolean(threadId),
    staleTime: 20_000,
  });

  const traces = useMemo(() => {
    const remote = threadQuery.data?.traces || [];
    return remote.length > 0 ? remote : fallbackTraces;
  }, [fallbackTraces, threadQuery.data?.traces]);
  const selectedTrace = selectedTraceId
    ? traces.find((trace) => trace.trace_id === selectedTraceId)
    : traces[0];
  const locator = traceLocator(selectedTrace);

  if (!threadId) {
    return (
      <section className="eval-panel eval-thread-view">
        <div className="eval-thread-empty">
          <MessagesSquare size={24} />
          <p>{t("eval.workbench.thread.empty")}</p>
        </div>
      </section>
    );
  }

  return (
    <section className="eval-panel eval-thread-view" aria-label={t("eval.workbench.thread.title")}>
      <div className="eval-panel-heading">
        <div>
          <h2>{t("eval.workbench.thread.title")}</h2>
          <p>{t("eval.workbench.thread.description")}</p>
        </div>
        <Tag>{t("eval.workbench.thread.turns", { count: traces.length })}</Tag>
      </div>

      {threadQuery.isLoading || threadQuery.isFetching ? (
        <div className="eval-thread-loading">
          <Spin size="small" />
        </div>
      ) : null}
      {threadQuery.error ? (
        <Alert className="eval-thread-alert" type="warning" showIcon title={t("eval.workbench.thread.unavailable")} />
      ) : null}

      <div className="eval-thread-layout">
        <div className="eval-thread-turns" aria-label={t("eval.workbench.thread.turns", { count: traces.length })}>
          {traces.length > 0 ? (
            traces.map((trace) => {
              const turn = traceTurn(trace) || "-";
              return (
                <button
                  key={trace.trace_id}
                  type="button"
                  className={`eval-thread-turn ${trace.trace_id === selectedTraceId ? "is-selected" : ""}`}
                  onClick={() => onSelectTrace(trace.trace_id)}
                >
                  <span className={`eval-thread-status ${statusClass(trace.status)}`} />
                  <span>
                    <strong>{t("eval.list.turnShort", { turn })}</strong>
                    <small>{trace.request_id || trace.run_id || trace.trace_id}</small>
                  </span>
                  <Tag>{formatDuration(trace.total_latency_ms)}</Tag>
                </button>
              );
            })
          ) : (
            <Empty image={Empty.PRESENTED_IMAGE_SIMPLE} description={t("eval.list.empty")} />
          )}
        </div>

        <div className="eval-thread-detail">
          {selectedTrace ? (
            <>
              <div className="eval-thread-detail-heading">
                <div>
                  <h3>{selectedTrace.request_id || selectedTrace.trace_id}</h3>
                  <p>{formatDate(selectedTrace.started_at, i18n.language)}</p>
                </div>
                <Button
                  icon={<ArrowRight size={15} />}
                  onClick={() => {
                    onSelectTrace(selectedTrace.trace_id);
                    onOpenRun?.(selectedTrace.trace_id);
                  }}
                >
                  {t("eval.workbench.thread.openRun")}
                </Button>
              </div>
              <div className="eval-thread-keyvals" aria-label={t("eval.workbench.thread.metrics")}>
                {[
                  { key: "thread", label: t("eval.workbench.context.selectedThread"), value: threadId },
                  { key: "run", label: t("eval.workbench.context.selectedRun"), value: selectedTrace.run_id || "-" },
                  { key: "status", label: t("eval.list.columns.status"), value: selectedTrace.status },
                  { key: "model", label: t("eval.list.columns.model"), value: selectedTrace.model_id || "-" },
                ].map((item) => (
                  <div key={item.key} className="eval-thread-keyval">
                    <span>{item.label}</span>
                    <strong>{item.value}</strong>
                  </div>
                ))}
              </div>
              <div className="eval-thread-transcript">
                <h3>{t("eval.workbench.thread.transcript")}</h3>
                <div className="eval-locator-strip">
                  <span>{t("eval.detail.locator.turn")}: {locatorText(locator, "turn_index")}</span>
                  <span>{t("eval.detail.locator.message")}: {locatorText(locator, "message_index")}</span>
                  <span>{t("eval.detail.locator.history")}: {locatorText(locator, "history_message_count")}</span>
                  <span>{t("eval.detail.locator.fingerprint")}: {locatorText(locator, "transcript_fingerprint")}</span>
                </div>
                <div className="eval-preview-grid">
                  <div className="eval-preview-block">
                    <div className="eval-preview-label">{t("eval.detail.locator.currentMessage")}</div>
                    <p className="eval-preview-text">{locatorText(locator, "current_message_preview")}</p>
                  </div>
                  <div className="eval-preview-block">
                    <div className="eval-preview-label">{t("eval.detail.locator.excerpt")}</div>
                    <p className="eval-preview-text">{locatorText(locator, "transcript_excerpt")}</p>
                  </div>
                </div>
              </div>
            </>
          ) : (
            <Empty image={Empty.PRESENTED_IMAGE_SIMPLE} description={t("eval.workbench.thread.empty")} />
          )}
        </div>
      </div>
    </section>
  );
}
