import { useTranslation } from "react-i18next";
import type { Document } from "@/types/knowledge";

const STAGES = new Set(["uploading", "waiting", "parsing", "splitting", "indexing", "completed"]);
const ACTIONS = new Set([
  "retry_document", "wait", "replace_source", "inspect_upload", "inspect_source_sync",
  "inspect_lifecycle", "inspect_execution", "restore_document", "enable_document",
  "inspect_document", "none",
]);

/** Do not render upstream error text or turn advisory guidance into permissions. */
export function DocumentProcessingDiagnostics({
  document,
  compact = false,
}: {
  document: Document;
  compact?: boolean;
}) {
  const { t } = useTranslation();
  const diagnostics = document.processing_diagnostics;
  if (!diagnostics || diagnostics.next_action === "none") return null;
  if (compact && diagnostics.next_action === "wait") return null;
  const stage = diagnostics.last_recorded_stage;
  const stageLabel = t(`knowledge.processingDiagnostics.stages.${stage && STAGES.has(stage) ? stage : "unknown"}`);
  const action = ACTIONS.has(diagnostics.next_action) ? diagnostics.next_action : "inspect_document";

  return (
    <div
      data-testid={`doc-processing-diagnostics-${document.document_id}${compact ? "-compact" : ""}`}
      className={compact ? "space-y-0.5 text-xs text-muted-foreground" : "mt-2 space-y-1 text-sm"}
    >
      <p>{t("knowledge.processingDiagnostics.lastStage", { stage: stageLabel })}</p>
      <p>{t(`knowledge.processingDiagnostics.actions.${action}`)}</p>
      {!compact && (
        <>
          <p className="text-xs">{t("knowledge.processingDiagnostics.documentId", { id: document.document_id })}</p>
          {diagnostics.next_action === "retry_document" && (
            <p className="text-xs">{t("knowledge.processingDiagnostics.retryAdvisory")}</p>
          )}
        </>
      )}
    </div>
  );
}
