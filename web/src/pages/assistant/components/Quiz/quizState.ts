/**
 * Quiz state-machine types, persistence schema and migration helpers.
 *
 * Split out from QuizCard.tsx so these can be unit-tested and so
 * fast-refresh isn't broken by non-component exports from a TSX file.
 */

import type { QuizAttemptResult } from "../../types";

// ---------------------------------------------------------------------------
// State machine
// ---------------------------------------------------------------------------

export type ViewMode =
  | "idle"
  | "quiz"
  | "quiz-all-answered"
  | "submitting"
  | "submit-error"
  | "result"
  | "review";

/** Persisted phase — transient `submitting` / `submit-error` are never saved. */
export type PersistedPhase = Exclude<ViewMode, "submitting" | "submit-error">;

// ---------------------------------------------------------------------------
// Persistence schemas
// ---------------------------------------------------------------------------

export interface PersistedQuizStateV1 {
  v: 1;
  selectedAnswers: Record<string, string>;
  currentIndex: number;
  result?: QuizAttemptResult;
}

export interface PersistedQuizStateV2 {
  v: 2;
  phase: PersistedPhase;
  selectedAnswers: Record<string, string>;
  currentIndex: number;
  result?: QuizAttemptResult;
  submittedAt?: number;
  attemptId?: string;
}

const STORAGE_PREFIX = "assistant:quiz:v1:";

/**
 * Scope-aware storage key. The author (in the main app) and an anonymous
 * viewer of the public share link must NEVER share localStorage rows for
 * the same quiz_id — their progress and results are independent. We namespace
 * by a caller-supplied scopeId:
 *   - main app:  scopeId = user id (fallback: "user")
 *   - share link: scopeId = shareCode
 *
 * Format: `assistant:quiz:v1:<scopeId>:<quizId>`.
 */
export const storageKey = (scopeId: string, quizId: string) =>
  `${STORAGE_PREFIX}${scopeId}:${quizId}`;

// ---------------------------------------------------------------------------
// Phase inference & migration
// ---------------------------------------------------------------------------

/**
 * Infer a (non-transient, non-idle, non-review) phase from the raw persisted
 * payload. Used for v1→v2 migration and as a recovery fallback.
 */
export function inferPhase(
  selectedAnswers: Record<string, string>,
  questions: { id: string }[],
  result: QuizAttemptResult | undefined,
): Exclude<PersistedPhase, "idle" | "review"> {
  if (result) return "result";
  if (questions.length === 0) return "quiz";
  const answered = questions.every((q) => {
    const a = selectedAnswers[q.id];
    return a != null && a.trim() !== "";
  });
  return answered ? "quiz-all-answered" : "quiz";
}

export function migrateV1toV2(
  v1: PersistedQuizStateV1,
  questions: { id: string }[],
): PersistedQuizStateV2 {
  const phase = inferPhase(v1.selectedAnswers ?? {}, questions, v1.result);
  return {
    v: 2,
    phase,
    selectedAnswers: v1.selectedAnswers ?? {},
    currentIndex: v1.currentIndex ?? 0,
    result: v1.result,
    submittedAt: v1.result ? Date.now() : undefined,
  };
}

// ---------------------------------------------------------------------------
// localStorage I/O
// ---------------------------------------------------------------------------

export function readPersisted(
  scopeId: string,
  quizId: string,
  questions: { id: string }[],
): PersistedQuizStateV2 | null {
  try {
    const raw = localStorage.getItem(storageKey(scopeId, quizId));
    if (!raw) return null;
    const parsed = JSON.parse(raw) as
      | (Partial<PersistedQuizStateV2> & Partial<PersistedQuizStateV1>)
      | null;
    if (!parsed) return null;
    if (parsed.v === 2 && parsed.phase) {
      return parsed as PersistedQuizStateV2;
    }
    if (parsed.v === 1) {
      return migrateV1toV2(parsed as PersistedQuizStateV1, questions);
    }
    return null;
  } catch {
    return null;
  }
}

export function writePersisted(
  scopeId: string,
  quizId: string,
  state: PersistedQuizStateV2,
): void {
  try {
    localStorage.setItem(storageKey(scopeId, quizId), JSON.stringify(state));
  } catch {
    // Quota / disabled storage — silent degrade.
  }
}

export function clearPersisted(scopeId: string, quizId: string): void {
  try {
    localStorage.removeItem(storageKey(scopeId, quizId));
  } catch {
    // ignore
  }
}
