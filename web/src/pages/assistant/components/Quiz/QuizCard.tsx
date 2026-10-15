/**
 * QuizCard — Main quiz container rendered within the chat stream.
 *
 * 7-state machine:
 *   idle → quiz → quiz-all-answered → submitting → result → review
 *                              │          │
 *                              │          └─► submit-error → (retry → submitting)
 *                              └◄── back-to-quiz from quiz-all-answered ──┘
 *   retake (from result) → clears state → idle
 *
 * Phase 3 retheme: single-accent (gold) palette. Gold is used sparingly —
 * ONLY on progress bar fill, selected option border, active tab indicator
 * in review, primary CTA tinted fill, and the correct-answer check glyph.
 * All other chrome lives on the neutral --assistant-* token family.
 */

import {
  useCallback,
  useEffect,
  useMemo,
  useRef,
  useState,
} from "react";
import { useTranslation } from "react-i18next";
import type { TFunction } from "i18next";
import { motion, AnimatePresence } from "framer-motion";
import {
  AlertTriangle,
  BookOpen,
  ChevronLeft,
  ChevronRight,
  Link2,
  Loader2,
  RefreshCw,
  Send,
} from "lucide-react";
import { getQuizAttemptResult, submitQuiz, submitSharedQuiz } from "@/api/quiz";
import { useAuthStore } from "@/store/useAuthStore";
import { generateUUID } from "@/lib/utils";
import type { QuizData, QuizAttemptResult } from "../../types";
import { QuizIdle } from "./QuizIdle";
import { QuizQuestion } from "./QuizQuestion";
import { QuizResult } from "./QuizResult";
import { QuizShareDialog } from "./QuizShareDialog";
import {
  clearPersisted,
  inferPhase,
  readPersisted,
  writePersisted,
  type PersistedPhase,
  type ViewMode,
} from "./quizState";

interface QuizCardProps {
  quizData: QuizData;
  /** Callback to store result on the ChatMessage */
  onResult?: (result: QuizAttemptResult) => void;
  /** Pre-existing result (if already submitted) */
  existingResult?: QuizAttemptResult;
  /**
   * Render scope. "main" = authed user in the main app; "share" = anonymous
   * viewer on `/share/:shareCode`. Scope decides both the submit endpoint
   * AND the localStorage namespace so an author's in-progress state never
   * leaks into a share viewer's session (and vice versa).
   */
  scope?: "main" | "share";
  /** Required when scope="share". Short share code from the URL. */
  shareCode?: string;
  /**
   * Optional explicit user-scope override for scope="main". When omitted
   * we derive from the auth store. Falls back to "user" if no user loaded.
   */
  userScopeId?: string;
}

type ReviewFilter = "all" | "wrong" | "unanswered";

// ---------------------------------------------------------------------------
// Component
// ---------------------------------------------------------------------------

export function QuizCard({
  quizData,
  onResult,
  existingResult,
  scope = "main",
  shareCode,
  userScopeId,
}: QuizCardProps) {
  const { t } = useTranslation();
  const authUserId = useAuthStore((s) => s.user?.user_id);

  const questions = quizData.questions;
  const totalQuestions = questions.length;

  // Resolve the localStorage namespace for this viewing context.
  // Stable across re-renders so writePersisted / readPersisted / clearPersisted
  // all use the same key.
  const scopeId =
    scope === "share"
      ? `${shareCode || "share-unknown"}${userScopeId ? `:${userScopeId}` : ""}`
      : userScopeId || authUserId || "user";

  // --- hydrate -----------------------------------------------------------
  // Server-restored result wins over persisted progress; falling back to
  // the client's stored phase lets a refresh drop the user back on the
  // correct question without re-taking the quiz.
  type HydrationState = {
    currentIndex: number;
    selectedAnswers: Record<string, string>;
    result: QuizAttemptResult | undefined;
    viewMode: ViewMode;
    attemptId?: string;
  };

  const hydratedQuizIdRef = useRef<string | null>(null);
  const resolveHydration = (): HydrationState => {
    const persisted = readPersisted(scopeId, quizData.quiz_id, questions);
    const restoredResult = existingResult ?? persisted?.result;

    if (restoredResult) {
      return {
        currentIndex: persisted?.currentIndex ?? 0,
        selectedAnswers: persisted?.selectedAnswers ?? {},
        result: restoredResult,
        viewMode: "result",
        attemptId: persisted?.attemptId,
      };
    }
    if (!persisted) {
      return {
        currentIndex: 0,
        selectedAnswers: {},
        result: undefined,
        viewMode: "idle",
        attemptId: undefined,
      };
    }
    let phase: PersistedPhase = persisted.phase ?? "quiz";
    if (phase === "idle") {
      phase = inferPhase(persisted.selectedAnswers, questions, undefined);
    }
    return {
      currentIndex: persisted.currentIndex ?? 0,
      selectedAnswers: persisted.selectedAnswers ?? {},
      result: undefined,
      viewMode: phase as ViewMode,
      attemptId: persisted.attemptId,
    };
  };

  // eslint-disable-next-line react-hooks/exhaustive-deps
  const initial = useMemo(() => resolveHydration(), []);

  const [currentIndex, setCurrentIndex] = useState(initial.currentIndex);
  const [selectedAnswers, setSelectedAnswers] = useState<Record<string, string>>(
    initial.selectedAnswers,
  );
  const [result, setResult] = useState<QuizAttemptResult | undefined>(
    initial.result,
  );
  const [viewMode, setViewMode] = useState<ViewMode>(initial.viewMode);
  const attemptIdRef = useRef<string | null>(initial.attemptId ?? null);
  const submitInFlightRef = useRef(false);

  useEffect(() => {
    if (scope !== "main" || result || !attemptIdRef.current) return;
    let disposed = false;
    void getQuizAttemptResult(quizData.quiz_id, attemptIdRef.current).then((saved) => {
      if (disposed) return;
      setResult(saved);
      setViewMode("result");
    }).catch(() => {
      // A not-yet-accepted submission remains manually retryable with the
      // same stable attempt id and selected answers.
    });
    return () => { disposed = true; };
  }, [quizData.quiz_id, result, scope]);
  const [reviewIndex, setReviewIndex] = useState(0);
  const [reviewFilter, setReviewFilter] = useState<ReviewFilter>("all");
  /** Snapshot of which question ids were empty at submission time. */
  const [unansweredAtSubmit, setUnansweredAtSubmit] = useState<string[]>([]);
  const [showShareDialog, setShowShareDialog] = useState(false);
  const [submitError, setSubmitError] = useState<string | undefined>();
  const [saveFlash, setSaveFlash] = useState(false);
  const lastSaveFlashRef = useRef(0);
  const saveFlashTimerRef = useRef<number | null>(null);

  // Re-hydrate when quiz id changes.
  useEffect(() => {
    if (hydratedQuizIdRef.current === quizData.quiz_id) return;
    hydratedQuizIdRef.current = quizData.quiz_id;
    const h = resolveHydration();
    setCurrentIndex(h.currentIndex);
    setSelectedAnswers(h.selectedAnswers);
    setResult(h.result);
    setViewMode(h.viewMode);
    attemptIdRef.current = h.attemptId ?? null;
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [quizData.quiz_id]);

  // --- save-flash helper -------------------------------------------------
  const triggerSaveFlash = useCallback(() => {
    const now = Date.now();
    if (now - lastSaveFlashRef.current < 1500) return;
    lastSaveFlashRef.current = now;
    setSaveFlash(true);
    if (saveFlashTimerRef.current != null) {
      window.clearTimeout(saveFlashTimerRef.current);
    }
    saveFlashTimerRef.current = window.setTimeout(() => {
      setSaveFlash(false);
      saveFlashTimerRef.current = null;
    }, 3000);
  }, []);

  useEffect(() => {
    return () => {
      if (saveFlashTimerRef.current != null) {
        window.clearTimeout(saveFlashTimerRef.current);
      }
    };
  }, []);

  // --- persistence write -------------------------------------------------
  useEffect(() => {
    if (
      viewMode === "submitting" ||
      viewMode === "submit-error" ||
      viewMode === "idle"
    ) {
      return;
    }
    const phase = viewMode as PersistedPhase;
    writePersisted(scopeId, quizData.quiz_id, {
      v: 2,
      phase,
      selectedAnswers,
      currentIndex,
      result,
      submittedAt: result ? Date.now() : undefined,
      attemptId: attemptIdRef.current ?? undefined,
    });
    if (phase === "quiz" || phase === "quiz-all-answered") {
      triggerSaveFlash();
    }
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [
    quizData.quiz_id,
    selectedAnswers,
    currentIndex,
    result,
    viewMode,
  ]);

  // --- derived -----------------------------------------------------------
  const answeredCount = useMemo(
    () =>
      questions.reduce((n, q) => {
        const a = selectedAnswers[q.id];
        return n + (a != null && a.trim() !== "" ? 1 : 0);
      }, 0),
    [questions, selectedAnswers],
  );
  const allAnswered = useMemo(
    () => questions.length > 0 && answeredCount === questions.length,
    [questions, answeredCount],
  );

  const reviewList = useMemo(() => {
    if (!result) return questions;
    if (reviewFilter === "wrong") {
      const wrongIds = new Set(
        result.per_question
          .filter((pq) => !pq.correct)
          .map((pq) => pq.question_id),
      );
      return questions.filter((q) => wrongIds.has(q.id));
    }
    if (reviewFilter === "unanswered") {
      const set = new Set(unansweredAtSubmit);
      return questions.filter((q) => set.has(q.id));
    }
    return questions;
  }, [result, reviewFilter, questions, unansweredAtSubmit]);

  const currentQuestion = questions[currentIndex];
  const wrongCount = result
    ? result.per_question.filter((pq) => !pq.correct).length
    : 0;
  const unansweredCount = unansweredAtSubmit.length;

  // --- handlers ----------------------------------------------------------
  const handleSelect = useCallback(
    (label: string) => {
      if (result) return;
      if (viewMode !== "quiz" && viewMode !== "quiz-all-answered") return;
      const q = questions[currentIndex];
      if (!q) return;
      setSelectedAnswers((prev) => {
        const next = { ...prev, [q.id]: label };
        const done =
          questions.length > 0 &&
          questions.every((qq) => {
            const a = next[qq.id];
            return a != null && a.trim() !== "";
          });
        if (done && viewMode === "quiz") {
          setViewMode("quiz-all-answered");
        } else if (!done && viewMode === "quiz-all-answered") {
          setViewMode("quiz");
        }
        return next;
      });
    },
    [result, viewMode, questions, currentIndex],
  );

  const doSubmit = useCallback(async () => {
    if (submitInFlightRef.current) return;
    submitInFlightRef.current = true;
    setSubmitError(undefined);
    setViewMode("submitting");
    const unanswered = questions
      .filter((q) => {
        const a = selectedAnswers[q.id];
        return !(a != null && a.trim() !== "");
      })
      .map((q) => q.id);
    setUnansweredAtSubmit(unanswered);
    const attemptId = attemptIdRef.current ?? generateUUID();
    attemptIdRef.current = attemptId;
    writePersisted(scopeId, quizData.quiz_id, {
      v: 2,
      phase: "quiz-all-answered",
      selectedAnswers,
      currentIndex,
      attemptId,
    });
    try {
      const res =
        scope === "share"
          ? await submitSharedQuiz(
              shareCode || "",
              quizData.quiz_id,
              selectedAnswers,
            )
          : await submitQuiz(quizData.quiz_id, selectedAnswers, attemptId);
      setResult(res);
      setViewMode("result");
      onResult?.(res);
    } catch (err) {
      console.error("Quiz submit failed:", err);
      const msg =
        err instanceof Error && err.message
          ? err.message
          : t("assistant.quiz.submitErrorGeneric", "提交时发生未知错误，请重试");
      setSubmitError(msg);
      setViewMode("submit-error");
    } finally {
      submitInFlightRef.current = false;
    }
  }, [scope, scopeId, shareCode, quizData.quiz_id, selectedAnswers, questions, currentIndex, onResult, t]);

  const handleSubmit = useCallback(() => {
    if (!allAnswered) return;
    void doSubmit();
  }, [allAnswered, doSubmit]);

  const handleRetry = useCallback(() => {
    void doSubmit();
  }, [doSubmit]);

  const handleBackToEdit = useCallback(() => {
    setSubmitError(undefined);
    setViewMode("quiz-all-answered");
  }, []);

  const handleStart = useCallback(() => {
    setViewMode("quiz");
    setCurrentIndex(0);
  }, []);

  const handleReview = useCallback(() => {
    setReviewIndex(0);
    setReviewFilter("all");
    setViewMode("review");
  }, []);

  const handleReviewIncorrect = useCallback(() => {
    setReviewIndex(0);
    setReviewFilter("wrong");
    setViewMode("review");
  }, []);

  const handleBackToResult = useCallback(() => {
    setViewMode("result");
  }, []);

  const handleRetake = useCallback(() => {
    clearPersisted(scopeId, quizData.quiz_id);
    attemptIdRef.current = null;
    setSelectedAnswers({});
    setCurrentIndex(0);
    setResult(undefined);
    setSubmitError(undefined);
    setUnansweredAtSubmit([]);
    setReviewFilter("all");
    setReviewIndex(0);
    setViewMode("idle");
  }, [quizData.quiz_id, scopeId]);

  const getQuestionResult = (questionId: string) => {
    if (!result) return undefined;
    const pq = result.per_question.find((x) => x.question_id === questionId);
    if (!pq) return undefined;
    return {
      correct: pq.correct,
      correct_answer: pq.correct_answer,
      explanation: pq.explanation,
    };
  };

  // --- keyboard ----------------------------------------------------------
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (viewMode !== "quiz" && viewMode !== "quiz-all-answered") return;
      if (result || showShareDialog || e.defaultPrevented) return;
      const target = e.target as HTMLElement | null;
      if (target) {
        const tag = target.tagName;
        if (tag === "INPUT" || tag === "TEXTAREA" || target.isContentEditable || target.closest('button, a, select, [role="dialog"]')) {
          return;
        }
      }
      const q = questions[currentIndex];
      if (!q) return;
      const qType = q.question_type || "mc_single";

      if (e.key === "ArrowRight") {
        if (currentIndex < totalQuestions - 1 && selectedAnswers[q.id]) {
          setCurrentIndex((i) => i + 1);
          e.preventDefault();
        }
        return;
      }
      if (e.key === "ArrowLeft") {
        if (currentIndex > 0) {
          setCurrentIndex((i) => i - 1);
          e.preventDefault();
        }
        return;
      }
      if (e.key === "Enter") {
        if (allAnswered) {
          handleSubmit();
          e.preventDefault();
        }
        return;
      }
      if (["1", "2", "3", "4"].includes(e.key)) {
        if (qType === "short_answer") return;
        const idx = parseInt(e.key, 10) - 1;
        const opt = q.options?.[idx];
        if (opt) {
          handleSelect(opt.label);
          e.preventDefault();
        }
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [
    viewMode,
    result,
    questions,
    currentIndex,
    totalQuestions,
    selectedAnswers,
    allAnswered,
    handleSelect,
    handleSubmit,
    showShareDialog,
  ]);

  // --- render helpers ----------------------------------------------------
  const secondaryBtn =
    "act-btn act-hover inline-flex items-center gap-1 h-8 px-2.5 rounded-md text-[13px] font-medium text-[hsl(var(--assistant-text-secondary))] hover:text-[hsl(var(--assistant-text-primary))] disabled:opacity-40 disabled:pointer-events-none";
  const primaryBtn =
    "act-btn inline-flex items-center gap-1.5 h-8 px-3 rounded-md text-[13px] font-medium bg-[hsl(var(--assistant-accent)/0.15)] text-[hsl(var(--assistant-accent))] hover:bg-[hsl(var(--assistant-accent)/0.25)] disabled:opacity-40 disabled:pointer-events-none";

  return (
    <div
      className="relative rounded-[10px] border border-[hsl(var(--assistant-border))] bg-[hsl(var(--assistant-surface-bg))] overflow-hidden w-full"
      style={{ maxWidth: "36rem" }}
    >
      {/* Header */}
      <div className="flex items-center gap-2.5 px-4 py-3 border-b border-[hsl(var(--assistant-border))]">
        <BookOpen className="w-[14px] h-[14px] text-[hsl(var(--assistant-text-secondary))] shrink-0" />
        <div className="flex-1 min-w-0">
          <h3 className="text-[13px] font-medium text-[hsl(var(--assistant-text-primary))] truncate leading-snug">
            {quizData.title}
          </h3>
          <p className="text-[11px] font-mono text-[hsl(var(--assistant-text-tertiary))] mt-0.5">
            {totalQuestions} {t("assistant.quiz.questions", "questions")}
            {quizData.difficulty && ` · ${quizData.difficulty}`}
          </p>
        </div>
        {scope === "main" && <button type="button" onClick={() => setShowShareDialog(true)}
          className="act-btn act-hover shrink-0 p-1.5 rounded-md" aria-label={t("assistant.quiz.shareQuiz")}>
          <Link2 className="w-[14px] h-[14px]" />
        </button>}
        <StateChip
          viewMode={viewMode}
          answeredCount={answeredCount}
          totalQuestions={totalQuestions}
          scorePct={result ? Math.round(result.total_score * 100) : undefined}
          reviewFilter={viewMode === "review" ? reviewFilter : undefined}
          t={t}
        />
      </div>

      <div className="p-4">
        <AnimatePresence mode="wait">
          {/* Idle */}
          {viewMode === "idle" && (
            <QuizIdle
              key="idle"
              quizData={quizData}
              onStart={handleStart}
            />
          )}

          {/* Quiz — one question at a time */}
          {viewMode === "quiz" && currentQuestion && (
            <motion.div key={`q-${currentIndex}`}>
              {/* Progress */}
              <div className="flex items-center gap-3 mb-4">
                <span className="text-[11px] font-mono text-[hsl(var(--assistant-text-tertiary))] tabular-nums">
                  {currentIndex + 1} / {totalQuestions}
                </span>
                <div className="flex-1 h-1 rounded-full bg-[hsl(var(--assistant-border)/0.5)] overflow-hidden">
                  <div
                    className="h-full bg-[hsl(var(--assistant-accent))] transition-all duration-300 ease-out"
                    style={{
                      width: `${((currentIndex + 1) / totalQuestions) * 100}%`,
                    }}
                  />
                </div>
              </div>

              <QuizQuestion
                question={currentQuestion}
                selectedAnswer={selectedAnswers[currentQuestion.id]}
                onSelect={handleSelect}
              />

              {/* Navigation */}
              <div className="flex items-center justify-between mt-5 pt-4 border-t border-[hsl(var(--assistant-border))]">
                <button
                  type="button"
                  disabled={currentIndex === 0}
                  onClick={() => setCurrentIndex((i) => i - 1)}
                  className={secondaryBtn}
                >
                  <ChevronLeft className="w-[14px] h-[14px]" />
                  {t("assistant.quiz.prev", "Previous")}
                </button>

                {currentIndex < totalQuestions - 1 ? (
                  <button
                    type="button"
                    disabled={!selectedAnswers[currentQuestion.id]}
                    onClick={() => setCurrentIndex((i) => i + 1)}
                    className={secondaryBtn}
                  >
                    {t("assistant.quiz.next", "Next")}
                    <ChevronRight className="w-[14px] h-[14px]" />
                  </button>
                ) : (
                  <button
                    type="button"
                    disabled={!allAnswered}
                    onClick={() => {
                      if (allAnswered) setViewMode("quiz-all-answered");
                    }}
                    className={primaryBtn}
                  >
                    <Send className="w-[14px] h-[14px]" />
                    {t("assistant.quiz.submit", "Submit")}
                  </button>
                )}
              </div>

              <p className="mt-3 text-[10px] font-mono text-[hsl(var(--assistant-text-tertiary))] text-center">
                <kbd className="px-1 py-0.5 rounded border border-[hsl(var(--assistant-border))] bg-[hsl(var(--assistant-border)/0.3)] text-[9px]">
                  1-4
                </kbd>
                {" "}{t("assistant.quiz.kbdSelect", "选择")} · {" "}
                <kbd className="px-1 py-0.5 rounded border border-[hsl(var(--assistant-border))] bg-[hsl(var(--assistant-border)/0.3)] text-[9px]">
                  ←
                </kbd>{" "}
                <kbd className="px-1 py-0.5 rounded border border-[hsl(var(--assistant-border))] bg-[hsl(var(--assistant-border)/0.3)] text-[9px]">
                  →
                </kbd>
                {" "}{t("assistant.quiz.kbdNav", "翻页")} · {" "}
                <kbd className="px-1 py-0.5 rounded border border-[hsl(var(--assistant-border))] bg-[hsl(var(--assistant-border)/0.3)] text-[9px]">
                  Enter
                </kbd>
                {" "}{t("assistant.quiz.kbdSubmit", "提交")}
              </p>
            </motion.div>
          )}

          {/* All-answered confirm */}
          {viewMode === "quiz-all-answered" && (
            <motion.div
              key="all-answered"
              initial={{ opacity: 0, y: 8 }}
              animate={{ opacity: 1, y: 0 }}
              exit={{ opacity: 0, y: -8 }}
              transition={{ duration: 0.2 }}
              className="space-y-4"
            >
              <div className="space-y-1">
                <h4 className="text-[13px] font-medium text-[hsl(var(--assistant-text-primary))]">
                  {t("assistant.quiz.allAnsweredTitle", "你已完成")} {totalQuestions}/
                  {totalQuestions} {t("assistant.quiz.questions", "题")}
                </h4>
                <p className="text-[11px] text-[hsl(var(--assistant-text-tertiary))]">
                  {t(
                    "assistant.quiz.allAnsweredHint",
                    "可点击下方任意题号跳回检查，或直接提交。",
                  )}
                </p>
              </div>

              <div className="grid grid-cols-8 sm:grid-cols-10 gap-1.5">
                {questions.map((q, idx) => {
                  const answered =
                    selectedAnswers[q.id] != null &&
                    selectedAnswers[q.id].trim() !== "";
                  return (
                    <button
                      key={q.id}
                      type="button"
                      onClick={() => {
                        setCurrentIndex(idx);
                        setViewMode("quiz");
                      }}
                      className={
                        "aspect-square rounded-md text-[11px] font-mono font-semibold border transition-colors tabular-nums " +
                        (answered
                          ? "border-[hsl(var(--assistant-accent)/0.4)] bg-[hsl(var(--assistant-accent)/0.12)] text-[hsl(var(--assistant-accent))] hover:bg-[hsl(var(--assistant-accent)/0.2)]"
                          : "border-[hsl(var(--assistant-border))] bg-[hsl(var(--assistant-border)/0.2)] text-[hsl(var(--assistant-text-tertiary))] hover:bg-[hsl(var(--assistant-border)/0.4)]")
                      }
                      title={`Q${idx + 1}`}
                    >
                      {idx + 1}
                    </button>
                  );
                })}
              </div>

              <div className="flex items-center justify-between pt-4 border-t border-[hsl(var(--assistant-border))]">
                <button
                  type="button"
                  onClick={() => {
                    setCurrentIndex(0);
                    setViewMode("quiz");
                  }}
                  className={secondaryBtn}
                >
                  <ChevronLeft className="w-[14px] h-[14px]" />
                  {t("assistant.quiz.reviewAnswers", "复查")}
                </button>
                <button
                  type="button"
                  onClick={handleSubmit}
                  className={primaryBtn}
                >
                  <Send className="w-[14px] h-[14px]" />
                  {t("assistant.quiz.submit", "提交")}
                </button>
              </div>
            </motion.div>
          )}

          {/* Submitting */}
          {viewMode === "submitting" && (
            <motion.div
              key="submitting"
              initial={{ opacity: 0 }}
              animate={{ opacity: 1 }}
              exit={{ opacity: 0 }}
              className="flex flex-col items-center justify-center py-12 gap-3"
            >
              <Loader2 className="w-5 h-5 text-[hsl(var(--assistant-accent))] animate-spin" />
              <p className="font-mono text-[11px] text-[hsl(var(--assistant-text-tertiary))]">
                Submitting…
              </p>
            </motion.div>
          )}

          {/* Submit error */}
          {viewMode === "submit-error" && (
            <motion.div
              key="submit-error"
              initial={{ opacity: 0, y: 8 }}
              animate={{ opacity: 1, y: 0 }}
              exit={{ opacity: 0, y: -8 }}
              className="space-y-4"
            >
              <div
                className="flex items-start gap-3 rounded-[10px] border p-4"
                style={{
                  borderColor: "hsl(var(--destructive) / 0.4)",
                  backgroundColor: "hsl(var(--destructive) / 0.08)",
                }}
              >
                <div
                  className="shrink-0 w-8 h-8 rounded-md flex items-center justify-center"
                  style={{
                    backgroundColor: "hsl(var(--destructive) / 0.15)",
                  }}
                >
                  <AlertTriangle
                    className="w-[16px] h-[16px]"
                    style={{ color: "hsl(var(--destructive))" }}
                  />
                </div>
                <div className="flex-1 min-w-0 space-y-1">
                  <h4
                    className="text-[13px] font-medium"
                    style={{ color: "hsl(var(--destructive))" }}
                  >
                    {t("assistant.quiz.submitErrorTitle", "提交失败")}
                  </h4>
                  <p className="text-[11px] text-[hsl(var(--assistant-text-secondary))] wrap-break-word leading-relaxed">
                    {submitError ??
                      t(
                        "assistant.quiz.submitErrorGeneric",
                        "提交时发生未知错误，请重试",
                      )}
                  </p>
                </div>
              </div>
              <div className="flex items-center justify-between gap-2">
                <button
                  type="button"
                  onClick={handleBackToEdit}
                  className={secondaryBtn}
                >
                  <ChevronLeft className="w-[14px] h-[14px]" />
                  {t("assistant.quiz.backToEdit", "返回修改")}
                </button>
                <button
                  type="button"
                  onClick={handleRetry}
                  className={primaryBtn}
                >
                  <RefreshCw className="w-[14px] h-[14px]" />
                  {t("assistant.quiz.retry", "重试")}
                </button>
              </div>
            </motion.div>
          )}

          {/* Result */}
          {viewMode === "result" && result && (
            <motion.div key="result">
              <QuizResult
                result={result}
                quizId={quizData.quiz_id}
                onReview={handleReview}
                onRetake={handleRetake}
                onReviewIncorrect={
                  wrongCount > 0 ? handleReviewIncorrect : undefined
                }
              />

            </motion.div>
          )}

          {/* Review */}
          {viewMode === "review" && result && (
            <motion.div key={`review-${reviewFilter}-${reviewIndex}`}>
              {/* Filter tabs */}
              <div className="flex items-center gap-0.5 mb-4 border-b border-[hsl(var(--assistant-border))]">
                {(
                  [
                    {
                      id: "all" as ReviewFilter,
                      label: t("assistant.quiz.reviewAll", "全部"),
                      count: totalQuestions,
                      enabled: totalQuestions > 0,
                    },
                    {
                      id: "wrong" as ReviewFilter,
                      label: t("assistant.quiz.reviewWrong", "仅错题"),
                      count: wrongCount,
                      enabled: wrongCount > 0,
                    },
                    {
                      id: "unanswered" as ReviewFilter,
                      label: t("assistant.quiz.reviewUnanswered", "仅未答"),
                      count: unansweredCount,
                      enabled: unansweredCount > 0,
                    },
                  ] as const
                )
                  .filter((tab) => tab.enabled)
                  .map((tab) => {
                    const active = reviewFilter === tab.id;
                    return (
                      <button
                        key={tab.id}
                        type="button"
                        onClick={() => {
                          setReviewFilter(tab.id);
                          setReviewIndex(0);
                        }}
                        className={
                          "px-2.5 py-1.5 text-[12px] font-medium border-b-2 -mb-px transition-colors " +
                          (active
                            ? "border-[hsl(var(--assistant-accent))] text-[hsl(var(--assistant-text-primary))]"
                            : "border-transparent text-[hsl(var(--assistant-text-tertiary))] hover:text-[hsl(var(--assistant-text-secondary))]")
                        }
                      >
                        {tab.label}
                        <span className="ml-1 text-[10px] font-mono opacity-70 tabular-nums">
                          {tab.count}
                        </span>
                      </button>
                    );
                  })}

                <button
                  type="button"
                  onClick={handleBackToResult}
                  className="act-btn act-hover ml-auto inline-flex items-center h-7 px-2 rounded-md text-[12px] font-medium text-[hsl(var(--assistant-text-secondary))] hover:text-[hsl(var(--assistant-text-primary))]"
                >
                  {t("assistant.quiz.backToResults", "Back to results")}
                </button>
              </div>

              {reviewList.length === 0 ? (
                <div className="py-10 text-center text-[12px] text-[hsl(var(--assistant-text-tertiary))]">
                  {t("assistant.quiz.reviewEmpty", "该筛选下没有题目。")}
                </div>
              ) : (
                <>
                  <div className="flex items-center gap-3 mb-4">
                    <span className="text-[11px] font-mono text-[hsl(var(--assistant-text-tertiary))] tabular-nums">
                      Review {reviewIndex + 1} / {reviewList.length}
                    </span>
                  </div>

                  {(() => {
                    const q = reviewList[reviewIndex];
                    if (!q) return null;
                    return (
                      <QuizQuestion
                        question={q}
                        selectedAnswer={selectedAnswers[q.id]}
                        onSelect={() => {}}
                        disabled
                        result={getQuestionResult(q.id)}
                      />
                    );
                  })()}

                  <div className="flex items-center justify-between mt-5 pt-4 border-t border-[hsl(var(--assistant-border))]">
                    <button
                      type="button"
                      disabled={reviewIndex === 0}
                      onClick={() => setReviewIndex((i) => i - 1)}
                      className={secondaryBtn}
                    >
                      <ChevronLeft className="w-[14px] h-[14px]" />
                      {t("assistant.quiz.prev", "Previous")}
                    </button>
                    <button
                      type="button"
                      disabled={reviewIndex >= reviewList.length - 1}
                      onClick={() => setReviewIndex((i) => i + 1)}
                      className={secondaryBtn}
                    >
                      {t("assistant.quiz.next", "Next")}
                      <ChevronRight className="w-[14px] h-[14px]" />
                    </button>
                  </div>
                </>
              )}
            </motion.div>
          )}
        </AnimatePresence>
      </div>

      {/* Save indicator */}
      <AnimatePresence>
        {saveFlash && (
          <motion.div
            key="save-flash"
            initial={{ opacity: 0, y: 4 }}
            animate={{ opacity: 1, y: 0 }}
            exit={{ opacity: 0, y: 4 }}
            transition={{ duration: 0.2 }}
            className="pointer-events-none absolute bottom-2 right-3 text-[10px] font-mono text-[hsl(var(--assistant-text-tertiary))]"
          >
            ✓ {t("assistant.quiz.draftSaved", "已保存草稿")}
          </motion.div>
        )}
      </AnimatePresence>

      {/* Share dialog */}
      {scope === "main" && <QuizShareDialog
        quizId={quizData.quiz_id}
        open={showShareDialog}
        onClose={() => setShowShareDialog(false)}
      />}
    </div>
  );
}

// ---------------------------------------------------------------------------
// StateChip — header badge that mirrors the 7-state machine so users can see
// at-a-glance which phase the quiz is in. Uses neutral ink + the single gold
// accent only while the user is actively answering (progress chip) and while
// submitting. Success/destructive tokens carry result + error phases.
// ---------------------------------------------------------------------------
type StateChipProps = {
  viewMode: ViewMode;
  answeredCount: number;
  totalQuestions: number;
  scorePct?: number;
  reviewFilter?: "all" | "wrong" | "unanswered";
  // Accept the real i18next TFunction; a hand-written (key, fallback) => string
  // signature is not assignable from it.
  t: TFunction<"translation", undefined>;
};

function StateChip({
  viewMode,
  answeredCount,
  totalQuestions,
  scorePct,
  reviewFilter,
  t,
}: StateChipProps) {
  // Idle: no chip — the big "开始作答" CTA already signals intent.
  if (viewMode === "idle") return null;

  let label = "";
  let toneClass =
    "text-[hsl(var(--assistant-text-secondary))] border-[hsl(var(--assistant-border))] bg-[hsl(var(--assistant-surface-soft))]";
  let dotClass = "bg-[hsl(var(--assistant-text-tertiary))]";
  let pulse = false;

  if (viewMode === "quiz") {
    label = t("assistant.quiz.chipAnswering", {
      defaultValue: "作答中 · {{done}}/{{total}}",
      done: answeredCount,
      total: totalQuestions,
    }) as string;
    toneClass =
      "text-[hsl(var(--assistant-accent))] border-[hsl(var(--assistant-accent)/0.4)] bg-[hsl(var(--assistant-accent)/0.1)]";
    dotClass = "bg-[hsl(var(--assistant-accent))]";
  } else if (viewMode === "quiz-all-answered") {
    label = t("assistant.quiz.chipReadyToSubmit", "全部已答 · 待提交");
    toneClass =
      "text-[hsl(var(--assistant-accent))] border-[hsl(var(--assistant-accent)/0.4)] bg-[hsl(var(--assistant-accent)/0.1)]";
    dotClass = "bg-[hsl(var(--assistant-accent))]";
  } else if (viewMode === "submitting") {
    label = t("assistant.quiz.chipSubmitting", "提交中…");
    toneClass =
      "text-[hsl(var(--assistant-accent))] border-[hsl(var(--assistant-accent)/0.4)] bg-[hsl(var(--assistant-accent)/0.1)]";
    dotClass = "bg-[hsl(var(--assistant-accent))]";
    pulse = true;
  } else if (viewMode === "submit-error") {
    label = t("assistant.quiz.chipSubmitError", "提交失败");
    toneClass =
      "text-[hsl(var(--destructive))] border-[hsl(var(--destructive)/0.4)] bg-[hsl(var(--destructive)/0.08)]";
    dotClass = "bg-[hsl(var(--destructive))]";
  } else if (viewMode === "result") {
    const pct = scorePct ?? 0;
    const good = pct >= 70;
    label = t("assistant.quiz.chipResult", {
      defaultValue: "已提交 · {{pct}}%",
      pct,
    }) as string;
    toneClass = good
      ? "text-[hsl(var(--success))] border-[hsl(var(--success)/0.4)] bg-[hsl(var(--success)/0.08)]"
      : "text-[hsl(var(--destructive))] border-[hsl(var(--destructive)/0.4)] bg-[hsl(var(--destructive)/0.08)]";
    dotClass = good ? "bg-[hsl(var(--success))]" : "bg-[hsl(var(--destructive))]";
  } else if (viewMode === "review") {
    const filterLabel =
      reviewFilter === "wrong"
        ? t("assistant.quiz.chipReviewWrong", "审阅 · 错题")
        : reviewFilter === "unanswered"
        ? t("assistant.quiz.chipReviewUnanswered", "审阅 · 未答")
        : t("assistant.quiz.chipReview", "审阅模式");
    label = filterLabel;
    toneClass =
      "text-[hsl(var(--assistant-text-secondary))] border-[hsl(var(--assistant-border))] bg-[hsl(var(--assistant-surface-soft))]";
    dotClass = "bg-[hsl(var(--assistant-text-secondary))]";
  }

  return (
    <span
      className={
        "shrink-0 inline-flex items-center gap-1.5 h-6 px-2 rounded-full border text-[11px] font-medium tabular-nums " +
        toneClass
      }
      aria-live="polite"
      role="status"
    >
      <span
        className={
          "w-1.5 h-1.5 rounded-full " +
          dotClass +
          (pulse ? " animate-pulse" : "")
        }
      />
      {label}
    </span>
  );
}
