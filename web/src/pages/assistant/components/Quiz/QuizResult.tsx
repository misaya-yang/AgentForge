/**
 * QuizResult — Score summary, per-question review, retake, and attempts history.
 *
 * Phase 3 retheme: single-accent (gold) palette.
 * The score carries weight through typography (large tabular numeral in
 * primary ink), not color. Correct/incorrect pills use semantic
 * --success / --destructive at ~12% alpha. Gold is reserved here for the
 * primary CTA (Retake) — one authorized use.
 */

import { useCallback, useEffect, useState } from "react";
import { useTranslation } from "react-i18next";
import { motion, useReducedMotion } from "framer-motion";
import { CheckCircle2, Clock, RefreshCw, Users, XCircle } from "lucide-react";
import { cn } from "@/lib/utils";
import { listAttempts, type QuizAttemptSummary } from "@/api/quiz";
import type { QuizAttemptResult } from "../../types";

interface QuizResultProps {
  result: QuizAttemptResult;
  quizId?: string;
  onReview?: () => void;
  onRetake?: () => void;
  /** Optional: jump into review mode already filtered to incorrect questions. */
  onReviewIncorrect?: () => void;
}

/**
 * Runs a 0 → target counter over ~300ms (ease-out cubic). Single-shot per mount.
 */
function useCountUp(target: number, durationMs = 300, reduceMotion = false) {
  const [value, setValue] = useState(reduceMotion ? target : 0);
  useEffect(() => {
    if (reduceMotion) {
      setValue(target);
      return;
    }
    setValue(0);
    let raf = 0;
    const start = performance.now();
    const tick = (now: number) => {
      const t = Math.min(1, (now - start) / durationMs);
      // ease-out cubic
      const eased = 1 - Math.pow(1 - t, 3);
      setValue(target * eased);
      if (t < 1) raf = requestAnimationFrame(tick);
    };
    raf = requestAnimationFrame(tick);
    return () => cancelAnimationFrame(raf);
  }, [durationMs, reduceMotion, target]);
  return value;
}

export function QuizResult({
  result,
  quizId,
  onReview,
  onRetake,
  onReviewIncorrect,
}: QuizResultProps) {
  const { t } = useTranslation();
  const shouldReduceMotion = useReducedMotion();
  const pct = Math.round(result.total_score * 100);
  const [showHistory, setShowHistory] = useState(false);
  const [attempts, setAttempts] = useState<QuizAttemptSummary[]>([]);
  const [loadingHistory, setLoadingHistory] = useState(false);

  const animatedPct = useCountUp(pct, 300, Boolean(shouldReduceMotion));
  const animatedCorrect = useCountUp(result.correct_count, 300, Boolean(shouldReduceMotion));
  const incorrectCount = result.per_question.filter((pq) => !pq.correct).length;

  const fetchHistory = useCallback(async () => {
    if (!quizId || loadingHistory) return;
    setLoadingHistory(true);
    try {
      const data = await listAttempts(quizId);
      setAttempts(data.attempts);
      setShowHistory(true);
    } catch {
      // Silently fail — history is optional
    } finally {
      setLoadingHistory(false);
    }
  }, [quizId, loadingHistory]);

  return (
    <motion.div
      initial={shouldReduceMotion ? false : { opacity: 0, y: 6 }}
      animate={{ opacity: 1, y: 0 }}
      transition={{ duration: shouldReduceMotion ? 0 : 0.15, ease: [0.16, 1, 0.3, 1] }}
      className="space-y-5"
    >
      {/* Score counter — large tabular numeral, centered, no chrome */}
      <div className="flex flex-col items-center py-2">
        <div className="flex items-baseline gap-1">
          <span className="text-5xl font-semibold tabular-nums text-[hsl(var(--assistant-text-primary))] leading-none">
            {Math.round(animatedPct)}
          </span>
          <span className="text-2xl font-medium text-[hsl(var(--assistant-text-secondary))]">
            %
          </span>
        </div>
        <p className="mt-2 text-[13px] text-[hsl(var(--assistant-text-secondary))] tabular-nums">
          {t("assistant.quiz.correctOf", {
            correct: Math.round(animatedCorrect),
            total: result.total_count,
          })}
        </p>
      </div>

      {/* Per-question summary */}
      <div className="space-y-1">
        {result.per_question.map((pq) => (
          <div
            key={pq.question_id}
            className={cn(
              "flex items-center gap-2 px-3 py-1.5 rounded-md text-[12px]",
              pq.correct
                ? "bg-[hsl(var(--success)/0.12)] text-[hsl(var(--success))]"
                : "bg-[hsl(var(--destructive)/0.12)] text-[hsl(var(--destructive))]",
            )}
          >
            {pq.correct ? (
              <CheckCircle2 className="w-[14px] h-[14px] shrink-0" />
            ) : (
              <XCircle className="w-[14px] h-[14px] shrink-0" />
            )}
            <span className="font-medium font-mono tabular-nums">Q{pq.question_num}</span>
            <span className="text-[11px] opacity-80 truncate flex-1">
              {pq.correct
                ? t("assistant.quiz.correct", "Correct")
                : t("assistant.quiz.wrongDetail", {
                    defaultValue: "Wrong (you: {{user}}, answer: {{correct}})",
                    user: pq.user_answer,
                    correct: pq.correct_answer,
                  })}
            </span>
          </div>
        ))}
      </div>

      {/* Incorrect-count jump chip — muted destructive tint, hairline only */}
      {incorrectCount > 0 && onReviewIncorrect && (
        <button
          type="button"
          onClick={onReviewIncorrect}
          className="inline-flex items-center gap-1.5 rounded-md border border-[hsl(var(--destructive)/0.4)] bg-[hsl(var(--destructive)/0.06)] px-2.5 py-1 text-[12px] font-medium text-[hsl(var(--destructive))] hover:bg-[hsl(var(--destructive)/0.12)] transition-colors duration-150"
        >
          <XCircle className="w-[13px] h-[13px]" />
          {t("assistant.quiz.wrongCountChip")}{t("assistant.quiz.wrongCountSep")}{incorrectCount}{" "}
          {t("assistant.quiz.countQ")}
        </button>
      )}

      {/* Action buttons */}
      <div className="flex items-center gap-1.5 flex-wrap">
        {onReview && (
          <button
            type="button"
            onClick={onReview}
            className="act-btn act-hover inline-flex items-center h-8 px-2.5 rounded-md text-[13px] font-medium text-[hsl(var(--assistant-text-secondary))] hover:text-[hsl(var(--assistant-text-primary))]"
          >
            {t("assistant.quiz.reviewAnswers")}
          </button>
        )}
        {onRetake && (
          <button
            type="button"
            onClick={onRetake}
            className="act-btn inline-flex items-center gap-1.5 h-8 px-2.5 rounded-md text-[13px] font-medium bg-[hsl(var(--assistant-accent)/0.15)] text-[hsl(var(--assistant-accent))] hover:bg-[hsl(var(--assistant-accent)/0.25)] transition-colors duration-150"
          >
            <RefreshCw className="w-[13px] h-[13px]" />
            {t("assistant.quiz.retake")}
          </button>
        )}
        {quizId && (
          <button
            type="button"
            onClick={fetchHistory}
            disabled={loadingHistory}
            className="act-btn act-hover ml-auto inline-flex items-center gap-1.5 h-8 px-2.5 rounded-md text-[13px] font-medium text-[hsl(var(--assistant-text-secondary))] hover:text-[hsl(var(--assistant-text-primary))] disabled:opacity-40 disabled:pointer-events-none"
          >
            <Users className="w-[13px] h-[13px]" />
            {t("assistant.quiz.viewHistory", "History")}
          </button>
        )}
      </div>

      {/* Attempts history */}
      {showHistory && attempts.length > 0 && (
        <motion.div
          initial={shouldReduceMotion ? false : { opacity: 0, height: 0 }}
          animate={{ opacity: 1, height: "auto" }}
          transition={{ duration: shouldReduceMotion ? 0 : 0.15, ease: [0.16, 1, 0.3, 1] }}
          className="rounded-[10px] border border-[hsl(var(--assistant-border))] overflow-hidden"
        >
          <div className="px-3.5 py-2 border-b border-[hsl(var(--assistant-border))]">
            <span className="text-[11px] font-mono uppercase tracking-wider text-[hsl(var(--assistant-text-tertiary))]">
              {t("assistant.quiz.attemptHistory", "Attempt History")} ({attempts.length})
            </span>
          </div>
          <div className="divide-y divide-[hsl(var(--assistant-border))]">
            {attempts.map((a) => {
              const good = (a.total_score ?? 0) >= 0.7;
              return (
                <div
                  key={a.attempt_id}
                  className="flex items-center gap-3 px-3.5 py-2 text-[12px]"
                >
                  <span className="font-medium text-[hsl(var(--assistant-text-primary))] truncate">
                    {a.display_name || a.user_id || "Anonymous"}
                  </span>
                  <span
                    className={cn(
                      "font-semibold font-mono tabular-nums",
                      good
                        ? "text-[hsl(var(--success))]"
                        : "text-[hsl(var(--assistant-text-secondary))]",
                    )}
                  >
                    {a.correct_count}/{a.total_count} ({Math.round((a.total_score ?? 0) * 100)}%)
                  </span>
                  <span className="text-[hsl(var(--assistant-text-tertiary))] ml-auto flex items-center gap-1 font-mono">
                    <Clock className="w-3 h-3" />
                    {a.completed_at
                      ? new Date(a.completed_at).toLocaleDateString(undefined, {
                          month: "short",
                          day: "numeric",
                          hour: "2-digit",
                          minute: "2-digit",
                        })
                      : "In progress"}
                  </span>
                </div>
              );
            })}
          </div>
        </motion.div>
      )}
    </motion.div>
  );
}
