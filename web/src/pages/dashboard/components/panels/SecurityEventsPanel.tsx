// web/src/pages/dashboard/components/panels/SecurityEventsPanel.tsx
// 安全事件面板 - 修复 timeseries 格式，增加 quota_exceeded 事件类型

import { Row, Col, Select } from "antd";
import { useQuery } from "@tanstack/react-query";
import {
  BarChart,
  Bar,
  XAxis,
  YAxis,
  CartesianGrid,
  Tooltip,
  Legend,
} from "recharts";
import { SafeResponsiveChart } from "@/components/SafeResponsiveChart";
import dayjs from "dayjs";
import { useState, useMemo } from "react";
import { PanelWrapper } from "../PanelWrapper";
import { useDashboardContext } from "../../DashboardContext";
import { useAppStore } from "@/store/useAppStore";
import { api } from "@/lib/api";
import { useTranslation } from "react-i18next";
import { getColors } from "../../styles";

interface SecurityBreakdownResponse {
  items: Array<{
    name: string;
    count: number;
    percentage: number;
  }>;
  dimension: string;
  event_type: string;
  start_date: string;
  end_date: string;
  data_status: string;
  data_freshness_minutes: number;
  last_ingested_at?: string | null;
}

interface SecurityTimeseriesPoint {
  date: string;
  count: number;
}

interface SecurityTimeseriesResponse {
  data: SecurityTimeseriesPoint[];
  event_type: string;
  data_status: string;
  data_freshness_minutes: number;
}

async function getSecurityBreakdown(params: {
  dimension: string;
  event_type: string;
  start_date?: string;
  end_date?: string;
}): Promise<SecurityBreakdownResponse> {
  const searchParams = new URLSearchParams();
  searchParams.set("dimension", params.dimension);
  searchParams.set("event_type", params.event_type);
  if (params.start_date) searchParams.set("start_date", params.start_date);
  if (params.end_date) searchParams.set("end_date", params.end_date);
  const response = await api.get<SecurityBreakdownResponse>(`/api/v1/metrics/security/breakdown?${searchParams}`);
  return response.data;
}

async function getSecurityTimeseries(params: {
  event_type: string;
  start_date?: string;
  end_date?: string;
  granularity?: string;
}): Promise<SecurityTimeseriesResponse> {
  const searchParams = new URLSearchParams();
  searchParams.set("event_type", params.event_type);
  if (params.start_date) searchParams.set("start_date", params.start_date);
  if (params.end_date) searchParams.set("end_date", params.end_date);
  if (params.granularity) searchParams.set("granularity", params.granularity);
  const response = await api.get<SecurityTimeseriesResponse>(`/api/v1/metrics/security/timeseries?${searchParams}`);
  return response.data;
}

export function SecurityEventsPanel() {
  const { t } = useTranslation();
  const { darkMode } = useAppStore();
  const colors = getColors(darkMode);
  const { dateRange, granularity, lastRefresh } = useDashboardContext();
  const [timeRange, setTimeRange] = useState<"today" | "week">("today");

  const actualStartDate = timeRange === "today" ? dayjs().format("YYYY-MM-DD") : dateRange[0];
  const actualEndDate = timeRange === "today" ? dayjs().format("YYYY-MM-DD") : dateRange[1];

  // 并行请求各 event_type 的 breakdown
  const authBreakdownQuery = useQuery({
    queryKey: ["sec-breakdown-auth", actualStartDate, actualEndDate, lastRefresh.getTime()],
    queryFn: () =>
      getSecurityBreakdown({
        dimension: "user",
        event_type: "auth_failed",
        start_date: actualStartDate,
        end_date: actualEndDate,
      }),
    staleTime: 30000,
  });

  const rateLimitBreakdownQuery = useQuery({
    queryKey: ["sec-breakdown-rate", actualStartDate, actualEndDate, lastRefresh.getTime()],
    queryFn: () =>
      getSecurityBreakdown({
        dimension: "user",
        event_type: "rate_limited",
        start_date: actualStartDate,
        end_date: actualEndDate,
      }),
    staleTime: 30000,
  });

  const quotaBreakdownQuery = useQuery({
    queryKey: ["sec-breakdown-quota", actualStartDate, actualEndDate, lastRefresh.getTime()],
    queryFn: () =>
      getSecurityBreakdown({
        dimension: "user",
        event_type: "quota_exceeded",
        start_date: actualStartDate,
        end_date: actualEndDate,
      }),
    staleTime: 30000,
  });

  // 并行请求各 event_type 的 timeseries
  const authTsQuery = useQuery({
    queryKey: ["sec-ts-auth", actualStartDate, actualEndDate, granularity, lastRefresh.getTime()],
    queryFn: () =>
      getSecurityTimeseries({
        event_type: "auth_failed",
        start_date: actualStartDate,
        end_date: actualEndDate,
        granularity,
      }),
    staleTime: 30000,
  });

  const rateLimitTsQuery = useQuery({
    queryKey: ["sec-ts-rate", actualStartDate, actualEndDate, granularity, lastRefresh.getTime()],
    queryFn: () =>
      getSecurityTimeseries({
        event_type: "rate_limited",
        start_date: actualStartDate,
        end_date: actualEndDate,
        granularity,
      }),
    staleTime: 30000,
  });

  const quotaTsQuery = useQuery({
    queryKey: ["sec-ts-quota", actualStartDate, actualEndDate, granularity, lastRefresh.getTime()],
    queryFn: () =>
      getSecurityTimeseries({
        event_type: "quota_exceeded",
        start_date: actualStartDate,
        end_date: actualEndDate,
        granularity,
      }),
    staleTime: 30000,
  });

  const refetch = () => {
    authBreakdownQuery.refetch();
    rateLimitBreakdownQuery.refetch();
    quotaBreakdownQuery.refetch();
    authTsQuery.refetch();
    rateLimitTsQuery.refetch();
    quotaTsQuery.refetch();
  };

  // 计算汇总
  const authFailures = (authBreakdownQuery.data?.items || []).reduce((sum, i) => sum + i.count, 0);
  const rateLimitHits = (rateLimitBreakdownQuery.data?.items || []).reduce((sum, i) => sum + i.count, 0);
  const quotaExceeded = (quotaBreakdownQuery.data?.items || []).reduce((sum, i) => sum + i.count, 0);

  // 合并 timeseries 为统一 chart data
  const chartData = useMemo(() => {
    const authData = authTsQuery.data?.data || [];
    const rateData = rateLimitTsQuery.data?.data || [];
    const quotaData = quotaTsQuery.data?.data || [];

    // 收集所有日期
    const dateMap = new Map<string, { auth_failed: number; rate_limited: number; quota_exceeded: number }>();

    for (const point of authData) {
      const existing = dateMap.get(point.date) || { auth_failed: 0, rate_limited: 0, quota_exceeded: 0 };
      existing.auth_failed = point.count;
      dateMap.set(point.date, existing);
    }
    for (const point of rateData) {
      const existing = dateMap.get(point.date) || { auth_failed: 0, rate_limited: 0, quota_exceeded: 0 };
      existing.rate_limited = point.count;
      dateMap.set(point.date, existing);
    }
    for (const point of quotaData) {
      const existing = dateMap.get(point.date) || { auth_failed: 0, rate_limited: 0, quota_exceeded: 0 };
      existing.quota_exceeded = point.count;
      dateMap.set(point.date, existing);
    }

    return Array.from(dateMap.entries())
      .sort(([a], [b]) => a.localeCompare(b))
      .map(([date, counts]) => ({ date, ...counts }));
  }, [authTsQuery.data, rateLimitTsQuery.data, quotaTsQuery.data]);

  // Top users with events (合并所有事件类型)
  const userEvents = useMemo(() => {
    const map: Record<string, { user: string; authFailed: number; rateLimited: number; quotaExceeded: number }> = {};

    for (const item of (authBreakdownQuery.data?.items || [])) {
      const key = item.name || "unknown";
      if (!map[key]) map[key] = { user: key, authFailed: 0, rateLimited: 0, quotaExceeded: 0 };
      map[key].authFailed += item.count;
    }
    for (const item of (rateLimitBreakdownQuery.data?.items || [])) {
      const key = item.name || "unknown";
      if (!map[key]) map[key] = { user: key, authFailed: 0, rateLimited: 0, quotaExceeded: 0 };
      map[key].rateLimited += item.count;
    }
    for (const item of (quotaBreakdownQuery.data?.items || [])) {
      const key = item.name || "unknown";
      if (!map[key]) map[key] = { user: key, authFailed: 0, rateLimited: 0, quotaExceeded: 0 };
      map[key].quotaExceeded += item.count;
    }

    return Object.values(map)
      .sort((a, b) => (b.authFailed + b.rateLimited + b.quotaExceeded) - (a.authFailed + a.rateLimited + a.quotaExceeded))
      .slice(0, 3);
  }, [authBreakdownQuery.data, rateLimitBreakdownQuery.data, quotaBreakdownQuery.data]);

  // Data status from any available response
  const dataStatus = authBreakdownQuery.data?.data_status || rateLimitBreakdownQuery.data?.data_status;
  const dataFreshness = authBreakdownQuery.data?.data_freshness_minutes ?? rateLimitBreakdownQuery.data?.data_freshness_minutes;

  const gridColor = colors.divider;

  return (
    <PanelWrapper
      title={t("dashboard.security.title")}
      onRefresh={refetch}
      loading={authBreakdownQuery.isLoading}
      dataStatus={dataStatus}
      dataFreshnessMinutes={dataFreshness}
      extra={
        <Select
          value={timeRange}
          onChange={setTimeRange}
          size="small"
          style={{ width: 80 }}
          options={[
            { value: "today", label: t("dashboard.security.range.today") },
            { value: "week", label: t("dashboard.security.range.week") },
          ]}
        />
      }
    >
      {/* Summary stats */}
      <Row gutter={16} style={{ marginBottom: 16 }}>
        <Col span={6}>
          <div
            style={{
              padding: 10,
              borderRadius: 8,
              background: colors.innerBg,
              textAlign: "center",
            }}
          >
            <div style={{ fontSize: 22, fontWeight: 700, color: colors.error }}>{authFailures}</div>
            <div style={{ fontSize: 11, color: colors.textMuted }}>{t("dashboard.security.authFailed")}</div>
          </div>
        </Col>
        <Col span={6}>
          <div
            style={{
              padding: 10,
              borderRadius: 8,
              background: colors.innerBg,
              textAlign: "center",
            }}
          >
            <div style={{ fontSize: 22, fontWeight: 700, color: colors.warning }}>{rateLimitHits}</div>
            <div style={{ fontSize: 11, color: colors.textMuted }}>{t("dashboard.security.rateLimited")}</div>
          </div>
        </Col>
        <Col span={6}>
          <div
            style={{
              padding: 10,
              borderRadius: 8,
              background: colors.innerBg,
              textAlign: "center",
            }}
          >
            <div style={{ fontSize: 22, fontWeight: 700, color: colors.gold }}>{quotaExceeded}</div>
            <div style={{ fontSize: 11, color: colors.textMuted }}>{t("dashboard.security.quotaExceeded", "配额超限")}</div>
          </div>
        </Col>
        <Col span={6}>
          <div
            style={{
              padding: 10,
              borderRadius: 8,
              background: colors.innerBg,
              textAlign: "center",
            }}
          >
            <div style={{ fontSize: 22, fontWeight: 700, color: colors.textPrimary }}>
              {authFailures + rateLimitHits + quotaExceeded}
            </div>
            <div style={{ fontSize: 11, color: colors.textMuted }}>{t("dashboard.security.total")}</div>
          </div>
        </Col>
      </Row>

      {/* Chart */}
      <SafeResponsiveChart height={120} minWidth={100} minHeight={100} style={{ marginBottom: 12 }}>
        <BarChart data={chartData} margin={{ top: 5, right: 5, left: 0, bottom: 0 }}>
          <CartesianGrid stroke={gridColor} strokeDasharray="3 3" />
          <XAxis
            dataKey="date"
            tick={{ fontSize: 10 }}
            tickFormatter={(v) => dayjs(v).format(granularity === "hour" ? "HH:mm" : "MM-DD")}
          />
          <YAxis tick={{ fontSize: 10 }} width={30} />
          <Tooltip
            labelFormatter={(label) => dayjs(label).format("YYYY-MM-DD HH:mm")}
          />
          <Legend wrapperStyle={{ fontSize: 10 }} />
          <Bar dataKey="auth_failed" name={t("dashboard.security.authFailed")} fill={colors.error} stackId="a" />
          <Bar dataKey="rate_limited" name={t("dashboard.security.rateLimitedShort", "限流")} fill={colors.warning} stackId="a" />
          <Bar dataKey="quota_exceeded" name={t("dashboard.security.quotaExceeded", "配额超限")} fill={colors.gold} stackId="a" />
        </BarChart>
      </SafeResponsiveChart>

      {/* Top users */}
      <div>
        <div style={{ fontSize: 12, fontWeight: 600, color: colors.textSecondary, marginBottom: 8 }}>
          {t("dashboard.security.topUsers")}
        </div>
        {userEvents.length === 0 ? (
          <div style={{ fontSize: 12, color: colors.textMuted, textAlign: "center", padding: "8px 0" }}>
            {t("dashboard.security.noEvents", "当前时间范围内无安全事件")}
          </div>
        ) : (
          userEvents.map((user, index) => (
            <div
              key={user.user}
              style={{
                display: "flex",
                alignItems: "center",
                justifyContent: "space-between",
                padding: "6px 0",
                borderBottom: index < userEvents.length - 1 ? `1px solid ${gridColor}` : "none",
              }}
            >
              <span style={{ fontSize: 12, color: colors.textPrimary }}>
                {index + 1}. {user.user}
              </span>
              <div style={{ display: "flex", gap: 10 }}>
                {user.authFailed > 0 && (
                  <span style={{ fontSize: 11, color: colors.error }}>{t("dashboard.security.authFailedCount", { count: user.authFailed })}</span>
                )}
                {user.rateLimited > 0 && (
                  <span style={{ fontSize: 11, color: colors.warning }}>{t("dashboard.security.rateLimitedCount", { count: user.rateLimited })}</span>
                )}
                {user.quotaExceeded > 0 && (
                  <span style={{ fontSize: 11, color: colors.gold }}>{t("dashboard.security.quotaExceededCount", { count: user.quotaExceeded })}</span>
                )}
              </div>
            </div>
          ))
        )}
      </div>
    </PanelWrapper>
  );
}
