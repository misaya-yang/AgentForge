/**
 * Persistence bridge between the retrieval eval workbench and the platform
 * eval-dataset store (PRD §5-#22, F6).
 *
 * Every KB dataset gets one linked eval dataset, found by
 * `metadata.kb_dataset_id` (created lazily on first save/import). Workbench
 * cases round-trip as eval examples:
 *
 *   input.query                     <- the test question
 *   expected_output.relevant_segment_ids <- the labelled segments
 *   metadata.case_id                <- stable id the store dedupes by
 *
 * The store's `skip_duplicates` import dedupes on `metadata.case_id` alone
 * (verified against the backend repository), so edited cases must go through
 * PATCH while new cases go through import — `diffEvalCases` computes that
 * split. Persisted cases explicitly removed from the workbench go through the
 * tenant-scoped DELETE endpoint.
 */

import type { EvalDataset, EvalExample, EvalExampleImportItem } from "@/api/eval";

export const KB_EVAL_DATASET_SOURCE = "kb-retrieval-workbench";
export const HIT_TEST_EVAL_SOURCE = "kb-hit-test";
export const QA_EVAL_SOURCE = "kb-qa";
export const KB_FAILURE_CASE_KIND = "kb_failure";
export const KB_FAILURE_EVAL_SPLIT = "review";
export const KB_EVAL_SPLIT = "regression";
export const KB_EVAL_DATASET_LIST_LIMIT = 200;
export const KB_EVAL_EXAMPLE_LIST_LIMIT = 500;

export interface PersistedEvalCase {
  caseId: string;
  exampleId: string;
  query: string;
  relevantSegmentIds: string[];
}

export function kbEvalDatasetName(kbDatasetId: string): string {
  return `kb-retrieval-eval-${kbDatasetId}`;
}

export function findKbEvalDataset(
  datasets: EvalDataset[],
  kbDatasetId: string
): EvalDataset | undefined {
  return datasets.find((dataset) => dataset.metadata?.kb_dataset_id === kbDatasetId);
}

/** Walk the tenant's eval datasets before deciding to create a KB link. */
export async function findKbEvalDatasetPaged(
  kbDatasetId: string,
  loadPage: (offset: number) => Promise<{ datasets: EvalDataset[]; total: number }>
): Promise<EvalDataset | undefined> {
  let offset = 0;
  while (true) {
    const page = await loadPage(offset);
    const found = findKbEvalDataset(page.datasets, kbDatasetId);
    if (found) return found;
    offset += page.datasets.length;
    if (offset >= page.total) return undefined;
    if (page.datasets.length === 0) {
      throw new Error("Eval dataset listing ended before all datasets were returned");
    }
  }
}

/** Normalize `expected_output.relevant_segment_ids`: strings only, trimmed, deduped, order kept. */
export function extractRelevantSegmentIds(value: unknown): string[] {
  if (!Array.isArray(value)) return [];
  const ids = value
    .filter((item): item is string => typeof item === "string" && item.trim().length > 0)
    .map((item) => item.trim());
  return [...new Set(ids)];
}

function metadataCaseId(example: EvalExample): string {
  const raw = example.metadata?.case_id;
  return typeof raw === "string" && raw.trim().length > 0 ? raw.trim() : "";
}

/**
 * Project an eval example into a workbench case. Only examples carrying a
 * non-empty `input.query` materialize — golden sets written by other surfaces
 * (QA references, trace-derived cases) stay in the eval dataset untouched.
 */
export function exampleToEvalCase(example: EvalExample): PersistedEvalCase | null {
  if (example.metadata?.source_kind === KB_FAILURE_CASE_KIND) return null;
  const rawQuery = example.input?.query;
  const query = typeof rawQuery === "string" ? rawQuery.trim() : "";
  if (!query) return null;
  return {
    caseId: metadataCaseId(example) || example.example_id,
    exampleId: example.example_id,
    query,
    relevantSegmentIds: extractRelevantSegmentIds(example.expected_output?.relevant_segment_ids),
  };
}

export interface KnowledgeFailureHit {
  segment_id: string;
  document_id: string;
  metadata?: Record<string, unknown>;
  source_version?: number | null;
  source_hash?: string | null;
}

/** Source identity is recorded only when the response contains a complete pair. */
export function sourceVersionsFromHits(kbDatasetId: string, hits: KnowledgeFailureHit[]) {
  return hits.flatMap((hit) => {
    // Only top-level identities emitted by the API's generation fence count.
    // Segment metadata can contain older, user-authored source fields.
    const version = hit.source_version;
    const hash = hit.source_hash;
    if (
      !hit.document_id || !hit.segment_id ||
      typeof version !== "number" || !Number.isInteger(version) || version <= 0 ||
      typeof hash !== "string" || !/^[0-9a-f]{64}$/i.test(hash)
    ) return [];
    return [{
      kb_dataset_id: kbDatasetId,
      document_id: hit.document_id,
      segment_id: hit.segment_id,
      source_version: version,
      source_hash: hash.toLowerCase(),
    }];
  });
}

/** Keep a failed observation separate from reviewed regression labels. */
export function knowledgeFailureToImportItem(params: {
  kbDatasetId: string;
  query: string;
  expectedAnswer: string;
  observedHits: KnowledgeFailureHit[];
  source: typeof HIT_TEST_EVAL_SOURCE | typeof QA_EVAL_SOURCE;
  sourceTraceId?: string;
  confirmedTraceId?: string;
  queryFingerprint?: string;
  observedAnswer?: string;
  failureReason?: string;
}): EvalExampleImportItem {
  const query = params.query.trim();
  const expectedAnswer = params.expectedAnswer.trim();
  if (!query || !expectedAnswer) throw new Error("Question and expected answer are required");
  const observationId = params.sourceTraceId || params.queryFingerprint || query;
  const caseId = `kb-failure-${params.source}-${hashEvalCaseInput(params.kbDatasetId)}-${hashEvalCaseInput(observationId)}`;
  return {
    case_id: caseId,
    split: KB_FAILURE_EVAL_SPLIT,
    input: { query },
    expected_output: { answer: expectedAnswer },
    expected_trajectory: {},
    assertions: [],
    source_trace_id: params.confirmedTraceId || null,
    metadata: {
      source: params.source,
      source_kind: KB_FAILURE_CASE_KIND,
      review_status: "pending",
      behavior_confirmed: false,
      kb_dataset_id: params.kbDatasetId,
      kb_trace_id: params.sourceTraceId || null,
      kb_query_fingerprint: params.queryFingerprint || null,
      kb_observed_segment_ids: params.observedHits.map((hit) => hit.segment_id),
      kb_source_versions: sourceVersionsFromHits(params.kbDatasetId, params.observedHits),
      kb_observed_answer: params.observedAnswer || null,
      kb_failure_reason: params.failureReason?.trim() || null,
    },
  };
}

/** Build the import item for a workbench case (shape passes backend `validate_case`). */
export function evalCaseToImportItem(params: {
  caseId: string;
  kbDatasetId: string;
  query: string;
  relevantSegmentIds: string[];
  source?: string;
  sourceTraceId?: string;
}): EvalExampleImportItem {
  return {
    case_id: params.caseId,
    split: KB_EVAL_SPLIT,
    input: { query: params.query },
    expected_output: { relevant_segment_ids: [...params.relevantSegmentIds] },
    expected_trajectory: {},
    assertions: [],
    source_trace_id: params.sourceTraceId || null,
    metadata: {
      source: params.source ?? KB_EVAL_DATASET_SOURCE,
      kb_dataset_id: params.kbDatasetId,
    },
  };
}

export interface EvalCaseDiff {
  /** Cases whose case_id the store has not seen: send through import. */
  toImport: EvalExampleImportItem[];
  /** Cases whose case_id exists but whose content changed: send through PATCH. */
  toUpdate: Array<{
    exampleId: string;
    caseId: string;
    query: string;
    relevantSegmentIds: string[];
  }>;
  unchangedCount: number;
  /** Persisted examples the user explicitly removed from the workbench. */
  toDelete: Array<{ exampleId: string; caseId: string }>;
}

function caseContentKey(query: string, relevantSegmentIds: string[]): string {
  return JSON.stringify({ query: query.trim(), ids: [...relevantSegmentIds].sort() });
}

export function diffEvalCases(params: {
  localCases: Array<{ id: string; query: string; relevantSegmentIds: string[] }>;
  serverCases: PersistedEvalCase[];
  kbDatasetId: string;
  /**
   * When supplied, only these explicit removals may be deleted. This prevents
   * a stale workbench from deleting cases another client added concurrently.
   */
  removedExampleIds?: string[];
}): EvalCaseDiff {
  const serverByCaseId = new Map(params.serverCases.map((entry) => [entry.caseId, entry]));
  const localCaseIds = new Set<string>();
  const diff: EvalCaseDiff = {
    toImport: [],
    toUpdate: [],
    unchangedCount: 0,
    toDelete: [],
  };

  for (const localCase of params.localCases) {
    const serverCase = serverByCaseId.get(localCase.id);
    localCaseIds.add(localCase.id);
    const ids = extractRelevantSegmentIds(localCase.relevantSegmentIds);
    if (!serverCase) {
      diff.toImport.push(
        evalCaseToImportItem({
          caseId: localCase.id,
          kbDatasetId: params.kbDatasetId,
          query: localCase.query,
          relevantSegmentIds: ids,
        })
      );
      continue;
    }
    if (
      caseContentKey(localCase.query, ids) ===
      caseContentKey(serverCase.query, serverCase.relevantSegmentIds)
    ) {
      diff.unchangedCount += 1;
      continue;
    }
    diff.toUpdate.push({
      exampleId: serverCase.exampleId,
      caseId: localCase.id,
      query: localCase.query,
      relevantSegmentIds: ids,
    });
  }

  const explicitlyRemoved = params.removedExampleIds
    ? new Set(params.removedExampleIds)
    : null;
  for (const serverCase of params.serverCases) {
    const shouldDelete = explicitlyRemoved
      ? explicitlyRemoved.has(serverCase.exampleId)
      : !localCaseIds.has(serverCase.caseId);
    if (shouldDelete) {
      diff.toDelete.push({
        exampleId: serverCase.exampleId,
        caseId: serverCase.caseId,
      });
    }
  }
  return diff;
}

/**
 * FNV-1a 32-bit over the normalized query. Deterministic case ids make
 * repeated "send to eval set" clicks dedupe through `skip_duplicates`
 * instead of piling up copies.
 */
export function hashEvalCaseInput(query: string): string {
  const normalized = query.trim().replace(/\s+/g, " ");
  let hash = 0x811c9dc5;
  for (let index = 0; index < normalized.length; index += 1) {
    hash ^= normalized.charCodeAt(index);
    hash = Math.imul(hash, 0x01000193) >>> 0;
  }
  return hash.toString(16).padStart(8, "0");
}

export function hitTestEvalCaseId(kbDatasetId: string, query: string): string {
  return `kb-hit-${kbDatasetId}-${hashEvalCaseInput(query)}`;
}
