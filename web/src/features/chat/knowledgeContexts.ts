export interface KnowledgeContextChunk {
  content: string;
  score: number;
  metadata?: Record<string, unknown>;
  source_url?: string;
  image_url?: string;
  dataset_id?: string;
  document_id?: string;
  segment_id?: string;
  source_version?: number;
  source_hash?: string;
}

export interface KnowledgeContext {
  dataset_id: string;
  dataset_name: string;
  chunks: KnowledgeContextChunk[];
  query: string;
  took_ms: number;
}

function nonEmptyString(value: unknown): string | undefined {
  return typeof value === "string" && value.trim() ? value : undefined;
}

function chunkDatasetId(
  chunk: Record<string, unknown>,
  fallbackDatasetId?: string,
): string | undefined {
  const metadata =
    chunk.metadata && typeof chunk.metadata === "object" && !Array.isArray(chunk.metadata)
      ? chunk.metadata as Record<string, unknown>
      : undefined;
  return (
    nonEmptyString(chunk.dataset_id) ||
    nonEmptyString(metadata?.dataset_id) ||
    fallbackDatasetId
  );
}

function chunkIdentity(chunk: KnowledgeContextChunk): string {
  // Equal excerpts are not equal source targets. Keep both immutable versions
  // and metadata-only identities; preserve conflicts for citation validation.
  const metadata = chunk.metadata || {};
  return JSON.stringify([
    chunk.dataset_id,
    metadata.dataset_id ?? chunk.dataset_id,
    chunk.document_id,
    metadata.document_id,
    chunk.segment_id,
    metadata.segment_id,
    chunk.source_version,
    chunk.source_hash,
    metadata.source_version,
    metadata.source_hash,
    chunk.content,
  ]);
}

/** Fold per-search Runtime events into unique, selected-dataset UI context. */
export function mergeKnowledgeContexts(
  current: KnowledgeContext[],
  eventData: unknown,
  selectedDatasetIds: string[],
  datasetNames: Readonly<Record<string, string>> = {},
): KnowledgeContext[] {
  if (!eventData || typeof eventData !== "object" || Array.isArray(eventData)) {
    return current;
  }
  const data = eventData as Record<string, unknown>;
  const selected = new Set(selectedDatasetIds);
  const explicitDatasetId = nonEmptyString(data.dataset_id);
  const datasetIds = Array.isArray(data.dataset_ids)
    ? data.dataset_ids.map(nonEmptyString).filter((value): value is string => Boolean(value))
    : [];
  const fallbackDatasetId =
    explicitDatasetId ||
    (datasetIds.length === 1 ? datasetIds[0] : undefined) ||
    (selected.size === 1 ? selectedDatasetIds[0] : undefined);
  const query = nonEmptyString(data.query) || "";
  const tookMs = typeof data.took_ms === "number" && Number.isFinite(data.took_ms)
    ? data.took_ms
    : 0;
  const chunks = Array.isArray(data.chunks) ? data.chunks : [];
  const grouped = new Map<string, KnowledgeContextChunk[]>();

  for (const rawChunk of chunks) {
    if (!rawChunk || typeof rawChunk !== "object" || Array.isArray(rawChunk)) continue;
    const chunk = rawChunk as Record<string, unknown>;
    const datasetId = chunkDatasetId(chunk, fallbackDatasetId);
    if (!datasetId || (selected.size > 0 && !selected.has(datasetId))) continue;
    if (typeof chunk.content !== "string" || typeof chunk.score !== "number") continue;
    const normalized = { ...chunk, dataset_id: datasetId } as KnowledgeContextChunk;
    grouped.set(datasetId, [...(grouped.get(datasetId) || []), normalized]);
  }

  if (grouped.size === 0) return current;
  const next = current.map((context) => ({ ...context, chunks: [...context.chunks] }));
  for (const [datasetId, incoming] of grouped) {
    const index = next.findIndex((context) => context.dataset_id === datasetId);
    const datasetName =
      (explicitDatasetId === datasetId ? nonEmptyString(data.dataset_name) : undefined) ||
      datasetNames[datasetId] ||
      datasetId;
    if (index < 0) {
      next.push({
        dataset_id: datasetId,
        dataset_name: datasetName,
        chunks: incoming,
        query,
        took_ms: tookMs,
      });
      continue;
    }
    const context = next[index];
    const seen = new Set(context.chunks.map(chunkIdentity));
    const uniqueIncoming = incoming.filter((chunk) => {
      const identity = chunkIdentity(chunk);
      if (seen.has(identity)) return false;
      seen.add(identity);
      return true;
    });
    next[index] = {
      ...context,
      chunks: [...context.chunks, ...uniqueIncoming],
      query: query || context.query,
      took_ms: context.took_ms + tookMs,
    };
  }
  return next;
}
