export type EmbeddingActionName = "backfill" | "verify" | "gate";
export type NormalizedEmbeddingJobState =
  | "queued"
  | "running"
  | "succeeded"
  | "failed"
  | "cancelled";

export interface EmbeddingActionJobLike {
  job_id: string;
  migration_id: string;
  dataset_id?: string;
  action: EmbeddingActionName;
  state: string;
  error?: string | null;
  poll_after_ms?: number;
  reused?: boolean;
}

export interface EmbeddingActionJobPointer {
  version: 1;
  jobId: string;
  migrationId: string;
  action: EmbeddingActionName;
  pollAfterMs: number;
}

interface StorageLike {
  getItem(key: string): string | null;
  setItem(key: string, value: string): void;
  removeItem(key: string): void;
}

const MIN_POLL_MS = 250;
const DEFAULT_POLL_MS = 1_000;
const MAX_POLL_MS = 5_000;
const JOB_POINTER_PREFIX = "kb-embedding-action-job:v1:";
const ACTIONS = new Set<EmbeddingActionName>(["backfill", "verify", "gate"]);

export function isEmbeddingActionName(value: unknown): value is EmbeddingActionName {
  return typeof value === "string" && ACTIONS.has(value as EmbeddingActionName);
}

export function normalizeEmbeddingActionJobState(
  job: Pick<EmbeddingActionJobLike, "state" | "error">
): NormalizedEmbeddingJobState {
  const state = String(job.state || "").toLowerCase();
  const error = String(job.error || "").toLowerCase();
  if (
    state === "cancelled" ||
    state === "canceled" ||
    (state === "failed" && (error.includes("cancelled") || error.includes("canceled")))
  ) {
    return "cancelled";
  }
  if (state === "queued" || state === "running" || state === "succeeded") {
    return state;
  }
  return "failed";
}

export function shouldPollEmbeddingActionJob(job: EmbeddingActionJobLike): boolean {
  const state = normalizeEmbeddingActionJobState(job);
  return state === "queued" || state === "running";
}

export function canRetryEmbeddingActionJob(job: EmbeddingActionJobLike): boolean {
  return normalizeEmbeddingActionJobState(job) === "failed";
}

export function embeddingActionJobPollDelay(job: EmbeddingActionJobLike): number {
  const requested = Number(job.poll_after_ms);
  if (!Number.isFinite(requested)) return DEFAULT_POLL_MS;
  return Math.min(Math.max(Math.round(requested), MIN_POLL_MS), MAX_POLL_MS);
}

export function mergeEmbeddingActionJob<T extends EmbeddingActionJobLike>(
  previous: T | null,
  next: T
): T {
  if (previous?.job_id !== next.job_id) return next;
  return {
    ...next,
    reused: next.reused ?? previous.reused,
    poll_after_ms: next.poll_after_ms ?? previous.poll_after_ms,
  };
}

export function embeddingActionJobPointerKey(datasetId: string): string {
  return `${JOB_POINTER_PREFIX}${encodeURIComponent(datasetId)}`;
}

export function persistEmbeddingActionJobPointer(
  storage: StorageLike,
  datasetId: string,
  job: EmbeddingActionJobLike
): void {
  const pointer: EmbeddingActionJobPointer = {
    version: 1,
    jobId: job.job_id,
    migrationId: job.migration_id,
    action: job.action,
    pollAfterMs: embeddingActionJobPollDelay(job),
  };
  try {
    storage.setItem(embeddingActionJobPointerKey(datasetId), JSON.stringify(pointer));
  } catch {
    // Browser storage is only a fallback when the server describe surface is
    // unavailable; quota/privacy failures must never break the control plane.
  }
}

export function readEmbeddingActionJobPointer(
  storage: StorageLike,
  datasetId: string,
  migrationId?: string
): EmbeddingActionJobPointer | null {
  const key = embeddingActionJobPointerKey(datasetId);
  let raw: string | null;
  try {
    raw = storage.getItem(key);
  } catch {
    return null;
  }
  if (!raw) return null;
  try {
    const value = JSON.parse(raw) as Partial<EmbeddingActionJobPointer>;
    const jobId = value.jobId;
    const pointerMigrationId = value.migrationId;
    const action = value.action;
    if (
      value.version !== 1 ||
      typeof jobId !== "string" ||
      jobId.length === 0 ||
      typeof pointerMigrationId !== "string" ||
      pointerMigrationId.length === 0 ||
      !isEmbeddingActionName(action) ||
      (migrationId !== undefined && pointerMigrationId !== migrationId)
    ) {
      throw new Error("invalid embedding action job pointer");
    }
    return {
      version: 1,
      jobId,
      migrationId: pointerMigrationId,
      action,
      pollAfterMs: Math.min(
        Math.max(Number(value.pollAfterMs) || DEFAULT_POLL_MS, MIN_POLL_MS),
        MAX_POLL_MS
      ),
    };
  } catch {
    try {
      storage.removeItem(key);
    } catch {
      // Ignore privacy/quota implementations that reject removeItem.
    }
    return null;
  }
}

export function clearEmbeddingActionJobPointer(
  storage: StorageLike,
  datasetId: string,
  expectedJobId?: string
): void {
  if (expectedJobId) {
    const pointer = readEmbeddingActionJobPointer(storage, datasetId);
    if (pointer && pointer.jobId !== expectedJobId) return;
  }
  try {
    storage.removeItem(embeddingActionJobPointerKey(datasetId));
  } catch {
    // Server describe remains authoritative when browser storage is blocked.
  }
}

export function selectServerEmbeddingActionJob<T extends EmbeddingActionJobLike>(
  activeJob: T | null | undefined,
  recentJobs: T[] | null | undefined,
  migrationId: string | null | undefined
): T | null {
  // active_action_job is dataset-scoped. During lease recovery it may belong
  // to a just-finished migration rather than the currently selected latest
  // migration; it must still be shown and must block a new dataset action.
  if (activeJob) return activeJob;
  return (
    recentJobs?.find((job) => job.migration_id === migrationId) ?? null
  );
}
