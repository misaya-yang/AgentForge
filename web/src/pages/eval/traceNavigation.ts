import type { EvalTraceExportResponse, TraceFamily } from "@/api/eval";

export function traceFamilyFromParam(value: string | null): TraceFamily {
  return value === "rag" || value === "langgraph_proxy" ? value : "assistant";
}

/** Only an absent target may select the first row. Missing/denied targets stay selected. */
export function selectedTraceFromNavigation(
  requestedTraceId: string | undefined,
  traces: readonly { trace_id: string }[],
): string | undefined {
  return requestedTraceId || traces[0]?.trace_id;
}

/** One navigation retains source filters and writes the target family and ID together. */
export function traceNavigationParams(
  current: URLSearchParams,
  family: TraceFamily,
  traceId?: string,
  tab?: string,
): URLSearchParams {
  const next = new URLSearchParams(current);
  next.set("family", family);
  if (traceId) next.set("trace_id", traceId);
  else next.delete("trace_id");
  if (tab) next.set("tab", tab);
  return next;
}

export interface TraceExportBinding {
  accessKey: string;
  navigationEpoch: string;
  family: TraceFamily;
  traceId: string | undefined;
}

export interface BoundTraceExport {
  binding: TraceExportBinding;
  result: EvalTraceExportResponse;
}

export function sameTraceExportBinding(binding: TraceExportBinding, current: TraceExportBinding): boolean {
  return Boolean(current.traceId)
    && binding.accessKey === current.accessKey
    && binding.navigationEpoch === current.navigationEpoch
    && binding.family === current.family
    && binding.traceId === current.traceId;
}

export function currentTraceExport(
  observed: BoundTraceExport | null,
  current: TraceExportBinding,
): EvalTraceExportResponse | null {
  return observed && sameTraceExportBinding(observed.binding, current)
    && observed.result.trace_id === current.traceId
    ? observed.result : null;
}
