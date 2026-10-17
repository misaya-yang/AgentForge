/**
 * Knowledge Base API Client
 * 
 * Complete API for managing knowledge bases, documents, segments,
 * retrieval testing, and QA evaluation.
 */

import { api } from "@/lib/api";
import { sseFetch, sseFetchEvents, type SSEEvent, type SSEConnectionState } from "@/lib/sse";
import type {
  Dataset,
  Document,
  Segment,
  RetrieveRequest,
  RetrieveResponse,
  QARequest,
  QAResponse,
  QAStreamEvent,
  DatasetConfig,
  DatasetDebugInfo,
  ChunkingConfig,
  RetrieveMode,
  VectorRetrievalConfig,
  KeywordRetrievalConfig,
  FusionConfig,
  RerankConfig,
  MMRConfig,
  DatasetCreateRequest,
  DocumentCreateTextRequest,
  BatchOperationResult,
  DocumentMetadataField,
  DocumentMetadataRegistry,
  OffsetPage,
  QueryFeedback,
  QueryFeedbackInput,
  QueryFeedbackPage,
  QueryFeedbackRating,
  QueryFeedbackTarget,
  QueryHistoryPage,
} from "@/types/knowledge";

// ============================================================
// Dataset APIs
// ============================================================

export async function listDatasets() {
  const datasets = new Map<string, Dataset>();
  const seen = new Set<string>();
  let cursor: string | undefined;
  do {
    const response = await api.get<Dataset[]>("/api/v1/knowledge/datasets", {
      params: { limit: 200, ...(cursor ? { cursor } : {}) },
    });
    response.data.forEach((dataset) => datasets.set(dataset.dataset_id, dataset));
    const next = response.headers["x-next-cursor"] as string | undefined;
    if (!next) break;
    if (seen.has(next)) throw new Error("Dataset pagination did not advance");
    seen.add(next);
    cursor = next;
  } while (cursor);
  return [...datasets.values()];
}

export async function createDataset(payload: DatasetCreateRequest) {
  const { data } = await api.post<Dataset>("/api/v1/knowledge/datasets", payload);
  return data;
}

export async function getDataset(datasetId: string) {
  const { data } = await api.get<Dataset>(`/api/v1/knowledge/datasets/${datasetId}`);
  return data;
}

export interface DatasetAgentImpact {
  dataset_id: string;
  visible_agents: Array<{
    agent_id: string;
    name: string;
    current_draft: boolean;
    active_publication: boolean;
    historical_version_count: number;
  }>;
  hidden_agent_count: number;
  counts: {
    current_draft: { visible: number; hidden: number };
    active_publication: { visible: number; hidden: number };
    historical_version: { visible: number; hidden: number };
  };
}

export async function getDatasetAgentImpact(datasetId: string): Promise<DatasetAgentImpact> {
  const { data } = await api.get<DatasetAgentImpact>(
    `/api/v1/knowledge/datasets/${encodeURIComponent(datasetId)}/impact`
  );
  return data;
}

export async function updateDataset(datasetId: string, patch: Partial<Dataset>) {
  const { data } = await api.put<Dataset>(`/api/v1/knowledge/datasets/${datasetId}`, patch);
  return data;
}

export interface DeleteDatasetPayload {
  password: string;
  reason?: string;
}

export async function deleteDataset(datasetId: string, payload: DeleteDatasetPayload) {
  const { data } = await api.delete(`/api/v1/knowledge/datasets/${datasetId}`, {
    data: payload,
  });
  return data;
}

// ============================================================
// Embedding blue-green migration operator APIs
// ============================================================

export type EmbeddingMigrationState =
  | "shadow_build"
  | "backfilling"
  | "verified"
  | "gating"
  | "gate_failed"
  | "ready"
  | "completed"
  | "rolled_back"
  | "failed"
  | "abandoned";

export interface EmbeddingCollectionBinding {
  binding_id: string;
  dataset_id: string;
  tenant_id: string;
  collection_name: string;
  embedding_provider: string;
  embedding_model: string;
  embedding_model_version: string;
  embedding_dimension: number;
  capabilities: string[];
  state: "serving" | "shadow" | "retained" | "retired";
  created_at?: string | null;
  activated_at?: string | null;
  retired_at?: string | null;
  retained_until?: string | null;
}

export interface EmbeddingMigrationJob {
  migration_id: string;
  dataset_id: string;
  source_binding_id?: string | null;
  target_binding_id: string;
  state: EmbeddingMigrationState;
  checkpoint: Record<string, unknown>;
  totals: Record<string, unknown>;
  gate?: Record<string, unknown> | null;
  error?: string | null;
  created_at?: string | null;
  updated_at?: string | null;
}

export type EmbeddingMigrationAction = "backfill" | "verify" | "gate";
export type EmbeddingMigrationActionJobState =
  | "queued"
  | "running"
  | "succeeded"
  | "failed"
  | "cancelled";

export interface EmbeddingMigrationActionJob {
  job_id: string;
  migration_id: string;
  dataset_id: string;
  action: EmbeddingMigrationAction;
  state: EmbeddingMigrationActionJobState;
  payload: Record<string, unknown>;
  result: Record<string, unknown> | null;
  error: string | null;
  attempt_count: number;
  request_hash?: string;
  reused?: boolean;
  poll_after_ms?: number;
  requested_by?: string;
  available_at?: string | null;
  lease_expires_at?: string | null;
  last_heartbeat_at?: string | null;
  started_at?: string | null;
  finished_at?: string | null;
  created_at?: string | null;
  updated_at?: string | null;
}

/**
 * A server-witnessed collection receipt. Older deployments omit this field;
 * callers must render that as unknown rather than inferring a healthy Qdrant
 * collection from a PostgreSQL binding row.
 */
export interface EmbeddingCollectionScopeEvidence {
  authority_kind?: string;
  dataset_id?: string;
  tenant_id?: string;
  serving_collection_name?: string;
  content_revision?: string | number;
  point_count?: number;
  point_ids_sha256?: string;
  source_text_sha256?: string;
  [key: string]: unknown;
}

export interface EmbeddingCollectionHealthReceipt {
  status: "healthy" | "drifted" | "unknown";
  checked_live: boolean;
  collection_name: string | null;
  pending_chunks: number | null;
  authority: EmbeddingCollectionScopeEvidence | null;
  target_scope: EmbeddingCollectionScopeEvidence | null;
  verified_authority: EmbeddingCollectionScopeEvidence | null;
  verified_target_scope: EmbeddingCollectionScopeEvidence | null;
  gate_report: Record<string, unknown> | null;
  reason: string | null;
}

export interface EmbeddingMigrationDescription {
  dataset_id: string;
  serving_binding: EmbeddingCollectionBinding | null;
  live_migration: EmbeddingMigrationJob | null;
  /** Newer servers retain the latest terminal job so reload can restore rollback controls. */
  latest_migration?: EmbeddingMigrationJob | null;
  /** Newest-first operator history, bounded by the server. */
  recent_migrations?: EmbeddingMigrationJob[];
  /** Server authority for refresh/new-session recovery. */
  active_action_job?: EmbeddingMigrationActionJob | null;
  /** Newest-first terminal action jobs for status/error receipts. */
  recent_action_jobs?: EmbeddingMigrationActionJob[];
  source_binding?: EmbeddingCollectionBinding | null;
  target_binding?: EmbeddingCollectionBinding | null;
  collection_health?: EmbeddingCollectionHealthReceipt | null;
  pending_chunks: number | null;
  enabled_chunks: number;
}

export interface StartEmbeddingMigrationRequest {
  embedding_provider: string;
  embedding_model: string;
  embedding_model_version?: string;
  embedding_dimension: number;
  /** Omit to inherit serving capabilities; [] is an explicit empty set. */
  capabilities?: string[];
}

export interface EmbeddingMigrationGateRequest {
  sample_size?: number;
  top_k?: number;
  tolerance?: number;
  floor?: number;
}

export interface EmbeddingMigrationActionResult {
  migration?: EmbeddingMigrationJob;
  migration_id?: string;
  state?: EmbeddingMigrationState;
  passed?: boolean;
  verdict?: Record<string, unknown>;
  embedded?: number;
  pending?: number;
  enabled_chunks?: number;
  points?: number | null;
  rounds?: number;
  [key: string]: unknown;
}

const embeddingMigrationPath = (datasetId: string) =>
  `/api/v1/knowledge/datasets/${datasetId}/embedding-migration`;

export async function describeEmbeddingMigration(
  datasetId: string
): Promise<EmbeddingMigrationDescription> {
  const { data } = await api.get<EmbeddingMigrationDescription>(
    embeddingMigrationPath(datasetId)
  );
  return data;
}

export async function startEmbeddingMigration(
  datasetId: string,
  request: StartEmbeddingMigrationRequest
): Promise<EmbeddingMigrationActionResult> {
  const { data } = await api.post<EmbeddingMigrationActionResult>(
    `${embeddingMigrationPath(datasetId)}/start`,
    request
  );
  return data;
}

async function postEmbeddingMigrationAction<T = EmbeddingMigrationActionResult>(
  datasetId: string,
  migrationId: string,
  action: "backfill" | "verify" | "gate" | "cutover" | "rollback" | "abort",
  body?: Record<string, unknown>
): Promise<T> {
  const { data } = await api.post<T>(
    `${embeddingMigrationPath(datasetId)}/${migrationId}/${action}`,
    body
  );
  return data;
}

export function backfillEmbeddingMigration(
  datasetId: string,
  migrationId: string
): Promise<EmbeddingMigrationActionJob> {
  return postEmbeddingMigrationAction<EmbeddingMigrationActionJob>(
    datasetId,
    migrationId,
    "backfill"
  );
}

export function verifyEmbeddingMigration(
  datasetId: string,
  migrationId: string
): Promise<EmbeddingMigrationActionJob> {
  return postEmbeddingMigrationAction<EmbeddingMigrationActionJob>(
    datasetId,
    migrationId,
    "verify"
  );
}

export function gateEmbeddingMigration(
  datasetId: string,
  migrationId: string,
  request: EmbeddingMigrationGateRequest = {}
): Promise<EmbeddingMigrationActionJob> {
  return postEmbeddingMigrationAction<EmbeddingMigrationActionJob>(
    datasetId,
    migrationId,
    "gate",
    { ...request }
  );
}

export async function getEmbeddingMigrationActionJob(
  datasetId: string,
  migrationId: string,
  jobId: string
): Promise<EmbeddingMigrationActionJob> {
  const { data } = await api.get<EmbeddingMigrationActionJob>(
    `${embeddingMigrationPath(datasetId)}/${migrationId}/jobs/${jobId}`
  );
  return data;
}

export function cutoverEmbeddingMigration(
  datasetId: string,
  migrationId: string,
  retentionSeconds?: number
): Promise<EmbeddingMigrationActionResult> {
  const body = retentionSeconds === undefined ? {} : { retention_seconds: retentionSeconds };
  return postEmbeddingMigrationAction(datasetId, migrationId, "cutover", body);
}

export function rollbackEmbeddingMigration(
  datasetId: string,
  migrationId: string,
  keepShadow = true
): Promise<EmbeddingMigrationActionResult> {
  return postEmbeddingMigrationAction(datasetId, migrationId, "rollback", {
    keep_shadow: keepShadow,
  });
}

export function abortEmbeddingMigration(
  datasetId: string,
  migrationId: string,
  options: { reason?: string; purgeShadow?: boolean } = {}
): Promise<EmbeddingMigrationActionResult> {
  return postEmbeddingMigrationAction(datasetId, migrationId, "abort", {
    reason: options.reason || "aborted from web console",
    purge_shadow: options.purgeShadow ?? true,
  });
}

// ============================================================
// Document APIs
// ============================================================

function parsePageTotal(value: unknown, fallback: number): number {
  const parsed = Number(value);
  return Number.isSafeInteger(parsed) && parsed >= 0 ? parsed : fallback;
}

export async function listDocuments(
  datasetId: string,
  params: { limit?: number; offset?: number } = {}
): Promise<OffsetPage<Document>> {
  const limit = params.limit ?? 50;
  const offset = params.offset ?? 0;
  const response = await api.get<Document[]>(`/api/v1/knowledge/${datasetId}/documents`, {
    params: { limit, offset },
  });
  return {
    items: response.data,
    total: parsePageTotal(response.headers["x-total-count"], response.data.length),
    limit,
    offset,
  };
}

/** Resolve one source document with the caller's current KB permission. */
export async function getDocument(
  datasetId: string,
  documentId: string,
  options: KnowledgeRequestOptions = {},
): Promise<Document> {
  const { data } = await api.get<Document>(
    `/api/v1/knowledge/${encodeURIComponent(datasetId)}/documents/${encodeURIComponent(documentId)}`,
    { signal: options.signal },
  );
  return data;
}

/** Citation reads require the document to remain active even for editors. */
export async function getActiveDocumentSource(datasetId: string, documentId: string): Promise<Document> {
  const { data } = await api.get<Document>(
    `/api/v1/knowledge/${encodeURIComponent(datasetId)}/documents/${encodeURIComponent(documentId)}/source`,
  );
  return data;
}


export async function createDocumentFromText(datasetId: string, payload: DocumentCreateTextRequest) {
  const { data } = await api.post<Document>(`/api/v1/knowledge/${datasetId}/documents/text`, payload);
  return data;
}

export async function createDocumentFromUrl(datasetId: string, payload: { url: string; title?: string }) {
  const { data } = await api.post<Document>(`/api/v1/knowledge/${datasetId}/documents/url`, payload);
  return data;
}

/**
 * Public document upload is intentionally text-only until the multimodal
 * create/ingest/retrieve/delete contract is released end to end.
 */
export type ProcessingMode = "text_only";

export async function uploadDocument(
  datasetId: string,
  file: File,
  processingMode: ProcessingMode = "text_only"
) {
  const form = new FormData();
  form.append("file", file);
  form.append("processing_mode", processingMode);
  // Don't set Content-Type manually - axios will auto-set it with correct boundary
  const { data } = await api.post<Document>(
    `/api/v1/knowledge/${datasetId}/documents/upload`,
    form
  );
  return data;
}

/**
 * 批量上传结果
 */
export interface BatchUploadResult {
  batch_id: string;
  total: number;
  accepted: number;
  rejected: number;
  documents: Document[];
  errors: Array<{
    file_index?: number;
    filename: string;
    error: string;
    document_id?: string;
    retry_safe?: boolean;
  }>;
}

/**
 * 批量上传文档到知识库
 * 支持格式: PDF, DOCX, TXT, MD, HTML
 * 最大文件数: 50
 * 
 * 优势:
 * - 一次请求上传多个文件
 * - 并行处理（Worker并发由服务器配置决定）
 * - 返回batch_id用于追踪处理进度
 */
export async function batchUploadDocuments(datasetId: string, files: File[]): Promise<BatchUploadResult> {
  const form = new FormData();
  files.forEach((file) => {
    form.append("files", file);
  });
  const { data } = await api.post<BatchUploadResult>(
    `/api/v1/knowledge/${datasetId}/documents/batch-upload`,
    form,
    {
      timeout: 300000, // 5 minutes for large batch uploads
    }
  );
  return data;
}

export interface ImageUploadResult {
  uploaded: Array<{
    document_id: string;
    filename: string;
    size_bytes: number;
  }>;
  success_count: number;
  failed_count: number;
  errors: Array<{
    filename: string;
    error: string;
  }>;
}



export async function deleteDocument(datasetId: string, documentId: string) {
  const { data } = await api.delete(`/api/v1/knowledge/${datasetId}/documents/${documentId}`);
  return data;
}

export type DocumentPipelineAction = "reembed" | "reprocess" | "recover" | "retry";

export interface DocumentPipelineResult {
  status: "queuing";
  document_id: string;
  action?: DocumentPipelineAction;
  recover_stage?: "parsing" | "splitting" | "indexing";
}

export interface DocumentProgressPayload {
  document_id: string;
  progress: {
    percent: number;
    stage: string;
    state: string;
  };
  terminal?: boolean;
  error?: string | null;
}

/** Read-only durable document progress; callers own reconnect/cursor policy. */
export async function* streamDocumentProgress(
  datasetId: string,
  options: { signal?: AbortSignal; lastEventId?: string; onConnectionState?: (state: SSEConnectionState) => void } = {},
): AsyncGenerator<SSEEvent<DocumentProgressPayload>, void, void> {
  const headers = new Headers({ Accept: "text/event-stream" });
  if (options.lastEventId) headers.set("Last-Event-ID", options.lastEventId);
  for await (const event of sseFetchEvents<DocumentProgressPayload>(
    `/api/v1/knowledge/${datasetId}/documents/progress`,
    {
      method: "GET",
      headers,
      signal: options.signal,
      onConnectionState: options.onConnectionState,
      // Dataset feeds are continuous; each per-document terminal is still followed by other documents.
      resume: { isTerminal: () => false, isReset: event => event.event === "reset" },
    },
  )) {
    yield event;
  }
}

async function requestDocumentPipelineAction(
  datasetId: string,
  documentId: string,
  endpoint: "reindex" | "reprocess" | "recover" | "retry"
): Promise<DocumentPipelineResult> {
  const { data } = await api.post<DocumentPipelineResult>(
    `/api/v1/knowledge/${datasetId}/documents/${documentId}/${endpoint}`
  );
  return data;
}

/**
 * Repair vectors from persisted segments. The backend route retains its
 * legacy `/reindex` spelling, but its contract is reembed-only: it never
 * re-parses the source or changes chunk boundaries.
 */
export function reembedDocument(
  datasetId: string,
  documentId: string
): Promise<DocumentPipelineResult> {
  return requestDocumentPipelineAction(datasetId, documentId, "reindex");
}

/** Re-run parsing, splitting, and embedding from the captured rule snapshot. */
export function reprocessDocument(
  datasetId: string,
  documentId: string
): Promise<DocumentPipelineResult> {
  return requestDocumentPipelineAction(datasetId, documentId, "reprocess");
}

/** Resume an errored generation from its furthest durable stage. */
export function recoverDocument(
  datasetId: string,
  documentId: string
): Promise<DocumentPipelineResult> {
  return requestDocumentPipelineAction(datasetId, documentId, "recover");
}

/** Rerun the pinned full pipeline and atomically replace the serving generation. */
export function retryDocument(
  datasetId: string,
  documentId: string
): Promise<DocumentPipelineResult> {
  return requestDocumentPipelineAction(datasetId, documentId, "retry");
}

/**
 * Document enable/disable (canonical PATCH status route; the POST /enable
 * sibling is compat-only). Returns the full document with a fresh
 * display_status stamp.
 */
export async function setDocumentEnabled(
  datasetId: string,
  documentId: string,
  enabled: boolean
) {
  const { data } = await api.patch<Document>(
    `/api/v1/knowledge/${datasetId}/documents/${documentId}/status`,
    { enabled }
  );
  return data;
}

/**
 * The column behind archived_reason is VARCHAR(255) even though the request
 * schema advertises 2000 (backend dependency D2); the client caps input at
 * the column's truth so saves never truncate server-side.
 */
export const DOCUMENT_ARCHIVE_REASON_LIMIT = 255;

/** Archive or unarchive a document. The reason is stored only when archiving. */
export async function setDocumentArchived(
  datasetId: string,
  documentId: string,
  archived: boolean,
  reason?: string
) {
  const { data } = await api.patch<Document>(
    `/api/v1/knowledge/${datasetId}/documents/${documentId}/archive`,
    archived ? { archived, reason: reason ?? null } : { archived }
  );
  return data;
}

export async function getDocumentMetadataRegistry(
  datasetId: string
): Promise<DocumentMetadataRegistry> {
  const { data } = await api.get<DocumentMetadataRegistry>(
    `/api/v1/knowledge/${datasetId}/metadata-schema`
  );
  return data;
}

export async function updateDocumentMetadataRegistry(
  datasetId: string,
  expectedRevision: number,
  fields: DocumentMetadataField[]
): Promise<DocumentMetadataRegistry> {
  const { data } = await api.put<DocumentMetadataRegistry>(
    `/api/v1/knowledge/${datasetId}/metadata-schema`,
    { expected_revision: expectedRevision, fields }
  );
  return data;
}

export interface DocumentMetadataPatch {
  metadataPatch?: Record<string, unknown>;
  metadataRemove?: string[];
  metadataSchemaRevision: number;
}

export async function updateDocumentMetadata(
  datasetId: string,
  documentId: string,
  patch: DocumentMetadataPatch
): Promise<Document> {
  const { data } = await api.patch<Document>(
    `/api/v1/knowledge/${datasetId}/documents/${documentId}`,
    {
      metadata_patch: patch.metadataPatch ?? {},
      metadata_remove: patch.metadataRemove ?? [],
      metadata_schema_revision: patch.metadataSchemaRevision,
    }
  );
  return data;
}

export async function batchUpdateDocumentMetadata(
  datasetId: string,
  documentIds: string[],
  patch: DocumentMetadataPatch
): Promise<BatchOperationResult & { metadata_schema_revision: number }> {
  const { data } = await api.patch<
    BatchOperationResult & { metadata_schema_revision: number }
  >(`/api/v1/knowledge/${datasetId}/documents/metadata/batch`, {
    document_ids: documentIds,
    metadata_patch: patch.metadataPatch ?? {},
    metadata_remove: patch.metadataRemove ?? [],
    metadata_schema_revision: patch.metadataSchemaRevision,
  });
  return data;
}


// ============================================================
// Segment APIs
// ============================================================

export async function listSegments(
  datasetId: string,
  params: { documentId?: string; q?: string; limit?: number; offset?: number } = {},
  options: KnowledgeRequestOptions = {},
): Promise<OffsetPage<Segment>> {
  const limit = params.limit ?? 100;
  const offset = params.offset ?? 0;
  const response = await api.get<Segment[]>(`/api/v1/knowledge/${datasetId}/segments`, {
    signal: options.signal,
    params: {
      document_id: params.documentId,
      q: params.q,
      limit,
      offset,
    },
  });
  return {
    items: response.data,
    total: parsePageTotal(response.headers["x-total-count"], response.data.length),
    limit,
    offset,
  };
}


/**
 * Editable fields of a segment update. The backend SegmentUpdateSchema
 * requires `text` (1..200_000 chars) and accepts optional `answer`
 * (<= 200_000 chars) and `keywords` (<= 100 items, each 1..256 chars).
 * Sending a field replaces it, so the edit dialog always sends all three;
 * omitting `answer`/`keywords` keeps the stored values.
 */
export interface SegmentUpdateFields {
  text: string;
  answer?: string;
  keywords?: string[];
}

export async function updateSegment(
  datasetId: string,
  segmentId: string,
  fields: SegmentUpdateFields
) {
  const { data } = await api.put<Segment>(
    `/api/v1/knowledge/${datasetId}/segments/${segmentId}`,
    fields
  );
  return data;
}

export async function deleteSegment(datasetId: string, segmentId: string) {
  const { data } = await api.delete(`/api/v1/knowledge/${datasetId}/segments/${segmentId}`);
  return data;
}

/** Enable or disable a single segment (canonical PATCH status route). */
export async function setSegmentEnabled(datasetId: string, segmentId: string, enabled: boolean) {
  const { data } = await api.patch<Segment>(
    `/api/v1/knowledge/${datasetId}/segments/${segmentId}/status`,
    { enabled }
  );
  return data;
}

export interface SegmentBatchResult {
  success: boolean;
  updated: number;
  total: number;
}

/**
 * Batch enable/disable. The backend accepts 1..500 segment ids and reports
 * partial success: per-item failures are skipped, so `updated` can be less
 * than `total` and the UI must surface that instead of assuming all-or-nothing.
 */
export async function batchSetSegmentsEnabled(
  datasetId: string,
  segmentIds: string[],
  enabled: boolean
) {
  const { data } = await api.post<SegmentBatchResult>(
    `/api/v1/knowledge/${datasetId}/segments/batch/enable`,
    { segment_ids: segmentIds, enabled }
  );
  return data;
}


// ============================================================
// Retrieval APIs
// ============================================================

export interface KnowledgeRequestOptions {
  signal?: AbortSignal;
  timeoutMs?: number;
}

export async function retrieve(
  datasetId: string,
  req: RetrieveRequest,
  options: KnowledgeRequestOptions = {}
) {
  const { data } = await api.post<RetrieveResponse>(
    `/api/v1/knowledge/${datasetId}/retrieve`,
    req,
    {
      signal: options.signal,
      timeout: options.timeoutMs,
    }
  );
  return data;
}

export async function hitTest(
  datasetId: string,
  req: RetrieveRequest,
  options: KnowledgeRequestOptions = {}
) {
  const { data } = await api.post<RetrieveResponse>(
    `/api/v1/knowledge/${datasetId}/hit_test`,
    req,
    {
      signal: options.signal,
      timeout: options.timeoutMs,
    }
  );
  return data;
}

export async function listDatasetQueries(
  datasetId: string,
  params: {
    limit?: number;
    zeroResults?: boolean;
    mode?: string;
    cursor?: string;
  } = {}
) {
  const { data } = await api.get<QueryHistoryPage>(
    `/api/v1/knowledge/${datasetId}/queries`,
    {
      params: {
        limit: params.limit,
        zero_results: params.zeroResults,
        mode: params.mode,
        cursor: params.cursor,
      },
    }
  );
  return data;
}

export async function putQueryFeedback(datasetId: string, input: QueryFeedbackInput) {
  const { data } = await api.put<QueryFeedback>(
    `/api/v1/knowledge/${datasetId}/feedback`,
    input
  );
  return data;
}

export async function listQueryFeedback(
  datasetId: string,
  params: {
    limit?: number;
    rating?: QueryFeedbackRating;
    reasonCode?: string;
    targetType?: QueryFeedbackTarget;
    traceId?: string;
    cursor?: string;
  } = {}
) {
  const { data } = await api.get<QueryFeedbackPage>(
    `/api/v1/knowledge/${datasetId}/feedback`,
    {
      params: {
        limit: params.limit,
        rating: params.rating,
        reason_code: params.reasonCode,
        target_type: params.targetType,
        trace_id: params.traceId,
        cursor: params.cursor,
      },
    }
  );
  return data;
}

// ============================================================
// Retrieval Evaluation (IR metrics) + Presets
// ============================================================

/** One labelled evaluation case: a query plus its ground-truth relevance. */
export interface RetrievalEvalCase {
  query: string;
  case_id?: string;
  /** Binary ground truth: segment IDs that are relevant. */
  relevant_segment_ids?: string[];
  /** Graded ground truth: segment ID -> relevance grade in [0, 1]. */
  relevance?: Record<string, number>;
}

/** Per-K aggregated IR metrics returned by the evaluation endpoint. */
export interface RetrievalMetricsAtK {
  k: number;
  num_queries: number;
  hit_rate: number;
  precision_at_k: number;
  recall_at_k: number;
  mrr: number;
  ndcg_at_k: number;
  map: number;
}

export interface RetrievalEvalCaseResult {
  case_id: string;
  query: string;
  retrieved: Array<{
    segment_id: string;
    document_id: string;
    score: number;
    relevant: boolean;
    relevance_grade: number;
  }>;
}

interface RetrievalEvalEnvelope {
  cases: RetrievalEvalCase[];
  k_values?: number[];
  return_retrieved?: boolean;
}

export type RetrievalEvalRequest = RetrievalEvalEnvelope &
  (Partial<RetrieveRequest> | RetrievalPresetConfig);

export interface RetrievalEvalResponse {
  dataset_id: string;
  num_cases: number;
  k_values: number[];
  metrics: Record<string, RetrievalMetricsAtK>;
  primary_metrics?: RetrievalMetricsAtK;
  cases?: RetrievalEvalCaseResult[];
  per_query?: Record<string, unknown>;
  requested_config?: Record<string, unknown>;
  case_metadata?: Array<{
    case_id: string;
    provider_retrieved_count: number;
    retrieved_count: number;
    unique_retrieved_count: number;
    duplicate_segment_ids: string[];
    retrieval_metadata: Record<string, unknown>;
  }>;
}

/**
 * Run the retrieval pipeline against a labelled test set and score it with
 * deterministic IR metrics (hit-rate / precision / recall / MRR / nDCG / MAP).
 * Call twice with two configs to get A/B-comparable metrics.
 */
export async function retrieveEvaluate(
  datasetId: string,
  req: RetrievalEvalRequest,
  options: KnowledgeRequestOptions = {}
) {
  const { data } = await api.post<RetrievalEvalResponse>(
    `/api/v1/knowledge/${datasetId}/retrieve_evaluate`,
    req,
    {
      signal: options.signal,
      timeout: options.timeoutMs,
    }
  );
  return data;
}

interface RetrievalPresetVectorConfig {
  enabled: boolean;
  top_k: number;
  score_threshold: number | null;
}

interface RetrievalPresetKeywordConfig {
  enabled: boolean;
  top_k: number;
  candidate_pool_size: number;
  bm25_k1: number;
  bm25_b: number;
}

interface RetrievalPresetFusionConfig {
  strategy: "rrf" | "weighted";
  rrf_k: number;
  rrf_weights: Record<string, number>;
  alpha: number;
}

interface RetrievalPresetRerankConfig {
  enabled: boolean;
  provider: "dashscope" | "cohere" | "jina" | "bge" | "custom";
  model: string;
  top_n: number | null;
  score_threshold: number | null;
}

interface RetrievalPresetMmrConfig {
  enabled: boolean;
  lambda: number;
  similarity_threshold: number | null;
}

interface RetrievalPresetMultimodalConfig {
  enabled: boolean;
  image_search_enabled: boolean;
  image_score_threshold: number;
  text_score_threshold: number;
  use_separate_thresholds: boolean;
  image_boost: number;
  vlm_rerank_enabled: boolean;
  vlm_rerank_weight: number;
  content_type_filter: string | null;
}

export interface RetrievalPresetConfig {
  mode: "vector" | "keyword" | "hybrid";
  top_k: number;
  score_threshold: number | null;
  vector: RetrievalPresetVectorConfig;
  keyword: RetrievalPresetKeywordConfig;
  fusion: RetrievalPresetFusionConfig;
  rerank: RetrievalPresetRerankConfig;
  mmr: RetrievalPresetMmrConfig;
  multimodal: RetrievalPresetMultimodalConfig;
}

export interface RetrievalPreset {
  name: string;
  label: string;
  summary: string;
  recommended_for: string;
  config: RetrievalPresetConfig;
}

export interface RetrievalPresetsResponse {
  presets: RetrievalPreset[];
  recommended_default: string;
  notes: Record<string, string>;
}

function requireFinitePresetNumber(name: string, value: unknown): asserts value is number {
  if (typeof value !== "number" || !Number.isFinite(value)) {
    throw new Error(`Invalid retrieval preset field: ${name}`);
  }
}

function requirePresetBoolean(name: string, value: unknown): asserts value is boolean {
  if (typeof value !== "boolean") {
    throw new Error(`Invalid retrieval preset field: ${name}`);
  }
}

function requirePresetInteger(name: string, value: unknown, maximum: number): asserts value is number {
  requireFinitePresetNumber(name, value);
  if (!Number.isInteger(value) || value < 1 || value > maximum) {
    throw new Error("Retrieval preset values are outside supported bounds");
  }
}

function requirePresetUnitInterval(name: string, value: unknown): asserts value is number {
  requireFinitePresetNumber(name, value);
  if (value < 0 || value > 1) {
    throw new Error("Retrieval preset values are outside supported bounds");
  }
}

/**
 * Validate the bounded preset shape before it is allowed to drive requests.
 *
 * The evaluation endpoint accepts this canonical nested shape directly and
 * returns requested_config/case_metadata evidence. The legacy hit-test endpoint
 * remains flat and must use retrievalPresetToFlatRequest below.
 */
export function validateRetrievalPresetConfig(config: RetrievalPresetConfig): void {
  if (
    !config ||
    !config.vector ||
    !config.keyword ||
    !config.fusion ||
    !config.rerank ||
    !config.mmr ||
    !config.multimodal ||
    !config.fusion.rrf_weights ||
    typeof config.fusion.rrf_weights !== "object" ||
    Array.isArray(config.fusion.rrf_weights) ||
    !["vector", "keyword", "hybrid"].includes(config.mode) ||
    !["rrf", "weighted"].includes(config.fusion.strategy) ||
    !["dashscope", "cohere", "jina", "bge", "custom"].includes(config.rerank.provider)
  ) {
    throw new Error("Invalid retrieval preset configuration");
  }

  requirePresetBoolean("vector.enabled", config.vector.enabled);
  requirePresetBoolean("keyword.enabled", config.keyword.enabled);
  requirePresetBoolean("rerank.enabled", config.rerank.enabled);
  requirePresetBoolean("mmr.enabled", config.mmr.enabled);
  requirePresetBoolean("multimodal.enabled", config.multimodal.enabled);
  requirePresetBoolean(
    "multimodal.image_search_enabled",
    config.multimodal.image_search_enabled
  );
  requirePresetBoolean(
    "multimodal.use_separate_thresholds",
    config.multimodal.use_separate_thresholds
  );
  requirePresetBoolean(
    "multimodal.vlm_rerank_enabled",
    config.multimodal.vlm_rerank_enabled
  );

  requirePresetInteger("top_k", config.top_k, 100);
  requirePresetInteger("vector.top_k", config.vector.top_k, 1_000);
  requirePresetInteger("keyword.top_k", config.keyword.top_k, 1_000);
  requirePresetInteger("keyword.candidate_pool_size", config.keyword.candidate_pool_size, 500);
  requirePresetInteger("fusion.rrf_k", config.fusion.rrf_k, 10_000);
  requirePresetUnitInterval("fusion.alpha", config.fusion.alpha);
  requirePresetUnitInterval("mmr.lambda", config.mmr.lambda);
  requireFinitePresetNumber("keyword.bm25_k1", config.keyword.bm25_k1);
  requirePresetUnitInterval("keyword.bm25_b", config.keyword.bm25_b);
  requirePresetUnitInterval(
    "multimodal.image_score_threshold",
    config.multimodal.image_score_threshold
  );
  requirePresetUnitInterval(
    "multimodal.text_score_threshold",
    config.multimodal.text_score_threshold
  );
  requirePresetUnitInterval(
    "multimodal.vlm_rerank_weight",
    config.multimodal.vlm_rerank_weight
  );
  requireFinitePresetNumber("multimodal.image_boost", config.multimodal.image_boost);

  if (config.score_threshold !== null) {
    requirePresetUnitInterval("score_threshold", config.score_threshold);
  }
  if (config.vector.score_threshold !== null) {
    requirePresetUnitInterval("vector.score_threshold", config.vector.score_threshold);
  }
  if (config.rerank.top_n !== null) {
    requirePresetInteger("rerank.top_n", config.rerank.top_n, 1_000);
  }
  if (config.rerank.score_threshold !== null) {
    requirePresetUnitInterval("rerank.score_threshold", config.rerank.score_threshold);
  }
  if (config.mmr.similarity_threshold !== null) {
    requirePresetUnitInterval("mmr.similarity_threshold", config.mmr.similarity_threshold);
  }
  if (
    typeof config.rerank.model !== "string" ||
    config.rerank.model.length === 0 ||
    config.rerank.model.length > 256 ||
    (config.multimodal.content_type_filter !== null &&
      (typeof config.multimodal.content_type_filter !== "string" ||
        config.multimodal.content_type_filter.length > 64)) ||
    config.keyword.bm25_k1 < 0 ||
    config.keyword.bm25_k1 > 10 ||
    config.multimodal.image_boost < 0 ||
    config.multimodal.image_boost > 10
  ) {
    throw new Error("Retrieval preset values are outside supported bounds");
  }

  const rrfWeightEntries = Object.entries(config.fusion.rrf_weights);
  if (rrfWeightEntries.length > 16) {
    throw new Error("Retrieval preset values are outside supported bounds");
  }
  for (const [source, weight] of rrfWeightEntries) {
    requireFinitePresetNumber(`fusion.rrf_weights.${source}`, weight);
    if (source.length === 0 || source.length > 64 || weight < 0 || weight > 100) {
      throw new Error("Retrieval preset values are outside supported bounds");
    }
  }
}

/** Project a validated canonical preset onto the legacy flat hit-test contract. */
export function retrievalPresetToFlatRequest(
  config: RetrievalPresetConfig
): Partial<RetrieveRequest> {
  validateRetrievalPresetConfig(config);
  const alpha = config.fusion.alpha;
  const request: Partial<RetrieveRequest> = {
    mode: config.mode,
    top_k: config.top_k,
    score_threshold: config.score_threshold ?? undefined,
    rerank: config.rerank.enabled,
    rerank_model: config.rerank.model,
    rerank_top_n: config.rerank.top_n ?? undefined,
    mmr: config.mmr.enabled,
    mmr_lambda: config.mmr.lambda,
    mmr_threshold: config.mmr.similarity_threshold ?? undefined,
  };
  if (config.mode !== "keyword") {
    request.vector_top_k = config.vector.top_k;
  }
  if (config.mode !== "vector") {
    request.keyword_top_k = config.keyword.top_k;
    request.keyword_candidate_k = config.keyword.candidate_pool_size;
  }
  if (config.mode === "hybrid") {
    request.fusion = config.fusion.strategy;
    request.fusion_method = config.fusion.strategy;
    request.dense_weight = alpha;
    request.bm25_weight = 1 - alpha;
    request.rrf_k = config.fusion.rrf_k;
    request.rrf_weights = config.fusion.rrf_weights;
    request.alpha = alpha;
  }
  return request;
}

/** Built-in retrieval presets (fast / balanced / accurate / diverse / sota). */
export async function listRetrievalPresets(options: KnowledgeRequestOptions = {}) {
  const { data } = await api.get<RetrievalPresetsResponse>(
    "/api/v1/knowledge/retrieval/presets",
    {
      signal: options.signal,
      timeout: options.timeoutMs,
    }
  );
  return data;
}

// ============================================================
// QA Testing APIs
// ============================================================

export async function qaQuery(datasetId: string, req: QARequest) {
  const { data } = await api.post<QAResponse>(`/api/v1/knowledge/${datasetId}/qa`, req);
  return data;
}

export async function* qaQueryStream(
  datasetId: string,
  req: QARequest,
  signal?: AbortSignal
) {
  const url = `/api/v1/knowledge/${datasetId}/qa/stream`;
  for await (const chunk of sseFetch<QAStreamEvent>(url, {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify(req),
    signal,
  })) {
    yield chunk;
  }
}


// ============================================================
// Configuration APIs
// ============================================================

export async function getDatasetConfig(datasetId: string) {
  const { data } = await api.get<DatasetConfig>(`/api/v1/knowledge/${datasetId}/config`);
  return data;
}

/**
 * PUT /knowledge/{id}/config semantics (knowledge_service route
 * update_dataset_config, PRD T1):
 * - chunking_config REPLACES the stored chunking object wholesale — always
 *   send a complete object (fields omitted here are reset). It is validated
 *   by ChunkingConfigSchema (extra="forbid"), which accepts more fields than
 *   the local ChunkingConfig interface types, so the payload is an open
 *   record; callers must restrict keys to CHUNKING_CONFIG_API_FIELDS.
 * - retrieval_config is a recursive PATCH — only explicitly supplied fields
 *   (including nested fusion/rerank/mmr keys) overwrite stored values, so
 *   omitting a field preserves it. Hence the deep-partial type below.
 * - Flat legacy retrieval fields (fusion_strategy/alpha/rerank_*...) pass the
 *   schema but are ignored at retrieval time; send the nested objects.
 */
export interface RetrievalConfigPatch {
  mode?: RetrieveMode;
  top_k?: number;
  score_threshold?: number;
  vector?: Partial<VectorRetrievalConfig>;
  keyword?: Partial<KeywordRetrievalConfig>;
  fusion?: Partial<FusionConfig>;
  rerank?: Partial<RerankConfig>;
  mmr?: Partial<MMRConfig>;
}

export async function updateDatasetConfig(
  datasetId: string,
  config: {
    chunking_config?: Record<string, unknown>;
    retrieval_config?: RetrievalConfigPatch;
    embedding_provider?: string;
    embedding_model?: string;
    embedding_dimension?: number;
  }
) {
  const { data } = await api.put(`/api/v1/knowledge/${datasetId}/config`, config);
  return data;
}

export async function debugDataset(datasetId: string) {
  const { data } = await api.get<DatasetDebugInfo>(`/api/v1/knowledge/${datasetId}/debug`);
  return data;
}


// ============================================================
// Sources APIs
// ============================================================

export interface DatasetSources {
  file_uploads: { count: number };
  url_imports: { count: number };
  confluence_bindings: Array<{
    binding_id: string;
    space_name: string;
    page_count: number;
    status: string;
    last_success_at: string | null;
    has_problem: boolean;
    manual_sync_available?: boolean;
    current_sync_task?: SourceSyncTask | null;
  }>;
  connector_status: "not_configured" | "available" | "unavailable";
  total_documents: number;
  source_binding_available?: boolean;
  source_connection_controls_available?: boolean;
  source_connection_reconnect?: { available: boolean; reason: string | null };
  source_connection_setup?: { available: boolean; reason: string | null };
}

export async function getDatasetSources(datasetId: string): Promise<DatasetSources> {
  const { data } = await api.get<DatasetSources>(
    `/api/v1/knowledge/${datasetId}/sources`
  );
  return data;
}


/** Exact durable receipt; processed_items counts attempts, not successful updates. */
export interface SourceSyncTask {
  task_id: string;
  binding_id: string;
  status: "pending" | "processing" | "completed" | "failed";
  progress: number | null;
  total_items: number | null;
  processed_items: number | null;
  synced_items: number | null;
  failed_items: number | null;
  error_count: number | null;
  error_summary: string | null;
  started_at: string | null;
  completed_at: string | null;
}

export async function startSourceSync(datasetId: string, bindingId: string, requestId: string): Promise<SourceSyncTask> {
  const { data } = await api.post<SourceSyncTask>(
    `/api/v1/knowledge/${encodeURIComponent(datasetId)}/sources/confluence/${encodeURIComponent(bindingId)}/sync`,
    { request_id: requestId }
  );
  return data;
}

export async function getSourceSync(datasetId: string, bindingId: string, requestId: string): Promise<SourceSyncTask> {
  const { data } = await api.get<SourceSyncTask>(
    `/api/v1/knowledge/${encodeURIComponent(datasetId)}/sources/confluence/${encodeURIComponent(bindingId)}/sync/${encodeURIComponent(requestId)}`
  );
  return data;
}


// ============================================================
// Batch Operations APIs
// ============================================================

/** UI request chunk. The server accepts larger durable batches; keeping
 * request chunks bounded makes partial progress and retries reviewable. */
export const DOCUMENT_BATCH_REINDEX_LIMIT = 100;
export const DOCUMENT_BATCH_DELETE_LIMIT = 100;

export type DocumentBatchStatus =
  | "pending"
  | "running"
  | "completed"
  | "partial"
  | "failed";

export interface DocumentBatchProblemItem {
  document_id: string;
  status: "skipped" | "failed";
  error_code?: string | null;
  error?: string | null;
}

export interface DocumentBatchOperation {
  operation_id: string;
  tenant_id: string;
  dataset_id: string;
  operation: "reembed" | "delete";
  status: DocumentBatchStatus;
  total_count: number;
  queued_count: number;
  skipped_count: number;
  failed_count: number;
  problem_items: DocumentBatchProblemItem[];
  problem_items_truncated: boolean;
  created_at?: string;
  updated_at?: string;
  completed_at?: string | null;
}

export function isDocumentBatchTerminal(status: DocumentBatchStatus): boolean {
  return status === "completed" || status === "partial" || status === "failed";
}

export async function batchReindexDocuments(
  datasetId: string,
  documentIds: string[] = [],
  allDocuments = false
): Promise<DocumentBatchOperation> {
  const { data } = await api.post<DocumentBatchOperation>(
    `/api/v1/knowledge/${datasetId}/documents/batch-reindex`,
    { document_ids: documentIds, all_documents: allDocuments }
  );
  return data;
}

export async function batchDeleteDocuments(
  datasetId: string,
  documentIds: string[]
): Promise<DocumentBatchOperation> {
  const { data } = await api.post<DocumentBatchOperation>(
    `/api/v1/knowledge/${datasetId}/documents/batch-delete`,
    { document_ids: documentIds }
  );
  return data;
}

export async function getDocumentBatchOperation(
  datasetId: string,
  operationId: string
): Promise<DocumentBatchOperation> {
  const { data } = await api.get<DocumentBatchOperation>(
    `/api/v1/knowledge/${datasetId}/document-batches/${operationId}`
  );
  return data;
}

export async function waitForDocumentBatchOperation(
  datasetId: string,
  operationId: string,
  options: { timeoutMs?: number; pollMs?: number } = {}
): Promise<DocumentBatchOperation> {
  const deadline = Date.now() + (options.timeoutMs ?? 120_000);
  const pollMs = options.pollMs ?? 500;
  while (true) {
    const operation = await getDocumentBatchOperation(datasetId, operationId);
    if (isDocumentBatchTerminal(operation.status)) return operation;
    if (Date.now() >= deadline) throw new Error("document batch operation timed out");
    await new Promise((resolve) => setTimeout(resolve, pollMs));
  }
}


// ============================================================
// Chunk Preview API
// ============================================================

export interface ChunkPreviewItem {
  content: string;
  char_count: number;
  token_count: number;
  metadata?: Record<string, unknown>;
}

export interface ChunkPreviewResponse {
  total_chunks: number;
  chunks: ChunkPreviewItem[];
}

export async function previewChunking(
  datasetId: string,
  text: string,
  config?: Partial<ChunkingConfig>
) {
  const url = (datasetId === "create" || datasetId === "temp" || datasetId === "preview_temp")
    ? "/api/v1/knowledge/preview"
    : `/api/v1/knowledge/${datasetId}/chunk/preview`;

  const { data } = await api.post<ChunkPreviewResponse>(
    url,
    {
      text,
      config,
    }
  );
  return data;
}

// ============================================================
// Document Version History APIs
// ============================================================

export interface DocumentVersion {
  version_id: string;
  document_id: string;
  version_number: number;
  content: string;
  content_hash: string;
  confluence_version?: number;
  confluence_updated_at?: string;
  title?: string;
  metadata?: Record<string, unknown>;
  word_count: number;
  change_type: "created" | "updated" | "restored" | "deleted";
  change_reason?: string;
  changed_by?: string;
  created_at: string;
}

export interface VersionListResponse {
  versions: DocumentVersion[];
  total: number;
  current_version: number;
}

export interface VersionDiffItem {
  type: "insert" | "delete" | "equal";
  content: string;
  old_line?: number;
  new_line?: number;
}

export interface VersionCompareResponse {
  from_version: number;
  to_version: number;
  diff: VersionDiffItem[];
  stats: {
    additions: number;
    deletions: number;
    changes: number;
  };
}

export interface VersionRestoreResponse {
  document_id: string;
  restored_version: number;
  new_version: number;
  status: string;
}

/**
 * List document versions
 */
export async function listDocumentVersions(
  datasetId: string,
  documentId: string,
  limit = 20,
  offset = 0
): Promise<VersionListResponse> {
  const { data } = await api.get<VersionListResponse>(
    `/api/v1/knowledge/${datasetId}/documents/${documentId}/versions`,
    { params: { limit, offset } }
  );
  return data;
}


/**
 * Compare two versions
 */
export async function compareDocumentVersions(
  datasetId: string,
  documentId: string,
  fromVersion: number,
  toVersion: number
): Promise<VersionCompareResponse> {
  const { data } = await api.get<VersionCompareResponse>(
    `/api/v1/knowledge/${datasetId}/documents/${documentId}/versions/compare`,
    { params: { from_version: fromVersion, to_version: toVersion } }
  );
  return data;
}

/**
 * Restore document to specific version
 */
export async function restoreDocumentVersion(
  datasetId: string,
  documentId: string,
  versionNumber: number
): Promise<VersionRestoreResponse> {
  const { data } = await api.post<VersionRestoreResponse>(
    `/api/v1/knowledge/${datasetId}/documents/${documentId}/versions/${versionNumber}/restore`
  );
  return data;
}

/** Existing saved credentials are never returned by this source-setup API. */
export interface SourceConnection { connection_id: string; name: string; }
export interface SourceSpace { connection_id: string; space_key: string; space_id: string; space_name: string; root_page_id: string; root_page_title: string; }
export interface SourceBindingRequest extends Omit<SourceSpace, "space_name" | "root_page_title"> { request_id: string; }
export interface SourceBindingReceipt extends SourceSpace { binding_id: string; dataset_id: string; sync_mode: "manual"; }
export async function getSourceConnections(datasetId: string, offset = 0, signal?: AbortSignal) {
  const { data } = await api.get<{ connections: SourceConnection[]; next_offset: number | null }>(
    `/api/v1/knowledge/${encodeURIComponent(datasetId)}/sources/confluence/connections`, { params: { offset }, signal });
  return data;
}
export async function getSourceSpace(datasetId: string, connectionId: string, spaceKey: string, signal?: AbortSignal) {
  const { data } = await api.get<SourceSpace>(
    `/api/v1/knowledge/${encodeURIComponent(datasetId)}/sources/confluence/connections/${encodeURIComponent(connectionId)}/space`,
    { params: { space_key: spaceKey }, signal });
  return data;
}
export async function createSourceBinding(datasetId: string, request: SourceBindingRequest) {
  const { data } = await api.post<SourceBindingReceipt>(
    `/api/v1/knowledge/${encodeURIComponent(datasetId)}/sources/confluence/bindings`, request);
  return data;
}
export async function getSourceBinding(datasetId: string, requestId: string, signal?: AbortSignal) {
  const { data } = await api.get<SourceBindingReceipt>(
    `/api/v1/knowledge/${encodeURIComponent(datasetId)}/sources/confluence/bindings/${encodeURIComponent(requestId)}`, { signal });
  return data;
}


export interface SourceConnectionIntent { request_id: string; name: string; domain: string; }
export interface SourceConnectionReceipt { connection_id: string; name: string; domain: string; status: string; sync_mode: "manual"; }
export async function createSourceConnection(datasetId: string, request: SourceConnectionIntent & { email: string; api_token: string }) {
  const { data } = await api.post<SourceConnectionReceipt>(`/api/v1/knowledge/${encodeURIComponent(datasetId)}/sources/confluence/connections`, request);
  return data;
}
export async function getSourceConnection(datasetId: string, requestId: string, signal?: AbortSignal) {
  const { data } = await api.get<SourceConnectionReceipt>(`/api/v1/knowledge/${encodeURIComponent(datasetId)}/sources/confluence/connections/${encodeURIComponent(requestId)}`, { signal });
  return data;
}


export interface SourceConnectionControl extends Omit<SourceConnectionReceipt, "sync_mode"> { sync_mode: string; revision: string; }
export interface SourceConnectionImpact {
  connection: SourceConnectionControl;
  binding_count: number;
  bindings: Array<{ binding_id: string; dataset_id: string; space_name: string | null; status: string | null }>;
  hidden_count: number;
  next_offset: number | null;
}
export interface SourceConnectionChange {
  connection_id: string;
  expected_revision: string;
  expected_status: "active" | "disabled";
  status: "active" | "disabled";
}
export async function getManagedSourceConnections(datasetId: string, offset = 0, signal?: AbortSignal) {
  const { data } = await api.get<{ connections: SourceConnectionControl[]; next_offset: number | null }>(
    `/api/v1/knowledge/${encodeURIComponent(datasetId)}/sources/confluence/connections/manage`, { params: { offset }, signal });
  return data;
}
export async function getSourceConnectionImpact(datasetId: string, connectionId: string, offset = 0, signal?: AbortSignal) {
  const { data } = await api.get<SourceConnectionImpact>(
    `/api/v1/knowledge/${encodeURIComponent(datasetId)}/sources/confluence/connections/${encodeURIComponent(connectionId)}/control`, { params: { offset }, signal });
  return data;
}
export async function changeSourceConnection(datasetId: string, change: SourceConnectionChange) {
  const { connection_id, ...body } = change;
  const { data } = await api.patch<SourceConnectionControl>(
    `/api/v1/knowledge/${encodeURIComponent(datasetId)}/sources/confluence/connections/${encodeURIComponent(connection_id)}/control`, body);
  return data;
}

export interface SourceCredentialIntent {
  operation: "credentials"; connection_id: string; expected_revision: string; expected_status: "active" | "disabled";
}
export async function replaceSourceCredentials(datasetId: string, intent: SourceCredentialIntent, credentials: { email: string; api_token: string }) {
  const { data } = await api.post<SourceConnectionControl>(
    `/api/v1/knowledge/${encodeURIComponent(datasetId)}/sources/confluence/connections/${encodeURIComponent(intent.connection_id)}/credentials`,
    { expected_revision: intent.expected_revision, expected_status: intent.expected_status, ...credentials });
  return data;
}
