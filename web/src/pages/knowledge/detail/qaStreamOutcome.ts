import type { QAResponse } from "../../../types/knowledge.ts";

export type QAStreamOutcome =
  | { phase: "receiving"; text: string }
  | { phase: "interrupted"; text: string }
  | { phase: "completed"; text: string; response: QAResponse };
const record = (value: unknown): value is Record<string, unknown> =>
  value !== null && typeof value === "object" && !Array.isArray(value);
const finite = (value: unknown): value is number => typeof value === "number" && Number.isFinite(value);

/** Validate the service receipt consumed by this view, not provider completeness. */
export function isQAResponse(value: unknown, query: string): value is QAResponse {
  if (!record(value) || value.query !== query || typeof value.answer !== "string"
    || typeof value.model !== "string" || !record(value.retrieval_metadata)
    || !record(value.timing) || !["retrieval_ms", "llm_ms", "total_ms"].every(key =>
      finite((value.timing as Record<string, unknown>)[key]) && Number((value.timing as Record<string, unknown>)[key]) >= 0)
    || !Array.isArray(value.context_segments)) return false;
  return value.context_segments.every(segment => record(segment)
    && typeof segment.segment_id === "string" && typeof segment.document_id === "string"
    && typeof segment.text === "string" && finite(segment.score))
    && ["trace_id", "query_fingerprint"].every(key => value[key] == null || typeof value[key] === "string")
    && (value.tokens_used == null || (finite(value.tokens_used) && value.tokens_used >= 0));
}

export function qaStreamEvent(state: QAStreamOutcome, event: unknown, query: string): QAStreamOutcome {
  if (state.phase !== "receiving") return state;
  if (!record(event)) return { phase: "interrupted", text: state.text };
  if (event.event === "retrieval") return state;
  const data = event.data;
  if (event.event === "delta" && record(data) && typeof data.content === "string") {
    return { phase: "receiving", text: state.text + data.content };
  }
  if (event.event === "done" && record(data) && isQAResponse(data.result, query)) {
    return { phase: "completed", text: data.result.answer, response: data.result };
  }
  // An error or unrecognized/malformed frame never authorizes a replacement POST.
  return { phase: "interrupted", text: state.text };
}

export function finishQAStream(state: QAStreamOutcome): QAStreamOutcome {
  return state.phase === "receiving" ? { phase: "interrupted", text: state.text } : state;
}
