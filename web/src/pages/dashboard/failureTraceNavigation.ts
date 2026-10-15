import type { FailureBreakdownItem } from "@/api/usage";
import type { TraceFilter } from "./types";

export const failureDimensions = ["service", "model", "provider"] as const;
export type FailureDimension = typeof failureDimensions[number];
const traceKeys = ["trace_status", "trace_error", "trace_service", "trace_model", "trace_provider", "trace_sample", "trace_origin", "trace_dimension", "trace_value"];
export function clearFailureTrace(params: URLSearchParams): URLSearchParams {
  const next = new URLSearchParams(params); traceKeys.forEach(key => next.delete(key)); return next;
}
export function readTraceFilter(params: URLSearchParams): TraceFilter {
  const status = params.get("trace_status");
  return { status: status === "error" || status === "success" ? status : undefined,
    error_type: params.get("trace_error") || undefined, service_id: params.get("trace_service") || undefined,
    model: params.get("trace_model") || undefined, provider: params.get("trace_provider") || undefined,
    sample_reason: params.get("trace_sample") || undefined };
}
export function writeTraceFilter(params: URLSearchParams, filter: TraceFilter): URLSearchParams {
  const next = clearFailureTrace(params);
  for (const [field, key] of Object.entries({ status: "trace_status", error_type: "trace_error", service_id: "trace_service", model: "trace_model", provider: "trace_provider", sample_reason: "trace_sample" })) {
    const value = filter[field as keyof TraceFilter]; if (value) next.set(key, value);
  }
  return next;
}
export function canDrillIntoFailureGroup(item: FailureBreakdownItem): boolean {
  // The aggregate normalizes NULL/empty errors to "unknown", but trace equality
  // filters raw error_type. Do not silently drop that filter or imply a match.
  return failureDimensions.includes(item.dimension as FailureDimension) && Boolean(item.error_type) && item.error_type !== "unknown";
}
export function failureTraceLocation(params: URLSearchParams, item: FailureBreakdownItem, context: { dateRange: [string, string]; serviceId: string; userId: string }): URLSearchParams {
  if (!canDrillIntoFailureGroup(item)) return new URLSearchParams(params);
  const next = writeTraceFilter(params, { status: "error", error_type: item.error_type,
    ...(item.dimension === "service" ? { service_id: item.dimension_value } : item.dimension === "model" ? { model: item.dimension_value } : { provider: item.dimension_value }) });
  next.set("from", context.dateRange[0]); next.set("to", context.dateRange[1]); next.set("service", context.serviceId); next.set("user", context.userId);
  next.set("trace_origin", params.get("tab") === "tracing" ? "tracing" : "reliability"); next.set("trace_dimension", item.dimension); next.set("trace_value", item.dimension_value);
  next.set("failure_dimension", item.dimension); next.set("tab", "tracing"); return next;
}
export function failureTraceReturn(params: URLSearchParams): URLSearchParams {
  const next = clearFailureTrace(params); next.set("tab", params.get("trace_origin") === "tracing" ? "tracing" : "reliability"); return next;
}

export function dashboardDateRange(params: URLSearchParams, fallback: [string, string]): [string, string] {
  const from = params.get("from"); const to = params.get("to");
  const valid = (value: string | null): value is string => Boolean(value && /^\d{4}-\d{2}-\d{2}$/.test(value) && Number.isFinite(Date.parse(`${value}T00:00:00Z`)) && new Date(`${value}T00:00:00Z`).toISOString().slice(0, 10) === value);
  return valid(from) && valid(to) && from <= to ? [from, to] : fallback;
}
