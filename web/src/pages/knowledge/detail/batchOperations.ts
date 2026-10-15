import type { DocumentBatchOperation } from "@/api/knowledge";

export interface DocumentBatchSummary {
  total: number;
  succeeded: number;
  skipped: number;
  failed: number;
}

export interface DocumentBatchAttempt {
  requestedIds: string[];
  operation?: DocumentBatchOperation;
  /** A request or receipt poll failed; the server may still be processing it. */
  outcomeUnknown: boolean;
  /** Explicitly released after the operator checked a request with no ID. */
  acknowledged?: boolean;
}

export function hasBlockingUnknown(attempts: readonly DocumentBatchAttempt[]): boolean {
  return attempts.some((attempt) => attempt.outcomeUnknown && !attempt.acknowledged);
}

/** Reconcile a prior ambiguous attempt by operation ID; never submit it again. */
export async function settleDocumentBatchAttempt(
  requestedIds: string[],
  previous: DocumentBatchAttempt | undefined,
  submit: () => Promise<DocumentBatchOperation>,
  wait: (operationId: string) => Promise<DocumentBatchOperation>,
  onAccepted?: (operation: DocumentBatchOperation) => void
): Promise<DocumentBatchAttempt> {
  if (previous && !previous.outcomeUnknown) return previous;
  if (previous && !previous.operation?.operation_id) return previous;
  let operation = previous?.operation;
  try {
    if (!operation) {
      operation = await submit();
      onAccepted?.(operation);
    }
    const resolved = await wait(operation.operation_id);
    if (resolved.operation_id !== operation.operation_id) throw new Error("batch receipt identity mismatch");
    operation = resolved;
    return { requestedIds, operation, outcomeUnknown: false };
  } catch {
    return { requestedIds, operation, outcomeUnknown: true };
  }
}

export type DocumentBatchItemOutcome = "succeeded" | "skipped" | "failed" | "unknown";

/** Only infer per-item success when the terminal receipt accounts for every requested ID. */
export function documentBatchItemOutcomes(
  attempt: DocumentBatchAttempt
): Map<string, DocumentBatchItemOutcome> {
  const outcomes = new Map<string, DocumentBatchItemOutcome>();
  const operation = attempt.operation;
  for (const id of attempt.requestedIds) outcomes.set(id, "unknown");
  if (!operation || attempt.outcomeUnknown || operation.problem_items_truncated) return outcomes;
  const problems = new Map(operation.problem_items.map((item) => [item.document_id, item.status]));
  const reconciled = operation.total_count === attempt.requestedIds.length
    && operation.queued_count + operation.skipped_count + operation.failed_count === operation.total_count
    && problems.size === operation.skipped_count + operation.failed_count
    && [...problems.keys()].every((id) => outcomes.has(id));
  if (!reconciled) return outcomes;
  for (const id of attempt.requestedIds) {
    outcomes.set(id, problems.get(id) ?? "succeeded");
  }
  return outcomes;
}

export function partitionIds(ids: Iterable<string>, size: number): string[][] {
  if (!Number.isSafeInteger(size) || size < 1) throw new Error("batch size must be positive");
  const normalized = Array.from(new Set(Array.from(ids).filter(Boolean)));
  const chunks: string[][] = [];
  for (let offset = 0; offset < normalized.length; offset += size) {
    chunks.push(normalized.slice(offset, offset + size));
  }
  return chunks;
}

export function summarizeDocumentBatches(
  operations: readonly DocumentBatchOperation[]
): DocumentBatchSummary {
  return operations.reduce<DocumentBatchSummary>(
    (summary, operation) => ({
      total: summary.total + operation.total_count,
      succeeded: summary.succeeded + operation.queued_count,
      skipped: summary.skipped + operation.skipped_count,
      failed: summary.failed + operation.failed_count,
    }),
    { total: 0, succeeded: 0, skipped: 0, failed: 0 }
  );
}

/** Local selection recovery is unavailable unless every submitted identity reconciles. */
export function remainingDocumentBatchIds(value: unknown, datasetId?: string, tenantId?: string): string[] {
  if (!datasetId || !tenantId || !value || typeof value !== "object") return [];
  const receipt = value as Record<string, unknown>;
  const ids = receipt.requestedIds;
  if (receipt.allDocuments || (receipt.action !== "reembed" && receipt.action !== "delete")
    || !Array.isArray(ids) || !ids.length || ids.some((id) => typeof id !== "string" || !id.trim())
    || new Set(ids).size !== ids.length || receipt.requestedCount !== ids.length
    || !Array.isArray(receipt.attempts) || !receipt.attempts.length) return [];
  const requested = new Set(ids), seen = new Set<string>(), successes = new Set<string>(), operations = new Set<string>();
  for (const value of receipt.attempts) {
    if (!value || typeof value !== "object") return [];
    const attempt = value as Record<string, unknown>;
    const submitted = attempt.requestedIds;
    if (attempt.outcomeUnknown !== false || !Array.isArray(submitted) || !submitted.length
      || submitted.some((id) => typeof id !== "string" || !requested.has(id) || seen.has(id))
      || new Set(submitted).size !== submitted.length || !attempt.operation || typeof attempt.operation !== "object") return [];
    const operation = attempt.operation as Record<string, unknown>;
    if (typeof operation.operation_id !== "string" || !operation.operation_id.trim()
      || operations.has(operation.operation_id)
      || operation.dataset_id !== datasetId || operation.tenant_id !== tenantId || operation.operation !== receipt.action
      || typeof operation.status !== "string" || !["completed", "partial", "failed"].includes(operation.status)
      || operation.problem_items_truncated !== false || operation.total_count !== submitted.length
      || ![operation.queued_count, operation.skipped_count, operation.failed_count].every((n) => Number.isSafeInteger(n) && (n as number) >= 0)
      || !Array.isArray(operation.problem_items)) return [];
    operations.add(operation.operation_id);
    const queued = operation.queued_count as number, skipped = operation.skipped_count as number, failed = operation.failed_count as number;
    if (queued + skipped + failed !== submitted.length) return [];
    const problems = new Set<string>(); let skippedItems = 0, failedItems = 0;
    for (const value of operation.problem_items) {
      if (!value || typeof value !== "object") return [];
      const item = value as Record<string, unknown>;
      if (typeof item.document_id !== "string" || !submitted.includes(item.document_id) || problems.has(item.document_id)) return [];
      if (item.status === "skipped") skippedItems++;
      else if (item.status === "failed") failedItems++;
      else return [];
      problems.add(item.document_id);
    }
    if (skippedItems !== skipped || failedItems !== failed) return [];
    for (const id of submitted as string[]) { seen.add(id); if (!problems.has(id)) successes.add(id); }
  }
  return (ids as string[]).filter((id) => !successes.has(id));
}
