// web/src/pages/dashboard/components/DataStatusBadge.tsx
// 数据新鲜度指示器 - 显示面板数据来源状态

import { Tooltip } from "antd";
import { useTranslation } from "react-i18next";

export type DataStatus = "live" | "stale" | "no_data" | "error";

interface DataStatusBadgeProps {
  dataStatus?: string;
  dataFreshnessMinutes?: number;
}

const STATUS_CONFIG: Record<DataStatus, { color: string; label: string; bgColor: string }> = {
  live: { color: "hsl(var(--success))", label: "dashboard.dataStatus.live", bgColor: "hsl(var(--success) / 0.15)" },
  stale: { color: "hsl(var(--warning))", label: "dashboard.dataStatus.stale", bgColor: "hsl(var(--warning) / 0.15)" },
  error: { color: "hsl(var(--destructive))", label: "dashboard.dataStatus.unknown", bgColor: "hsl(var(--destructive) / 0.15)" },
  no_data: { color: "hsl(var(--muted-foreground))", label: "dashboard.dataStatus.noData", bgColor: "hsl(var(--muted) / 0.75)" },
};

function resolveStatus(dataStatus?: string, freshnessMinutes?: number): DataStatus {
  if (["error", "collection_error", "unavailable"].includes(dataStatus || "")) return "error";
  if (dataStatus === "no_data" || dataStatus === "empty") return "no_data";
  if (dataStatus === "stale" || dataStatus === "delayed") return "stale";
  if (dataStatus === "ok") return "live";
  if (dataStatus === "live") {
    // Double check with freshness
    if (freshnessMinutes !== undefined && freshnessMinutes >= 5) return "stale";
    return "live";
  }
  // Infer from freshness if dataStatus not provided
  if (freshnessMinutes === undefined || freshnessMinutes < 0) return "no_data";
  if (freshnessMinutes < 5) return "live";
  return "stale";
}

export function DataStatusBadge({ dataStatus, dataFreshnessMinutes }: DataStatusBadgeProps) {
  const { t } = useTranslation();
  const status = resolveStatus(dataStatus, dataFreshnessMinutes);
  const config = STATUS_CONFIG[status];

  const freshnessText = dataFreshnessMinutes !== undefined && dataFreshnessMinutes >= 0
    ? t("dashboard.dataStatus.freshnessMinutes", { minutes: Math.round(dataFreshnessMinutes) })
    : t("dashboard.dataStatus.unknown");

  return (
    <Tooltip
      title={
        <div>
          <div>{t(config.label, status)}</div>
          <div style={{ fontSize: 11, opacity: 0.8, marginTop: 2 }}>{freshnessText}</div>
        </div>
      }
    >
      <div
        style={{
          display: "inline-flex",
          alignItems: "center",
          gap: 4,
          padding: "2px 8px",
          borderRadius: 10,
          background: config.bgColor,
          cursor: "help",
        }}
      >
        <div
          style={{
            width: 6,
            height: 6,
            borderRadius: "50%",
            background: config.color,
            boxShadow: status === "live" ? `0 0 6px ${config.color}` : "none",
          }}
        />
        <span style={{ fontSize: 10, fontWeight: 500, color: config.color }}>
          {status === "live" ? "LIVE" : status === "stale" ? "STALE" : status === "error" ? "ERROR" : "N/A"}
        </span>
      </div>
    </Tooltip>
  );
}
