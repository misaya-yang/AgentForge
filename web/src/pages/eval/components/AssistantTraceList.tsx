import { Button, DatePicker, Empty, Input, InputNumber, Select, Space, Table, Tag, Tooltip } from "antd";
import type { ColumnsType } from "antd/es/table";
import { Activity, Filter, RefreshCw, Search } from "lucide-react";
import type { Dispatch, SetStateAction } from "react";
import { useTranslation } from "react-i18next";

import type { AgentTraceSummary, TraceStatus } from "@/api/eval";

import { formatDate, formatDuration, formatTraceTokens, traceThreadId, traceTurn } from "./tracePresentation";

const { RangePicker } = DatePicker;

export type ScoreStatusFilter = "all" | "scored" | "unscored";

export interface AssistantTraceFilters {
  status?: TraceStatus | "all";
  model_id?: string;
  user_id?: string;
  session_id?: string;
  run_id?: string;
  request_id?: string;
  transcript_query?: string;
  turn_index?: number;
  span_kind?: string;
  score_name?: string;
  min_score?: number;
  max_score?: number;
  min_latency_ms?: number;
  max_latency_ms?: number;
  score_status?: ScoreStatusFilter;
  start_date?: string;
  end_date?: string;
}

interface AssistantTraceListProps {
  traces: AgentTraceSummary[];
  total: number;
  offset: number;
  pageSize: number;
  onPageChange: (page: number) => void;
  filters: AssistantTraceFilters;
  setFilters: Dispatch<SetStateAction<AssistantTraceFilters>>;
  title?: string;
  ariaLabel?: string;
  emptyText?: string;
  selectedTraceId?: string;
  loading: boolean;
  error?: Error | null;
  onSelect: (traceId: string) => void;
  onRefresh: () => void;
}

function statusColor(status: TraceStatus) {
  if (status === "succeeded") return "success";
  if (status === "failed" || status === "timeout") return "error";
  if (status === "cancelled") return "warning";
  return "processing";
}

export function AssistantTraceList({
  traces,
  total,
  offset,
  pageSize,
  onPageChange,
  filters,
  setFilters,
  title,
  ariaLabel,
  emptyText,
  selectedTraceId,
  loading,
  error,
  onSelect,
  onRefresh,
}: AssistantTraceListProps) {
  const { t, i18n } = useTranslation();
  const updateFilter = <K extends keyof AssistantTraceFilters>(
    key: K,
    value: AssistantTraceFilters[K]
  ) => {
    setFilters((current) => ({ ...current, [key]: value }));
  };

  const clearFilters = () => setFilters({ status: "all", score_status: "all" });

  const columns: ColumnsType<AgentTraceSummary> = [
    {
      title: t("eval.list.columns.trace"),
      dataIndex: "trace_id",
      key: "trace",
      width: 260,
      fixed: "left",
      render: (traceId, item) => {
        const turn = traceTurn(item);
        return (
          <div className="eval-trace-id-cell">
            <div className="eval-trace-id-line">{traceId}</div>
            <div className="eval-trace-subline">
              {turn ? t("eval.list.turnShort", { turn }) + " · " : ""}
              {item.session_id || "-"} · {item.run_id || "-"}
            </div>
          </div>
        );
      },
    },
    {
      title: t("eval.list.columns.status"),
      dataIndex: "status",
      key: "status",
      width: 116,
      render: (status: TraceStatus, item) => (
        <Space size={6} wrap>
          <Tag color={statusColor(status)} className="eval-status-tag">
            {t(`eval.status.${status}`, status)}
          </Tag>
          {item.scores_count > 0 ? <Tag className="eval-status-tag">{t("eval.score.scored")}</Tag> : null}
        </Space>
      ),
    },
    {
      title: t("eval.list.columns.thread"),
      key: "thread",
      width: 150,
      render: (_, item) => (
        <span className="eval-trace-subline eval-trace-thread-cell">
          {traceThreadId(item) || "-"}
        </span>
      ),
    },
    {
      title: t("eval.list.columns.turn"),
      key: "turn",
      width: 86,
      render: (_, item) => traceTurn(item) || "-",
    },
    {
      title: t("eval.list.columns.model"),
      key: "model",
      width: 170,
      render: (_, item) => (
        <div className="eval-trace-id-cell">
          <div className="eval-trace-model">{item.model_id || "-"}</div>
          <div className="eval-trace-subline">{item.provider || "-"}</div>
        </div>
      ),
    },
    {
      title: t("eval.list.columns.latency"),
      key: "latency",
      width: 128,
      render: (_, item) => (
        <div>
          <div className="eval-trace-metric">{formatDuration(item.total_latency_ms)}</div>
          <div className="eval-trace-subline">
            {t("eval.list.firstTokenShort", { duration: formatDuration(item.first_token_latency_ms) })}
          </div>
        </div>
      ),
    },
    {
      title: t("eval.list.columns.tokens"),
      dataIndex: "total_tokens",
      key: "tokens",
      width: 110,
      render: (_, item) => <span className="eval-trace-metric">{formatTraceTokens(item)}</span>,
    },
    {
      title: t("eval.list.columns.scores"),
      dataIndex: "scores_count",
      key: "scores",
      width: 92,
      render: (count: number) => (
        <Tag className="eval-status-tag" color={count > 0 ? "blue" : undefined}>
          {count.toLocaleString()}
        </Tag>
      ),
    },
    {
      title: t("eval.list.columns.user"),
      dataIndex: "user_id",
      key: "user",
      width: 150,
      ellipsis: true,
    },
    {
      title: t("eval.list.columns.started"),
      dataIndex: "started_at",
      key: "started",
      width: 168,
      render: (value) => formatDate(value, i18n.language),
    },
  ];

  return (
    <section className="eval-panel eval-trace-list" aria-label={ariaLabel || t("eval.list.ariaLabel")}>
      <div className="eval-panel-heading">
        <div>
          <h2>{title || t("eval.list.title")}</h2>
          <p>
            {t("eval.list.counts", {
              total: total.toLocaleString(),
              visible: traces.length.toLocaleString(),
            })}
          </p>
        </div>
        <Space size={8}>
          <Tooltip title={t("eval.list.resetFilters")}>
            <Button icon={<Filter size={15} />} onClick={clearFilters}>
              {t("common.reset")}
            </Button>
          </Tooltip>
          <Tooltip title={t("eval.list.refreshTraces")}>
            <Button icon={<RefreshCw size={15} />} onClick={onRefresh} loading={loading}>
              {t("common.refresh")}
            </Button>
          </Tooltip>
        </Space>
      </div>

      <div className="eval-filter-stack" aria-label="Trace filters">
        <div className="eval-primary-filter-row">
          <Input
            className="eval-transcript-filter"
            aria-label={t("eval.filters.transcript")}
            prefix={<Search size={14} />}
            placeholder={t("eval.filters.transcript")}
            value={filters.transcript_query}
            onChange={(event) => updateFilter("transcript_query", event.target.value)}
            allowClear
          />
          <Select
            aria-label={t("eval.filters.status")}
            value={filters.status || "all"}
            onChange={(value) => updateFilter("status", value as TraceStatus | "all")}
            options={[
              { label: t("eval.filters.allStatuses"), value: "all" },
              { label: t("eval.status.succeeded"), value: "succeeded" },
              { label: t("eval.status.running"), value: "running" },
              { label: t("eval.status.failed"), value: "failed" },
              { label: t("eval.status.cancelled"), value: "cancelled" },
              { label: t("eval.status.timeout"), value: "timeout" },
            ]}
          />
          <Input
            aria-label={t("eval.filters.model")}
            prefix={<Search size={14} />}
            placeholder={t("eval.filters.model")}
            value={filters.model_id}
            onChange={(event) => updateFilter("model_id", event.target.value)}
            allowClear
          />
        </div>
        <details className="eval-advanced-filters">
          <summary>
            <Filter size={14} />
            <span>{t("eval.filters.advanced")}</span>
          </summary>
          <div className="eval-filter-grid">
            <Input
              aria-label={t("eval.filters.user")}
              placeholder={t("eval.filters.user")}
              value={filters.user_id}
              onChange={(event) => updateFilter("user_id", event.target.value)}
              allowClear
            />
            <Input
              aria-label={t("eval.filters.session")}
              placeholder={t("eval.filters.session")}
              value={filters.session_id}
              onChange={(event) => updateFilter("session_id", event.target.value)}
              allowClear
            />
            <Input
              aria-label={t("eval.filters.run")}
              placeholder={t("eval.filters.run")}
              value={filters.run_id}
              onChange={(event) => updateFilter("run_id", event.target.value)}
              allowClear
            />
            <Input
              aria-label={t("eval.filters.request")}
              placeholder={t("eval.filters.request")}
              value={filters.request_id}
              onChange={(event) => updateFilter("request_id", event.target.value)}
              allowClear
            />
            <InputNumber
              aria-label={t("eval.filters.turn")}
              placeholder={t("eval.filters.turn")}
              min={1}
              precision={0}
              value={filters.turn_index}
              onChange={(value) =>
                updateFilter("turn_index", typeof value === "number" ? value : undefined)
              }
              style={{ width: "100%" }}
            />
            <Input
              aria-label={t("eval.filters.spanKind")}
              placeholder={t("eval.filters.spanKind")}
              value={filters.span_kind}
              onChange={(event) => updateFilter("span_kind", event.target.value)}
              allowClear
            />
            <Input
              aria-label={t("eval.filters.scoreName")}
              placeholder={t("eval.filters.scoreName")}
              value={filters.score_name}
              onChange={(event) => updateFilter("score_name", event.target.value)}
              allowClear
            />
            <InputNumber
              aria-label={t("eval.filters.minScore")}
              placeholder={t("eval.filters.minScore")}
              min={0}
              max={1}
              step={0.05}
              value={filters.min_score}
              onChange={(value) => updateFilter("min_score", typeof value === "number" ? value : undefined)}
              style={{ width: "100%" }}
            />
            <InputNumber
              aria-label={t("eval.filters.maxScore")}
              placeholder={t("eval.filters.maxScore")}
              min={0}
              max={1}
              step={0.05}
              value={filters.max_score}
              onChange={(value) => updateFilter("max_score", typeof value === "number" ? value : undefined)}
              style={{ width: "100%" }}
            />
            <InputNumber
              aria-label={t("eval.filters.minLatency")}
              placeholder={t("eval.filters.minLatency")}
              min={0}
              precision={0}
              value={filters.min_latency_ms}
              onChange={(value) => updateFilter("min_latency_ms", typeof value === "number" ? value : undefined)}
              style={{ width: "100%" }}
            />
            <InputNumber
              aria-label={t("eval.filters.maxLatency")}
              placeholder={t("eval.filters.maxLatency")}
              min={0}
              precision={0}
              value={filters.max_latency_ms}
              onChange={(value) => updateFilter("max_latency_ms", typeof value === "number" ? value : undefined)}
              style={{ width: "100%" }}
            />
            <Select
              aria-label={t("eval.filters.scoreStatus")}
              value={filters.score_status || "all"}
              onChange={(value) => updateFilter("score_status", value as ScoreStatusFilter)}
              options={[
                { label: t("eval.filters.allScoreStates"), value: "all" },
                { label: t("eval.score.scored"), value: "scored" },
                { label: t("eval.score.unscored"), value: "unscored" },
              ]}
            />
            <RangePicker
              aria-label={t("eval.filters.dateRange")}
              className="eval-date-range"
              placeholder={[t("eval.filters.startDate"), t("eval.filters.endDate")]}
              onChange={(_dates, dateStrings) => {
                updateFilter("start_date", dateStrings[0] || undefined);
                updateFilter("end_date", dateStrings[1] || undefined);
              }}
            />
          </div>
        </details>
      </div>

      {error ? (
        <div className="eval-state-panel" role="alert">
          <Activity size={18} />
          <div>
            <strong>{t("eval.list.unavailableTitle")}</strong>
            <span>{error.message || t("eval.list.unavailableDescription")}</span>
          </div>
        </div>
      ) : (
        <Table
          className="eval-trace-table"
          columns={columns}
          dataSource={traces}
          loading={loading}
          rowKey="trace_id"
          size="small"
          pagination={{
            current: Math.floor(offset / pageSize) + 1,
            pageSize,
            total,
            showSizeChanger: false,
            size: "small",
            hideOnSinglePage: true,
            onChange: onPageChange,
          }}
          scroll={{ x: 1320, y: 420 }}
          locale={{
            emptyText: (
              <Empty
                image={Empty.PRESENTED_IMAGE_SIMPLE}
                description={emptyText || t("eval.list.empty")}
              />
            ),
          }}
          rowClassName={(trace) => (trace.trace_id === selectedTraceId ? "eval-trace-row-selected" : "")}
          onRow={(trace) => ({
            onClick: () => onSelect(trace.trace_id),
            onKeyDown: (event) => {
              if (event.key === "Enter" || event.key === " ") {
                event.preventDefault();
                onSelect(trace.trace_id);
              }
            },
            tabIndex: 0,
            "aria-label": t("eval.list.openTrace", {
              traceId: trace.trace_id,
              turn: traceTurn(trace) || "-",
            }),
          })}
        />
      )}
    </section>
  );
}
