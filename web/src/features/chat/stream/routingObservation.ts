/** Upstream-reported selection only: never execution, accounting, or retry authority. */
export interface ReportedRouting {
  source: "upstream_reported";
  status: "selected";
  provider_id: string;
  model_id: string;
  reason?: string;
}
const reasons = new Set(["primary_unavailable", "upstream_error", "rate_limited", "timeout", "unavailable", "fallback", "initial"]);
function identifier(value: unknown): value is string {
  return typeof value === "string" && /^[A-Za-z0-9][A-Za-z0-9._/-]{0,127}$/.test(value) &&
    !value.includes("//") && !/(?:^|\/)(?:sk-|Bearer|api[_-]?key|token|secret|gh[pousr]_|github_pat_|xox[baprs]-|AKIA|ASIA)/i.test(value) &&
    !/(?:^|\/)eyJ[A-Za-z0-9_-]*\.eyJ[A-Za-z0-9_-]*\.[A-Za-z0-9_-]+/.test(value);
}
export function readReportedRouting(value: unknown): ReportedRouting | undefined {
  if (!value || typeof value !== "object" || Array.isArray(value)) return undefined;
  const data = value as Record<string, unknown>;
  if (data.status !== "selected" || !identifier(data.provider_id) || !identifier(data.model_id)) return undefined;
  return { source: "upstream_reported", status: "selected", provider_id: data.provider_id, model_id: data.model_id,
    ...(typeof data.reason === "string" && reasons.has(data.reason) ? { reason: data.reason } : {}) };
}
