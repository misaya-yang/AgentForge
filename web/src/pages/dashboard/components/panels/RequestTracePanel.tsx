import { TaskTraceLookup } from "@/pages/tasks/components/TaskTraceLookup";
import { Input, Empty, Tag, Spin, Button, Table, Segmented } from "antd";
import {
  SearchOutlined,
  ReloadOutlined,
  ClockCircleOutlined,
  CheckCircleOutlined,
  ExclamationCircleOutlined,
  CloseCircleOutlined,
} from "@ant-design/icons";
import { useState, useCallback, useLayoutEffect, useRef } from "react";
import { useQuery } from "@tanstack/react-query";
import { PanelWrapper } from "../PanelWrapper";
import { useDashboardContext } from "../../DashboardContext";
import { useAppStore } from "@/store/useAppStore";
import { getColors } from "../../styles";
import { getRequestTrace, listRequestTraces, type RequestTrace } from "@/api/usage";
import { useTranslation } from "react-i18next";
import { useDashboardEntityLabels } from "../../hooks/useDashboardEntityLabels";

import { useLocation, useSearchParams } from "react-router-dom";
import { failureTraceReturn, writeTraceFilter } from "../../failureTraceNavigation";
import { useAuthenticatedReadScope } from "@/hooks/useAuthenticatedReadScope";

type TraceTab = "all" | "error" | "slow" | "sampled";

interface TraceSpan {
  name: string;
  duration_ms: number;
  status: "success" | "error" | "pending";
  start_offset_ms: number;
}

function formatDuration(ms: number): string {
  if (ms >= 1000) return `${(ms / 1000).toFixed(2)}s`;
  return `${ms}ms`;
}

function normalizeTraceSpans(trace: RequestTrace): TraceSpan[] {
  const raw = Array.isArray(trace.trace_steps) ? trace.trace_steps : [];
  const spans = raw
    .map((step) => ({
      name: step.name || "unknown_step",
      duration_ms: Math.max(Number(step.duration_ms || 0), 0),
      status: (step.status || "success") as "success" | "error" | "pending",
      start_offset_ms: Math.max(Number(step.start_offset_ms || 0), 0),
    }))
    .sort((a, b) => a.start_offset_ms - b.start_offset_ms);

  if (spans.length > 0) {
    return spans;
  }

  return [
    {
      name: "gateway.request_total",
      duration_ms: trace.request_total_duration_ms || 0,
      start_offset_ms: 0,
      status: trace.status,
    },
  ];
}

function TraceTimeline({ trace }: { trace: RequestTrace }) {
  const { t } = useTranslation();
  const { darkMode } = useAppStore();
  const colors = getColors(darkMode);
  const { resolveServiceLabel, resolveUserLabel } = useDashboardEntityLabels();
  const spans = normalizeTraceSpans(trace);
  const maxDuration = Math.max(trace.request_total_duration_ms || 0, 1);

  return (
    <div style={{ minWidth: 0 }}>
      <div
        style={{
          display: "flex",
          justifyContent: "space-between",
          alignItems: "flex-start",
          marginBottom: 16,
          padding: "12px 16px",
          background: colors.innerBg,
          borderRadius: 8,
          border: `1px solid ${colors.border}`,
          gap: 12,
          flexWrap: "wrap",
          minWidth: 0,
        }}
      >
        <div style={{ minWidth: 0, flex: "1 1 260px" }}>
          <div
            style={{
              fontSize: 13,
              fontWeight: 600,
              color: colors.textPrimary,
              overflowWrap: "anywhere",
            }}
          >
            {trace.request_id}
          </div>
          <div style={{ fontSize: 11, color: colors.textMuted, marginTop: 2 }}>
            {trace.timestamp ? new Date(trace.timestamp).toLocaleString() : "-"}
          </div>
        </div>
        <div style={{ display: "flex", gap: 8, alignItems: "center", flexWrap: "wrap", justifyContent: "flex-end", minWidth: 0 }}>
          <Tag color={trace.status === "success" ? "success" : "error"}>
            {trace.status === "success" ? t("dashboard.requestTrace.status.success") : t("dashboard.requestTrace.status.error")}
          </Tag>
          <Tag>{trace.model}</Tag>
          <Tag color="blue">{trace.provider}</Tag>
          <Tag color="purple">{resolveServiceLabel(trace.service_id)}</Tag>
          <Tag color="cyan">{resolveUserLabel(trace.user_id)}</Tag>
          {trace.error_type ? <Tag color="red">{trace.error_type}</Tag> : null}
        </div>
      </div>

      <div
        style={{
          display: "grid",
          gridTemplateColumns: "repeat(auto-fit, minmax(112px, 1fr))",
          gap: 8,
          marginBottom: 16,
        }}
      >
        <div style={{ padding: 8, border: `1px solid ${colors.border}`, borderRadius: 6, background: colors.innerBg }}>
          <div style={{ fontSize: 11, color: colors.textMuted }}>TTFB</div>
          <div style={{ fontSize: 14, fontWeight: 700, color: colors.info }}>{formatDuration(trace.first_token_latency_ms || 0)}</div>
        </div>
        <div style={{ padding: 8, border: `1px solid ${colors.border}`, borderRadius: 6, background: colors.innerBg }}>
          <div style={{ fontSize: 11, color: colors.textMuted }}>LLM</div>
          <div style={{ fontSize: 14, fontWeight: 700, color: colors.gold }}>{formatDuration(trace.llm_inference_duration_ms || 0)}</div>
        </div>
        <div style={{ padding: 8, border: `1px solid ${colors.border}`, borderRadius: 6, background: colors.innerBg }}>
          <div style={{ fontSize: 11, color: colors.textMuted }}>Retrieval</div>
          <div style={{ fontSize: 14, fontWeight: 700, color: colors.success }}>{formatDuration(trace.retrieval_duration_ms || 0)}</div>
        </div>
        <div style={{ padding: 8, border: `1px solid ${colors.border}`, borderRadius: 6, background: colors.innerBg }}>
          <div style={{ fontSize: 11, color: colors.textMuted }}>Tool</div>
          <div style={{ fontSize: 14, fontWeight: 700, color: colors.warning }}>{formatDuration(trace.tool_call_duration_ms || 0)}</div>
        </div>
      </div>

      <div
        style={{
          position: "relative",
          height: 24,
          background: colors.innerBg,
          borderRadius: 4,
          marginBottom: 16,
          overflow: "hidden",
          border: `1px solid ${colors.border}`,
        }}
      >
        {spans.map((span, index) => {
          const left = (span.start_offset_ms / maxDuration) * 100;
          const width = (span.duration_ms / maxDuration) * 100;
          const bgColor =
            span.status === "success"
                ? colors.success
                : span.status === "error"
                ? colors.error
                : colors.warning;

          return (
            <div
              key={`${span.name}-${index}`}
              title={`${span.name}: ${formatDuration(span.duration_ms)}`}
              style={{
                position: "absolute",
                left: `${left}%`,
                width: `${Math.max(width, 1)}%`,
                height: "100%",
                background: bgColor,
                borderRight: "1px solid rgba(255,255,255,0.3)",
              }}
            />
          );
        })}
      </div>

      <div style={{ display: "flex", flexDirection: "column", gap: 8 }}>
        {spans.map((span, index) => (
          <div
            key={`${span.name}-${index}`}
            style={{
              display: "flex",
              alignItems: "center",
              justifyContent: "space-between",
              padding: "8px 12px",
              background: colors.innerBg,
              borderRadius: 6,
              border: `1px solid ${colors.border}`,
              gap: 12,
              minWidth: 0,
            }}
          >
            <div style={{ display: "flex", alignItems: "center", gap: 8, minWidth: 0 }}>
              {span.status === "success" ? (
                <CheckCircleOutlined style={{ color: colors.success, fontSize: 14 }} />
              ) : span.status === "error" ? (
                <ExclamationCircleOutlined style={{ color: colors.error, fontSize: 14 }} />
              ) : (
                <ClockCircleOutlined style={{ color: colors.warning, fontSize: 14 }} />
              )}
              <span style={{ fontSize: 12, color: colors.textPrimary, overflow: "hidden", textOverflow: "ellipsis", whiteSpace: "nowrap" }}>{span.name}</span>
            </div>
            <div style={{ display: "flex", alignItems: "center", gap: 12, flexShrink: 0 }}>
              <span style={{ fontSize: 11, color: colors.textMuted }}>+{formatDuration(span.start_offset_ms)}</span>
              <span
                style={{
                  fontSize: 12,
                  fontWeight: 600,
                  color: span.duration_ms > 100 ? colors.warning : colors.textSecondary,
                  minWidth: 60,
                  textAlign: "right",
                }}
              >
                {formatDuration(span.duration_ms)}
              </span>
            </div>
          </div>
        ))}
      </div>

      <div
        style={{
          marginTop: 12,
          padding: "8px 12px",
          background: colors.infoBg,
          borderRadius: 6,
          border: `1px solid ${colors.info}44`,
          display: "flex",
          justifyContent: "space-between",
          alignItems: "center",
          gap: 12,
          flexWrap: "wrap",
        }}
      >
        <span style={{ fontSize: 12, color: colors.textSecondary }}>{t("dashboard.requestTrace.totalDuration")}</span>
        <span style={{ fontSize: 14, fontWeight: 700, color: colors.info }}>
          {formatDuration(trace.request_total_duration_ms || 0)}
        </span>
      </div>

      <div
        style={{
          marginTop: 8,
          display: "grid",
          gridTemplateColumns: "repeat(auto-fit, minmax(120px, 1fr))",
          gap: 8,
        }}
      >
        <div style={{ padding: 8, border: `1px solid ${colors.border}`, borderRadius: 6, background: colors.innerBg }}>
          <div style={{ fontSize: 11, color: colors.textMuted }}>{t("metrics.totalTokens", "Token 消耗")}</div>
          <div style={{ fontSize: 14, fontWeight: 700, color: colors.textPrimary }}>
            {(trace.input_tokens + trace.output_tokens).toLocaleString()}
          </div>
        </div>
        <div style={{ padding: 8, border: `1px solid ${colors.border}`, borderRadius: 6, background: colors.innerBg }}>
          <div style={{ fontSize: 11, color: colors.textMuted }}>{t("analytics.cost", "成本")}</div>
          <div style={{ fontSize: 14, fontWeight: 700, color: colors.success }}>
            ${trace.total_cost_usd >= 1 ? trace.total_cost_usd.toFixed(2) : trace.total_cost_usd.toFixed(4)}
          </div>
        </div>
        <div style={{ padding: 8, border: `1px solid ${colors.border}`, borderRadius: 6, background: colors.innerBg }}>
          <div style={{ fontSize: 11, color: colors.textMuted }}>trace_id</div>
          <div style={{ fontSize: 12, fontWeight: 650, color: colors.textPrimary, overflow: "hidden", textOverflow: "ellipsis", whiteSpace: "nowrap" }}>
            {trace.trace_id || "-"}
          </div>
        </div>
      </div>
    </div>
  );
}

export function RequestTracePanel() {
  const scope = useAuthenticatedReadScope();
  const location = useLocation();
  const { dateRange, serviceId, userId, traceFilter } = useDashboardContext();
  const exactLookup = new URLSearchParams(location.search).has("request_id");
  const navigation = JSON.stringify([dateRange, serviceId, userId, traceFilter]);
  return scope.isCurrent() ? exactLookup
    ? <TaskTraceLookup key={`${scope.id}:${location.key}:${location.search}`} isCurrent={scope.isCurrent} renderTrace={trace => <TraceTimeline trace={trace} />} />
    : <RequestTraceContent key={`${scope.id}:${location.key}:${navigation}`} scope={scope} /> : null;
}

function RequestTraceContent({ scope }: { scope: ReturnType<typeof useAuthenticatedReadScope> }) {
  const { t } = useTranslation();
  const { darkMode } = useAppStore();
  const colors = getColors(darkMode);
  const { dateRange, serviceId, userId, lastRefresh, traceFilter, clearTraceFilter } = useDashboardContext();
  const { resolveServiceLabel, resolveUserLabel } = useDashboardEntityLabels();
  const [searchId, setSearchId] = useState("");
  const [loading, setLoading] = useState(false);
  const [trace, setTrace] = useState<RequestTrace | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [activeTab, setActiveTab] = useState<TraceTab>(traceFilter.status === "error" ? "error" : traceFilter.sample_reason === "slow_request" ? "slow" : traceFilter.sample_reason === "baseline_sample" ? "sampled" : "all");
  const [params, setParams] = useSearchParams();
  const drilldown = Boolean(params.get("trace_origin"));
  const owner = useRef({ mounted: true, request: 0, controller: null as AbortController | null });
  const currentData = useRef<unknown>(null);
  const traceListOwner = useRef<unknown>(null);
  useLayoutEffect(() => {
    const lifetime = owner.current; lifetime.mounted = true;
    return () => { lifetime.mounted = false; lifetime.request += 1; lifetime.controller?.abort(); };
  }, []);
  const isCurrent = useCallback(() => owner.current.mounted && scope.isCurrent(), [scope]);
  const invalidateDetail = useCallback(() => {
    owner.current.request += 1; owner.current.controller?.abort();
    setTrace(null); setError(null); setLoading(false);
  }, []);

  // 根据 tab 和 traceFilter 构建查询参数
  const getQueryParams = () => {
    const params: Parameters<typeof listRequestTraces>[0] = {
      start_date: dateRange[0],
      end_date: dateRange[1],
      service_id: serviceId !== "all" ? serviceId : undefined,
      user_id: userId !== "all" ? userId : undefined,
      limit: 15,
    };
    if (activeTab === "error") {
      params.status = "error";
    }
    // Apply any traceFilter overrides
    if (traceFilter.error_type) {
      params.error_type = traceFilter.error_type;
    }
    if (traceFilter.service_id) {
      params.service_id = traceFilter.service_id;
    }
    if (traceFilter.model) params.model = traceFilter.model;
    if (traceFilter.provider) params.provider = traceFilter.provider;
    return params;
  };

  const recentQuery = useQuery({
    queryKey: ["dashboard-request-trace-list", scope.id, dateRange, serviceId, userId, activeTab, traceFilter, lastRefresh.getTime()],
    queryFn: async ({ signal }) => {
      if (!isCurrent()) throw new Error("Dashboard scope changed");
      const result = await listRequestTraces(getQueryParams(), signal);
      if (!isCurrent()) throw new Error("Dashboard scope changed");
      return result;
    },
    retry: false,
    staleTime: 30000,
  });

  currentData.current = recentQuery.isError ? null : recentQuery.data;

  // 对 slow / sampled 做客户端过滤 (API 不直接支持 sample_reason 查询)
  const filteredTraces = (() => {
    const raw = isCurrent() && !recentQuery.isError ? recentQuery.data || [] : [];
    if (activeTab === "slow") {
      return raw.filter((t) => t.sample_reason === "slow_request" || t.request_total_duration_ms > 5000);
    }
    if (activeTab === "sampled") {
      return raw.filter((t) => t.sample_reason === "baseline_sample");
    }
    return raw;
  })();

  const handleSearch = useCallback(async () => {
    const reqId = searchId.trim();
    if (!reqId || !isCurrent() || drilldown) return;
    invalidateDetail();
    const request = owner.current.request;
    const controller = new AbortController(); owner.current.controller = controller;
    const current = () => isCurrent() && request === owner.current.request;
    setLoading(true);
    try {
      const result = await getRequestTrace(reqId, controller.signal);
      if (current()) { traceListOwner.current = null; setTrace(result); setError(null); }
    } catch {
      if (current()) { setTrace(null); setError(t("dashboard.requestTrace.notFound")); }
    } finally { if (current()) setLoading(false); }
  }, [searchId, t, isCurrent, drilldown, invalidateDetail]);

  const handleReset = useCallback(() => {
    if (!isCurrent()) return;
    invalidateDetail(); setSearchId("");
  }, [isCurrent, invalidateDetail]);

  const hasActiveFilter = Object.values(traceFilter).some(Boolean);
  const displayTrace = isCurrent() && (traceListOwner.current === null || (!recentQuery.isError && traceListOwner.current === recentQuery.data)) ? trace : null;
  const selectedTrace = isCurrent() ? displayTrace || filteredTraces[0] || null : null;
  const traceColumns = [
    {
      title: t("dashboard.requestTrace.requestId", "请求 ID"),
      dataIndex: "request_id",
      key: "request_id",
      width: 220,
      ellipsis: true,
      render: (requestId: string, item: RequestTrace) => (
        <div style={{ minWidth: 0 }}>
          <div style={{ fontSize: 12, fontWeight: 600, color: colors.textPrimary, whiteSpace: "nowrap", overflow: "hidden", textOverflow: "ellipsis" }}>
            {requestId}
          </div>
          <div style={{ fontSize: 11, color: colors.textMuted, marginTop: 2, whiteSpace: "nowrap", overflow: "hidden", textOverflow: "ellipsis" }}>
            {resolveServiceLabel(item.service_id)} · {resolveUserLabel(item.user_id)}
          </div>
        </div>
      ),
    },
    {
      title: t("common.model", "模型"),
      key: "model",
      width: 180,
      render: (_: unknown, item: RequestTrace) => (
        <div style={{ minWidth: 0 }}>
          <div style={{ fontSize: 12, color: colors.textPrimary, whiteSpace: "nowrap", overflow: "hidden", textOverflow: "ellipsis" }}>
            {item.model || "-"}
          </div>
          <div style={{ fontSize: 11, color: colors.textMuted }}>{item.provider || "-"}</div>
        </div>
      ),
    },
    {
      title: t("common.status", "状态"),
      dataIndex: "status",
      key: "status",
      width: 92,
      render: (status: RequestTrace["status"]) => (
        <Tag color={status === "success" ? "success" : "error" } style={{ margin: 0, borderRadius: 5 }}>
          {status}
        </Tag>
      ),
    },
    {
      title: t("metrics.avgLatency", "延迟"),
      dataIndex: "request_total_duration_ms",
      key: "latency",
      width: 86,
      render: (duration: number) => (
        <span style={{ fontSize: 12, fontWeight: 650, color: duration > 5000 ? colors.warning : colors.textPrimary }}>
          {formatDuration(duration)}
        </span>
      ),
    },
    {
      title: t("analytics.cost", "成本"),
      dataIndex: "total_cost_usd",
      key: "cost",
      width: 86,
      render: (cost: number) => (
        <span style={{ fontSize: 12, fontWeight: 650, color: colors.success }}>
          {cost > 0 ? `$${cost >= 1 ? cost.toFixed(2) : cost.toFixed(4)}` : "—"}
        </span>
      ),
    },
  ];

  return (
    <PanelWrapper
      title={t("dashboard.requestTrace.title")}
      onRefresh={() => { if (isCurrent()) { invalidateDetail(); void recentQuery.refetch(); } }}
      loading={recentQuery.isLoading}
      dataStatus={recentQuery.isError && !displayTrace ? "error" : undefined}
    >
      <div
        className="request-trace-panel"
        style={{
          display: "flex",
          flexDirection: "column",
          height: "100%",
          minHeight: 0,
          minWidth: 0,
          containerType: "inline-size",
        }}
      >
      <p style={{ fontSize: 12, color: colors.textMuted }}>{t("dashboard.failureDrilldown.sampleNotice", "Up to 15 returned sampled traces. Failure-group links show status=error only; other non-success statuses and unsampled requests are not covered.")}</p>
      {drilldown && <div className="mb-3 space-y-2" role="status">
        <p className="break-all text-xs">{t("dashboard.failureDrilldown.provenance", "Failure group")}: {params.get("trace_dimension")} = {params.get("trace_value")} · {traceFilter.error_type} · {dateRange.join(" – ")} · {serviceId} · {userId}</p>
        <Button size="small" onClick={() => { if (isCurrent()) setParams(current => failureTraceReturn(current)); }}>{t("dashboard.failureDrilldown.return", "Return to failure analysis")}</Button>
        <p className="text-xs">{t("dashboard.failureDrilldown.lookupNotice", "Clear the group filter to look up an unrelated request ID.")}</p>
      </div>}
      {/* 搜索栏 */}
      <div id="request-trace-panel" className="trace-toolbar" style={{ display: "flex", gap: 8, marginBottom: 8, flexWrap: "wrap", flexShrink: 0, minWidth: 0 }}>
        <Input
          placeholder={t("dashboard.requestTrace.searchPlaceholder")}
          prefix={<SearchOutlined style={{ color: colors.textMuted }} />}
          value={searchId}
          disabled={drilldown}
          onChange={(e) => setSearchId(e.target.value)}
          onPressEnter={handleSearch}
          style={{ flex: "1 1 260px", minWidth: 0 }}
        />
        <Button type="primary" icon={<SearchOutlined />} onClick={handleSearch} loading={loading} disabled={drilldown}>
          {t("common.search")}
        </Button>
        {(trace || error) && (
          <Button icon={<ReloadOutlined />} onClick={handleReset}>
            {t("common.reset")}
          </Button>
        )}
      </div>

      {/* 筛选 Tab */}
      <div style={{ display: "flex", alignItems: "center", gap: 8, marginBottom: 12, flexWrap: "wrap", flexShrink: 0, minWidth: 0 }}>
        <Segmented
          size="small"
          value={activeTab}
          onChange={(val) => {
            if (!isCurrent()) return;
            invalidateDetail();
            setActiveTab(val as TraceTab);
            if (hasActiveFilter) setParams(current => writeTraceFilter(current, val === "error" ? { status: "error" } : val === "slow" ? { sample_reason: "slow_request" } : val === "sampled" ? { sample_reason: "baseline_sample" } : {}));
          }}
          options={[
            { value: "all", label: t("dashboard.requestTrace.tab.all", "全部") },
            { value: "error", label: t("dashboard.requestTrace.tab.error", "失败请求") },
            { value: "slow", label: t("dashboard.requestTrace.tab.slow", "慢请求") },
            { value: "sampled", label: t("dashboard.requestTrace.tab.sampled", "采样") },
          ]}
        />
        {hasActiveFilter && (
          <Tag
            closable
            onClose={() => { if (isCurrent()) { invalidateDetail(); clearTraceFilter(); setActiveTab("all"); } }}
            color="blue"
            style={{ margin: 0 }}
          >
            <CloseCircleOutlined style={{ marginRight: 4 }} />
            {t("dashboard.requestTrace.kpiFilter", "来自KPI联动")}
          </Tag>
        )}
      </div>

      {loading ? (
        <div style={{ textAlign: "center", padding: 40, flex: 1 }}>
          <Spin tip={t("dashboard.requestTrace.loading")} />
        </div>
      ) : error ? (
        <div style={{ flex: 1, display: "flex", alignItems: "center", justifyContent: "center" }}>
          <Empty image={Empty.PRESENTED_IMAGE_SIMPLE} description={<span style={{ color: colors.textMuted }}>{error}</span>} />
        </div>
      ) : (displayTrace || filteredTraces.length > 0) ? (
        <div
          className="trace-explorer-grid"
          style={{
            display: "grid",
            gridTemplateColumns: "minmax(0, 1.18fr) minmax(300px, 0.82fr)",
            gap: 12,
            alignItems: "stretch",
            flex: 1,
            minHeight: 0,
            minWidth: 0,
            overflow: "hidden",
          }}
        >
          <div
            style={{
              border: `1px solid ${colors.borderSoft}`,
              borderRadius: 8,
              overflow: "hidden",
              minWidth: 0,
              minHeight: 0,
            }}
          >
            <Table
              dataSource={filteredTraces}
              columns={traceColumns}
              rowKey="request_id"
              size="small"
              pagination={false}
              scroll={{ x: 680, y: 300 }}
              rowClassName={(item) => item.request_id === selectedTrace?.request_id ? "trace-row-active" : ""}
              onRow={(item) => ({
                onClick: () => {
                  if (!isCurrent() || currentData.current !== recentQuery.data) return;
                  invalidateDetail();
                  traceListOwner.current = recentQuery.data;
                  setTrace(item);
                  setSearchId(item.request_id);
                  setError(null);
                },
                style: { cursor: "pointer" },
              })}
            />
          </div>
          <div
            className="trace-detail-panel"
            style={{
              minWidth: 0,
              minHeight: 0,
              height: "100%",
              border: `1px solid ${colors.borderSoft}`,
              borderRadius: 8,
              background: colors.cardBg,
              padding: 12,
              overflow: "auto",
            }}
          >
            <div style={{ display: "flex", alignItems: "flex-start", justifyContent: "space-between", gap: 12, marginBottom: 12, minWidth: 0 }}>
              <div style={{ minWidth: 0 }}>
                <div style={{ fontSize: 14, fontWeight: 700, color: colors.textPrimary }}>
                  {t("dashboard.requestTrace.detailTitle", "Trace 详情")}
                </div>
                <div style={{ fontSize: 11, color: colors.textMuted, marginTop: 2 }}>
                  {t("dashboard.requestTrace.detailHint", "阶段耗时、Token、成本与安全元数据")}
                </div>
              </div>
              {selectedTrace?.sample_reason && <Tag style={{ margin: 0 }}>{selectedTrace.sample_reason}</Tag>}
            </div>
            {selectedTrace ? <TraceTimeline trace={selectedTrace} /> : null}
          </div>
          <style>{`
            .trace-row-active > td {
              background: hsl(var(--primary) / 0.14) !important;
            }
            .request-trace-panel .ant-table-wrapper,
            .request-trace-panel .ant-spin-nested-loading,
            .request-trace-panel .ant-spin-container,
            .request-trace-panel .ant-table,
            .request-trace-panel .ant-table-container {
              min-width: 0;
            }
            .request-trace-panel .ant-table-body {
              overflow: auto !important;
            }
            @container (max-width: 900px) {
              .trace-explorer-grid {
                grid-template-columns: minmax(0, 1fr) !important;
                overflow: auto !important;
                align-content: start;
              }
              .trace-detail-panel { min-height: 300px; }
            }
            @container (max-width: 520px) {
              .trace-toolbar .ant-input-affix-wrapper { flex-basis: 100% !important; }
            }
          `}</style>
        </div>
      ) : (
        <div style={{ flex: 1, display: "flex", alignItems: "center", justifyContent: "center" }}>
          <Empty
            image={Empty.PRESENTED_IMAGE_SIMPLE}
            description={
              <div style={{ color: colors.textMuted }}>
                <p>{activeTab !== "all"
                  ? t("dashboard.requestTrace.emptyFiltered", "当前筛选条件下无匹配的请求追踪")
                  : t("dashboard.requestTrace.emptyTitle")
                }</p>
                <p style={{ fontSize: 12, marginTop: 4 }}>{t("dashboard.requestTrace.emptyDesc")}</p>
              </div>
            }
          />
        </div>
      )}
      </div>
    </PanelWrapper>
  );
}
