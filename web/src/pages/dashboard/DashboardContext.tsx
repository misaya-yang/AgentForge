// web/src/pages/dashboard/DashboardContext.tsx

import { createContext, useContext, useState, useCallback, useMemo, type ReactNode } from "react";
import { useSearchParams } from "react-router-dom";
import { dashboardDateRange, clearFailureTrace, readTraceFilter, writeTraceFilter } from "./failureTraceNavigation";
import { useDashboardRefresh } from "./hooks/useDashboardRefresh";
import dayjs from "dayjs";
import type { DashboardContext, SourceFilter, ServiceFilter, UserFilter, RefreshInterval, TraceFilter } from "./types";

interface DashboardContextValue extends DashboardContext {
  setDateRange: (range: [string, string]) => void;
  setGranularity: (granularity: "hour" | "day") => void;
  setSource: (source: SourceFilter) => void;
  setServiceId: (serviceId: ServiceFilter) => void;
  setUserId: (userId: UserFilter) => void;
  setRefreshInterval: (interval: RefreshInterval) => void;
  triggerRefresh: () => void;
  setTraceFilter: (filter: TraceFilter) => void;
  clearTraceFilter: () => void;
}

const Context = createContext<DashboardContextValue | null>(null);

export function DashboardProvider({ children }: { children: ReactNode }) {
  const [defaultRange] = useState<[string, string]>([
    dayjs().subtract(7, "day").format("YYYY-MM-DD"),
    dayjs().format("YYYY-MM-DD"),
  ]);
  const [granularity, setGranularity] = useState<"hour" | "day">("day");
  const [source, setSource] = useState<SourceFilter>("all");
  const [params, setParams] = useSearchParams();
  const dateRange = useMemo<[string, string]>(() => dashboardDateRange(params, defaultRange), [params, defaultRange]);
  const serviceId = params.get("service") || "all";
  const userId = params.get("user") || "all";
  const setDateRange = useCallback((range: [string, string]) => setParams(current => { const next = clearFailureTrace(current); next.set("from", range[0]); next.set("to", range[1]); return next; }), [setParams]);
  const setServiceId = useCallback((value: ServiceFilter) => setParams(current => { const next = clearFailureTrace(current); next.set("service", value); return next; }), [setParams]);
  const setUserId = useCallback((value: UserFilter) => setParams(current => { const next = clearFailureTrace(current); next.set("user", value); return next; }), [setParams]);
  const [refreshInterval, setRefreshInterval] = useState<RefreshInterval>(60);
  const { lastRefresh, triggerRefresh, refreshMode } = useDashboardRefresh(refreshInterval);
  const traceFilter = useMemo(() => readTraceFilter(params), [params]);
  const setTraceFilter = useCallback((filter: TraceFilter) => setParams(current => writeTraceFilter(current, filter)), [setParams]);

  const clearTraceFilter = useCallback(() => {
    setTraceFilter({});
  }, [setTraceFilter]);

  // Memoize context value to prevent unnecessary re-renders of consumers
  const value = useMemo<DashboardContextValue>(
    () => ({
      dateRange,
      granularity,
      source,
      serviceId,
      userId,
      refreshInterval,
      lastRefresh,
      refreshMode,
      traceFilter,
      setDateRange,
      setGranularity,
      setSource,
      setServiceId,
      setUserId,
      setRefreshInterval,
      triggerRefresh,
      setTraceFilter,
      clearTraceFilter,
    }),
    [dateRange, granularity, source, serviceId, userId, refreshInterval, lastRefresh, refreshMode, traceFilter, triggerRefresh, clearTraceFilter, setTraceFilter, setDateRange, setServiceId, setUserId]
  );

  return <Context.Provider value={value}>{children}</Context.Provider>;
}

// eslint-disable-next-line react-refresh/only-export-components
export function useDashboardContext() {
  const context = useContext(Context);
  if (!context) {
    throw new Error("useDashboardContext must be used within DashboardProvider");
  }
  return context;
}
