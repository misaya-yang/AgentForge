import { useEffect } from "react";
import { useLocation } from "react-router-dom";
import { recordRouteCommit } from "./browserTelemetry";

/** Commit-to-next-paint duration, explicitly separate from backend or full navigation latency. */
export function RouteTelemetry() {
  const { pathname, key } = useLocation();
  useEffect(() => {
    const started = performance.now();
    const frame = requestAnimationFrame(() => recordRouteCommit(pathname, performance.now() - started));
    return () => cancelAnimationFrame(frame);
  }, [pathname, key]);
  return null;
}
