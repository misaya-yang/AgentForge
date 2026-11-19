// web/src/pages/dashboard/components/panels/UserQuotaPanel.tsx
// 用户配额面板 - 接入真实配额数据

import { Table, Progress, Tag, Select, Tooltip, Button } from "antd";
import { useQuery } from "@tanstack/react-query";
import { WarningOutlined, ExclamationCircleOutlined, ExpandOutlined, StopOutlined } from "@ant-design/icons";
import { PanelWrapper } from "../PanelWrapper";
import { useDashboardContext } from "../../DashboardContext";
import { useAppStore } from "@/store/useAppStore";
import { getQuotaUsersOverview, type QuotaUserOverviewItem } from "@/api/usage";
import { useState } from "react";
import { useTranslation } from "react-i18next";
import { useDashboardEntityLabels } from "../../hooks/useDashboardEntityLabels";
import { getColors } from "../../styles";

function formatTokens(num: number): string {
  if (num >= 1_000_000) return `${(num / 1_000_000).toFixed(1)}M`;
  if (num >= 1_000) return `${(num / 1_000).toFixed(1)}K`;
  return num.toLocaleString();
}

const STRATEGY_COLORS: Record<string, string> = {
  hard_block: "red",
  rate_limit: "orange",
  downgrade_model: "blue",
  allow_but_alert: "gold",
};

export function UserQuotaPanel() {
  const { t } = useTranslation();
  const { darkMode } = useAppStore();
  const colors = getColors(darkMode);
  const { lastRefresh } = useDashboardContext();
  const { resolveUserLabel } = useDashboardEntityLabels();
  const [sortBy, setSortBy] = useState<"daily_tokens" | "monthly_cost" | "status">("daily_tokens");

  const { data, isLoading, refetch } = useQuery({
    queryKey: ["dashboard-user-quota-overview", sortBy, lastRefresh.getTime()],
    queryFn: () =>
      getQuotaUsersOverview({
        limit: 20,
        sort_by: sortBy,
      }),
    staleTime: 30000,
  });

  const users = data?.users || [];
  const summary = data?.summary || { total: 0, blocked: 0, exceeded: 0, warning: 0, ok: 0 };
  const warningCount = summary.warning + summary.exceeded + summary.blocked;

  const columns = [
    {
      title: t("common.user"),
      dataIndex: "user_id",
      key: "user_id",
      width: 100,
      ellipsis: true,
      render: (text: string) => (
        <span style={{ fontWeight: 500, color: colors.textPrimary }}>
          {resolveUserLabel(text)}
        </span>
      ),
    },
    {
      title: t("metrics.totalTokens"),
      key: "daily",
      width: 130,
      render: (_: unknown, record: QuotaUserOverviewItem) => {
        const observation = record.limit_observation;
        const limit = observation ? observation.effective.daily_tokens : record.daily_tokens_limit;
        const hasLimit = limit !== null && limit !== 0;
        const percent = hasLimit ? Math.min((record.daily_tokens_used / limit!) * 100, 100) : 0;
        const baseLabel = record.daily_tokens_limit === null || record.daily_tokens_limit === 0
          ? "∞" : formatTokens(record.daily_tokens_limit);
        const detail = observation
          ? t("dashboard.userQuota.observationDetail", {
              observed: observation.observed_at,
              source: t(`dashboard.userQuota.limitSources.${observation.sources.daily_tokens}`),
              extra: formatTokens(observation.temporary_boost.extra_tokens),
              active: t(observation.temporary_boost.active
                ? "dashboard.userQuota.boostActive" : "dashboard.userQuota.boostInactive"),
              expires: observation.temporary_boost.expires_at || t("dashboard.userQuota.noExpiry"),
            })
          : t("dashboard.userQuota.configuredOnly");
        return (
          <Tooltip title={detail}>
            <div>
              <div style={{ fontSize: 10, color: colors.textMuted }}>
                {t(observation ? "dashboard.userQuota.effectiveSnapshot" : "dashboard.userQuota.configuredOnly")}
              </div>
              {hasLimit ? (
                <>
                  <Progress
                    percent={percent}
                    size="small"
                    strokeColor={percent >= 80 ? colors.warning : colors.info}
                    showInfo={false}
                  />
                  <div style={{ fontSize: 11, color: colors.textMuted }}>
                    {formatTokens(record.daily_tokens_used)}/{formatTokens(limit!)}
                  </div>
                </>
              ) : (
                <div style={{ fontSize: 11, color: colors.textMuted }}>
                  {formatTokens(record.daily_tokens_used)} / <span style={{ color: colors.success }}>∞</span>
                </div>
              )}
              {observation && (
                <div style={{ fontSize: 10, color: colors.textMuted }}>
                  {t("dashboard.userQuota.configuredLimit", { value: baseLabel })}
                </div>
              )}
            </div>
          </Tooltip>
        );
      },
    },
    {
      title: t("analytics.cost"),
      key: "cost",
      width: 80,
      render: (_: unknown, record: QuotaUserOverviewItem) => {
        const costUsd = record.monthly_cost_used_cents / 100;
        return (
          <span style={{ fontWeight: 500, color: colors.success }}>
            ${costUsd >= 1 ? costUsd.toFixed(2) : costUsd.toFixed(4)}
          </span>
        );
      },
    },
    {
      title: t("dashboard.userQuota.strategy", "策略"),
      key: "strategy",
      width: 70,
      render: (_: unknown, record: QuotaUserOverviewItem) => {
        const color = STRATEGY_COLORS[record.overage_strategy] || "default";
        const label = t(`dashboard.userQuota.strategy_labels.${record.overage_strategy}`, record.overage_strategy);
        return <Tag color={color} style={{ margin: 0, fontSize: 10 }}>{label}</Tag>;
      },
    },
    {
      title: t("common.status"),
      dataIndex: "status",
      key: "status",
      width: 70,
      render: (status: string, record: QuotaUserOverviewItem) => {
        if (record.is_blocked) {
          return (
            <Tooltip title={record.blocked_reason || t("dashboard.userQuota.status.blocked")}>
              <Tag icon={<StopOutlined />} color="error">{t("dashboard.userQuota.status.blocked")}</Tag>
            </Tooltip>
          );
        }
        const config: Record<string, { color: string; text: string }> = {
          ok: { color: "success", text: t("dashboard.userQuota.status.normal") },
          warning: { color: "warning", text: t("dashboard.userQuota.status.warning") },
          exceeded: { color: "error", text: t("dashboard.userQuota.status.exceeded") },
        };
        const c = config[status] || { color: "default", text: status };
        return <Tag color={c.color}>{c.text}</Tag>;
      },
    },
  ];

  return (
    <PanelWrapper
      title={t("dashboard.userQuota.title")}
      onRefresh={refetch}
      loading={isLoading}
      extra={
        <Select
          value={sortBy}
          onChange={setSortBy}
          size="small"
          style={{ width: 100 }}
          options={[
            { value: "daily_tokens", label: t("dashboard.userQuota.sort.usage") },
            { value: "monthly_cost", label: t("dashboard.userQuota.sort.cost") },
            { value: "status", label: t("common.status") },
          ]}
        />
      }
    >
      {/* Summary badges */}
      <div
        style={{
          display: "flex",
          gap: 8,
          marginBottom: 12,
          flexWrap: "wrap",
        }}
      >
        <Tag style={{ margin: 0 }}>{t("dashboard.userQuota.total", "总计")}: {summary.total}</Tag>
        {summary.blocked > 0 && (
          <Tag color="error" style={{ margin: 0 }}>{t("dashboard.userQuota.status.blocked", "封禁")}: {summary.blocked}</Tag>
        )}
        {summary.exceeded > 0 && (
          <Tag color="warning" style={{ margin: 0 }}>{t("dashboard.userQuota.status.exceeded")}: {summary.exceeded}</Tag>
        )}
        {summary.warning > 0 && (
          <Tag color="gold" style={{ margin: 0 }}>{t("dashboard.userQuota.status.warning")}: {summary.warning}</Tag>
        )}
      </div>

      {/* Warning summary - more prominent for exceeded users */}
      {warningCount > 0 && (
        <div
          style={{
            padding: "12px 16px",
            marginBottom: 12,
            borderRadius: 8,
            background: summary.exceeded > 0 || summary.blocked > 0
              ? colors.errorBg
              : colors.warningBg,
            border: summary.exceeded > 0 || summary.blocked > 0
              ? `1px solid ${colors.error}66`
              : `1px solid ${colors.warning}66`,
          }}
        >
          <div style={{ display: "flex", alignItems: "center", justifyContent: "space-between" }}>
            <div style={{ display: "flex", alignItems: "center", gap: 8 }}>
              {summary.exceeded > 0 || summary.blocked > 0 ? (
                <ExclamationCircleOutlined style={{ fontSize: 16, color: colors.error }} />
              ) : (
                <WarningOutlined style={{ fontSize: 16, color: colors.warning }} />
              )}
              <div>
                <div
                  style={{
                    fontSize: 13,
                    fontWeight: 600,
                    color: summary.exceeded > 0 || summary.blocked > 0 ? colors.error : colors.warning,
                  }}
                >
                  {summary.blocked > 0
                    ? t("dashboard.userQuota.alert.blocked", { count: summary.blocked })
                    : summary.exceeded > 0
                    ? t("dashboard.userQuota.alert.exceeded", { count: summary.exceeded })
                    : t("dashboard.userQuota.alert.warning", { count: summary.warning })}
                </div>
                <div
                  style={{
                    fontSize: 11,
                    color: colors.textMuted,
                    marginTop: 2,
                  }}
                >
                  {t("dashboard.userQuota.alert.hint")}
                </div>
              </div>
            </div>
            <Tooltip title={t("dashboard.userQuota.alert.upgradeHint")}>
              <Button size="small" type="text" icon={<ExpandOutlined />}>
                {t("dashboard.userQuota.alert.upgradeAction")}
              </Button>
            </Tooltip>
          </div>
        </div>
      )}

      {/* User table */}
      <Table
        dataSource={users}
        columns={columns}
        rowKey="user_id"
        size="small"
        pagination={false}
        scroll={{ x: 540, y: 200 }}
        style={{
          background: "transparent",
        }}
        rowClassName={(record) =>
          record.status === "exceeded" || record.is_blocked
            ? "quota-exceeded-row"
            : record.status === "warning"
            ? "quota-warning-row"
            : ""
        }
      />
      <style>{`
        .quota-exceeded-row {
          background: ${darkMode ? "rgba(239, 68, 68, 0.08)" : "rgba(239, 68, 68, 0.04)"} !important;
        }
        .quota-exceeded-row:hover > td {
          background: ${darkMode ? "rgba(239, 68, 68, 0.12)" : "rgba(239, 68, 68, 0.08)"} !important;
        }
        .quota-warning-row {
          background: ${darkMode ? "rgba(245, 158, 11, 0.06)" : "rgba(245, 158, 11, 0.03)"} !important;
        }
        .quota-warning-row:hover > td {
          background: ${darkMode ? "rgba(245, 158, 11, 0.10)" : "rgba(245, 158, 11, 0.06)"} !important;
        }
      `}</style>
    </PanelWrapper>
  );
}
