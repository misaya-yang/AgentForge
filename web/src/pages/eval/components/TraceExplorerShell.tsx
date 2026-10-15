import { Alert, Button, Segmented, Space, Tag } from "antd";
import { Database, Download, Gauge, LayoutList, ListTree, MessagesSquare, TimerReset } from "lucide-react";
import type { Dispatch, SetStateAction } from "react";
import { useEffect, useMemo, useState } from "react";
import { useTranslation } from "react-i18next";

import type {
  AgentTraceDetailResponse,
  AgentTraceScoreCreate,
  AgentTraceSummary,
  EvalDashboardResponse,
  EvalTraceExportResponse,
  TraceFamily,
} from "@/api/eval";

import { AssistantTraceDetail } from "./AssistantTraceDetail";
import { AssistantTraceList, type AssistantTraceFilters } from "./AssistantTraceList";
import { ThreadView } from "./ThreadView";
import { TraceScorePanel } from "./TraceScorePanel";
import { formatDuration, formatTraceTokens, isErrorStatus, traceThreadId } from "./tracePresentation";

type TraceWorkspaceView = "explorer" | "thread" | "run";

interface TraceListCopy {
  title: string;
  empty: string;
  aria: string;
}

interface TraceExplorerShellProps {
  activeTraceFamily: TraceFamily;
  traceAccessKey: string;
  onTraceFamilyChange: (key: string) => void;
  familyCoverageMessage?: string | null;
  hasCapturedFamilyTraces: boolean;
  traces: AgentTraceSummary[];
  traceTotal: number;
  traceOffset: number;
  tracePageSize: number;
  onTracePageChange: (page: number) => void;
  filters: AssistantTraceFilters;
  setFilters: Dispatch<SetStateAction<AssistantTraceFilters>>;
  traceListCopy: TraceListCopy;
  selectedTraceId?: string;
  runFocusRevision?: number;
  onSelectTrace: (traceId: string | undefined) => void;
  onRefresh: () => void;
  tracesLoading: boolean;
  tracesError?: Error | null;
  detail?: AgentTraceDetailResponse;
  detailLoading: boolean;
  detailError?: Error | null;
  scoreError?: Error | null;
  scoreSubmitting: boolean;
  onScoreSubmit: (payload: AgentTraceScoreCreate) => Promise<void>;
  exportPreview?: EvalTraceExportResponse | null;
  exportLoading: boolean;
  onExport: () => void;
  activeDatasetName?: string | null;
  datasetActionLoading: boolean;
  onPromoteToGolden: () => void;
  onAddToReview: () => void;
  onCreateFailureCase: () => void;
  dashboard?: EvalDashboardResponse;
  readOnly?: boolean;
}

function percentile(values: number[], ratio: number) {
  if (values.length === 0) return 0;
  const sorted = [...values].sort((left, right) => left - right);
  const index = Math.min(sorted.length - 1, Math.max(0, Math.ceil(sorted.length * ratio) - 1));
  return sorted[index] || 0;
}

export function TraceExplorerShell({
  activeTraceFamily,
  traceAccessKey,
  onTraceFamilyChange,
  familyCoverageMessage,
  hasCapturedFamilyTraces,
  traces,
  traceTotal,
  traceOffset,
  tracePageSize,
  onTracePageChange,
  filters,
  setFilters,
  traceListCopy,
  selectedTraceId,
  runFocusRevision = 0,
  onSelectTrace,
  onRefresh,
  tracesLoading,
  tracesError,
  detail,
  detailLoading,
  detailError,
  scoreError,
  scoreSubmitting,
  onScoreSubmit,
  exportPreview,
  exportLoading,
  onExport,
  activeDatasetName,
  datasetActionLoading,
  onPromoteToGolden,
  onAddToReview,
  onCreateFailureCase,
  dashboard,
  readOnly = false,
}: TraceExplorerShellProps) {
  const { t } = useTranslation();
  const [workspaceView, setWorkspaceView] = useState<TraceWorkspaceView>("explorer");
  useEffect(() => {
    setWorkspaceView("explorer");
  }, [activeTraceFamily]);
  useEffect(() => {
    if (runFocusRevision > 0) setWorkspaceView("run");
  }, [runFocusRevision]);
  const selectedTrace = detail?.trace || traces.find((trace) => trace.trace_id === selectedTraceId) || null;
  const selectedThreadId = traceThreadId(selectedTrace);
  const threadFallbackTraces = useMemo(
    () => traces.filter((trace) => traceThreadId(trace) === selectedThreadId),
    [selectedThreadId, traces]
  );
  const latencies = traces.map((trace) => trace.total_latency_ms).filter((value) => Number.isFinite(value));
  const errorRate = traces.length === 0
    ? 0
    : Math.round((traces.filter((trace) => isErrorStatus(trace.status)).length / traces.length) * 100);
  const contextCards = [
    { key: "family", label: t("eval.workbench.context.family"), value: activeTraceFamily },
    { key: "count", label: t("eval.workbench.context.traceCount"), value: `${traces.length}/${traceTotal}` },
    { key: "error", label: t("eval.workbench.context.errorRate"), value: `${errorRate}%` },
    { key: "p50", label: t("eval.workbench.context.p50Latency"), value: formatDuration(percentile(latencies, 0.5)) },
    { key: "p99", label: t("eval.workbench.context.p99Latency"), value: formatDuration(percentile(latencies, 0.99)) },
    { key: "thread", label: t("eval.workbench.context.selectedThread"), value: selectedThreadId || t("eval.workbench.context.noSelection") },
    { key: "run", label: t("eval.workbench.context.selectedRun"), value: selectedTrace?.run_id || t("eval.workbench.context.noSelection") },
    { key: "retention", label: t("eval.workbench.context.retention"), value: t("eval.workbench.context.last7Days") },
  ];
  const selectedRunMetrics = selectedTrace
    ? [
        { key: "status", label: t("eval.list.columns.status"), value: selectedTrace.status },
        { key: "thread", label: t("eval.workbench.context.selectedThread"), value: selectedThreadId || "-" },
        { key: "latency", label: t("eval.list.columns.latency"), value: formatDuration(selectedTrace.total_latency_ms) },
        { key: "tokens", label: t("eval.list.columns.tokens"), value: formatTraceTokens(selectedTrace) },
        { key: "scores", label: t("eval.list.columns.scores"), value: selectedTrace.scores_count.toLocaleString() },
      ]
    : [];

  const detailPanel = (
    <AssistantTraceDetail
      detail={detail}
      loading={detailLoading}
      error={detailError}
    />
  );
  const inspectorPanel = (
    <TraceScorePanel
      traceId={selectedTraceId}
      trace={selectedTrace}
      detail={detail}
      scores={detail?.scores || []}
      loading={detailLoading}
      submitting={scoreSubmitting}
      error={scoreError}
      exportPreview={exportPreview}
      exportLoading={exportLoading}
      datasetActionLoading={datasetActionLoading}
      activeDatasetName={activeDatasetName}
      onSubmit={onScoreSubmit}
      onExport={onExport}
      onPromoteToGolden={onPromoteToGolden}
      onAddToReview={onAddToReview}
      onCreateFailureCase={onCreateFailureCase}
      readOnly={readOnly}
    />
  );

  return (
    <section className="eval-trace-shell">
      <div className="eval-context-bar">
        <div className="eval-context-title">
          <Gauge size={16} />
          <span>{t("eval.workbench.traceExplorer")}</span>
        </div>
        <div className="eval-context-metrics">
          {contextCards.map((card) => (
            <div className="eval-context-card" key={card.key}>
              <span>{card.label}</span>
              <strong>{card.value}</strong>
            </div>
          ))}
        </div>
        <Space size={8} wrap className="eval-context-actions">
          <Button icon={<Download size={15} />} onClick={onExport} loading={exportLoading} disabled={!selectedTraceId}>
            {t("eval.workbench.exportOpenInference")}
          </Button>
          <Button icon={<Database size={15} />} disabled={readOnly || !selectedTraceId || !activeDatasetName} onClick={onPromoteToGolden}>
            {t("eval.workbench.promoteToGolden")}
          </Button>
        </Space>
      </div>

      <div className="eval-trace-toolbar">
        <Segmented
          value={activeTraceFamily}
          onChange={(value) => onTraceFamilyChange(String(value))}
          options={[
            { label: t("eval.tabs.assistant"), value: "assistant" },
            { label: t("eval.tabs.langgraphProxy"), value: "langgraph_proxy" },
            { label: t("eval.tabs.rag"), value: "rag" },
          ]}
        />
        <Segmented
          value={workspaceView}
          onChange={(value) => setWorkspaceView(value as TraceWorkspaceView)}
          options={[
            { label: <span className="eval-segment-label"><LayoutList size={14} />{t("eval.workbench.views.explorer")}</span>, value: "explorer" },
            { label: <span className="eval-segment-label"><MessagesSquare size={14} />{t("eval.workbench.views.thread")}</span>, value: "thread" },
            { label: <span className="eval-segment-label"><ListTree size={14} />{t("eval.workbench.views.run")}</span>, value: "run" },
          ]}
        />
        <Tag className="eval-retention-tag" icon={<TimerReset size={13} />}>
          {String(dashboard?.latest_gate_status?.status || "") || t("eval.workbench.context.last7Days")}
        </Tag>
      </div>

      {familyCoverageMessage ? (
        <Alert
          className="eval-family-coverage-note"
          type={hasCapturedFamilyTraces ? "success" : "warning"}
          showIcon
          title={familyCoverageMessage}
        />
      ) : null}

      {workspaceView === "explorer" ? (
        <div className="eval-table-workspace">
          <AssistantTraceList
            traces={traces}
            total={traceTotal}
            offset={traceOffset}
            pageSize={tracePageSize}
            onPageChange={onTracePageChange}
            filters={filters}
            setFilters={setFilters}
            title={traceListCopy.title}
            ariaLabel={traceListCopy.aria}
            emptyText={traceListCopy.empty}
            selectedTraceId={selectedTraceId}
            loading={tracesLoading}
            error={tracesError}
            onSelect={onSelectTrace}
            onRefresh={onRefresh}
          />
          <div className="eval-selected-run-strip" aria-live="polite">
            <div className="eval-selected-run-title">
              <span>{t("eval.workbench.activeSelection")}</span>
              <strong>
                {selectedTrace?.request_id || selectedTrace?.run_id || selectedTraceId || t("eval.workbench.noTraceSelected")}
              </strong>
            </div>
            {selectedRunMetrics.length > 0 ? (
              <div className="eval-run-strip-metrics">
                {selectedRunMetrics.map((metric) => (
                  <span key={metric.key}>
                    {metric.label}
                    <strong>{metric.value}</strong>
                  </span>
                ))}
              </div>
            ) : null}
            <Space size={8} wrap className="eval-selected-run-actions">
              <Button
                icon={<MessagesSquare size={15} />}
                disabled={!selectedThreadId}
                onClick={() => setWorkspaceView("thread")}
              >
                {t("eval.workbench.openThreadView")}
              </Button>
              <Button
                type="primary"
                icon={<ListTree size={15} />}
                disabled={!selectedTraceId}
                onClick={() => setWorkspaceView("run")}
              >
                {t("eval.workbench.openRunDetail")}
              </Button>
            </Space>
          </div>
        </div>
      ) : null}

      {workspaceView === "thread" ? (
        <div className="eval-thread-workspace">
          <ThreadView
            traceAccessKey={traceAccessKey}
            threadId={selectedThreadId}
            fallbackTraces={threadFallbackTraces}
            selectedTraceId={selectedTraceId}
            onSelectTrace={onSelectTrace}
            onOpenRun={() => setWorkspaceView("run")}
          />
        </div>
      ) : null}

      {workspaceView === "run" ? (
        <div className="eval-run-workspace">
          {detailPanel}
          {inspectorPanel}
        </div>
      ) : null}
    </section>
  );
}
