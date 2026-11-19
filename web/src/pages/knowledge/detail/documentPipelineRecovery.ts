import type { DocumentPipelineAction } from "../../../api/knowledge";

export interface PipelineAttempt {
  documentId: string;
  attemptId: string;
  action: DocumentPipelineAction;
  mutationAccepted?: true;
  executionId?: string;
}

const ACTIONS = new Set(["retry", "recover", "reprocess", "reembed"]);

export function pipelineAttemptKey(userId?: string, datasetId?: string): string | null {
  return userId && datasetId ? `kb-pipeline-attempt:${JSON.stringify([userId, datasetId])}` : null;
}

/** An interrupted request remains unknown, even when a later GET is terminal. */
export function readPipelineAttempts(key: string | null): PipelineAttempt[] {
  if (!key) return [];
  const raw = sessionStorage.getItem(key);
  if (!raw) return [];
  const value: unknown = JSON.parse(raw);
  if (!Array.isArray(value) || !value.every((item) =>
    item && typeof item === "object" && typeof item.documentId === "string"
    && item.documentId.length > 0 && typeof item.attemptId === "string"
    && item.attemptId.length > 0 && ACTIONS.has(item.action))) {
    throw new Error("Invalid processing request receipt");
  }
  return value.map(({ documentId, attemptId, action, mutationAccepted, executionId }) => ({ documentId, attemptId, action,
    ...(mutationAccepted === true ? { mutationAccepted: true as const } : {}),
    ...(typeof executionId === "string" && executionId ? { executionId } : {}),
  }));
}

/** Persist before dispatch; inability to preserve uncertainty blocks the write. */
export function writePipelineAttempts(key: string, attempts: PipelineAttempt[]): void {
  if (attempts.length) {
    sessionStorage.setItem(key, JSON.stringify(attempts.map(({ documentId, attemptId, action, mutationAccepted, executionId }) => ({ documentId, attemptId, action,
      ...(mutationAccepted ? { mutationAccepted: true } : {}), ...(executionId ? { executionId } : {}),
    }))));
  } else {
    sessionStorage.removeItem(key);
  }
}

/** Only explicit request rejection is a known non-acceptance. No network/5xx replay. */
export function isDefinitePipelineRejection(error: unknown): boolean {
  const status = (error as { response?: { status?: number } } | null)?.response?.status;
  return status !== undefined && [400, 401, 403, 404, 405, 409, 413, 422].includes(status);
}
