/**
 * Runtime resolver for the eval dataset linked to a KB dataset.
 *
 * Kept apart from `evalCaseStore.ts` on purpose: the store is pure mapping /
 * diff / hashing logic (unit-tested under `node --test`, which cannot resolve
 * the `@/` alias at runtime), while this module carries the only API import.
 */

import { knowledgeSaveAuthority } from "./knowledgeSaveAuthority";

import {
  createEvalDataset,
  getAgentTraceDetail,
  getLatestKbFailureExample,
  listEvalDatasets,
  saveKbFailureRevision,
  type EvalDataset,
} from "@/api/eval";
import {
  findKbEvalDatasetPaged,
  HIT_TEST_EVAL_SOURCE,
  kbEvalDatasetName,
  knowledgeFailureToImportItem,
  KB_EVAL_DATASET_LIST_LIMIT,
  KB_EVAL_DATASET_SOURCE,
  QA_EVAL_SOURCE,
  sourceVersionsFromHits,
  type KnowledgeFailureHit,
} from "./evalCaseStore";

/**
 * Resolve the eval dataset linked to a KB dataset, creating it on first use.
 * Creation is lazy (save/import path) so a workbench that only reads never
 * writes to the eval store.
 */
export async function resolveKbEvalDataset(
  kbDatasetId: string,
  assertCurrent: () => void = () => {},
): Promise<EvalDataset> {
  assertCurrent();
  const found = await findKbEvalDatasetPaged(kbDatasetId, async offset => {
    assertCurrent();
    const page = await listEvalDatasets({ limit: KB_EVAL_DATASET_LIST_LIMIT, offset });
    assertCurrent();
    return page;
  });
  assertCurrent();
  if (found) return found;
  const created = await createEvalDataset({
    name: kbEvalDatasetName(kbDatasetId),
    description: `Retrieval evaluation cases linked to knowledge dataset ${kbDatasetId}`,
    metadata: { kb_dataset_id: kbDatasetId, source: KB_EVAL_DATASET_SOURCE },
  });
  assertCurrent();
  return created;
}

/**
 * Save a failed KB observation to the platform Eval review queue. The observed
 * hits are evidence, not labels; the user supplies the expected answer. A
 * repeated save of one trace is deduped by case_id in the authoritative store.
 */
export async function saveKnowledgeFailureToEvalDataset(params: {
  kbDatasetId: string;
  query: string;
  expectedAnswer: string;
  observedHits: KnowledgeFailureHit[];
  source: typeof HIT_TEST_EVAL_SOURCE | typeof QA_EVAL_SOURCE;
  sourceTraceId?: string;
  queryFingerprint?: string;
  observedAnswer?: string;
  failureReason?: string;
}, isViewCurrent: () => boolean = () => true): Promise<{ created: boolean; revision: number }> {
  const guard = knowledgeSaveAuthority(isViewCurrent);
  try {
    guard.assertCurrent();
    const snapshot = { ...params, observedHits: params.observedHits.map(hit => ({
      document_id: hit.document_id, segment_id: hit.segment_id,
      source_version: hit.source_version, source_hash: hit.source_hash,
    })) };
    return await saveCurrentKnowledgeFailure(snapshot, guard.assertCurrent);
  } finally {
    guard.close();
  }
}

async function saveCurrentKnowledgeFailure(
  params: Parameters<typeof saveKnowledgeFailureToEvalDataset>[0],
  assertCurrent: () => void,
): Promise<{ created: boolean; revision: number }> {
  if (!params.query.trim() || !params.expectedAnswer.trim()) {
    throw new Error("Question and expected answer are required");
  }
  const evalDataset = await resolveKbEvalDataset(params.kbDatasetId, assertCurrent);
  assertCurrent();
  let confirmedTraceId: string | undefined;
  if (params.sourceTraceId) {
    for (const delayMs of [0, 100, 300]) {
      assertCurrent();
      if (delayMs > 0) {
        await new Promise((resolve) => setTimeout(resolve, delayMs));
        assertCurrent();
      }
      try {
        assertCurrent();
        await getAgentTraceDetail(params.sourceTraceId, "rag");
        assertCurrent();
        confirmedTraceId = params.sourceTraceId;
        break;
      } catch {
        assertCurrent();
        // Trace ingest is asynchronous; retain its ID in metadata for review.
      }
    }
  }
  const item = knowledgeFailureToImportItem({ ...params, confirmedTraceId });
  assertCurrent();
  const latest = await getLatestKbFailureExample(evalDataset.dataset_id, item.case_id);
  assertCurrent();
  const rawRevision = latest?.metadata?.case_revision;
  const expectedRevision = latest
    ? typeof rawRevision === "number" && Number.isInteger(rawRevision) && rawRevision > 0
      ? rawRevision
      : 1
    : 0;
  assertCurrent();
  const response = await saveKbFailureRevision(evalDataset.dataset_id, {
    case_id: item.case_id,
    kb_dataset_id: params.kbDatasetId,
    query: params.query.trim(),
    expected_answer: params.expectedAnswer.trim(),
    source: params.source,
    observed_segment_ids: params.observedHits.map((hit) => hit.segment_id),
    source_versions: sourceVersionsFromHits(params.kbDatasetId, params.observedHits),
    source_trace_id: confirmedTraceId || null,
    kb_trace_id: params.sourceTraceId || null,
    query_fingerprint: params.queryFingerprint || null,
    observed_answer: params.observedAnswer || null,
    failure_reason: params.failureReason?.trim() || null,
    expected_revision: expectedRevision,
  });
  assertCurrent();
  return { created: response.created, revision: response.revision };
}
