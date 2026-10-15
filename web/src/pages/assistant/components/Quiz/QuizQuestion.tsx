/**
 * QuizQuestion — Renders a single question based on its type.
 *
 * Supports: mc_single, mc_multi, true_false, short_answer
 *
 * Phase 3 retheme: single-accent (gold) palette.
 * Gold appears ONLY on the selected-option border and the correct-answer
 * Check glyph (the two authorized uses in this file). Hover, default
 * and incorrect states all use neutral / semantic tokens.
 */

import { useCallback, useMemo } from "react";
import { motion, useReducedMotion } from "framer-motion";
import { Check, X } from "lucide-react";
import { cn } from "@/lib/utils";
import type { QuizQuestionData } from "../../types";

interface QuizQuestionProps {
  question: QuizQuestionData;
  /** For mc_single/true_false: selected label. For mc_multi: comma-separated labels. For short_answer: text. */
  selectedAnswer: string | undefined;
  onSelect: (answer: string) => void;
  disabled?: boolean;
  result?: {
    correct: boolean;
    correct_answer: string;
    explanation?: string;
  };
}

export function QuizQuestion({
  question,
  selectedAnswer,
  onSelect,
  disabled = false,
  result,
}: QuizQuestionProps) {
  const qType = question.question_type || "mc_single";
  const shouldReduceMotion = useReducedMotion();

  return (
    <motion.div
      initial={shouldReduceMotion ? false : { opacity: 0, y: 8 }}
      animate={{ opacity: 1, y: 0 }}
      transition={{ duration: shouldReduceMotion ? 0 : 0.15, ease: [0.16, 1, 0.3, 1] }}
      className="space-y-3.5"
    >
      <p className="text-[14px] font-medium text-[hsl(var(--assistant-text-primary))] leading-relaxed">
        {question.question_text}
        {qType === "mc_multi" && (
          <span className="ml-1.5 text-[11px] font-mono text-[hsl(var(--assistant-text-tertiary))]">
            (Select all that apply)
          </span>
        )}
      </p>

      {qType === "short_answer" ? (
        <ShortAnswerInput
          value={selectedAnswer || ""}
          onChange={onSelect}
          disabled={disabled || result != null}
          result={result}
        />
      ) : (
        <OptionList
          options={question.options}
          questionType={qType}
          selectedAnswer={selectedAnswer || ""}
          onSelect={onSelect}
          disabled={disabled || result != null}
          result={result}
        />
      )}

      {result && result.explanation && (
        <motion.div
          initial={shouldReduceMotion ? false : { opacity: 0, height: 0 }}
          animate={{ opacity: 1, height: "auto" }}
          transition={{ duration: shouldReduceMotion ? 0 : 0.15, ease: [0.16, 1, 0.3, 1] }}
          className="rounded-md bg-[hsl(var(--assistant-surface-soft))] border border-[hsl(var(--assistant-border))] px-3.5 py-2.5"
        >
          <p className="text-[11px] font-mono uppercase tracking-wider text-[hsl(var(--assistant-text-tertiary))] mb-1">
            Explanation
          </p>
          <p className="text-[13px] text-[hsl(var(--assistant-text-primary))] leading-relaxed">
            {result.explanation}
          </p>
        </motion.div>
      )}
    </motion.div>
  );
}

// ---------------------------------------------------------------------------
// Sub-components
// ---------------------------------------------------------------------------

function OptionList({
  options,
  questionType,
  selectedAnswer,
  onSelect,
  disabled,
  result,
}: {
  options: QuizQuestionData["options"];
  questionType: string;
  selectedAnswer: string;
  onSelect: (answer: string) => void;
  disabled: boolean;
  result?: QuizQuestionProps["result"];
}) {
  const isMulti = questionType === "mc_multi";
  const selectedSet = useMemo(
    () =>
      new Set(
        isMulti
          ? selectedAnswer.split(",").filter(Boolean).map((l) => l.trim().toUpperCase())
          : selectedAnswer ? [selectedAnswer.toUpperCase()] : [],
      ),
    [isMulti, selectedAnswer],
  );

  const handleClick = useCallback(
    (label: string) => {
      if (disabled) return;
      if (isMulti) {
        const current = new Set(selectedSet);
        if (current.has(label.toUpperCase())) {
          current.delete(label.toUpperCase());
        } else {
          current.add(label.toUpperCase());
        }
        onSelect([...current].sort().join(","));
      } else {
        onSelect(label);
      }
    },
    [disabled, isMulti, selectedSet, onSelect],
  );

  const showResult = result != null;
  const correctSet = showResult
    ? new Set(result.correct_answer.split(",").map((l) => l.trim().toUpperCase()))
    : new Set<string>();

  return (
    <div className="space-y-1.5">
      {options.map((option) => {
        const label = option.label;
        const labelUp = label.toUpperCase();
        const isSelected = selectedSet.has(labelUp);
        const isCorrectOption = showResult && correctSet.has(labelUp);
        const isWrong = showResult && isSelected && !isCorrectOption;

        // Base class tokens. Border thickness is handled inline so the
        // selected state can go 1.5px without fighting Tailwind's utilities.
        const baseClass =
          "w-full flex items-start gap-2.5 rounded-md px-3 py-2 text-left text-[13px] transition-colors duration-150 ease-out";

        let stateClass: string;
        const style: React.CSSProperties = {};
        if (!showResult) {
          if (isSelected) {
            // Authorized gold use: selected option border (thicker) + soft tint.
            stateClass =
              "bg-[hsl(var(--assistant-accent)/0.08)] text-[hsl(var(--assistant-text-primary))]";
            style.border = "1.5px solid hsl(var(--assistant-accent))";
          } else {
            stateClass =
              "border border-[hsl(var(--assistant-border))] bg-transparent text-[hsl(var(--assistant-text-primary))] hover:bg-[hsl(var(--assistant-surface-soft))]";
          }
        } else if (isCorrectOption) {
          stateClass =
            "border border-[hsl(var(--success))] bg-[hsl(var(--success)/0.07)] text-[hsl(var(--assistant-text-primary))]";
        } else if (isWrong) {
          stateClass =
            "border border-[hsl(var(--destructive))] bg-[hsl(var(--destructive)/0.07)] text-[hsl(var(--assistant-text-primary))]";
        } else {
          stateClass =
            "border border-[hsl(var(--assistant-border))] bg-transparent text-[hsl(var(--assistant-text-primary))] opacity-60";
        }

        return (
          <button
            key={label}
            type="button"
            disabled={disabled}
            onClick={() => handleClick(label)}
            style={style}
            className={cn(
              baseClass,
              stateClass,
              disabled && "cursor-default",
            )}
          >
            {/* Label prefix: mono, tertiary — no filled bubble, just a character */}
            <span
              className={cn(
                "shrink-0 w-5 pt-px text-[11px] font-mono font-medium tabular-nums",
                isSelected && !showResult
                  ? "text-[hsl(var(--assistant-accent))]"
                  : "text-[hsl(var(--assistant-text-tertiary))]",
              )}
            >
              {isMulti
                ? (isSelected ? (
                    <Check className="w-3.5 h-3.5" />
                  ) : (
                    label
                  ))
                : label}
            </span>

            <span className="flex-1 text-[hsl(var(--assistant-text-primary))]">
              {option.text}
            </span>

            {showResult && isCorrectOption && (
              <Check
                className="w-[14px] h-[14px] mt-[2px] shrink-0 text-[hsl(var(--assistant-accent))]"
                aria-label="Correct"
              />
            )}
            {isWrong && (
              <X
                className="w-[14px] h-[14px] mt-[2px] shrink-0 text-[hsl(var(--destructive))]"
                aria-label="Incorrect"
              />
            )}
          </button>
        );
      })}
    </div>
  );
}

function ShortAnswerInput({
  value,
  onChange,
  disabled,
  result,
}: {
  value: string;
  onChange: (val: string) => void;
  disabled: boolean;
  result?: QuizQuestionProps["result"];
}) {
  const showResult = result != null;

  const borderColor = !showResult
    ? "hsl(var(--assistant-border))"
    : result.correct
    ? "hsl(var(--success))"
    : "hsl(var(--destructive))";
  const bgColor = !showResult
    ? "hsl(var(--assistant-surface-bg))"
    : result.correct
    ? "hsl(var(--success) / 0.07)"
    : "hsl(var(--destructive) / 0.07)";

  return (
    <div className="space-y-2">
      <textarea
        value={value}
        onChange={(e) => onChange(e.target.value)}
        disabled={disabled}
        rows={3}
        placeholder="Type your answer..."
        style={{
          borderColor,
          background: bgColor,
        }}
        className={cn(
          "w-full rounded-md border px-3 py-2.5 text-[13px] text-[hsl(var(--assistant-text-primary))] placeholder:text-[hsl(var(--assistant-text-tertiary))] focus:outline-hidden focus:border-[hsl(var(--assistant-accent))] resize-none transition-colors duration-150",
          disabled && "cursor-default opacity-80",
        )}
      />
      {showResult && (
        <div className="flex items-center gap-2 text-[12px]">
          <span
            className={cn(
              "font-medium",
              result.correct
                ? "text-[hsl(var(--success))]"
                : "text-[hsl(var(--destructive))]",
            )}
          >
            {result.correct ? "Correct" : "Incorrect"}
          </span>
          {!result.correct && result.correct_answer && (
            <span className="text-[hsl(var(--assistant-text-secondary))]">
              Expected: {result.correct_answer}
            </span>
          )}
        </div>
      )}
    </div>
  );
}
