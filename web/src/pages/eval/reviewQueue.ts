import type { EvalExample } from "@/api/eval";

export function kbDeepLinkNavigationKey(locationKey: string, kbDatasetId: string): string {
  return `${locationKey}:${kbDatasetId}`;
}

function revision(example: EvalExample): number {
  const value = example.metadata?.case_revision;
  return typeof value === "number" && Number.isInteger(value) && value > 0 ? value : 1;
}

/** Older KB revisions remain inspectable in Eval, but only the latest is reviewed. */
export function latestReviewCandidates(examples: EvalExample[]): EvalExample[] {
  const latest = new Map<string, EvalExample>();
  for (const example of examples) {
    if (example.metadata?.source_kind !== "kb_failure") continue;
    const caseId = example.metadata?.case_id;
    if (typeof caseId !== "string" || !caseId) continue;
    const prior = latest.get(caseId);
    if (!prior || revision(example) > revision(prior)) latest.set(caseId, example);
  }
  return examples.filter((example) => {
    const status = example.metadata?.review_status;
    if (status !== "pending" && status !== "needs_fix") return false;
    if (example.metadata?.source_kind !== "kb_failure") return true;
    const caseId = example.metadata?.case_id;
    return typeof caseId !== "string" || !caseId || latest.get(caseId) === example;
  });
}

export function canApproveReviewExample(example: EvalExample): boolean {
  if (example.metadata?.source_kind !== "kb_failure") return true;
  const expected = example.expected_output?.answer;
  return Boolean(
    example.source_trace_id
    && typeof expected === "string"
    && expected.trim()
  );
}

export function kbFailureSourceVersions(example: EvalExample) {
  const raw = example.metadata?.kb_source_versions;
  if (!Array.isArray(raw)) return [];
  return raw.filter((item): item is {
    kb_dataset_id: string;
    document_id: string;
    segment_id: string;
    source_version: number;
    source_hash: string;
  } => Boolean(
    item && typeof item === "object"
    && typeof item.kb_dataset_id === "string"
    && typeof item.document_id === "string"
    && typeof item.segment_id === "string"
    && typeof item.source_version === "number"
    && Number.isInteger(item.source_version)
    && item.source_version > 0
    && typeof item.source_hash === "string"
    && /^[0-9a-f]{64}$/i.test(item.source_hash)
  ));
}
