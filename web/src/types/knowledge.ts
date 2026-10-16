/**
 * Production-grade Knowledge Base Type Definitions
 * 
 * Aligned with:
 * - Alibaba Cloud Knowledge Base
 * - Dify Knowledge Base
 * - Custom RAG pipeline requirements
 */

// ============================================================
// Base Types
// ============================================================

export type RetrieveMode = "keyword" | "hybrid" | "vector" | "dense" | "bm25";
export type ChunkingMode = "automatic" | "fixed_size" | "paragraph" | "page" | "heading" | "regex" | "separator" | "recursive" | "hierarchical" | "qa";
export type FusionStrategy = "rrf" | "weighted";
export type Visibility = "private" | "tenant" | "public";
/**
 * Internal document lifecycle vocabulary as it appears on the wire.
 *
 * Current pipeline: queued/pending -> parsing -> segmenting -> embedding
 * (-> embedding_images / uploading_images for multimodal) -> completed/failed.
 * Forward contract (migration 101): waiting -> parsing -> splitting ->
 * indexing -> completed/error, plus legacy "syncing" and upload-phase states.
 *
 * UIs must render `display_status` instead of these raw values — see
 * `deriveDisplayStatus` below.
 */
export type DocumentStatus =
  | "pending"
  | "queued"
  | "waiting"
  | "detecting"
  | "processing"
  | "uploading"
  | "parsing"
  | "segmenting"
  | "splitting"
  | "embedding"
  | "embedding_images"
  | "uploading_images"
  | "indexing"
  | "syncing"
  | "uploaded"
  | "paused"
  | "completed"
  | "failed"
  | "error";

/**
 * Display-safe document status vocabulary (Dify-parity contract, PRD T1.1).
 * The backend stamps `display_status` on every document payload so the raw
 * lifecycle states never reach the UI verbatim.
 */
export type DocumentDisplayStatus =
  | "queuing"
  | "indexing"
  | "paused"
  | "error"
  | "available"
  | "disabled"
  | "archived";
export type KBType = "document" | "data" | "image" | "audio_video";
export type UseCase = "basic_qa" | "rich_text_response";

// ============================================================
// Dataset Types
// ============================================================

export interface Dataset {
  dataset_id: string;
  name: string;
  description?: string;
  tenant_id?: string;
  visibility: Visibility;
  kb_type?: KBType;  // document | data | image | audio_video
  use_case?: UseCase;  // basic_qa | rich_text_response
  embedding_provider: string;
  embedding_model: string;
  embedding_dimension?: number;
  embedding_config?: Record<string, unknown>;
  index_config?: IndexConfig;
  collection_name?: string;
  indexing_technique?: string;
  created_at?: string;
  updated_at?: string;
  created_by?: string;
  my_permission?: string;
  statistics?: DatasetStatistics;
}

export interface IndexConfig {
  chunking?: ChunkingConfig;
  retrieval?: RetrievalConfig;
  document_metadata_registry?: DocumentMetadataRegistry;
}

export type DocumentMetadataFieldType = "string" | "number" | "datetime";

export interface DocumentMetadataField {
  name: string;
  label: string;
  type: DocumentMetadataFieldType;
  description?: string;
}

export interface DocumentMetadataRegistry {
  version: 1;
  revision: number;
  fields: DocumentMetadataField[];
}

export interface OffsetPage<T> {
  items: T[];
  total: number;
  limit: number;
  offset: number;
}

// ============================================================
// Document Types
// ============================================================

/** Advisory observations only; server permission and enqueue checks remain authoritative. */
export interface DocumentProcessingDiagnostics {
  last_recorded_stage: string | null;
  next_action: string;
  retry_blocked_reason: string | null;
  retry_requires_editor: boolean;
}

export interface Document {
  document_id: string;
  dataset_id: string;
  title: string;
  content?: string;
  doc_type?: string;
  doc_form?: string;
  doc_language?: string;
  status: DocumentStatus;
  /** Derived display status stamped by the backend (PRD T1.1). When a payload
   *  does not carry it yet, fall back to `resolveDisplayStatus`. */
  display_status?: DocumentDisplayStatus;
  progress?: number;
  error?: string;
  failure_code?: string;
  processing_diagnostics?: DocumentProcessingDiagnostics;
  segment_count?: number;
  word_count?: number;
  char_count?: number;
  size_bytes?: number;
  hit_count?: number;
  enabled?: boolean;
  archived?: boolean;
  /** Archive metadata returned by PATCH /documents/{id}/archive. */
  archived_at?: string;
  archived_by?: string;
  archived_reason?: string;
  metadata?: Record<string, unknown>;
  /** Public source classification; text sources are the only editable sources. */
  source_type?: "text" | "upload" | "url" | "confluence";
  mime_type?: string;
  /** Current published source generation, included by source/detail reads. */
  current_version?: number;
  created_at?: string;
  updated_at?: string;
  created_by?: string;
  /** Lifecycle stage timestamps (migration 101 forward contract). */
  started_at?: string;
  completed_at?: string;
  parsing_started_at?: string;
  splitting_started_at?: string;
  indexing_started_at?: string;
}

/**
 * Reserved backend marker for an enable/unarchive transition that has been
 * durably queued but has not completed its vector rebuild yet. During this
 * phase the backend deliberately keeps the old enabled/archived fields.
 */
export const DOCUMENT_LIFECYCLE_MARKER_KEY = "_document_lifecycle_reindex";

export interface DocumentLifecycleInput extends DocumentDisplayStatusInput {
  metadata?: Record<string, unknown> | null;
}

const DOCUMENT_POLLING_STATUSES = new Set([
  "pending",
  "queued",
  "waiting",
  "detecting",
  "processing",
  "uploading",
  "parsing",
  "segmenting",
  "splitting",
  "embedding",
  "embedding_images",
  "uploading_images",
  "indexing",
  "syncing",
]);

/** Return true while an enable/unarchive rebuild still owns the document. */
export function hasPendingDocumentActivation(
  input: Pick<DocumentLifecycleInput, "metadata"> | null | undefined
): boolean {
  const marker = input?.metadata?.[DOCUMENT_LIFECYCLE_MARKER_KEY];
  if (!marker || typeof marker !== "object" || Array.isArray(marker)) {
    return false;
  }
  return String((marker as Record<string, unknown>).status ?? "").toLowerCase() === "pending";
}

// ============================================================
// Segment Types
// ============================================================

export interface Segment {
  segment_id: string;
  dataset_id: string;
  document_id: string;
  position: number;
  text: string;
  token_count?: number;
  word_count?: number;
  char_count?: number;
  vector_id?: string;
  enabled?: boolean;
  status?: string;
  metadata?: Record<string, unknown>;
  keywords?: string[];
  answer?: string;
  hit_count?: number;
  created_at?: string;
  created_by?: string;
  // Hierarchical segment fields
  level?: 1 | 2 | 3; // 1=document, 2=section(parent), 3=paragraph(child)
  parent_segment_id?: string;
  summary?: string;
  // Image segment fields
  content_type?: "text" | "image";
  image_url?: string;
  image_attachment_id?: string;
  image_filename?: string;
  image_media_type?: string;
  image_file_size?: number;
}

/**
 * Parse the keyword editor's free-text input into the list shape the
 * segment-update API expects. Splits on ASCII/full-width commas and the CJK
 * enumeration comma, trims, drops empties, and de-duplicates (first wins).
 * Count/length limits (100 items, 256 chars each) are validated by callers
 * against the backend SegmentUpdateSchema.
 */
export function parseSegmentKeywords(raw: string): string[] {
  const seen = new Set<string>();
  const keywords: string[] = [];
  for (const part of raw.split(/[,，、]/)) {
    const keyword = part.trim();
    if (!keyword || seen.has(keyword)) continue;
    seen.add(keyword);
    keywords.push(keyword);
  }
  return keywords;
}

// ============================================================
// Chunking Configuration Types
// ============================================================

export interface ChunkingConfig {
  mode: ChunkingMode;
  chunk_size: number;
  chunk_overlap: number;
  overlap?: number;
  max_chunk_size?: number;
  min_chunk_size?: number;
  strict_section_traceability?: boolean;

  // Token-based
  use_token_count?: boolean;
  token_limit?: number;
  max_tokens?: number;
  min_chunk_tokens?: number;
  max_chunk_tokens?: number;
  parent_token_limit?: number;
  child_token_limit?: number;

  // Separators (for separator mode)
  separators?: string[];
  separator?: string;
  primary_separator?: string;
  keep_separator?: boolean;

  // Regex mode
  regex_pattern?: string;
  
  // Heading mode
  heading_patterns?: string[];
  heading_level?: "h1" | "h2" | "h3" | "h4" | "h5" | "h6";
  
  // Paragraph mode
  min_paragraph_length?: number;
  merge_short_paragraphs?: boolean;
  
  // Hierarchical mode
  parent_chunk_size?: number;
  parent_overlap?: number;
  parent_chunk_overlap?: number;
  child_chunk_size?: number;
  child_overlap?: number;
  child_chunk_overlap?: number;
  parent_mode?: "full_doc" | "paragraph" | "section";

  // QA mode
  question_prefix?: string;
  answer_prefix?: string;
  
  // Preprocessing
  remove_extra_spaces?: boolean;
  remove_urls_emails?: boolean;
  normalize_whitespace?: boolean;
  strip_html?: boolean;
  
  // Metadata extraction
  extract_metadata?: boolean;
  metadata_fields?: string[];
  page_marker?: string;

  // Parser/image compatibility fields accepted by the runtime contract.
  preserve_images?: boolean;
  image_context_chars?: number;
  segmentation?: { max_tokens: number };
}

// ============================================================
// Metadata Enhancement Configuration Types
// ============================================================

export interface MetadataEnhancementConfig {
  enabled: boolean;
  extract_title?: boolean;
  extract_summary?: boolean;
  extract_keywords?: boolean;
  extract_entities?: boolean;
  detect_language?: boolean;
}

// ============================================================
// Table Processing Configuration Types
// ============================================================

export interface TableProcessingConfig {
  enabled: boolean;
  mode?: "markdown" | "row_based" | "cell_based" | "structured" | "natural_language";
  include_headers?: boolean;
  generate_summary?: boolean;
  max_rows?: number;
}

// ============================================================
// Retrieval Configuration Types
// ============================================================

export interface VectorRetrievalConfig {
  enabled: boolean;
  top_k: number;
  score_threshold?: number;
}

export interface KeywordRetrievalConfig {
  enabled: boolean;
  top_k: number;
  candidate_pool_size?: number;
  bm25_k1?: number;
  bm25_b?: number;
}

export interface FusionConfig {
  strategy: FusionStrategy;
  rrf_k: number;
  rrf_weights?: Record<string, number>;
  alpha: number;
}

export interface RerankConfig {
  enabled: boolean;
  provider?: "dashscope" | "cohere" | "jina" | "bge";
  model: string;
  top_n?: number;
  score_threshold?: number;
}

export interface MMRConfig {
  enabled: boolean;
  lambda: number;
  similarity_threshold?: number;
}

export interface RetrievalConfig {
  mode: RetrieveMode;
  top_k: number;
  score_threshold?: number;

  vector?: VectorRetrievalConfig;
  keyword?: KeywordRetrievalConfig;
  fusion?: FusionConfig;
  rerank?: RerankConfig;
  mmr?: MMRConfig;
}

// Flat retrieval config for API requests
export interface FlatRetrievalConfig {
  mode?: RetrieveMode | "dense" | "bm25" | "hybrid";  // Support new mode names
  top_k?: number;
  score_threshold?: number;
  vector_top_k?: number;
  keyword_top_k?: number;
  candidate_top_k?: number;
  keyword_candidate_k?: number;
  // Fusion parameters
  fusion?: FusionStrategy;
  fusion_method?: "weighted" | "rrf";  // New: explicit fusion method
  dense_weight?: number;   // New: [0, 1] weight for dense scores
  bm25_weight?: number;    // New: [0, 1] weight for BM25 scores
  rrf_k?: number;
  alpha?: number;  // Legacy: converted to weights
  // Post-processing
  rerank?: boolean;
  rerank_model?: string;
  rerank_top_n?: number;
  mmr?: boolean;
  mmr_lambda?: number;
  mmr_threshold?: number;
}

// ============================================================
// Retrieval Request/Response Types
// ============================================================

export interface RetrieveRequest extends FlatRetrievalConfig {
  query: string;
  document_id?: string | null;
  rrf_weights?: Record<string, number>;
}

export interface RetrieveHit {
  segment_id: string;
  document_id: string;
  text: string;
  score: number;
  metadata?: Record<string, unknown>;
  source_version?: number | null;
  source_hash?: string | null;
}

export interface RetrieveResponse {
  results: RetrieveHit[];
  trace_id?: string;
  query_fingerprint?: string;
  metadata: {
    mode?: string;
    top_k?: number;
    score_threshold?: number;
    vector_hits_count?: number;
    vector_hits_raw_count?: number;  // Before filtering
    keyword_hits_count?: number;
    keyword_hits_raw_count?: number;  // Before filtering
    rerank?: boolean;
    mmr?: boolean;
    collection_name?: string;
    error?: string;
    [key: string]: unknown;
  };
}

export interface QueryHistoryItem {
  id: string;
  dataset_id: string;
  content: string;
  source: string;
  source_app_id?: string | null;
  created_by_role?: string | null;
  created_by?: string | null;
  metadata: Record<string, unknown>;
  trace_id?: string | null;
  query_fingerprint?: string | null;
  mode?: string | null;
  top_k?: number | null;
  hit_count?: number | null;
  stage_timings: Record<string, number>;
  created_at: string;
}

export interface QueryHistoryPage {
  queries: QueryHistoryItem[];
  next_cursor?: string | null;
  has_more: boolean;
}

export type QueryFeedbackTarget = "retrieval_hit" | "qa_answer";
export type QueryFeedbackRating = "positive" | "negative";
export type QueryFeedbackReason =
  | "relevant"
  | "helpful"
  | "well_cited"
  | "irrelevant"
  | "incorrect"
  | "missing_context"
  | "bad_citation"
  | "stale"
  | "unsafe"
  | "other";

export interface QueryFeedbackInput {
  trace_id: string;
  query_fingerprint: string;
  target_type: QueryFeedbackTarget;
  segment_id?: string;
  rating: QueryFeedbackRating;
  reason_code: QueryFeedbackReason;
  comment?: string;
}

export interface QueryFeedback {
  feedback_id: string;
  tenant_id: string;
  dataset_id: string;
  trace_id: string;
  query_fingerprint: string;
  target_type: QueryFeedbackTarget;
  target_id: string;
  rating: QueryFeedbackRating;
  reason_code: QueryFeedbackReason;
  comment?: string | null;
  created_by: string;
  query_content?: string | null;
  created_at: string;
  updated_at: string;
}

export interface QueryFeedbackPage {
  feedback: QueryFeedback[];
  next_cursor?: string | null;
  has_more: boolean;
}

// ============================================================
// QA Testing Types
// ============================================================

export interface LLMConfig {
  provider?: string;
  model?: string;
  temperature?: number;
  max_tokens?: number;
  system_prompt?: string;
}

export interface QARequest {
  query: string;
  top_k?: number;
  mode?: RetrieveMode;
  fusion_method?: "weighted" | "rrf";
  dense_weight?: number;
  bm25_weight?: number;
  document_id?: string | null;
  rerank?: boolean;
  mmr?: boolean;
  llm_config?: LLMConfig;
  include_raw_results?: boolean;
}

export interface QAContextSegment {
  segment_id: string;
  document_id: string;
  text: string;
  score: number;
  metadata?: Record<string, unknown>;
  source_version?: number | null;
  source_hash?: string | null;
}

export interface QAResponse {
  query: string;
  answer: string;
  context_segments: QAContextSegment[];
  retrieval_metadata: Record<string, unknown>;
  timing: {
    retrieval_ms: number;
    llm_ms: number;
    total_ms: number;
  };
  model: string;
  tokens_used?: number;
  trace_id?: string;
  query_fingerprint?: string;
}

export type QAStreamEvent =
  | {
      event: "retrieval";
      data: {
        query: string;
        context_segments: QAContextSegment[];
        retrieval_metadata: Record<string, unknown>;
        trace_id?: string;
        query_fingerprint?: string;
        timing: {
          retrieval_ms: number;
        };
      };
    }
  | {
      event: "delta";
      data: {
        content: string;
      };
    }
  | {
      event: "done";
      data: {
        result: QAResponse;
      };
    }
  | {
      event: "error";
      data: {
        message: string;
      };
    };

export interface QATestCase {
  query: string;
  expected_answer?: string;
  expected_segments?: string[];
}

export interface QATestResult {
  test_case: QATestCase;
  result: QAResponse;
  answer_correct?: boolean;
  retrieval_recall?: number;
  retrieval_precision?: number;
}

export interface QABatchTestResult {
  results: QATestResult[];
  summary: {
    total_tests: number;
    average_retrieval_ms: number;
    average_llm_ms: number;
    average_recall?: number;
    average_precision?: number;
  };
}

// ============================================================
// Dataset Configuration Types
// ============================================================

export interface DatasetStatistics {
  document_count: number;
  available_document_count: number;
  segment_count: number;
  available_segment_count: number;
  word_count: number;
  hit_count: number;
}

export interface DatasetConfig {
  dataset_id: string;
  chunking: ChunkingConfig;
  retrieval: RetrievalConfig;
  parsing?: {
    enabled: boolean;
    source: "default" | "dataset";
    backends: Array<{ name: string; available: boolean }>;
    warnings: string[];
  };
  configuration_warnings?: string[];
  embedding: {
    provider: string;
    model: string;
    dimension?: number;
    collection_name?: string;
  };
  statistics?: DatasetStatistics;
}

export interface DatasetDebugInfo {
  dataset: {
    id: string;
    name: string;
    embedding_provider: string;
    embedding_model: string;
    embedding_dimension?: number;
    collection_name?: string;
  };
  statistics: DatasetStatistics;
  sample_segments: Array<{
    segment_id: string;
    document_id: string;
    text_preview: string;
    token_count: number;
    vector_id?: string;
  }>;
  has_segments: boolean;
  has_collection: boolean;
}

// ============================================================
// Process Rule Types (Dify-compatible)
// ============================================================

export interface PreProcessingRule {
  id: "remove_extra_spaces" | "remove_urls_emails" | "remove_stopwords";
  enabled: boolean;
}

export interface SegmentationConfig {
  separator: string;
  max_tokens: number;
  chunk_overlap: number;
}

export interface ProcessRule {
  mode: "automatic" | "custom" | "hierarchical";
  pre_processing_rules: PreProcessingRule[];
  segmentation: SegmentationConfig;
  parent_mode?: string;
  child_chunk_size?: number;
}

// ============================================================
// Create/Update Request Types
// ============================================================

export interface DatasetCreateRequest {
  dataset_id?: string;
  name: string;
  description?: string;
  visibility?: Visibility;
  kb_type?: KBType;
  use_case?: UseCase;
  embedding_provider: string;
  embedding_model: string;
  embedding_dimension?: number;
  embedding_config?: Record<string, unknown>;
  index_config?: IndexConfig;
  indexing_technique?: "high_quality" | "economy";
}

export interface DocumentCreateTextRequest {
  title: string;
  content: string;
  metadata?: Record<string, unknown>;
  process_rule?: ProcessRule;
  doc_form?: "text_model" | "qa_model";
  doc_language?: string;
}

// ============================================================
// Batch Operation Types
// ============================================================

export interface BatchOperationResult {
  success_count: number;
  failed_count: number;
  failed_ids: string[];
  errors: Record<string, string>;
}

// ============================================================
// Default Configurations
// ============================================================
//
// Single source of truth for KB configuration defaults (PRD T5-1, §5-#19/#20).
// Every inline default in the KB pages (create wizard, settings tab, upload
// dialog, hit-test panel, e2e fixtures) must read from these constants.
//
// The values below are the PRODUCT BASELINE — the majority values the running
// UI already uses (chunk 500/50, hybrid top_k 5, score threshold 0.3, fusion
// alpha 0.7, gte-rerank). Per the "先基线后阈值" discipline they are frozen
// until the T0 baseline metrics exist: unify the source, do not tune values.
//
// `satisfies` keeps the constants assignable to their config types while
// preserving precise member types (callers can read `.fusion.alpha` etc.
// without null assertions). Fields whose config type is a literal union or a
// boolean are widened explicitly: under `satisfies` those keep their literal
// types, which leak through `useState(DEFAULT_*.field)` into
// `SetStateAction<"automatic">`-style states and break setters/switches.
// Exception: DEFAULT_RETRIEVAL_CONFIG.mode keeps its "hybrid" literal — the
// hit-test/settings states declare narrower UI mode unions that accept it.

export const DEFAULT_CHUNKING_CONFIG = {
  mode: "automatic" as ChunkingMode,
  chunk_size: 500,
  chunk_overlap: 50,
  remove_extra_spaces: true as boolean,
  remove_urls_emails: false as boolean,
} satisfies ChunkingConfig;

export const DEFAULT_RETRIEVAL_CONFIG = {
  mode: "hybrid",
  top_k: 5,
  score_threshold: 0.3,
  vector: {
    enabled: true,
    top_k: 20,
  },
  keyword: {
    enabled: true,
    top_k: 20,
  },
  fusion: {
    strategy: "rrf",
    rrf_k: 60,
    alpha: 0.7,
  },
  rerank: {
    enabled: false as boolean,
    model: "gte-rerank",
  },
  mmr: {
    enabled: false as boolean,
    lambda: 0.5,
  },
} satisfies RetrievalConfig;

// ============================================================
// Config API contract mirrors (backend ChunkingConfigSchema)
// ============================================================
//
// PUT /knowledge/{id}/config validates chunking_config with
// ChunkingConfigSchema (extra="forbid"): unknown fields are rejected (422),
// and fields omitted from a chunking save are reset because chunking is
// replaced wholesale. The settings page therefore round-trips stored configs
// through this allow-list when saving. Mirrors
// knowledge_service/api/schemas/knowledge.py::ChunkingConfigSchema.
// regex_pattern is deliberately absent: the backend disables custom regex
// chunking. Every other runtime-safe field is carried so a one-field edit
// cannot erase parser or metadata-extraction settings (D6).
export const CHUNKING_CONFIG_API_FIELDS = [
  "mode",
  "chunk_size",
  "chunk_overlap",
  "overlap",
  "max_chunk_size",
  "min_chunk_size",
  "use_token_count",
  "token_limit",
  "max_tokens",
  "min_chunk_tokens",
  "max_chunk_tokens",
  "parent_token_limit",
  "child_token_limit",
  "separators",
  "separator",
  "primary_separator",
  "keep_separator",
  "heading_level",
  "heading_patterns",
  "min_paragraph_length",
  "merge_short_paragraphs",
  "parent_mode",
  "parent_chunk_size",
  "parent_overlap",
  "parent_chunk_overlap",
  "child_chunk_size",
  "child_overlap",
  "child_chunk_overlap",
  "question_prefix",
  "answer_prefix",
  "remove_extra_spaces",
  "remove_urls_emails",
  "normalize_whitespace",
  "strip_html",
  "extract_metadata",
  "metadata_fields",
  "page_marker",
  "strict_section_traceability",
  "preserve_images",
  "image_context_chars",
  "segmentation",
] as const;

// The only heading_patterns the backend accepts (its
// _validate_safe_chunking_contract rejects any other tuple).
export const SAFE_CHUNK_HEADING_PATTERNS = [
  "^#{1,6}\\s+.+$",
  "^第[一二三四五六七八九十\\d]+[章节条款]",
  "^[A-Z][A-Z \\t]{4,}:?$",
] as const;

/** True when `value` is exactly the backend's safe heading-pattern triple. */
export function isSafeHeadingPatterns(value: unknown): boolean {
  return (
    Array.isArray(value) &&
    value.length === SAFE_CHUNK_HEADING_PATTERNS.length &&
    SAFE_CHUNK_HEADING_PATTERNS.every((pattern, index) => value[index] === pattern)
  );
}

// ============================================================
// Helper Functions
// ============================================================

// NOTE: Use i18n keys (knowledge.chunkModeLabels.*, knowledge.retrieveModeLabels.*)
// instead of these deprecated helpers. They will be removed in a future release.

/** @deprecated Use i18n key knowledge.chunkModeLabels instead */
export function getChunkingModeLabel(mode: ChunkingMode): string {
  // Deprecated: use t("knowledge.chunkModeLabels.<mode>") from i18n
  return mode;
}

/** @deprecated Use i18n key knowledge.chunkModeDescriptions instead */
export function getChunkingModeDescription(mode: ChunkingMode): string {
  // Deprecated: use t("knowledge.chunkModeDescriptions.<mode>") from i18n
  void mode;
  return "";
}

/** @deprecated Use i18n key knowledge.retrieveModeLabels instead */
export function getRetrievalModeLabel(mode: RetrieveMode): string {
  // Deprecated: use t("knowledge.retrieveModeLabels.<mode>") from i18n
  return mode;
}

// ============================================================
// Document Display Status Derivation (PRD T1.1)
// ============================================================

/**
 * The fixed display vocabulary the backend stamps onto every document payload
 * as `display_status` (Dify-parity contract). Kept as a runtime list so the
 * resolver below can fail-closed against unexpected values.
 */
export const DOCUMENT_DISPLAY_STATUS_VOCABULARY: readonly DocumentDisplayStatus[] = [
  "queuing",
  "indexing",
  "paused",
  "error",
  "available",
  "disabled",
  "archived",
];

export interface DocumentDisplayStatusInput {
  status?: string | null;
  enabled?: boolean | null;
  archived?: boolean | null;
  display_status?: DocumentDisplayStatus | string | null;
}

/**
 * Client-side mirror of the backend's `derive_document_display_status`.
 *
 * Collapses the internal lifecycle vocabulary (waiting/parsing/splitting/
 * indexing/completed/error plus legacy syncing and upload-phase states) into
 * the display vocabulary. Precedence, exactly matching the backend:
 *   archived > error/failed > paused > completed(enabled?) > waiting > else.
 * Unknown or in-flight states FAIL CLOSED to "indexing" rather than leaking
 * raw internal text into the UI.
 */
export function deriveDisplayStatus(
  input: DocumentDisplayStatusInput | null | undefined
): DocumentDisplayStatus {
  const doc = input ?? {};
  if (doc.archived === true) {
    return "archived";
  }
  const status = String(doc.status ?? "").trim().toLowerCase();
  if (status === "error" || status === "failed") {
    return "error";
  }
  if (status === "paused") {
    return "paused";
  }
  if (status === "completed") {
    return doc.enabled === false ? "disabled" : "available";
  }
  if (status === "waiting") {
    return "queuing";
  }
  // parsing/splitting/indexing/syncing/uploading*/unknown in-flight states.
  return "indexing";
}

/**
 * Resolve the display status to render: prefer the backend-stamped
 * `display_status` when it is a known vocabulary member, otherwise derive it
 * client-side. This double-safety covers the transition window where some
 * payloads (e.g. list rows before backend dependency D1 lands) do not yet
 * carry `display_status`.
 */
export function resolveDisplayStatus(
  doc: DocumentDisplayStatusInput | null | undefined
): DocumentDisplayStatus {
  const stamped = doc?.display_status;
  if (
    typeof stamped === "string" &&
    (DOCUMENT_DISPLAY_STATUS_VOCABULARY as readonly string[]).includes(stamped)
  ) {
    return stamped as DocumentDisplayStatus;
  }
  return deriveDisplayStatus(doc);
}

/**
 * Decide whether the document list must keep polling.
 *
 * Activation is intentionally two-stage: enable/unarchive first returns a
 * durable `waiting` row with the old enabled/archived value and a pending
 * marker. Checking only the display status would therefore mistake an
 * archived restore for a terminal row and stop before the worker flips it.
 */
export function documentNeedsLifecyclePolling(
  doc: DocumentLifecycleInput | null | undefined
): boolean {
  const rawStatus = String(doc?.status ?? "").trim().toLowerCase();
  if (DOCUMENT_POLLING_STATUSES.has(rawStatus) || hasPendingDocumentActivation(doc)) {
    return true;
  }
  const displayStatus = resolveDisplayStatus(doc);
  return displayStatus === "queuing" || displayStatus === "indexing";
}
