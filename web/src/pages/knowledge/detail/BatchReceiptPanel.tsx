import { useTranslation } from "react-i18next";
import { Button } from "@/components/ui/button";
import type { Document } from "@/types/knowledge";
import { documentBatchItemOutcomes, hasBlockingUnknown, remainingDocumentBatchIds, summarizeDocumentBatches, type DocumentBatchAttempt } from "./batchOperations";

export interface BatchReceipt {
  /** Local dispatch owner, not a backend idempotency key. */
  attemptId?: string;
  action: "reembed" | "delete";
  requestedCount: number;
  requestedIds: string[];
  allDocuments?: boolean;
  attempts: DocumentBatchAttempt[];
}


export function BatchReceiptPanel({ receipt, documents, busy, onRecheck, onAcknowledge, onSelectRemaining, datasetId, tenantId }: {
  receipt: BatchReceipt;
  documents: Document[];
  busy: boolean;
  onRecheck: () => void;
  onAcknowledge: () => void;
  onSelectRemaining: () => void;
  datasetId?: string;
  tenantId?: string;
}) {
  const { t } = useTranslation();
  const summary = summarizeDocumentBatches(
    receipt.attempts.flatMap((attempt) => attempt.operation && !attempt.outcomeUnknown ? [attempt.operation] : [])
  );
  const remainingIds = remainingDocumentBatchIds(receipt, datasetId, tenantId);
  const attemptedIds = new Set(receipt.attempts.flatMap((attempt) => attempt.requestedIds));
  const notSubmittedIds = receipt.requestedIds.filter((id) => !attemptedIds.has(id));
  const attemptedCount = receipt.allDocuments ? receipt.requestedCount : attemptedIds.size;
  const unknownCount = attemptedCount - summary.total;
  const titleById = new Map(documents.map((document) => [document.document_id, document.title]));
  const outcomes = receipt.attempts.flatMap((attempt) => Array.from(documentBatchItemOutcomes(attempt)));
  return (
    <div data-testid="document-batch-receipt" role="status" className="rounded-lg border border-border bg-card p-3 text-sm space-y-2">
      <p className="font-medium">
        {t(receipt.action === "delete" ? "knowledge.detail.batchDeleteDone" : "knowledge.detail.batchReindexDone")}
      </p>
      <p className="text-muted-foreground">
        {t("knowledge.detail.batchReceiptCounts", {
          total: receipt.requestedCount,
          successLabel: t(receipt.action === "reembed"
            ? "knowledge.detail.batchReceiptQueued" : "knowledge.detail.batchReceiptDeleted"),
          succeeded: summary.succeeded,
          skipped: summary.skipped,
          failed: summary.failed,
          unknown: Math.max(0, unknownCount),
          notSubmitted: notSubmittedIds.length,
        })}
      </p>
      {receipt.attempts.some((attempt) => attempt.outcomeUnknown || attempt.operation?.problem_items_truncated) && (
        <p className="text-amber-700 dark:text-amber-400">
          {t("knowledge.detail.batchReceiptUnknownHint")}
        </p>
      )}
      {hasBlockingUnknown(receipt.attempts) && (
        <p className="text-amber-700 dark:text-amber-400">
          {t("knowledge.detail.batchReceiptBlocksNew")}
        </p>
      )}
      {receipt.attempts.some((attempt) => attempt.outcomeUnknown && attempt.operation?.operation_id) && (
        <Button size="sm" variant="outline" disabled={busy} onClick={onRecheck}>
          {t("knowledge.detail.batchReceiptRecheck")}
        </Button>
      )}
      {receipt.attempts.some((attempt) => attempt.outcomeUnknown && !attempt.operation?.operation_id && !attempt.acknowledged) && (
        <Button size="sm" variant="outline" disabled={busy} onClick={onAcknowledge}>
          {t("knowledge.detail.batchReceiptAcknowledge")}
        </Button>
      )}
      {remainingIds.length > 0 && (
        <Button size="sm" variant="outline" disabled={busy} onClick={onSelectRemaining}>
          {t("knowledge.detail.batchReceiptSelectRemaining", { count: remainingIds.length })}
        </Button>
      )}
      <div className="max-h-40 overflow-auto border-t border-border/60 pt-2 space-y-1">
        {receipt.attempts.map((attempt, index) => (
          <div key={attempt.operation?.operation_id ?? `unknown-${index}`}>
            <p className="text-xs text-muted-foreground">
              {t("knowledge.detail.batchReceiptOperation", { index: index + 1 })}: {attempt.operation?.operation_id ?? t("knowledge.detail.batchReceiptNoId")}
            </p>
          </div>
        ))}
        {outcomes.map(([id, outcome]) => (
          <p key={id} className="text-xs">
            {titleById.get(id) ?? id} <span className="text-muted-foreground">({id})</span>: {t(outcome === "succeeded"
              ? receipt.action === "reembed" ? "knowledge.detail.batchReceiptQueued" : "knowledge.detail.batchReceiptDeleted"
              : `knowledge.detail.batchReceiptStatus.${outcome}`)}
          </p>
        ))}
        {receipt.attempts.flatMap((attempt) => attempt.requestedIds.length === 0
          ? attempt.operation?.problem_items ?? [] : []).map((item) => (
          <p key={item.document_id} className="text-xs">
            {titleById.get(item.document_id) ?? item.document_id}: {t(`knowledge.detail.batchReceiptStatus.${item.status}`)}
          </p>
        ))}
        {notSubmittedIds.map((id) => (
          <p key={`not-submitted-${id}`} className="text-xs">
            {titleById.get(id) ?? id} <span className="text-muted-foreground">({id})</span>: {t("knowledge.detail.batchReceiptNotSubmitted")}
          </p>
        ))}
      </div>
    </div>
  );
}
