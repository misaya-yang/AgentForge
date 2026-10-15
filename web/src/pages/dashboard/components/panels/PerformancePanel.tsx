// web/src/pages/dashboard/components/panels/PerformancePanel.tsx

import { useState, useLayoutEffect, useRef } from "react";
import { useAuthenticatedReadScope } from "@/hooks/useAuthenticatedReadScope";
import { Select, Row, Col, Statistic } from "antd";
import { useQuery } from "@tanstack/react-query";
import {
  AreaChart,
  Area,
  XAxis,
  YAxis,
  CartesianGrid,
  Tooltip,
} from "recharts";
import { SafeResponsiveChart } from "@/components/SafeResponsiveChart";
import dayjs from "dayjs";
import { PanelWrapper } from "../PanelWrapper";
import { useDashboardContext } from "../../DashboardContext";
import { useAppStore } from "@/store/useAppStore";
import { getPerformanceBreakdown } from "@/api/usage";
import { useTranslation } from "react-i18next";
import { getColors } from "../../styles";

type LatencyMetric = "p50" | "p95" | "p99" | "avg";

export function PerformancePanel() {
  const scope = useAuthenticatedReadScope();
  const { dateRange, granularity, serviceId, userId, lastRefresh } = useDashboardContext();
  const [selectedMetric, setSelectedMetric] = useState<LatencyMetric>("p95");
  const identity = JSON.stringify([scope.id, dateRange, granularity, serviceId, userId, lastRefresh.getTime()]);
  return scope.isCurrent() ? <PerformanceContent key={identity} scope={scope} selectedMetric={selectedMetric} onMetricChange={setSelectedMetric} /> : null;
}

function PerformanceContent({ scope, selectedMetric, onMetricChange }: { scope: ReturnType<typeof useAuthenticatedReadScope>; selectedMetric: LatencyMetric; onMetricChange: (metric: LatencyMetric) => void }) {
  const { t } = useTranslation();
  const { darkMode } = useAppStore();
  const colors = getColors(darkMode);
  const { dateRange, granularity, serviceId, userId, lastRefresh } = useDashboardContext();
  const mounted = useRef(true);
  useLayoutEffect(() => { mounted.current = true; return () => { mounted.current = false; }; }, []);
  const isCurrent = () => mounted.current && scope.isCurrent();
  const query = useQuery({
    queryKey: ["dashboard-performance", scope.id, dateRange, granularity, serviceId, userId, lastRefresh.getTime()],
    queryFn: async ({ signal }) => {
      if (!isCurrent() || signal.aborted) throw new Error("Dashboard scope changed");
      const result = await getPerformanceBreakdown({
        start_date: dateRange[0],
        end_date: dateRange[1],
        granularity,
        service_id: serviceId !== "all" ? serviceId : undefined,
        user_id: userId !== "all" ? userId : undefined,
      }, signal);
      if (!isCurrent() || signal.aborted) throw new Error("Dashboard scope changed");
      return result;
    },
    retry: false,
    staleTime: 30000,
  });
  const refetch = () => { if (isCurrent()) void query.refetch({ cancelRefetch: false }); };
  const { data } = query;
  const loading = query.isPending || query.isFetching;
  const failed = query.isError || ["error", "collection_error", "unavailable"].includes(data?.data_status || "");
  const rows = Array.isArray(data?.data) && data.data.every(point => point != null &&
    typeof point.date === "string" && point.date.length > 0 &&
    Number.isSafeInteger(point.request_count) && point.request_count >= 0) ? data.data : [];
  const noSamples = ["no_data", "empty"].includes(data?.data_status || "") ||
    rows.length === 0 || rows.every(point => point.request_count === 0);
  const visible = isCurrent() && !loading && !failed && !noSamples;
  const latency = (value: unknown, count: number): number | null => count > 0 &&
    typeof value === "number" && Number.isFinite(value) && value >= 0 ? value : null;
  const chartData = (visible ? rows : []).map((point) => ({
    date: point.date,
    requestCount: point.request_count,
    avgLatency: latency(point.avg_total_ms, point.request_count),
    p50: latency(point.p50_total_ms, point.request_count),
    p95: latency(point.p95_total_ms, point.request_count),
    p99: latency(point.p99_total_ms, point.request_count),
    ttfb: latency(point.avg_first_token_ms, point.request_count),
    llm: latency(point.avg_llm_inference_ms, point.request_count),
    retrieval: latency(point.avg_retrieval_ms, point.request_count),
    tool: latency(point.avg_tool_call_ms, point.request_count),
    overhead: latency(point.avg_overhead_ms, point.request_count),
  }));
  const latestData = chartData[chartData.length - 1];
  const state = loading ? "loading" : failed ? "error" : noSamples ? "unavailable" : "ready";
  const validReadTime = Number.isFinite(query.dataUpdatedAt) && query.dataUpdatedAt > 0;
  const gridColor = colors.divider;

  const metricOptions = [
    { value: "avg", label: t("metrics.avgLatency") },
    { value: "p50", label: "P50" },
    { value: "p95", label: "P95" },
    { value: "p99", label: "P99" },
  ];

  return (
    <PanelWrapper
      title={t("dashboard.performance.title")}
      onRefresh={refetch}
      loading={loading}
      dataStatus={loading ? undefined : failed ? "error" : noSamples ? "no_data" : data?.data_status}
      dataFreshnessMinutes={visible ? data?.data_freshness_minutes : undefined}
      extra={
        <Select
          value={selectedMetric}
          onChange={(metric: LatencyMetric) => { if (isCurrent()) onMetricChange(metric); }}
          options={metricOptions}
          size="small"
          style={{ width: 100 }}
        />
      }
    >
      <div data-performance-state={state} aria-busy={loading}>
      <p style={{ fontSize: 11, color: colors.textMuted }}>{t("dashboard.performance.sourceHint")}</p>
      <p style={{ fontSize: 11, color: colors.textMuted }}>{t("dashboard.performance.window", { start: dateRange[0], end: dateRange[1] })}</p>
      {visible ? <>
      <p style={{ fontSize: 11, color: colors.textMuted }}>{t("dashboard.performance.latestBucket", { date: latestData.date, count: latestData.requestCount })}</p>
      <p style={{ fontSize: 11, color: colors.textMuted }}>{t("dashboard.performance.readAt", { time: validReadTime ? dayjs(query.dataUpdatedAt).format("YYYY-MM-DD HH:mm:ss") : t("common.unknown") })}</p>
      {/* Summary stats */}
      <Row gutter={12} style={{ marginBottom: 10 }}>
        <Col span={6}>
          <Statistic
            title={<span style={{ fontSize: 11, color: colors.textMuted }}>P50</span>}
            value={latestData?.p50 ?? t("common.unknown")}
            suffix={latestData?.p50 != null ? "ms" : undefined}
            styles={{ content: { fontSize: 15, color: colors.textPrimary } }}
          />
        </Col>
        <Col span={6}>
          <Statistic
            title={<span style={{ fontSize: 11, color: colors.textMuted }}>P95</span>}
            value={latestData?.p95 ?? t("common.unknown")}
            suffix={latestData?.p95 != null ? "ms" : undefined}
            styles={{ content: { fontSize: 15, color: colors.warning } }}
          />
        </Col>
        <Col span={6}>
          <Statistic
            title={<span style={{ fontSize: 11, color: colors.textMuted }}>P99</span>}
            value={latestData?.p99 ?? t("common.unknown")}
            suffix={latestData?.p99 != null ? "ms" : undefined}
            styles={{ content: { fontSize: 15, color: colors.error } }}
          />
        </Col>
        <Col span={6}>
          <Statistic
            title={<span style={{ fontSize: 11, color: colors.textMuted }}>{t("dashboard.performance.avg")}</span>}
            value={latestData?.avgLatency ?? t("common.unknown")}
            suffix={latestData?.avgLatency != null ? "ms" : undefined}
            styles={{ content: { fontSize: 15, color: colors.info } }}
          />
        </Col>
      </Row>

      {/* Phase breakdown */}
      <Row gutter={12} style={{ marginBottom: 8 }}>
        <Col span={8}>
          <Statistic
            title={<span style={{ fontSize: 11, color: colors.textMuted }}>TTFB</span>}
            value={latestData?.ttfb ?? t("common.unknown")}
            suffix={latestData?.ttfb != null ? "ms" : undefined}
            styles={{ content: { fontSize: 13, color: colors.info } }}
          />
        </Col>
        <Col span={8}>
          <Statistic
            title={<span style={{ fontSize: 11, color: colors.textMuted }}>LLM</span>}
            value={latestData?.llm ?? t("common.unknown")}
            suffix={latestData?.llm != null ? "ms" : undefined}
            styles={{ content: { fontSize: 13, color: colors.gold } }}
          />
        </Col>
        <Col span={8}>
          <Statistic
            title={<span style={{ fontSize: 11, color: colors.textMuted }}>Retrieval</span>}
            value={latestData?.retrieval ?? t("common.unknown")}
            suffix={latestData?.retrieval != null ? "ms" : undefined}
            styles={{ content: { fontSize: 13, color: colors.success } }}
          />
        </Col>
      </Row>
      <Row gutter={12} style={{ marginBottom: 6 }}>
        <Col span={12}>
          <Statistic
            title={<span style={{ fontSize: 11, color: colors.textMuted }}>Tool Calls</span>}
            value={latestData?.tool ?? t("common.unknown")}
            suffix={latestData?.tool != null ? "ms" : undefined}
            styles={{ content: { fontSize: 13, color: colors.warning } }}
          />
        </Col>
        <Col span={12}>
          <Statistic
            title={<span style={{ fontSize: 11, color: colors.textMuted }}>Overhead</span>}
            value={latestData?.overhead ?? t("common.unknown")}
            suffix={latestData?.overhead != null ? "ms" : undefined}
            styles={{ content: { fontSize: 13, color: colors.error } }}
          />
        </Col>
      </Row>

      {/* Chart */}
      <SafeResponsiveChart height={138} minWidth={100} minHeight={100}>
        <AreaChart data={chartData} margin={{ top: 10, right: 10, left: 0, bottom: 0 }}>
          <CartesianGrid stroke={gridColor} strokeDasharray="3 3" />
          <XAxis
            dataKey="date"
            tick={{ fontSize: 11 }}
            tickFormatter={(value) =>
              dayjs(value).format(granularity === "hour" ? "HH:mm" : "MM-DD")
            }
          />
          <YAxis tick={{ fontSize: 11 }} width={40} />
          <Tooltip
            formatter={(value) => [typeof value === "number" && Number.isFinite(value) ? `${value} ms` : t("common.unknown"), ""]}
            labelFormatter={(label) => dayjs(String(label)).format("YYYY-MM-DD HH:mm")}
          />
          <Area
            type="monotone"
            dataKey={selectedMetric === "avg" ? "avgLatency" : selectedMetric}
            stroke={colors.info}
            fill={colors.infoBg}
            strokeWidth={2}
          />
        </AreaChart>
      </SafeResponsiveChart>
      </> : <div role="status">
        <p>{t("dashboard.performance.noSamples")}</p>
        <button type="button" onClick={refetch}>{t("common.retry")}</button>
      </div>}
      </div>
    </PanelWrapper>
  );
}
