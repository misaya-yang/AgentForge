/**
 * Quiz API client — fetch, submit, and share quizzes.
 * Generation happens through the in-chat generate_quiz assistant tool.
 */

import { api, getAuthToken } from "@/lib/api";
import type { QuizData, QuizAttemptResult } from "@/pages/assistant/types";

export async function getQuiz(quizId: string): Promise<QuizData> {
  const { data } = await api.get<QuizData>(`/api/v1/assistant/quiz/${quizId}`);
  return data;
}

export async function submitQuiz(
  quizId: string,
  answers: Record<string, string>,
  attemptId: string,
): Promise<QuizAttemptResult> {
  const { data } = await api.post<QuizAttemptResult>(
    `/api/v1/assistant/quiz/${quizId}/submit`,
    { answers, attempt_id: attemptId },
  );
  return data;
}

export async function getQuizAttemptResult(quizId: string, attemptId: string): Promise<QuizAttemptResult> {
  const { data } = await api.get<QuizAttemptResult>(
    `/api/v1/assistant/quiz/${quizId}/attempts/${attemptId}`,
  );
  return data;
}

/**
 * Anonymous quiz submit for the public /share/:shareCode page.
 * No auth required — rate-limited server-side by IP + ag_anon_id cookie.
 * Repeat submissions for the same (shareCode, anon, quizId) return the
 * cached first attempt with `cached: true` appended.
 */
export async function submitSharedQuiz(
  shareCode: string,
  quizId: string,
  answers: Record<string, string>,
): Promise<QuizAttemptResult & { cached?: boolean }> {
  const token = getAuthToken();
  const resp = await fetch(
    `/api/v1/assistant/shares/${encodeURIComponent(shareCode)}/quiz/${encodeURIComponent(quizId)}/submit`,
    {
      method: "POST",
      headers: {
        "Content-Type": "application/json",
        ...(token ? { Authorization: `Bearer ${token}` } : {}),
      },
      credentials: "include", // send ag_anon_id cookie
      body: JSON.stringify({ answers }),
    },
  );
  if (!resp.ok) {
    throw new Error(`Share quiz submit failed: ${resp.status}`);
  }
  return resp.json();
}


// --- Share ---

export interface ShareQuizRequest {
  expires_hours?: number;
  max_attempts?: number;
  require_name?: boolean;
  audience?: "public" | "internal";
}

export interface ShareQuizResponse {
  share_id: string;
  share_code: string;
  kind: string;
  quiz_id: string;
  quiz_title: string;
  expires_at: string | null;
  require_name: boolean;
  max_attempts: number | null;
  time_limit_minutes: number | null;
  audience: "public" | "internal";
}

export async function createQuizShare(
  quizId: string,
  data?: ShareQuizRequest,
): Promise<ShareQuizResponse> {
  const { data: result } = await api.post<ShareQuizResponse>(
    "/api/v1/artifact-shares",
    { kind: "quiz", quiz_id: quizId, ...(data ?? {}) },
  );
  return result;
}

export interface QuizShareSummary {
  share_id: string;
  share_code: string;
  is_active: boolean;
  expired: boolean;
  created_at: string;
  expires_at: string | null;
  revoked_at: string | null;
  require_name: boolean;
  audience: "public" | "internal";
}

export async function listQuizShares(quizId: string): Promise<QuizShareSummary[]> {
  const { data } = await api.get<QuizShareSummary[]>("/api/v1/artifact-shares", { params: { quiz_id: quizId, limit: 200 } });
  return data;
}

export async function revokeQuizShare(shareId: string): Promise<void> {
  await api.delete(`/api/v1/artifact-shares/${encodeURIComponent(shareId)}`);
}

export interface QuizAttemptSummary {
  attempt_id: string;
  user_id: string | null;
  display_name: string | null;
  total_score: number | null;
  correct_count: number | null;
  total_count: number | null;
  started_at: string | null;
  completed_at: string | null;
  status: string;
}

export async function listAttempts(
  quizId: string,
): Promise<{ attempts: QuizAttemptSummary[] }> {
  const { data } = await api.get<{ attempts: QuizAttemptSummary[] }>(
    `/api/v1/assistant/quiz/${quizId}/attempts`,
  );
  return data;
}
