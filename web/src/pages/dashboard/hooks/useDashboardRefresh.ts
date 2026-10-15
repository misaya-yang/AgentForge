import { useCallback, useEffect, useRef, useState } from "react";
import type { RefreshInterval } from "../types";
import { createDashboardRefreshScheduler } from "../dashboardRefreshScheduler";

export function useDashboardRefresh(interval: RefreshInterval) {
  const [lastRefresh, setLastRefresh] = useState(() => new Date());
  const [visible, setVisible] = useState(() => typeof document === "undefined" || document.visibilityState === "visible");
  const scheduler = useRef<ReturnType<typeof createDashboardRefreshScheduler> | null>(null);
  useEffect(() => {
    const current = createDashboardRefreshScheduler({
      intervalMs: interval * 1000,
      now: () => performance.now(),
      visible: () => document.visibilityState === "visible",
      schedule: (callback, delay) => { const timer = setTimeout(callback, delay); return () => clearTimeout(timer); },
      onRequest: () => setLastRefresh(new Date()),
      onVisibility: setVisible,
    });
    scheduler.current = current;
    document.addEventListener("visibilitychange", current.visibilityChanged);
    return () => {
      current.dispose(); document.removeEventListener("visibilitychange", current.visibilityChanged);
      if (scheduler.current === current) scheduler.current = null;
    };
  }, [interval]);
  const triggerRefresh = useCallback(() => scheduler.current?.manual(), []);
  const refreshMode: "manual" | "automatic" | "paused" = interval === 0 ? "manual" : visible ? "automatic" : "paused";
  return { lastRefresh, triggerRefresh, refreshMode };
}
