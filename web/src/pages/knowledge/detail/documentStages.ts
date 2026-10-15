/**
 * Stage-timing derivation for the document pipeline (migration 101 forward
 * contract, PRD A10/B3).
 *
 * The backend stamps `parsing_started_at` / `splitting_started_at` /
 * `indexing_started_at` (plus `started_at` / `completed_at`) on documents as
 * they move through the queue. When those timestamps are present the row can
 * show per-stage durations instead of the coarse progress bar; when they are
 * absent (legacy rows, lists before dependency D1 lands) the caller renders
 * nothing and the existing StatusBadge progress stays the fallback.
 *
 * Pure functions only — unit-tested with node:test, no React imports.
 */

import type { Document } from "@/types/knowledge";

export type DocumentStage = "parsing" | "splitting" | "indexing";

/** Last stage recorded for the current document attempt; never infer one. */
export function lastStartedDocumentStage(
  doc: Pick<Document, "parsing_started_at" | "splitting_started_at" | "indexing_started_at"> | null | undefined,
): DocumentStage | null {
  if (!doc) return null;
  const stamps: Array<[DocumentStage, number | null]> = [
    ["parsing", toEpochMs(doc.parsing_started_at)],
    ["splitting", toEpochMs(doc.splitting_started_at)],
    ["indexing", toEpochMs(doc.indexing_started_at)],
  ];
  // A retry can stamp parsing after an earlier attempt reached indexing.
  // The latest recorded time is the only stage we can report honestly.
  let latest: [DocumentStage, number] | null = null;
  for (const [stage, startedAt] of stamps) {
    if (startedAt !== null && (!latest || startedAt > latest[1])) {
      latest = [stage, startedAt];
    }
  }
  return latest?.[0] ?? null;
}

export interface StageTiming {
  stage: DocumentStage;
  /** ISO timestamp when the stage started. */
  startedAt: string;
  /**
   * Elapsed milliseconds for the stage. `null` while the stage has no end
   * timestamp yet — callers can either show it as running or compute a live
   * duration against their own `now`.
   */
  durationMs: number | null;
  /** True when the stage has started but has no end timestamp yet. */
  running: boolean;
}

interface StageBounds {
  stage: DocumentStage;
  startField: "parsing_started_at" | "splitting_started_at" | "indexing_started_at";
  /** End timestamps in precedence order; the first parseable one wins. */
  endFields: ReadonlyArray<"splitting_started_at" | "indexing_started_at" | "completed_at">;
}

const STAGE_BOUNDS: readonly StageBounds[] = [
  { stage: "parsing", startField: "parsing_started_at", endFields: ["splitting_started_at", "completed_at"] },
  { stage: "splitting", startField: "splitting_started_at", endFields: ["indexing_started_at", "completed_at"] },
  { stage: "indexing", startField: "indexing_started_at", endFields: ["completed_at"] },
];

function toEpochMs(value: string | undefined | null): number | null {
  if (typeof value !== "string" || !value.trim()) return null;
  const ms = Date.parse(value);
  return Number.isFinite(ms) ? ms : null;
}

/**
 * Build the ordered stage timings for a document. Stages without a start
 * timestamp are skipped entirely (partial data is normal during the backend
 * rollout). Negative durations from clock skew clamp to zero.
 *
 * `nowMs` only matters for reporting the caller's render time; end times come
 * from the document itself. A stage with a start but no end is `running` —
 * callers should only render the timeline for documents whose display status
 * is an active one (queuing/indexing) so "running" stays truthful.
 */
export function buildStageTimings(
  doc: Pick<
    Document,
    | "parsing_started_at"
    | "splitting_started_at"
    | "indexing_started_at"
    | "completed_at"
  > | null | undefined,
): StageTiming[] {
  if (!doc) return [];
  const timings: StageTiming[] = [];
  for (const bounds of STAGE_BOUNDS) {
    const startedAt = doc[bounds.startField];
    if (typeof startedAt !== "string" || !startedAt.trim()) continue;
    const startMs = toEpochMs(startedAt);
    if (startMs === null) continue;
    let endMs: number | null = null;
    for (const field of bounds.endFields) {
      endMs = toEpochMs(doc[field]);
      if (endMs !== null) break;
    }
    timings.push({
      stage: bounds.stage,
      startedAt,
      durationMs: endMs === null ? null : Math.max(0, endMs - startMs),
      running: endMs === null,
    });
  }
  return timings;
}

/**
 * Compact human duration for the stage timeline: `42s`, `3m 5s`, `1h 02m`.
 * Sub-second values render as `<1s`; sub-minute drops the zero-seconds part;
 * hour precision is minutes only (the pipeline never needs seconds there).
 */
export function formatStageDuration(ms: number): string {
  if (!Number.isFinite(ms) || ms < 0) return "-";
  const totalSeconds = Math.round(ms / 1000);
  if (totalSeconds < 1) return "<1s";
  if (totalSeconds < 60) return `${totalSeconds}s`;
  const totalMinutes = Math.floor(totalSeconds / 60);
  const seconds = totalSeconds % 60;
  if (totalMinutes < 60) {
    return seconds > 0 ? `${totalMinutes}m ${seconds}s` : `${totalMinutes}m`;
  }
  const hours = Math.floor(totalMinutes / 60);
  const minutes = totalMinutes % 60;
  return `${hours}h ${String(minutes).padStart(2, "0")}m`;
}

/** Live duration for a running stage at the caller's render time. */
export function runningStageDurationMs(timing: StageTiming, nowMs: number): number {
  const startMs = toEpochMs(timing.startedAt);
  if (startMs === null) return 0;
  return Math.max(0, nowMs - startMs);
}
