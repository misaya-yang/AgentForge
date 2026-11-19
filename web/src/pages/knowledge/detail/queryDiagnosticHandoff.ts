import type { QueryFeedback, QueryHistoryItem } from "@/types/knowledge";

/** Only fields from a currently authorized observation, never arbitrary metadata. */
export interface QueryDiagnosticHandoff {
  datasetId: string;
  observationId: string;
  origin: "query" | "feedback";
  question: string;
  observedAt: string;
  traceId?: string;
}

export function queryDiagnosticHandoff(
  datasetId: string | undefined,
  observation: QueryHistoryItem | QueryFeedback,
): QueryDiagnosticHandoff | null {
  if (!datasetId || observation.dataset_id !== datasetId) return null;
  const feedback = "feedback_id" in observation;
  const id = feedback ? observation.feedback_id : observation.id;
  if (typeof id !== "string" || !id) return null;
  const question = feedback ? observation.query_content : observation.content;
  const traceId = typeof observation.trace_id === "string"
    && /^[a-zA-Z0-9][a-zA-Z0-9._:-]{0,127}$/.test(observation.trace_id)
    ? observation.trace_id : undefined;
  return {
    datasetId, observationId: id, origin: feedback ? "feedback" : "query",
    question: typeof question === "string" ? question.trim() : "",
    observedAt: observation.created_at,
    traceId,
  };
}

/** Trace authorization and missing/expired handling stay with the existing Eval reader. */
export function queryDiagnosticTraceUrl(observation: QueryDiagnosticHandoff): string | null {
  if (!observation.traceId) return null;
  const params = new URLSearchParams({
    dataset_id: observation.datasetId, tab: "traces", family: "rag", trace_id: observation.traceId,
  });
  return `/eval?${params.toString()}`;
}
