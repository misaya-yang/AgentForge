import {
  normalizeEmbeddingActionJobState,
  shouldPollEmbeddingActionJob,
  type EmbeddingActionJobLike,
  type NormalizedEmbeddingJobState,
} from "./embeddingMigrationJobs.ts";

export type EmbeddingMigrationActionName =
  | "backfill"
  | "verify"
  | "gate"
  | "cutover"
  | "rollback"
  | "abort";

interface MigrationJobLike {
  state?: string | null;
  totals?: Record<string, unknown> | null;
}

interface MigrationDescriptionLike {
  live_migration?: MigrationJobLike | null;
  latest_migration?: MigrationJobLike | null;
  recent_migrations?: MigrationJobLike[] | null;
  pending_chunks?: number | null;
  enabled_chunks?: number | null;
}

export interface EmbeddingMigrationProgress {
  completed: number;
  pending: number | null;
  total: number;
  percent: number;
}

export interface EmbeddingMigrationControls {
  actions: EmbeddingMigrationActionName[];
  actionJobState: NormalizedEmbeddingJobState | null;
  actionJobActive: boolean;
  canStart: boolean;
}

const POLLED_STATES = new Set([
  "shadow_build",
  "backfilling",
  "verified",
  "gating",
  "ready",
]);

function finiteNonNegative(value: unknown): number | null {
  const numberValue = typeof value === "number" ? value : Number(value);
  if (!Number.isFinite(numberValue) || numberValue < 0) return null;
  return Math.floor(numberValue);
}

/** Prefer the live job, but retain terminal jobs returned by newer servers. */
export function selectEmbeddingMigrationJob<T extends MigrationJobLike>(
  description: (MigrationDescriptionLike & {
    live_migration?: T | null;
    latest_migration?: T | null;
    recent_migrations?: T[] | null;
  }) | null
): T | null {
  return (
    description?.live_migration ??
    description?.latest_migration ??
    description?.recent_migrations?.[0] ??
    null
  );
}

/** Derive progress only from the PostgreSQL authority/receipt ledger. */
export function getEmbeddingMigrationProgress(
  description: MigrationDescriptionLike | null
): EmbeddingMigrationProgress {
  const job = selectEmbeddingMigrationJob(description);
  const totals = job?.totals ?? {};
  const total =
    finiteNonNegative(description?.enabled_chunks) ??
    finiteNonNegative(totals.enabled_chunks) ??
    finiteNonNegative(totals.verified_enabled_chunks) ??
    0;

  let pending = finiteNonNegative(description?.pending_chunks);
  if (pending === null) {
    pending = finiteNonNegative(totals.pending_after_backfill);
  }
  if (
    pending === null &&
    ["verified", "gating", "gate_failed", "ready", "completed"].includes(
      String(job?.state ?? "")
    )
  ) {
    pending = 0;
  }

  const completed = pending === null ? 0 : Math.max(total - pending, 0);
  let percent = 0;
  if (total === 0) {
    if (pending === 0) percent = 100;
  } else {
    percent = Math.min(100, (completed / total) * 100);
  }
  return { completed, pending, total, percent };
}

export function getEmbeddingMigrationActions(
  state: string | null | undefined,
  pendingChunks: number | null
): EmbeddingMigrationActionName[] {
  switch (state) {
    case "shadow_build":
      return ["backfill", "abort"];
    case "backfilling":
    case "failed":
      return pendingChunks === 0
        ? ["backfill", "verify", "abort"]
        : ["backfill", "abort"];
    case "verified":
    case "gating":
    case "gate_failed":
      return ["gate", "abort"];
    case "ready":
      return ["cutover", "abort"];
    case "completed":
      return ["rollback"];
    case "rolled_back":
      return ["abort"];
    default:
      return [];
  }
}

export function canStartEmbeddingMigration(state: string | null | undefined): boolean {
  return !state || state === "completed" || state === "abandoned";
}

export function shouldPollEmbeddingMigration(state: string | null | undefined): boolean {
  return POLLED_STATES.has(String(state ?? ""));
}

export function getEmbeddingMigrationControls(
  migrationState: string | null | undefined,
  pendingChunks: number | null,
  actionJob: EmbeddingActionJobLike | null,
  selectedMigrationId: string | null
): EmbeddingMigrationControls {
  const baseActions = getEmbeddingMigrationActions(migrationState, pendingChunks);
  if (!actionJob) {
    return {
      actions: baseActions,
      actionJobState: null,
      actionJobActive: false,
      canStart: canStartEmbeddingMigration(migrationState),
    };
  }

  const actionJobState = normalizeEmbeddingActionJobState(actionJob);
  const actionJobActive = shouldPollEmbeddingActionJob(actionJob);
  let actions = baseActions;
  if (actionJobActive && actionJob.migration_id !== selectedMigrationId) {
    actions = [];
  } else if (actionJobState === "queued") {
    actions = baseActions.filter((action) => action === "abort");
  } else if (actionJobState === "running") {
    actions = [];
  } else if (actionJobState === "failed") {
    actions = baseActions.filter((action) => action !== actionJob.action);
  }

  return {
    actions,
    actionJobState,
    actionJobActive,
    canStart: canStartEmbeddingMigration(migrationState) && !actionJobActive,
  };
}
