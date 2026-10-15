// web/src/pages/dashboard/index.tsx
// 监控面板 — 1:1 port of design-handoff dashboard.jsx
// Title row · FilterBar · Tabs · KPI row · Charts row · Provider table

import { useEffect, useLayoutEffect, useRef, useState, useCallback } from "react";
import { DashboardProvider, useDashboardContext } from "./DashboardContext";
import { DashboardLayout } from "./DashboardLayout";
import { useAppStore } from "@/store/useAppStore";
import { FONT_FAMILY, getColors } from "./styles";
import { Select, DatePicker, Segmented } from "antd";
import { useSearchParams } from "react-router-dom";
import { clearFailureTrace } from "./failureTraceNavigation";
import dayjs from "dayjs";
import { useTranslation } from "react-i18next";
import { DashboardRefreshStatus } from "./components/DashboardRefreshStatus";
import { SetupChecklist } from "./components/SetupChecklist";
import { useDashboardEntityLabels } from "./hooks/useDashboardEntityLabels";
import type { SourceFilter, RefreshInterval } from "./types";

const { RangePicker } = DatePicker;

type DashTab = "summary" | "operations" | "reliability" | "governance" | "tracing";
const DASHBOARD_TAB_STORAGE_KEY = "dashboard-tab-v2";

function loadTab(): DashTab {
  try {
    const s = localStorage.getItem(DASHBOARD_TAB_STORAGE_KEY);
    if (s === "summary" || s === "operations" || s === "reliability" || s === "governance" || s === "tracing") return s;
  } catch { /* */ }
  return "operations";
}

// ── Design-handoff SVG icons (clock, refresh, expand, chev, cal) ─────
const ICON = {
  clock: (
    <svg width="13" height="13" viewBox="0 0 13 13" fill="none">
      <circle cx="6.5" cy="6.5" r="5" stroke="currentColor" strokeWidth="1.3" />
      <path d="M6.5 3.5V6.5L8.5 8" stroke="currentColor" strokeWidth="1.3" strokeLinecap="round" strokeLinejoin="round" />
    </svg>
  ),
  refresh: (
    <svg width="14" height="14" viewBox="0 0 14 14" fill="none">
      <path d="M12 7a5 5 0 11-1.5-3.5M12 2v2h-2" stroke="currentColor" strokeWidth="1.4" strokeLinecap="round" strokeLinejoin="round" />
    </svg>
  ),
  expand: (
    <svg width="14" height="14" viewBox="0 0 14 14" fill="none">
      <path d="M2 5V2h3M12 5V2H9M2 9v3h3M12 9v3H9" stroke="currentColor" strokeWidth="1.4" strokeLinecap="round" strokeLinejoin="round" />
    </svg>
  ),
};

function DashboardContent() {
  const { t } = useTranslation();
  const { darkMode } = useAppStore();
  const c = getColors(darkMode);
  const containerRef = useRef<HTMLDivElement>(null);
  const [containerWidth, setContainerWidth] = useState(0);
  const [defaultTab] = useState<DashTab>(loadTab);
  const [searchParams, setSearchParams] = useSearchParams();
  const requestedTab = searchParams.get("tab");
  const activeTab: DashTab = requestedTab === "summary" || requestedTab === "operations" || requestedTab === "reliability" || requestedTab === "governance" || requestedTab === "tracing" ? requestedTab : defaultTab;
  // Canonicalize the restored tab before interactive children can drill down.
  useLayoutEffect(() => {
    if (requestedTab === activeTab) return;
    setSearchParams(current => { const next = new URLSearchParams(current); next.set("tab", activeTab); return next; }, { replace: true });
  }, [requestedTab, activeTab, setSearchParams]);
  const effectiveWidth = containerWidth || 1200;

  const {
    dateRange, granularity, source, serviceId, userId, refreshInterval, lastRefresh, refreshMode,
    setDateRange, setGranularity, setSource, setServiceId, setUserId, setRefreshInterval, triggerRefresh,
  } = useDashboardContext();

  const { serviceOptions, userOptions } = useDashboardEntityLabels();

  const resizeTimeout = useRef<ReturnType<typeof setTimeout> | undefined>(undefined);
  const handleResize = useCallback(() => {
    clearTimeout(resizeTimeout.current);
    resizeTimeout.current = setTimeout(() => {
      if (containerRef.current) setContainerWidth(containerRef.current.offsetWidth);
    }, 150);
  }, []);

  useEffect(() => {
    if (containerRef.current) setContainerWidth(containerRef.current.offsetWidth);
    const ro = new ResizeObserver(handleResize);
    if (containerRef.current) ro.observe(containerRef.current);
    return () => { ro.disconnect(); clearTimeout(resizeTimeout.current); };
  }, [handleResize]);

  const handleTabChange = (tab: DashTab) => {
    setSearchParams(current => { const next = clearFailureTrace(current); next.set("tab", tab); return next; });
    try { localStorage.setItem(DASHBOARD_TAB_STORAGE_KEY, tab); } catch { /* */ }
  };

  const tabs: { key: DashTab; label: string }[] = [
    { key: "summary", label: t("dashboard.tabs.summary", "总览") },
    { key: "operations", label: t("dashboard.tabs.operations", "运营") },
    { key: "reliability", label: t("dashboard.tabs.reliability", "可靠性") },
    { key: "governance", label: t("dashboard.tabs.governance", "治理") },
    { key: "tracing", label: t("dashboard.tabs.tracing", "追踪") },
  ];

  const workspaceMap: Record<string, string> = {
    summary: "overview",
    operations: "operations", reliability: "reliability", governance: "governance", tracing: "tracing",
  };

  const refreshOptions: { label: string; value: RefreshInterval }[] = [
    { label: t("dashboard.refresh.manual", "手动"), value: 0 },
    { label: t("dashboard.refresh.30s", "30秒"), value: 30 },
    { label: t("dashboard.refresh.1min", "1分钟"), value: 60 },
    { label: t("dashboard.refresh.5min", "5分钟"), value: 300 },
  ];

  return (
    <div
      ref={containerRef}
      className="dashboard-scroll-shell"
      style={{
        minHeight: "100%",
        width: "100%",
        maxWidth: "100%",
        overflowX: "auto",
        background: c.pageBg,
        fontFamily: FONT_FAMILY.sans,
        color: c.textPrimary,
        overscrollBehaviorX: "contain",
      }}
    >
      <div className="dashboard-scroll-surface" style={{ minWidth: 0, width: "100%" }}>

        {/* ── First-run checklist (replaces the dashboard until a provider is configured) ── */}
        <SetupChecklist />

        {/* ── Command header ── */}
        <div style={{
          display: "flex",
          flexDirection: "column",
          gap: 10,
          padding: "2px 0 0",
        }}>
          <div className="dashboard-command-row" style={{
            display: "flex",
            alignItems: "center",
            gap: 10,
            justifyContent: "space-between",
            flexWrap: "wrap",
            minWidth: 0,
          }}>
            <Segmented
              size="middle"
              value={activeTab}
              onChange={(value) => handleTabChange(value as DashTab)}
              className="dash-tabs ui-scroll-affordance"
              options={tabs.map((tab) => ({ value: tab.key, label: tab.label }))}
            />

            <div className="dashboard-command-spacer" style={{ flex: 1, minWidth: 12 }} />

            <div className="dashboard-refresh-stamp" style={{
              display: "flex", alignItems: "center", gap: 6,
              padding: "5px 10px", borderRadius: 8,
              background: c.cardBg, border: `1px solid ${c.borderSoft}`,
              fontSize: 12, color: c.textSecondary, fontFamily: FONT_FAMILY.mono,
            }}>
              <span style={{ display: "flex", color: c.textMuted }}>{ICON.clock}</span>
              <DashboardRefreshStatus requestedAt={lastRefresh} mode={refreshMode} />
            </div>
          </div>

          {/* ── Filter bar ── */}
          <div className="dashboard-filter-bar" style={{
            display: "flex", alignItems: "center", gap: 10,
            flexWrap: "wrap",
            minWidth: 0,
            paddingBottom: 12,
            borderBottom: `1px solid ${c.borderSoft}`,
          }}>
            <Select
              size="middle"
              value={serviceId}
              onChange={setServiceId}
              options={serviceOptions}
              style={{ width: 170, flex: "0 0 170px" }}
              className="dash-filter-select"
            />
            <Select
              size="middle"
              value={userId}
              onChange={setUserId}
              options={userOptions}
              style={{ width: 160, flex: "0 0 160px" }}
              className="dash-filter-select"
            />
            <Select
              size="middle"
              value={source}
              onChange={(v: string) => setSource(v as SourceFilter)}
              options={[
                { label: t("dashboard.filters.allSources", "全部来源"), value: "all" },
                { label: t("dashboard.filters.internal", "内部"), value: "internal" },
                { label: t("dashboard.filters.external", "外部"), value: "external" },
              ]}
              style={{ width: 140, flex: "0 0 140px" }}
              className="dash-filter-select"
            />
            <RangePicker
              size="middle"
              value={[dayjs(dateRange[0]), dayjs(dateRange[1])]}
              onChange={(d) => d && d[0] && d[1] && setDateRange([d[0].format("YYYY-MM-DD"), d[1].format("YYYY-MM-DD")])}
              format="YYYY-MM-DD"
              allowClear={false}
              style={{ width: 300, flex: "0 0 300px" }}
              className="dash-filter-range"
            />
            <Select
              size="middle"
              value={granularity}
              onChange={setGranularity}
              options={[
                { label: t("dashboard.filters.byDay", "按天"), value: "day" },
                { label: t("dashboard.filters.byHour", "按小时"), value: "hour" },
              ]}
              style={{ width: 96 }}
              className="dash-filter-select"
            />
            <div className="dashboard-filter-spacer" style={{ flex: 1, minWidth: 16 }} />
            <Select
              size="middle"
              value={refreshInterval}
              onChange={setRefreshInterval}
              options={refreshOptions}
              style={{ width: 96 }}
              className="dash-filter-select"
            />
            <button onClick={triggerRefresh} className="dash-icon-btn" aria-label="refresh">{ICON.refresh}</button>
            <button
              onClick={() => document.documentElement.requestFullscreen?.()}
              className="dash-icon-btn"
              aria-label="fullscreen"
            >
              {ICON.expand}
            </button>
          </div>
        </div>

        {/* ── Tab content ── */}
        <div style={{ padding: "16px 0 20px", minWidth: 0 }}>
          <DashboardLayout
            width={effectiveWidth}
            forceWorkspace={workspaceMap[activeTab] as "overview" | "operations" | "reliability" | "governance" | "tracing"}
          />
        </div>
      </div>

      <style>{`
        /* Filter select — match design's 34px pill */
        .dash-filter-select .ant-select-selector {
          height: 34px !important;
          border-radius: 8px !important;
          border-color: ${c.border} !important;
          background: ${c.cardBg} !important;
          padding: 0 12px !important;
          display: flex;
          align-items: center;
          font-size: 12.5px !important;
        }
        .dash-filter-select .ant-select-selection-item {
          line-height: 32px !important;
          color: ${c.textPrimary};
        }
        .dash-filter-select .ant-select-arrow { color: ${c.textFaint}; }
        .dash-filter-select:hover .ant-select-selector { border-color: ${c.borderHover} !important; }

        /* Range picker — match same 34px pill */
        .dash-filter-range {
          height: 34px !important;
          border-radius: 8px !important;
          border-color: ${c.border} !important;
          background: ${c.cardBg} !important;
          font-family: ${FONT_FAMILY.mono} !important;
          font-size: 12.5px !important;
        }
        .dash-filter-range:hover { border-color: ${c.borderHover} !important; }
        .dash-filter-range .ant-picker-input input {
          font-family: ${FONT_FAMILY.mono} !important;
          font-size: 12.5px !important;
          color: ${c.textPrimary} !important;
        }
        .dash-filter-range .ant-picker-suffix { color: ${c.textFaint} !important; }

        .dash-tabs {
          flex: 0 1 auto;
          max-width: 100%;
          background: ${c.cardBg};
          border: 1px solid ${c.borderSoft};
          padding: 3px;
          border-radius: 8px;
        }
        .dash-tabs .ant-segmented-item {
          border-radius: 6px;
          min-height: 28px;
          color: ${c.textSecondary};
        }
        .dash-tabs .ant-segmented-item-selected {
          background: ${c.operatorSoft};
          color: ${c.operator};
          box-shadow: none;
        }
        .dash-tabs .ant-segmented-item-label {
          min-height: 28px;
          line-height: 28px;
          padding: 0 12px;
          font-size: 12.5px;
          font-weight: 600;
        }

        /* Icon buttons in header */
        .dash-icon-btn {
          width: 34px; height: 34px; border-radius: 8px;
          border: none; background: transparent;
          display: inline-flex; align-items: center; justify-content: center;
          color: ${c.textSecondary};
          cursor: pointer;
          transition: color .14s cubic-bezier(0.16, 1, 0.3, 1), background-color .14s cubic-bezier(0.16, 1, 0.3, 1);
        }
        .dash-icon-btn:hover {
          background: ${c.cardHover};
          color: ${c.textPrimary};
        }

        .dash-tabs {
          overflow-x: auto;
          overscroll-behavior-x: contain;
        }
        .dash-tabs .ant-segmented-group {
          width: max-content;
          min-width: 100%;
        }

        @media (max-width: 767px) {
          .dash-icon-btn {
            width: 40px;
            height: 40px;
          }
          .dash-tabs .ant-segmented-item,
          .dash-tabs .ant-segmented-item-label {
            min-height: 40px;
          }
          .dash-tabs .ant-segmented-item-label {
            line-height: 40px;
          }
          .dashboard-scroll-shell {
            overflow-x: hidden !important;
          }
          .dashboard-command-row {
            align-items: stretch !important;
            gap: 8px !important;
          }
          .dashboard-command-spacer {
            display: none;
          }
          .dashboard-refresh-stamp {
            width: 100%;
            justify-content: flex-start;
          }
          .dashboard-filter-bar {
            display: grid !important;
            grid-template-columns: repeat(2, minmax(0, 1fr));
            gap: 8px !important;
          }
          .dashboard-filter-spacer {
            display: none;
          }
          .dashboard-filter-bar .dash-filter-select,
          .dashboard-filter-bar .dash-filter-range {
            width: 100% !important;
            min-width: 0 !important;
            flex: 1 1 auto !important;
          }
          .dashboard-filter-bar .dash-filter-range {
            grid-column: 1 / -1;
          }
          .dashboard-filter-bar .dash-icon-btn {
            width: 100%;
            border: 1px solid ${c.borderSoft};
            background: ${c.cardBg};
          }
        }
      `}</style>
    </div>
  );
}

export function EnterpriseDashboard() {
  return (
    <DashboardProvider>
      <DashboardContent />
    </DashboardProvider>
  );
}

export default EnterpriseDashboard;
