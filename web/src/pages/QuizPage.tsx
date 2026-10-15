/**
 * Quiz Page — Standalone quiz experience for shared links.
 *
 * Accessible at /quiz/:shareCode (internal links require a current account).
 * Fetches quiz from public API, allows name input, full quiz + score.
 */

import { useCallback, useEffect, useRef, useState } from "react";
import { useParams } from "react-router-dom";
import { useTranslation } from "react-i18next";
import { motion, AnimatePresence, useReducedMotion } from "framer-motion";
import { BookOpen, ChevronLeft, ChevronRight, Loader2, Send, User } from "lucide-react";
import { cn } from "@/lib/utils";
import type { QuizQuestionData, QuizAttemptResult } from "@/pages/assistant/types";
import { QuizQuestion } from "@/pages/assistant/components/Quiz/QuizQuestion";
import { QuizResult } from "@/pages/assistant/components/Quiz/QuizResult";
import { getAuthToken } from "@/lib/api";
import { useAuthStore } from "@/store/useAuthStore";

function quizShareHeaders(json = false): HeadersInit {
  const token = getAuthToken();
  return {
    ...(json ? { "Content-Type": "application/json" } : {}),
    ...(token ? { Authorization: `Bearer ${token}` } : {}),
  };
}

interface PublicQuizData {
  quiz_id: string;
  share_code: string;
  title: string;
  description?: string;
  question_count: number;
  difficulty?: string;
  require_name: boolean;
  time_limit_minutes?: number | null;
  questions: QuizQuestionData[];
  audience: "public" | "internal";
}

type PageState = "loading" | "intro" | "quiz" | "result" | "error";

export function QuizPage() {
  const userId = useAuthStore((state) => state.user?.user_id);
  return <QuizPageSession key={userId || "anonymous"} userId={userId} />;
}

function QuizPageSession({ userId }: { userId?: string }) {
  const { t } = useTranslation();
  const { shareCode } = useParams<{ shareCode: string }>();
  const sessionKey = `quiz_session_${shareCode}_${userId || "anonymous"}`;
  const [quiz, setQuiz] = useState<PublicQuizData | null>(null);
  const [pageState, setPageState] = useState<PageState>("loading");
  const [error, setError] = useState<string>("");
  const [displayName, setDisplayName] = useState("");
  const [currentIndex, setCurrentIndex] = useState(0);
  const [selectedAnswers, setSelectedAnswers] = useState<Record<string, string>>({});
  const [submitting, setSubmitting] = useState(false);
  const [submitError, setSubmitError] = useState("");
  const [result, setResult] = useState<QuizAttemptResult | null>(null);
  const [attemptToken, setAttemptToken] = useState("");
  const [attemptExpiresAt, setAttemptExpiresAt] = useState("");
  const submitInFlightRef = useRef(false);
  const shouldReduceMotion = useReducedMotion();

  // Fetch quiz on mount
  useEffect(() => {
    if (!shareCode) {
      setError(t("assistant.quiz.publicInvalidLink"));
      setPageState("error");
      return;
    }

    const controller = new AbortController();
    let savedToken = "";
    try {
      const saved = sessionStorage.getItem(sessionKey);
      if (saved) {
        const parsed = JSON.parse(saved) as { token?: string; expiresAt?: string; answers?: Record<string, string>; displayName?: string };
        savedToken = parsed.token || "";
        setAttemptToken(savedToken);
        setAttemptExpiresAt(parsed.expiresAt || "");
        setSelectedAnswers(parsed.answers || {});
        setDisplayName(parsed.displayName || "");
      }
    } catch {
      // Ignore unavailable or malformed browser storage and load the quiz normally.
    }

    async function loadQuiz() {
      try {
        let acceptedResult: QuizAttemptResult | null = null;
        if (savedToken) {
          const prior = await fetch(`/api/v1/quiz/public/${shareCode}/attempts/result`, {
            method: "POST",
            headers: quizShareHeaders(true),
            body: JSON.stringify({ attempt_token: savedToken }),
            signal: controller.signal,
          });
          if (prior.ok) {
            acceptedResult = await prior.json() as QuizAttemptResult;
            setResult(acceptedResult);
          }
        }
        const resp = await fetch(`/api/v1/quiz/shared/${shareCode}`, {
          signal: controller.signal,
          headers: quizShareHeaders(),
        });
        if (!resp.ok) {
          if (acceptedResult && resp.status >= 500) {
            setError(t("assistant.quiz.publicMetadataUnavailable"));
            setPageState("result");
            return;
          }
          throw new Error(
            resp.status === 401
              ? t("assistant.shareSignIn", "Sign in to open this internal link")
              : resp.status === 403
                ? t("assistant.shareSourceUnavailable", "You no longer have access to this link's sources")
              : resp.status === 404
              ? t("assistant.quiz.publicNotFound")
              : t("assistant.quiz.publicLoadFailed"),
          );
        }
        const data = (await resp.json()) as PublicQuizData;
        setQuiz(data);
        setError("");
        setPageState(acceptedResult ? "result" : savedToken ? "quiz" : "intro");
      } catch (loadError) {
        if (loadError instanceof DOMException && loadError.name === "AbortError") return;
        setError(loadError instanceof Error ? loadError.message : t("assistant.quiz.publicLoadFailed"));
        setPageState("error");
      }
    }

    void loadQuiz();
    return () => controller.abort();
  }, [shareCode, sessionKey, t]);

  const handleStart = useCallback(async () => {
    if (quiz?.require_name && !displayName.trim()) return;
    if (attemptToken && (!attemptExpiresAt || Date.parse(attemptExpiresAt) > Date.now())) {
      setPageState("quiz");
      return;
    }
    try {
      const response = await fetch(`/api/v1/quiz/public/${shareCode}/attempts/start`, { method: "POST", headers: quizShareHeaders() });
      if (!response.ok) throw new Error(t("assistant.quiz.publicStartFailed"));
      const started = await response.json() as { attempt_token: string; expires_at: string };
      setAttemptToken(started.attempt_token);
      setAttemptExpiresAt(started.expires_at);
      try {
        sessionStorage.setItem(sessionKey, JSON.stringify({
          token: started.attempt_token, expiresAt: started.expires_at,
          answers: selectedAnswers, displayName,
        }));
      } catch {
        // The active tab still holds this token when session storage is disabled.
      }
      setPageState("quiz");
    } catch (startError) {
      setError(startError instanceof Error ? startError.message : t("assistant.quiz.publicStartFailed"));
    }
  }, [quiz, displayName, attemptToken, attemptExpiresAt, shareCode, sessionKey, selectedAnswers, t]);

  const handleSelect = useCallback(
    (label: string) => {
      if (!quiz) return;
      const q = quiz.questions[currentIndex];
      setSelectedAnswers((prev) => ({ ...prev, [q.id]: label }));
    },
    [quiz, currentIndex],
  );

  useEffect(() => {
    if (!shareCode || !attemptToken) return;
    try {
      sessionStorage.setItem(sessionKey, JSON.stringify({
        token: attemptToken, expiresAt: attemptExpiresAt,
        answers: selectedAnswers, displayName,
      }));
    } catch {
      // A browser with storage disabled can finish the current tab's attempt.
    }
  }, [shareCode, sessionKey, attemptToken, attemptExpiresAt, selectedAnswers, displayName]);

  const handleSubmit = useCallback(async () => {
    if (!quiz || submitting || submitInFlightRef.current || !attemptToken) return;
    submitInFlightRef.current = true;
    setSubmitting(true);
    setSubmitError("");
    try {
      const resp = await fetch(`/api/v1/quiz/shared/${shareCode}/submit`, {
        method: "POST",
        headers: quizShareHeaders(true),
        body: JSON.stringify({
          answers: selectedAnswers,
          display_name: displayName.trim() || null,
          attempt_token: attemptToken,
        }),
      });
      if (!resp.ok) {
        throw new Error(
          resp.status === 429
            ? t("assistant.quiz.publicLimitReached")
            : t("assistant.quiz.publicSubmitFailed"),
        );
      }
      const data = await resp.json();
      setResult(data);
      setPageState("result");
    } catch (submitFailure) {
      setSubmitError(
        submitFailure instanceof Error
          ? submitFailure.message
          : t("assistant.quiz.publicSubmitFailed"),
      );
    } finally {
      submitInFlightRef.current = false;
      setSubmitting(false);
    }
  }, [quiz, shareCode, selectedAnswers, displayName, submitting, attemptToken, t]);

  const allAnswered = quiz
    ? quiz.questions.every((q) => {
        const a = selectedAnswers[q.id];
        return a != null && a.trim() !== "";
      })
    : false;

  // --- Loading ---
  if (pageState === "loading") {
    return (
      <div className="flex min-h-dvh items-center justify-center bg-background">
        <div className="flex items-center gap-3 text-sm text-muted-foreground" role="status">
          <Loader2 className="h-5 w-5 animate-spin text-primary" aria-hidden="true" />
          <span>{t("assistant.quiz.publicLoading")}</span>
        </div>
      </div>
    );
  }

  // --- Error ---
  if (pageState === "error") {
    return (
      <div className="flex min-h-dvh items-center justify-center bg-background">
        <div className="max-w-md space-y-4 px-6 text-center" role="alert">
          <BookOpen className="w-12 h-12 mx-auto text-muted-foreground/40" />
          <h1 className="text-xl font-semibold text-foreground">{error}</h1>
          <p className="text-sm text-muted-foreground">
            {t("assistant.quiz.publicExpiredHint")}
          </p>
        </div>
      </div>
    );
  }

  if (!quiz && !(pageState === "result" && result)) return null;
  const currentQuestion = quiz?.questions[currentIndex];
  const quizTitle = quiz?.title ?? t("assistant.quiz.publicResultTitle");
  const questionCount = quiz?.question_count ?? result?.total_count ?? 0;

  return (
    <div className="min-h-dvh bg-background">
      {/* Header */}
      <header className="sticky top-0 z-10 bg-background/80 backdrop-blur-xs border-b border-border">
        <div className="max-w-2xl mx-auto px-4 py-3 flex items-center gap-3">
          <div className="w-8 h-8 rounded-lg bg-primary/10 flex items-center justify-center">
            <BookOpen className="w-4 h-4 text-primary" />
          </div>
          <div className="min-w-0">
            <h1 className="truncate text-sm font-semibold text-foreground">{quizTitle}</h1>
            <p className="text-xs text-muted-foreground">
              {questionCount} {t("assistant.quiz.questions")}
              {quiz?.difficulty && ` · ${quiz.difficulty}`}
              {quiz?.audience === "internal" && ` · ${t("assistant.shareInternalShort", "Internal")}`}
            </p>
          </div>
        </div>
      </header>

      <div className="max-w-2xl mx-auto px-4 py-8">
        <AnimatePresence mode="wait">
          {/* --- Intro screen --- */}
          {pageState === "intro" && quiz && (
            <motion.div
              key="intro"
              initial={shouldReduceMotion ? false : { opacity: 0, y: 12 }}
              animate={{ opacity: 1, y: 0 }}
              exit={shouldReduceMotion ? { opacity: 0 } : { opacity: 0, y: -12 }}
              transition={{ duration: shouldReduceMotion ? 0 : 0.2, ease: "easeOut" }}
              className="max-w-md mx-auto text-center space-y-6"
            >
              <div className="w-16 h-16 mx-auto rounded-2xl bg-primary/10 flex items-center justify-center">
                <BookOpen className="w-8 h-8 text-primary" />
              </div>
              <div>
                <h2 className="text-xl font-bold text-foreground">{quiz.title}</h2>
                {quiz.description && (
                  <p className="mt-2 text-sm text-muted-foreground">{quiz.description}</p>
                )}
                <p className="mt-3 text-xs text-muted-foreground">
                  {quiz.question_count} {t("assistant.quiz.questions")} · ~{Math.ceil(quiz.question_count * 0.5)} {t("assistant.quiz.publicMinutes")}
                </p>
              </div>

              {quiz.require_name && (
                <div className="relative max-w-xs mx-auto">
                  <label htmlFor="quiz-display-name" className="sr-only">
                    {t("assistant.quiz.publicYourName")}
                  </label>
                  <User className="absolute left-3 top-1/2 -translate-y-1/2 w-4 h-4 text-muted-foreground" />
                  <input
                    id="quiz-display-name"
                    type="text"
                    placeholder={t("assistant.quiz.publicYourName")}
                    value={displayName}
                    onChange={(e) => setDisplayName(e.target.value)}
                    onKeyDown={(e) => e.key === "Enter" && handleStart()}
                    className="w-full rounded-xl border border-border bg-card pl-10 pr-4 py-2.5 text-sm focus:outline-hidden focus:ring-2 focus:ring-primary/30"
                  />
                </div>
              )}

              <button
                type="button"
                onClick={handleStart}
                disabled={quiz.require_name && !displayName.trim()}
                className={cn(
                  "inline-flex items-center gap-2 rounded-xl px-8 py-3 text-sm font-medium transition-colors",
                  "bg-primary text-primary-foreground hover:bg-primary/90",
                  "disabled:cursor-not-allowed disabled:opacity-50",
                )}
              >
                {t("assistant.quiz.startQuiz")}
                <ChevronRight className="w-4 h-4" />
              </button>
              {error && <p role="alert" className="text-sm text-red-600">{error}</p>}
            </motion.div>
          )}

          {/* --- Quiz mode --- */}
          {pageState === "quiz" && quiz && currentQuestion && (
            <motion.div
              key={`q-${currentIndex}`}
              initial={shouldReduceMotion ? false : { opacity: 0, x: 16 }}
              animate={{ opacity: 1, x: 0 }}
              exit={shouldReduceMotion ? { opacity: 0 } : { opacity: 0, x: -16 }}
              transition={{ duration: shouldReduceMotion ? 0 : 0.18, ease: "easeOut" }}
              className="max-w-lg mx-auto"
            >
              {/* Progress */}
              <div className="flex items-center gap-2 mb-6">
                <span className="text-sm font-medium text-muted-foreground">
                  {currentIndex + 1} / {quiz.question_count}
                </span>
                <div
                  className="h-2 flex-1 overflow-hidden rounded-full bg-muted"
                  role="progressbar"
                  aria-label={t("assistant.quiz.publicProgress")}
                  aria-valuemin={1}
                  aria-valuemax={quiz.question_count}
                  aria-valuenow={currentIndex + 1}
                >
                  <div
                    className={cn(
                      "h-full rounded-full bg-primary",
                      shouldReduceMotion ? "transition-none" : "transition-[width] duration-300",
                    )}
                    style={{
                      width: `${((currentIndex + 1) / quiz.question_count) * 100}%`,
                    }}
                  />
                </div>
              </div>

              <QuizQuestion
                question={currentQuestion}
                selectedAnswer={selectedAnswers[currentQuestion.id]}
                onSelect={handleSelect}
              />

              {submitError && (
                <p className="mt-5 rounded-lg border border-destructive/30 bg-destructive/5 px-3 py-2 text-sm text-destructive" role="alert">
                  {submitError}
                </p>
              )}

              {/* Navigation */}
              <div className="flex items-center justify-between mt-8">
                <button
                  type="button"
                  disabled={currentIndex === 0}
                  onClick={() => setCurrentIndex((i) => i - 1)}
                  className="inline-flex items-center gap-1 text-sm text-muted-foreground hover:text-foreground disabled:opacity-40 disabled:cursor-not-allowed transition-colors"
                >
                  <ChevronLeft className="w-4 h-4" />
                  {t("assistant.quiz.prev")}
                </button>

                {currentIndex < quiz.question_count - 1 ? (
                  <button
                    type="button"
                    disabled={!selectedAnswers[currentQuestion.id]}
                    onClick={() => setCurrentIndex((i) => i + 1)}
                    className="inline-flex items-center gap-1 text-sm text-primary hover:text-primary/80 disabled:opacity-40 disabled:cursor-not-allowed transition-colors"
                  >
                    {t("assistant.quiz.next")}
                    <ChevronRight className="w-4 h-4" />
                  </button>
                ) : (
                  <button
                    type="button"
                    disabled={!allAnswered || submitting}
                    onClick={handleSubmit}
                    className={cn(
                      "inline-flex items-center gap-1.5 rounded-xl px-5 py-2 text-sm font-medium transition-colors",
                      "bg-primary text-primary-foreground",
                      "disabled:opacity-50 disabled:cursor-not-allowed",
                    )}
                  >
                    {submitting ? (
                      <Loader2 className="w-4 h-4 animate-spin" />
                    ) : (
                      <Send className="w-4 h-4" />
                    )}
                    {t("assistant.quiz.submit")}
                  </button>
                )}
              </div>
            </motion.div>
          )}

          {/* --- Result --- */}
          {pageState === "result" && result && (
            <motion.div
              key="result"
              initial={shouldReduceMotion ? false : { opacity: 0, scale: 0.98 }}
              animate={{ opacity: 1, scale: 1 }}
              transition={{ duration: shouldReduceMotion ? 0 : 0.18, ease: "easeOut" }}
              className="max-w-lg mx-auto"
            >
              {error && (
                <p className="mb-5 rounded-lg border border-amber-300/60 bg-amber-50 px-3 py-2 text-sm text-amber-900 dark:border-amber-400/30 dark:bg-amber-500/10 dark:text-amber-100" role="status">
                  {error}
                </p>
              )}
              <QuizResult result={result} />

              <div className="mt-6 text-center">
                <p className="text-xs text-muted-foreground">
                  Powered by AI Platform
                </p>
              </div>
            </motion.div>
          )}
        </AnimatePresence>
      </div>
    </div>
  );
}
