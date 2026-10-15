import { DataStatusBadge } from "./components/DataStatusBadge";
import { useMemo, useState, type ComponentType } from "react";
import { Segmented, Tooltip } from "antd";
import { useQuery } from "@tanstack/react-query";
import dayjs from "dayjs";
import {
  DashboardOutlined,
  FundProjectionScreenOutlined,
  SafetyCertificateOutlined,
  ApartmentOutlined,
} from "@ant-design/icons";
import { useTranslation } from "react-i18next";
import { useAppStore } from "@/store/useAppStore";
import { ProviderStatusCard } from "@/components/ProviderStatusCard";
import { useHealth, useServices } from "@/hooks/useServices";
import { getQuotaUsersOverview, getUsageSummary, listRequestTraces } from "@/api/usage";
import { useDashboardContext } from "./DashboardContext";
import { getColors, LAYOUT, TYPOGRAPHY } from "./styles";
import type { PanelType } from "./types";
import {
  ServiceHealthPanel,
  PerformancePanel,
  TokenUsagePanel,
  CostAnalysisPanel,
  UserQuotaPanel,
  SecurityEventsPanel,
  RequestTracePanel,
  FailureAnalysisPanel,
} from "./components/panels";

type WorkspaceKey = "overview" | "operations" | "reliability" | "governance" | "tracing";

interface PanelSlot {
  type: PanelType;
  minHeight: number;
  span: 1 | 2 | 3;
}

interface WorkspaceConfig {
  title: string;
  subtitle: string;
  intent: string;
  panels: PanelSlot[];
}

interface WorkspaceSignal {
  label: string;
  value: string;
  tone: "ok" | "warn" | "critical" | "neutral";
}

const WORKSPACE_STORAGE_KEY = "dashboard-workspace-v2";

const PANEL_COMPONENTS: Record<PanelType, ComponentType> = {
  "service-health": ServiceHealthPanel,
  performance: PerformancePanel,
  "token-usage": TokenUsagePanel,
  "cost-analysis": CostAnalysisPanel,
  "user-quota": UserQuotaPanel,
  "security-events": SecurityEventsPanel,
  "request-trace": RequestTracePanel,
  "failure-analysis": FailureAnalysisPanel,
  "provider-status": ProviderStatusCard,
};

function loadWorkspace(): WorkspaceKey {
  try {
    const saved = localStorage.getItem(WORKSPACE_STORAGE_KEY);
    if (
      saved === "overview" ||
      saved === "operations" ||
      saved === "reliability" ||
      saved === "governance" ||
      saved === "tracing"
    ) {
      return saved;
    }
  } catch {
    // ignored
  }
  return "overview";
}

function saveWorkspace(workspace: WorkspaceKey) {
  try {
    localStorage.setItem(WORKSPACE_STORAGE_KEY, workspace);
  } catch {
    // ignored
  }
}

function formatCost(value: number): string {
  if (value >= 1) return `$${value.toFixed(2)}`;
  return `$${value.toFixed(4)}`;
}

interface DashboardLayoutProps {
  width?: number;
  forceWorkspace?: WorkspaceKey;
}

export function DashboardLayout({ width = 1200, forceWorkspace }: DashboardLayoutProps) {
  const { t } = useTranslation();
  const { darkMode } = useAppStore();
  const colors = getColors(darkMode);
  const { dateRange, serviceId, userId, lastRefresh } = useDashboardContext();
  const [workspace, setWorkspace] = useState<WorkspaceKey>(() => forceWorkspace || loadWorkspace());
  const activeWorkspace = forceWorkspace || workspace;

  const servicesQuery = useServices();
  const healthQuery = useHealth();
  const summaryQuery = useQuery({
    queryKey: ["dashboard-workspace-summary", dateRange, serviceId, userId, lastRefresh.getTime()],
    queryFn: () =>
      getUsageSummary({
        start_date: dateRange[0],
        end_date: dateRange[1],
        service_id: serviceId !== "all" ? serviceId : undefined,
        user_id: userId !== "all" ? userId : undefined,
      }),
    staleTime: 30000,
  });
  const todayCostQuery = useQuery({
    queryKey: ["dashboard-workspace-cost-today", serviceId, userId, lastRefresh.getTime()],
    queryFn: () =>
      getUsageSummary({
        start_date: dayjs().format("YYYY-MM-DD"),
        end_date: dayjs().format("YYYY-MM-DD"),
        service_id: serviceId !== "all" ? serviceId : undefined,
        user_id: userId !== "all" ? userId : undefined,
      }),
    staleTime: 30000,
  });
  const monthCostQuery = useQuery({
    queryKey: ["dashboard-workspace-cost-month", serviceId, userId, lastRefresh.getTime()],
    queryFn: () =>
      getUsageSummary({
        start_date: dayjs().startOf("month").format("YYYY-MM-DD"),
        end_date: dayjs().format("YYYY-MM-DD"),
        service_id: serviceId !== "all" ? serviceId : undefined,
        user_id: userId !== "all" ? userId : undefined,
      }),
    staleTime: 30000,
  });
  const quotaQuery = useQuery({
    queryKey: ["dashboard-workspace-quota", lastRefresh.getTime()],
    queryFn: () => getQuotaUsersOverview({ limit: 20, sort_by: "status" }),
    staleTime: 30000,
  });
  const tracesQuery = useQuery({
    queryKey: ["dashboard-workspace-traces", dateRange, serviceId, userId, lastRefresh.getTime()],
    queryFn: () =>
      listRequestTraces({
        start_date: dateRange[0],
        end_date: dateRange[1],
        service_id: serviceId !== "all" ? serviceId : undefined,
        user_id: userId !== "all" ? userId : undefined,
        limit: 15,
      }),
    staleTime: 30000,
  });

  const workspaceConfigs = useMemo<Record<WorkspaceKey, WorkspaceConfig>>(
    () => ({
      overview: {
        title: t("dashboard.workspace.overview.title", "总览态势"),
        subtitle: t(
          "dashboard.workspace.overview.subtitle",
          "跨服务、成本、模型接入与安全事件的高层态势"
        ),
        intent: t("dashboard.workspace.overview.intent", "Executive overview"),
        panels: [
          { type: "provider-status", span: 2, minHeight: 300 },
          { type: "cost-analysis", span: 1, minHeight: 300 },
          { type: "service-health", span: 1, minHeight: 300 },
          { type: "security-events", span: 2, minHeight: 300 },
        ],
      },
      operations: {
        title: t("dashboard.workspace.operations.title", "运营控制台"),
        subtitle: t(
          "dashboard.workspace.operations.subtitle",
          "围绕服务可用性、吞吐、延迟、Token 和实时请求样本排障"
        ),
        intent: t("dashboard.workspace.operations.intent", "Live operations"),
        panels: [
          { type: "service-health", span: 2, minHeight: 262 },
          { type: "performance", span: 1, minHeight: 300 },
          { type: "token-usage", span: 1, minHeight: 300 },
          { type: "request-trace", span: 2, minHeight: 300 },
        ],
      },
      reliability: {
        title: t("dashboard.workspace.reliability.title", "故障与可靠性"),
        subtitle: t(
          "dashboard.workspace.reliability.subtitle",
          "围绕 SLO、错误预算、失败根因与慢请求来定位风险"
        ),
        intent: t("dashboard.workspace.reliability.intent", "Incident response"),
        panels: [
          { type: "failure-analysis", span: 1, minHeight: 336 },
          { type: "security-events", span: 2, minHeight: 336 },
          { type: "request-trace", span: 3, minHeight: 472 },
        ],
      },
      governance: {
        title: t("dashboard.workspace.governance.title", "成本与配额治理"),
        subtitle: t(
          "dashboard.workspace.governance.subtitle",
          "聚焦预算消耗、用户配额、归因缺口与策略执行状态"
        ),
        intent: t("dashboard.workspace.governance.intent", "Spend governance"),
        panels: [
          { type: "cost-analysis", span: 1, minHeight: 360 },
          { type: "user-quota", span: 1, minHeight: 360 },
          { type: "token-usage", span: 1, minHeight: 336 },
          { type: "provider-status", span: 3, minHeight: 300 },
        ],
      },
      tracing: {
        title: t("dashboard.workspace.tracing.title", "请求追踪"),
        subtitle: t(
          "dashboard.workspace.tracing.subtitle",
          "按请求 ID、状态与慢请求样本查看阶段耗时、模型、Token 与成本"
        ),
        intent: t("dashboard.workspace.tracing.intent", "Trace explorer"),
        panels: [
          { type: "request-trace", span: 3, minHeight: 560 },
          { type: "performance", span: 1, minHeight: 336 },
          { type: "failure-analysis", span: 2, minHeight: 300 },
        ],
      },
    }),
    [t]
  );

  const activeConfig = workspaceConfigs[activeWorkspace];
  const useSingleColumn = width < 1100;
  const useThreeColumn = width >= 1180;
  const toneColors: Record<WorkspaceSignal["tone"], { fg: string; bg: string; border: string }> = {
    ok: { fg: colors.success, bg: colors.successSoft, border: `${colors.success}33` },
    warn: { fg: colors.warning, bg: colors.warningSoft, border: `${colors.warning}33` },
    critical: { fg: colors.error, bg: colors.errorSoft, border: `${colors.error}33` },
    neutral: { fg: colors.navy, bg: colors.innerBg, border: colors.borderSoft },
  };
  const services = servicesQuery.data || [];
  const health = healthQuery.data || {};
  const healthyCount = services.filter((service) => health[service.service_id]?.status === "healthy").length;
  const availability = services.length > 0 ? (healthyCount / services.length) * 100 : 0;
  const observedSuccessRate = summaryQuery.data && summaryQuery.data.total_requests > 0
    ? summaryQuery.data.success_rate : null;
  const errorRate = observedSuccessRate == null ? null : Math.max(0, 100 - observedSuccessRate);
  const traces = tracesQuery.data || [];
  const failedTraceCount = traces.filter((trace) => trace.status === "error").length;
  const slowTraceCount = traces.filter((trace) => trace.sample_reason === "slow_request" || trace.request_total_duration_ms > 5000).length;
  const sampledTraceCount = traces.filter((trace) => trace.sample_reason === "baseline_sample").length;
  const quotaSummary = quotaQuery.data?.summary;
  const quotaRiskCount = (quotaSummary?.warning || 0) + (quotaSummary?.exceeded || 0) + (quotaSummary?.blocked || 0);
  const successRate = observedSuccessRate;
  const usesExternalTabs = Boolean(forceWorkspace);
  const workspaceSignals: Record<WorkspaceKey, WorkspaceSignal[]> = {
    overview: [
      {
        label: t("dashboard.serviceHealth.availability", "可用率"),
        value: services.length > 0 ? `${availability.toFixed(1)}%` : "—",
        tone: availability >= 99 || services.length === 0 ? "ok" : availability >= 90 ? "warn" : "critical",
      },
      { label: t("dashboard.cost.month", "本月成本"), value: formatCost(monthCostQuery.data?.total_cost_usd || 0), tone: "neutral" },
      {
        label: t("dashboard.governance.quotaRisk", "配额风险"),
        value: String(quotaRiskCount),
        tone: quotaRiskCount > 0 ? "warn" : "ok",
      },
      {
        label: t("dashboard.requestTrace.tab.error", "失败请求"),
        value: String(failedTraceCount),
        tone: failedTraceCount > 0 ? "critical" : "ok",
      },
    ],
    operations: [
      {
        label: t("dashboard.serviceHealth.availability", "可用率"),
        value: services.length > 0 ? `${availability.toFixed(1)}%` : "—",
        tone: availability >= 99 || services.length === 0 ? "ok" : availability >= 90 ? "warn" : "critical",
      },
      {
        label: t("dashboard.serviceHealth.errorRate", "错误率"),
        value: errorRate === null ? "—" : `${errorRate.toFixed(2)}%`,
        tone: errorRate === null ? "neutral" : errorRate > 5 ? "critical" : errorRate > 1 ? "warn" : "ok",
      },
      {
        label: t("dashboard.ops.services", "服务总数"),
        value: String(services.length),
        tone: "neutral",
      },
      {
        label: t("dashboard.ops.activeRuns", "追踪样本"),
        value: String(traces.length),
        tone: "neutral",
      },
    ],
    reliability: [
      {
        label: "SLO",
        value: t("dashboard.reliability.sloNotConfigured", "Not configured"),
        tone: "neutral",
      },
      {
        label: t("metrics.successRate", "成功率"),
        value: successRate == null ? "—" : `${successRate.toFixed(1)}%`,
        tone: successRate == null ? "neutral" : successRate >= 99.5 ? "ok" : successRate >= 95 ? "warn" : "critical",
      },
      {
        label: t("dashboard.requestTrace.tab.error", "失败请求"),
        value: String(failedTraceCount),
        tone: failedTraceCount > 0 ? "critical" : "ok",
      },
      {
        label: t("dashboard.requestTrace.tab.slow", "慢请求"),
        value: String(slowTraceCount),
        tone: slowTraceCount > 0 ? "warn" : "ok",
      },
    ],
    governance: [
      { label: t("dashboard.cost.month", "本月"), value: monthCostQuery.data ? formatCost(monthCostQuery.data.total_cost_usd) : "—", tone: "neutral" },
      { label: t("dashboard.cost.today", "今日"), value: todayCostQuery.data ? formatCost(todayCostQuery.data.total_cost_usd) : "—", tone: "ok" },
      {
        label: t("dashboard.governance.quotaRisk", "配额风险"),
        value: String(quotaRiskCount),
        tone: quotaRiskCount > 0 ? "warn" : "ok",
      },
      {
        label: t("metrics.totalTokens", "Token 消耗"),
        value: summaryQuery.data ? `${Math.round(summaryQuery.data.total_tokens / 1000)}K` : "—",
        tone: "neutral",
      },
    ],
    tracing: [
      { label: t("dashboard.requestTrace.tab.all", "全部"), value: String(traces.length), tone: "neutral" },
      { label: t("dashboard.requestTrace.tab.error", "失败请求"), value: String(failedTraceCount), tone: failedTraceCount > 0 ? "critical" : "ok" },
      { label: t("dashboard.requestTrace.tab.slow", "慢请求"), value: String(slowTraceCount), tone: slowTraceCount > 0 ? "warn" : "ok" },
      { label: t("dashboard.requestTrace.tab.sampled", "采样"), value: String(sampledTraceCount), tone: "neutral" },
    ],
  };

  return (
    <div className="dashboard-workspace" style={{ minHeight: "100%" }}>
      <div
        className="dashboard-workspace-header"
        style={{
          borderRadius: usesExternalTabs ? 0 : LAYOUT.CARD_RADIUS,
          border: usesExternalTabs ? "none" : `1px solid ${colors.borderSoft}`,
          background: usesExternalTabs ? "transparent" : colors.cardBg,
          boxShadow: usesExternalTabs ? "none" : colors.shadowSm,
          padding: usesExternalTabs ? "0 0 14px" : "11px 14px",
          marginBottom: usesExternalTabs ? 2 : LAYOUT.GRID_GAP,
        }}
      >
        <div
          style={{
            display: "flex",
            alignItems: "center",
            justifyContent: "space-between",
            gap: 12,
            flexWrap: "wrap",
          }}
        >
          <div style={{ minWidth: 0, flex: "1 1 420px" }}>
            <div
              style={{
                display: "inline-flex",
                alignItems: "center",
                height: 20,
                padding: "0 8px",
                borderRadius: 5,
                marginBottom: usesExternalTabs ? 4 : 6,
                background: colors.operatorSoft,
                color: colors.operator,
                fontSize: 11,
                fontWeight: 650,
              }}
            >
              {activeConfig.intent}
            </div>
            <div
              style={{
                ...TYPOGRAPHY.sectionTitle,
                color: colors.textPrimary,
                fontSize: usesExternalTabs ? 14 : 15,
                letterSpacing: "0",
              }}
            >
              {activeConfig.title}
            </div>
            <div
              style={{
                fontSize: 12,
                color: colors.textSecondary,
                marginTop: 3,
                overflow: "hidden",
                textOverflow: "ellipsis",
                whiteSpace: "nowrap",
              }}
            >
              {activeConfig.subtitle}
            </div>
          </div>

          <div className="dashboard-workspace-signals" style={{ display: "flex", alignItems: "center", gap: 7, flexWrap: "wrap", justifyContent: "flex-end" }}>
            <DataStatusBadge
              dataStatus={summaryQuery.isError ? "collection_error" : summaryQuery.data?.data_status}
              dataFreshnessMinutes={summaryQuery.data?.data_freshness_minutes}
            />
            {workspaceSignals[activeWorkspace].map((signal) => {
              const tone = toneColors[signal.tone];
              return (
                <div
                  key={`${signal.label}-${signal.value}`}
                  style={{
                    minWidth: 86,
                    padding: "6px 9px",
                    borderRadius: 8,
                    border: `1px solid ${tone.border}`,
                    background: tone.bg,
                  }}
                >
                  <div style={{ fontSize: 11, color: colors.textMuted, marginBottom: 2 }}>{signal.label}</div>
                  <div style={{ fontSize: 14, fontWeight: 700, color: tone.fg, fontFeatureSettings: '"tnum"' }}>
                    {signal.value}
                  </div>
                </div>
              );
            })}

            {!forceWorkspace && (
              <Segmented
                size="middle"
                value={workspace}
                onChange={(value) => {
                  const next = value as WorkspaceKey;
                  setWorkspace(next);
                  saveWorkspace(next);
                }}
                options={[
                  {
                    value: "overview",
                    label: (
                      <Tooltip title={t("dashboard.workspace.overview.title", "总览态势")}>
                        <span style={{ display: "inline-flex", alignItems: "center", gap: 6 }}>
                          <DashboardOutlined />
                          {t("dashboard.workspace.overview.short", "总览")}
                        </span>
                      </Tooltip>
                    ),
                  },
                  {
                    value: "operations",
                    label: (
                      <Tooltip title={t("dashboard.workspace.operations.title", "运营控制台")}>
                        <span style={{ display: "inline-flex", alignItems: "center", gap: 6 }}>
                          <FundProjectionScreenOutlined />
                          {t("dashboard.workspace.operations.short", "运营")}
                        </span>
                      </Tooltip>
                    ),
                  },
                  {
                    value: "reliability",
                    label: (
                      <Tooltip title={t("dashboard.workspace.reliability.title", "故障与可靠性")}>
                        <span style={{ display: "inline-flex", alignItems: "center", gap: 6 }}>
                          <FundProjectionScreenOutlined />
                          {t("dashboard.workspace.reliability.short", "可靠性")}
                        </span>
                      </Tooltip>
                    ),
                  },
                  {
                    value: "governance",
                    label: (
                      <Tooltip title={t("dashboard.workspace.governance.title", "成本与配额治理")}>
                        <span style={{ display: "inline-flex", alignItems: "center", gap: 6 }}>
                          <SafetyCertificateOutlined />
                          {t("dashboard.workspace.governance.short", "治理")}
                        </span>
                      </Tooltip>
                    ),
                  },
                  {
                    value: "tracing",
                    label: (
                      <Tooltip title={t("dashboard.workspace.tracing.title", "请求追踪")}>
                        <span style={{ display: "inline-flex", alignItems: "center", gap: 6 }}>
                          <ApartmentOutlined />
                          {t("dashboard.workspace.tracing.short", "追踪")}
                        </span>
                      </Tooltip>
                    ),
                  },
                ]}
              />
            )}
          </div>
        </div>
      </div>

      <div
        style={{
          display: "grid",
          gridTemplateColumns: useSingleColumn
            ? "minmax(0, 1fr)"
            : useThreeColumn
            ? "repeat(3, minmax(0, 1fr))"
            : "repeat(2, minmax(0, 1fr))",
          gap: LAYOUT.GRID_GAP,
          alignItems: "stretch",
        }}
      >
        {activeConfig.panels.map((slot) => {
          const PanelComponent = PANEL_COMPONENTS[slot.type];
          return (
            <div
              key={`${activeWorkspace}-${slot.type}`}
              style={{
                minHeight: useSingleColumn ? Math.min(slot.minHeight, 300) : slot.minHeight,
                height: useSingleColumn ? "auto" : slot.minHeight,
                minWidth: 0,
                gridColumn: useSingleColumn
                  ? "auto"
                  : useThreeColumn
                  ? `span ${slot.span}`
                  : slot.span === 1
                  ? "auto"
                  : "1 / -1",
              }}
            >
              <PanelComponent />
            </div>
          );
        })}
      </div>
      <style>{`
        @media (max-width: 1099px) {
          .dashboard-workspace-header > div {
            align-items: flex-start !important;
          }
          .dashboard-workspace-signals {
            width: 100%;
            display: grid !important;
            grid-template-columns: repeat(2, minmax(0, 1fr));
            justify-content: stretch !important;
          }
          .dashboard-workspace-signals > div {
            min-width: 0 !important;
          }
        }
      `}</style>
    </div>
  );
}
