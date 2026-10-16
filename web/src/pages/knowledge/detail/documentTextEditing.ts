import type { Document } from "@/types/knowledge";

/** The browser only offers edits for an active, completed native text source. */
export function canEditTextSource(document: Pick<Document, "source_type" | "status" | "enabled" | "archived" | "metadata">): boolean {
  const processingMode = document.metadata?.processing_mode;
  return document.source_type === "text"
    && document.status === "completed"
    && document.enabled !== false
    && document.archived !== true
    && (processingMode == null || processingMode === "" || processingMode === "text_only");
}
