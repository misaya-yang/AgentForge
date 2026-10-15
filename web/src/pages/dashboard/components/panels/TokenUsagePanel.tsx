// web/src/pages/dashboard/components/panels/TokenUsagePanel.tsx

import { Progress } from "antd";
import { useQuery } from "@tanstack/react-query";
import {
  LineChart,
  Line,
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
import { getUsageSummary, getUsageBreakdown, getUsageTimeSeries } from "@/api/usage";
import { useTranslation } from "react-i18next";
import { getChartPalette, getColors } from "../../styles";

function formatTokens(num: number): string {
  if (num >= 1_000_000) return `${(num / 1_000_000).toFixed(1)}M`;
  if (num >= 1_000) return `${(num / 1_000).toFixed(1)}K`;
  return num.toLocaleString();
}

export function TokenUsagePanel() {
  const { t } = useTranslation();
  const { darkMode } = useAppStore();
  const colors = getColors(darkMode);
  const { dateRange, granularity, serviceId, userId, lastRefresh } = useDashboardContext();

  const summaryQuery = useQuery({
    queryKey: ["dashboard-token-summary", dateRange, serviceId, userId, lastRefresh.getTime()],
    queryFn: () =>
      getUsageSummary({
        start_date: dateRange[0],
        end_date: dateRange[1],
        service_id: serviceId !== "all" ? serviceId : undefined,
        user_id: userId !== "all" ? userId : undefined,
      }),
    staleTime: 30000,
  });

  const breakdownQuery = useQuery({
    queryKey: ["dashboard-token-breakdown", dateRange, serviceId, userId, lastRefresh.getTime()],
    queryFn: () =>
      getUsageBreakdown({
        dimension: "model",
        start_date: dateRange[0],
        end_date: dateRange[1],
        service_id: serviceId !== "all" ? serviceId : undefined,
        user_id: userId !== "all" ? userId : undefined,
        limit: 5,
      }),
    staleTime: 30000,
  });

  // Provider breakdown query
  const providerQuery = useQuery({
    queryKey: ["dashboard-provider-breakdown", dateRange, serviceId, userId, lastRefresh.getTime()],
    queryFn: () =>
      getUsageBreakdown({
        dimension: "provider",
        start_date: dateRange[0],
        end_date: dateRange[1],
        service_id: serviceId !== "all" ? serviceId : undefined,
        user_id: userId !== "all" ? userId : undefined,
        limit: 10,
      }),
    staleTime: 30000,
  });

  const timeseriesQuery = useQuery({
    queryKey: ["dashboard-token-timeseries", dateRange, granularity, serviceId, userId, lastRefresh.getTime()],
    queryFn: () =>
      getUsageTimeSeries({
        start_date: dateRange[0],
        end_date: dateRange[1],
        granularity,
        service_id: serviceId !== "all" ? serviceId : undefined,
        user_id: userId !== "all" ? userId : undefined,
      }),
    staleTime: 30000,
  });

  const refetch = () => {
    summaryQuery.refetch();
    breakdownQuery.refetch();
    providerQuery.refetch();
    timeseriesQuery.refetch();
  };

  const summary = summaryQuery.data;
  const breakdown = breakdownQuery.data?.items || [];
  const providerBreakdown = (providerQuery.data?.items || []).filter((item) => {
    const provider = (item.provider || "").trim().toLowerCase();
    return provider !== "unknown" && provider !== "none" && provider !== "null";
  });
  const timeseries = timeseriesQuery.data?.data || [];

  const totalTokens = summary?.total_tokens || 0;
  const inputTokens = summary?.total_input_tokens || 0;
  const outputTokens = summary?.total_output_tokens || 0;
  const inputPercent = totalTokens > 0 ? (inputTokens / totalTokens) * 100 : 0;

  // 检测 unattributed 条目
  const hasUnattributed = breakdown.some((item) => (item.model || "").startsWith("unattributed")) ||
    providerBreakdown.some((item) => (item.provider || "").startsWith("unattributed"));

  const chartData = timeseries.map((point) => ({
    date: point.date,
    tokens: point.total_tokens,
  }));

  const gridColor = colors.divider;
  const chartPalette = getChartPalette(darkMode);
  const modelColors = [colors.accentDeep, colors.success, colors.info, colors.gold, colors.error];

  return (
    <PanelWrapper
      title={t("metrics.totalTokens")}
      onRefresh={refetch}
      loading={summaryQuery.isLoading}
    >
      {/* Unattributed 归因警告 */}
      {hasUnattributed && (
        <div
          style={{
            padding: "8px 12px",
            marginBottom: 12,
            borderRadius: 6,
            background: colors.warningBg,
            border: `1px solid ${colors.warning}66`,
            fontSize: 12,
            color: colors.warning,
            fontWeight: 500,
          }}
        >
          ⚠ {t("dashboard.cost.unattributedWarning", "部分请求未归因到具体厂商/模型，请检查服务配置")}
        </div>
      )}

      {/* Total and breakdown */}
      <div style={{ marginBottom: 16 }}>
        <div style={{ fontSize: 24, fontWeight: 700, color: colors.textPrimary }}>
          {formatTokens(totalTokens)}
          <span style={{ fontSize: 14, fontWeight: 400, marginLeft: 4 }}>{t("metrics.totalTokens")}</span>
        </div>

        {/* Input/Output bar */}
        <div style={{ marginTop: 12 }}>
          <div style={{ display: "flex", justifyContent: "space-between", fontSize: 12, marginBottom: 4 }}>
            <span style={{ color: colors.textMuted }}>
              {t("cost.inputTokens")}: {formatTokens(inputTokens)} ({inputPercent.toFixed(0)}%)
            </span>
            <span style={{ color: colors.textMuted }}>
              {t("cost.outputTokens")}: {formatTokens(outputTokens)} ({(100 - inputPercent).toFixed(0)}%)
            </span>
          </div>
          <Progress
            percent={100}
            success={{ percent: inputPercent }}
            showInfo={false}
            strokeColor={colors.accentDeep}
            railColor={colors.divider}
          />
        </div>
      </div>

      {/* Distribution sections - side by side */}
      <div
        style={{
          display: "grid",
          gridTemplateColumns: "repeat(auto-fit, minmax(150px, 1fr))",
          gap: 16,
          marginBottom: 16,
          minWidth: 0,
        }}
      >
        {/* Model breakdown */}
        <div style={{ minWidth: 0 }}>
          <div style={{ fontSize: 12, fontWeight: 600, color: colors.textSecondary, marginBottom: 8 }}>
            {t("dashboard.tokenUsage.modelBreakdown")}
          </div>
          {breakdown.slice(0, 3).map((item, index) => (
            <div
              key={item.model || index}
              style={{
                display: "flex",
                alignItems: "center",
                gap: 6,
                marginBottom: 5,
              }}
            >
              <div
                style={{
                  width: 6,
                  height: 6,
                  borderRadius: 2,
                  background: modelColors[index % modelColors.length],
                }}
              />
              <span
                style={{
                  flex: 1,
                  fontSize: 11,
                  color: colors.textPrimary,
                  overflow: "hidden",
                  textOverflow: "ellipsis",
                  whiteSpace: "nowrap",
                }}
              >
                {item.model || t("common.unknown")}
              </span>
              <span style={{ fontSize: 11, color: colors.textMuted }}>
                {item.percentage.toFixed(0)}%
              </span>
            </div>
          ))}
        </div>

        {/* Provider breakdown */}
        <div style={{ minWidth: 0 }}>
          <div style={{ fontSize: 12, fontWeight: 600, color: colors.textSecondary, marginBottom: 8 }}>
            {t("dashboard.tokenUsage.providerBreakdown")}
          </div>
          {providerBreakdown.slice(0, 3).map((item, index) => {
            const providerColors = chartPalette;
            const providerName = item.provider || "unattributed";
            const displayName = providerName === "unattributed"
              ? "UNATTRIBUTED"
              : t(`models.providers.${providerName}`, providerName);
            return (
              <div
                key={providerName}
                style={{
                  display: "flex",
                  alignItems: "center",
                  gap: 6,
                  marginBottom: 5,
                }}
              >
                <div
                  style={{
                    width: 6,
                    height: 6,
                    borderRadius: 2,
                    background: providerColors[index % providerColors.length],
                  }}
                />
                <span
                  style={{
                    flex: 1,
                    fontSize: 11,
                    color: colors.textPrimary,
                    overflow: "hidden",
                    textOverflow: "ellipsis",
                    whiteSpace: "nowrap",
                    minWidth: 0,
                  }}
                >
                  {displayName}
                </span>
                <span style={{ fontSize: 11, color: colors.success, fontWeight: 500 }}>
                  ${(item.cost_usd || 0).toFixed(2)}
                </span>
                <span style={{ fontSize: 11, color: colors.textMuted }}>
                  {item.percentage.toFixed(0)}%
                </span>
              </div>
            );
          })}
        </div>
      </div>

      {/* Trend chart */}
      <SafeResponsiveChart height={100} minWidth={100} minHeight={80}>
        <LineChart data={chartData} margin={{ top: 5, right: 5, left: 0, bottom: 0 }}>
          <CartesianGrid stroke={gridColor} strokeDasharray="3 3" vertical={false} />
          <XAxis dataKey="date" hide />
          <YAxis hide />
          <Tooltip
            formatter={(value) => [formatTokens(Number(value ?? 0)), t("metrics.totalTokens")]}
            labelFormatter={(label) => dayjs(String(label)).format("MM-DD")}
          />
          <Line
            type="monotone"
            dataKey="tokens"
            stroke={colors.accentDeep}
            strokeWidth={2}
            dot={false}
          />
        </LineChart>
      </SafeResponsiveChart>
    </PanelWrapper>
  );
}
