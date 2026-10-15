/**
 * QuizIdle — Cover card shown before the user starts answering a quiz.
 *
 * Displays title, question count, difficulty, question-type breakdown,
 * a single "开始作答" primary button and a keyboard-hint line.
 *
 * Phase 3 retheme: single-accent (gold) palette. Gold appears only on the
 * primary "开始作答" CTA (tinted fill). Everything else uses neutral
 * --assistant-* tokens. No pills, no second accent.
 */

import { useMemo } from "react";
import { useTranslation } from "react-i18next";
import { motion } from "framer-motion";
import { PlayCircle } from "lucide-react";
import type { QuizData } from "../../types";

interface QuizIdleProps {
  quizData: QuizData;
  onStart: () => void;
}

const TYPE_LABEL_ZH: Record<string, string> = {
  mc_single: "单选",
  mc_multi: "多选",
  true_false: "判断",
  short_answer: "简答",
};

const TYPE_LABEL_EN: Record<string, string> = {
  mc_single: "single",
  mc_multi: "multi",
  true_false: "T/F",
  short_answer: "short",
};

export function QuizIdle({ quizData, onStart }: QuizIdleProps) {
  const { t, i18n } = useTranslation();
  const isZh = (i18n.language || "").toLowerCase().startsWith("zh");

  const preview = useMemo(() => {
    const counts = new Map<string, number>();
    for (const q of quizData.questions) {
      const k = q.question_type || "mc_single";
      counts.set(k, (counts.get(k) ?? 0) + 1);
    }
    const map = isZh ? TYPE_LABEL_ZH : TYPE_LABEL_EN;
    return [...counts.entries()].map(([type, n]) => ({
      type,
      n,
      label: map[type] ?? type,
    }));
  }, [quizData.questions, isZh]);

  const total = quizData.questions.length;
  const difficulty = quizData.difficulty;

  return (
    <motion.div
      key="idle"
      initial={{ opacity: 0, y: 6 }}
      animate={{ opacity: 1, y: 0 }}
      exit={{ opacity: 0, y: -6 }}
      transition={{ duration: 0.15, ease: [0.16, 1, 0.3, 1] }}
      className="space-y-5 py-2"
    >
      {/* Centered title block */}
      <div className="flex flex-col items-center text-center space-y-1.5">
        <h2 className="text-[16px] font-semibold text-[hsl(var(--assistant-text-primary))] leading-snug">
          {quizData.title}
        </h2>
        <p className="text-[12px] font-mono text-[hsl(var(--assistant-text-tertiary))] tabular-nums">
          {total} {t("assistant.quiz.questions")}
          {difficulty && ` · ${difficulty}`}
        </p>
        {quizData.description && (
          <p className="max-w-sm text-[13px] text-[hsl(var(--assistant-text-secondary))] leading-relaxed pt-1">
            {quizData.description}
          </p>
        )}
      </div>

      {/* Type breakdown — plain text, centered, no cards */}
      {preview.length > 0 && (
        <p className="text-center text-[12px] text-[hsl(var(--assistant-text-secondary))]">
          {preview.map((p) => `${p.n} × ${p.label}`).join(" · ")}
        </p>
      )}

      {/* Primary CTA — authorized gold use */}
      <div className="flex justify-center pt-1">
        <button
          type="button"
          onClick={onStart}
          className="act-btn inline-flex items-center gap-2 h-9 px-5 rounded-md text-[13px] font-medium bg-[hsl(var(--assistant-accent)/0.15)] text-[hsl(var(--assistant-accent))] hover:bg-[hsl(var(--assistant-accent)/0.25)] transition-colors duration-150"
        >
          <PlayCircle className="w-[15px] h-[15px]" />
          {t("assistant.quiz.startQuiz")}
        </button>
      </div>

      {/* Keyboard hint */}
      <p className="text-[11px] font-mono text-[hsl(var(--assistant-text-tertiary))] text-center flex flex-wrap items-center justify-center gap-x-2 gap-y-1">
        <span className="inline-flex items-center gap-1">
          <kbd className="px-1 py-0.5 rounded border border-[hsl(var(--assistant-border))] bg-[hsl(var(--assistant-surface-soft))] text-[10px]">
            1-4
          </kbd>
          {t("assistant.quiz.kbdSelect")}
        </span>
        <span>·</span>
        <span className="inline-flex items-center gap-1">
          <kbd className="px-1 py-0.5 rounded border border-[hsl(var(--assistant-border))] bg-[hsl(var(--assistant-surface-soft))] text-[10px]">
            →
          </kbd>
          {t("assistant.quiz.kbdNext")}
        </span>
        <span>·</span>
        <span className="inline-flex items-center gap-1">
          <kbd className="px-1 py-0.5 rounded border border-[hsl(var(--assistant-border))] bg-[hsl(var(--assistant-surface-soft))] text-[10px]">
            ←
          </kbd>
          {t("assistant.quiz.kbdPrev")}
        </span>
        <span>·</span>
        <span className="inline-flex items-center gap-1">
          <kbd className="px-1 py-0.5 rounded border border-[hsl(var(--assistant-border))] bg-[hsl(var(--assistant-surface-soft))] text-[10px]">
            Enter
          </kbd>
          {t("assistant.quiz.kbdSubmit")}
        </span>
      </p>
    </motion.div>
  );
}
