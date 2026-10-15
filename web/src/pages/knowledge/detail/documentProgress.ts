/** Pure cursor/reconnect rules for the document-progress SSE client. */

const DOCUMENT_PROGRESS_STREAM_MAX_RECONNECT_MS = 10_000;

export function parseDocumentProgressEventSequence(
  datasetId: string,
  eventId: string | undefined,
): number | null {
  const raw = String(eventId ?? "").trim();
  const separator = raw.lastIndexOf(":");
  if (separator <= 0 || raw.slice(0, separator) !== datasetId) return null;
  const sequence = Number(raw.slice(separator + 1));
  if (!Number.isSafeInteger(sequence) || sequence < 0) return null;
  return sequence;
}

export function shouldApplyDocumentProgressEvent(
  datasetId: string,
  lastEventId: string | undefined,
  candidateEventId: string | undefined,
  eventType = "progress",
): boolean {
  const candidate = parseDocumentProgressEventSequence(datasetId, candidateEventId);
  if (candidate === null) return false;
  // A reset is emitted when retention or database restore invalidates the
  // stored cursor. It deliberately may move backwards to the current server
  // watermark, after which normal monotonic comparison resumes.
  if (eventType === "reset") return true;
  const previous = parseDocumentProgressEventSequence(datasetId, lastEventId);
  return previous === null || candidate > previous;
}

export function documentProgressReconnectDelay(attempt: number): number {
  const normalizedAttempt = Number.isFinite(attempt)
    ? Math.max(0, Math.floor(attempt))
    : 0;
  return Math.min(
    DOCUMENT_PROGRESS_STREAM_MAX_RECONNECT_MS,
    1_000 * 2 ** Math.min(normalizedAttempt, 4),
  );
}
