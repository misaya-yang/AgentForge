/**
 * Shared formatting utilities — single source of truth for display helpers
 * used across the application (artifacts panel, share page, agent cards, etc.).
 */

/** Format a byte count into a human-readable string (e.g. "1.2 KB"). */
export function formatFileSize(bytes?: number | null): string {
  if (bytes == null) return "";
  if (bytes === 0) return "0 B";
  if (bytes < 1024) return `${bytes} B`;
  if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(1)} KB`;
  return `${(bytes / (1024 * 1024)).toFixed(1)} MB`;
}

/** Format a duration in milliseconds into a human-readable string. */
export function formatDuration(ms: number): string {
  if (ms < 1000) return `${Math.round(ms)}ms`;
  if (ms < 60000) return `${(ms / 1000).toFixed(1)}s`;
  return `${Math.floor(ms / 60000)}m ${Math.floor((ms % 60000) / 1000)}s`;
}

/**
 * Return a short uppercase label for a file format (e.g. "PDF", "DOCX").
 * Checks the format string first, then falls back to MIME type heuristics.
 */
export function getFormatLabel(format?: string, mimeType?: string): string {
  const f = (format || "").toLowerCase();
  if (f === "docx" || f === "doc" || mimeType?.includes("word")) return "DOCX";
  if (f === "pdf" || mimeType?.includes("pdf")) return "PDF";
  if (f === "pptx" || f === "ppt" || mimeType?.includes("presentation")) return "PPTX";
  if (f === "xlsx" || f === "xls" || mimeType?.includes("sheet")) return "XLSX";
  if (f === "md" || f === "markdown") return "MD";
  if (f === "csv") return "CSV";
  if (
    f === "png" ||
    f === "jpg" ||
    f === "jpeg" ||
    f === "gif" ||
    f === "webp" ||
    mimeType?.startsWith("image/")
  ) {
    return (f || "IMG").toUpperCase();
  }
  if (f) return f.toUpperCase().slice(0, 6);
  return "FILE";
}
