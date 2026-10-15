// web/src/pages/dashboard/components/PanelWrapper.tsx

import { type ReactNode } from "react";
import { Tooltip } from "antd";
import { SyncOutlined } from "@ant-design/icons";
import { AlertTriangle, RefreshCw } from "lucide-react";
import { useAppStore } from "@/store/useAppStore";
import { LAYOUT, getColors, TRANSITION } from "../styles";
import { useTranslation } from "react-i18next";
import { DataStatusBadge } from "./DataStatusBadge";

// ── Shimmer skeleton ────────────────────────────────────────────────
function PanelSkeleton({ darkMode }: { darkMode: boolean }) {
  const shimmerBg = darkMode
    ? "linear-gradient(90deg, rgba(255,255,255,0.03) 0%, rgba(255,255,255,0.08) 50%, rgba(255,255,255,0.03) 100%)"
    : "linear-gradient(90deg, rgba(0,0,0,0.03) 0%, rgba(0,0,0,0.06) 50%, rgba(0,0,0,0.03) 100%)";

  return (
    <div style={{ padding: LAYOUT.CARD_PADDING }}>
      {[1, 0.7, 0.5, 0.8].map((w, i) => (
        <div
          key={i}
          style={{
            height: i === 0 ? 20 : 14,
            width: `${w * 100}%`,
            borderRadius: 6,
            backgroundImage: shimmerBg,
            backgroundSize: "200px 100%",
            backgroundRepeat: "no-repeat",
            animation: "shimmer 1.5s ease-in-out infinite",
            marginBottom: 12,
          }}
        />
      ))}
      <style>{`@keyframes shimmer { 0% { background-position: -200px 0; } 100% { background-position: calc(200px + 100%) 0; } }`}</style>
    </div>
  );
}

// ── Error state ─────────────────────────────────────────────────────
function PanelError({ onRetry, darkMode }: { onRetry?: () => void; darkMode: boolean }) {
  const { t } = useTranslation();
  const colors = getColors(darkMode);

  return (
    <div
      style={{
        display: "flex",
        flexDirection: "column",
        alignItems: "center",
        justifyContent: "center",
        padding: 32,
        gap: 12,
        color: colors.textSecondary,
      }}
    >
      <AlertTriangle size={24} color={colors.warning} />
      <span style={{ fontSize: 13, fontWeight: 500 }}>
        {t("dashboard.panel.loadError", "数据加载失败")}
      </span>
      {onRetry && (
        <button
          onClick={onRetry}
          style={{
            display: "flex",
            alignItems: "center",
            gap: 6,
            padding: "6px 14px",
            borderRadius: 8,
            border: `1px solid ${colors.border}`,
            background: colors.cardBg,
            color: colors.accent,
            fontSize: 13,
            fontWeight: 500,
            cursor: "pointer",
            transition: TRANSITION.fast,
          }}
        >
          <RefreshCw size={14} />
          {t("common.retry", "重试")}
        </button>
      )}
    </div>
  );
}

// ── Panel Wrapper ───────────────────────────────────────────────────
interface PanelWrapperProps {
  title: string;
  children: ReactNode;
  loading?: boolean;
  onRefresh?: () => void;
  extra?: ReactNode;
  className?: string;
  noPadding?: boolean;
  dataStatus?: string;
  dataFreshnessMinutes?: number;
}

export function PanelWrapper({
  title,
  children,
  loading = false,
  onRefresh,
  extra,
  className = "",
  noPadding = false,
  dataStatus,
  dataFreshnessMinutes,
}: PanelWrapperProps) {
  const { t } = useTranslation();
  const { darkMode } = useAppStore();
  const colors = getColors(darkMode);
  const isError = ["error", "collection_error", "unavailable"].includes(dataStatus || "");

  return (
    <div
      className={`h-full flex flex-col ${className}`}
      style={{
        borderRadius: LAYOUT.CARD_RADIUS,
        border: `1px solid ${colors.borderSoft}`,
        background: colors.cardBg,
        boxShadow: colors.shadowSm,
        overflow: "hidden",
        height: "100%",
        transition: TRANSITION.normal,
      }}
    >
      {/* Header */}
      <div
        style={{
          display: "flex",
          alignItems: "center",
          justifyContent: "space-between",
          padding: `0 ${LAYOUT.CARD_PADDING}px`,
          height: 42,
          borderBottom: `1px solid ${colors.borderSoft}`,
          background: darkMode ? "rgba(255,255,255,0.018)" : "#fff",
          flexShrink: 0,
          gap: 10,
          minWidth: 0,
        }}
      >
        <div style={{ display: "flex", alignItems: "center", gap: 8, minWidth: 0, flex: "1 1 auto" }}>
          <span
            style={{
              fontSize: 15,
              fontWeight: 650,
              color: colors.textPrimary,
              letterSpacing: "0",
              overflow: "hidden",
              textOverflow: "ellipsis",
              whiteSpace: "nowrap",
            }}
          >
            {title}
          </span>
          {dataStatus !== undefined && (
            <DataStatusBadge dataStatus={dataStatus} dataFreshnessMinutes={dataFreshnessMinutes} />
          )}
        </div>

        <div style={{ display: "flex", alignItems: "center", gap: 12, flexShrink: 0 }}>
          {extra}
          {onRefresh && (
            <Tooltip title={t("common.refresh")}>
              <button
                type="button"
                onClick={onRefresh}
                aria-label={t("common.refresh")}
                style={{
                  width: 32,
                  height: 32,
                  borderRadius: 8,
                  background: colors.cardBg,
                  display: "flex",
                  alignItems: "center",
                  justifyContent: "center",
                  cursor: "pointer",
                  transition: TRANSITION.normal,
                  border: `1px solid ${colors.borderSoft}`,
                }}
                onMouseEnter={(e) => {
                  e.currentTarget.style.background = darkMode ? colors.borderHover : colors.border;
                  e.currentTarget.style.transform = "rotate(180deg)";
                }}
                onMouseLeave={(e) => {
                  e.currentTarget.style.background = colors.cardBg;
                  e.currentTarget.style.transform = "rotate(0deg)";
                }}
              >
                <SyncOutlined
                  spin={loading}
                  style={{
                    fontSize: 14,
                    color: colors.textPrimary,
                  }}
                />
              </button>
            </Tooltip>
          )}
        </div>
      </div>

      {/* Body */}
      <div
        style={{
          padding: noPadding ? 0 : LAYOUT.CARD_PADDING,
          flex: 1,
          overflow: noPadding ? "hidden" : "auto",
          position: "relative",
          minHeight: 0,
          minWidth: 0,
          overscrollBehavior: "contain",
        }}
      >
        {isError ? (
          <PanelError onRetry={onRefresh} darkMode={darkMode} />
        ) : loading ? (
          <PanelSkeleton darkMode={darkMode} />
        ) : (
          children
        )}
      </div>
    </div>
  );
}
