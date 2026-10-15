import type {
  AgentTraceDetailResponse,
  AgentTraceScore,
  AgentTraceSpan,
  AgentTraceSummary,
  TraceStatus,
} from "@/api/eval";

export function formatDuration(ms: number | null | undefined) {
  const value = typeof ms === "number" && Number.isFinite(ms) ? ms : 0;
  if (!value) return "0ms";
  if (value >= 1000) return `${(value / 1000).toFixed(2)}s`;
  return `${value}ms`;
}

export function formatDate(value: string | null | undefined, locale: string) {
  if (!value) return "-";
  return new Date(value).toLocaleString(locale);
}

export function compactJson(value: unknown): string {
  if (value === undefined || value === null) return "{}";
  try {
    return JSON.stringify(value, null, 2);
  } catch {
    return String(value);
  }
}

export function metadataObject(value: unknown): Record<string, unknown> {
  return value && typeof value === "object" && !Array.isArray(value)
    ? (value as Record<string, unknown>)
    : {};
}

export function formatTraceTokens(trace: Pick<AgentTraceSummary, "total_tokens" | "metadata">): string {
  const usage = metadataObject(trace.metadata.runtime_model_usage);
  if (usage.tokens_complete === false || (trace.total_tokens === 0 && usage.tokens_complete !== true)) return "—";
  return trace.total_tokens.toLocaleString();
}

export function formatTraceCost(trace: Pick<AgentTraceSummary, "total_cost_cents" | "metadata">): string {
  const usage = metadataObject(trace.metadata.runtime_model_usage);
  const measured = usage.cost_microusd;
  const amount = typeof measured === "number" && Number.isFinite(measured) && measured >= 0
    ? measured / 1_000_000
    : trace.total_cost_cents > 0 && usage.dispatched_calls === undefined ? trace.total_cost_cents / 100 : null;
  if (amount === null) return "—";
  if (amount > 0 && amount < 0.01) return "<$0.01";
  return `$${amount.toFixed(2)}`;
}

export function traceLocator(trace?: AgentTraceSummary | null): Record<string, unknown> {
  return metadataObject(trace?.metadata?.transcript_locator);
}

export function locatorText(locator: Record<string, unknown>, key: string): string {
  const value = locator[key];
  if (typeof value === "number" || typeof value === "boolean") return String(value);
  if (typeof value === "string" && value.trim()) return value;
  return "-";
}

export function traceTurn(trace?: AgentTraceSummary | null): string | null {
  const turn = traceLocator(trace).turn_index;
  if (typeof turn === "number" || typeof turn === "string") return String(turn);
  return null;
}

export function traceThreadId(trace?: AgentTraceSummary | null): string | null {
  return trace?.thread_id || trace?.session_id || null;
}

export function traceStartedAt(trace?: AgentTraceSummary | null): string | null | undefined {
  return trace?.started_at || trace?.created_at;
}

export function isErrorStatus(status?: TraceStatus | string | null) {
  return status === "failed" || status === "timeout" || status === "cancelled";
}

export const KB_RAGAS_METRICS = [
  "faithfulness",
  "response_relevancy",
  "context_precision",
  "context_recall",
  "context_relevancy",
] as const;
export type KbRagasMetricName = (typeof KB_RAGAS_METRICS)[number];

export function isKbRagasScore(score: AgentTraceScore): boolean {
  return score.score_source === "kb_ragas" || score.metadata?.component === "kb_ragas";
}

export function ragQueryFromDetail(detail?: AgentTraceDetailResponse | null): string {
  if (!detail?.trace) return "";
  const metadata = metadataObject(detail.trace.metadata);
  return String(
    metadata["gen_ai.retrieval.query.text"] || detail.trace.input_preview || ""
  ).trim();
}

export function datasetIdFromDetail(detail?: AgentTraceDetailResponse | null): string | null {
  if (!detail?.trace) return null;
  const metadata = metadataObject(detail.trace.metadata);
  const datasetId = metadata.dataset_id;
  return datasetId === undefined || datasetId === null ? null : String(datasetId);
}

function documentContextsFromSpan(span: AgentTraceSpan): string[] {
  const attributes = metadataObject(span.attributes);
  const retrieval = metadataObject(attributes.retrieval);
  const documents = Array.isArray(retrieval.documents)
    ? retrieval.documents
    : Array.isArray(attributes["retrieval.documents"])
      ? attributes["retrieval.documents"]
      : [];
  const contexts: string[] = [];
  for (const document of documents) {
    if (!document || typeof document !== "object" || Array.isArray(document)) continue;
    const record = document as Record<string, unknown>;
    const content = String(
      record.content_eval || record.content_preview || record.text || ""
    ).trim();
    if (content && !contexts.includes(content)) contexts.push(content);
  }
  return contexts;
}

export function retrievalContextsFromDetail(detail?: AgentTraceDetailResponse | null): string[] {
  if (!detail?.spans?.length) return [];
  const contexts: string[] = [];
  for (const span of detail.spans) {
    if (!["retriever", "document_fetch"].includes(span.span_kind)) continue;
    for (const context of documentContextsFromSpan(span)) {
      if (!contexts.includes(context)) contexts.push(context);
    }
  }
  return contexts;
}

export function retrievalDocumentCountFromDetail(detail?: AgentTraceDetailResponse | null): number {
  return retrievalContextsFromDetail(detail).length;
}

export function kbRagasScoresFromDetail(detail?: AgentTraceDetailResponse | null): AgentTraceScore[] {
  return (detail?.scores || []).filter(isKbRagasScore);
}

export function kbRagasMetricValue(
  scores: AgentTraceScore[],
  metric: KbRagasMetricName,
): AgentTraceScore | undefined {
  return scores.find(
    (score) =>
      score.score_name === metric
      || score.metadata?.metric === metric,
  );
}

export function formatScorePercent(value?: number | null): string {
  if (typeof value !== "number" || !Number.isFinite(value)) return "—";
  return `${Math.round(value * 100)}%`;
}

export function ragasLabelColor(label?: string | null): string {
  if (label === "pass") return "green";
  if (label === "fail") return "red";
  return "gold";
}
