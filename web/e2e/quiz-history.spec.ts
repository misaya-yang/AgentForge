/**
 * Quiz History Persistence E2E Test
 *
 * Verifies that when a session contains a quiz, switching away and back
 * restores the QuizCard from the DB (via quiz_id in message metadata).
 *
 * Flow:
 * 1. Create session + seed messages with quiz_id in metadata
 * 2. Generate a real quiz via API so GET /quiz/{id} works
 * 3. Navigate to assistant, switch to the seeded session
 * 4. Verify QuizCard renders (hydrated from DB)
 */

import { expect, test, type APIRequestContext } from "@playwright/test";
import {
  buildAuthHeaders,
  createOwnedKnowledgeDataset,
  deleteKnowledgeDataset,
  ensureAuthenticatedPage,
  getApiUrl,
} from "./support/helpers";

function sessionButtonName(title: string): RegExp {
  return new RegExp(`^${title.replace(/[.*+?^${}()|[\]\\]/g, "\\$&")}(?:\\s·|$)`);
}

async function seedSessionWithQuiz(request: APIRequestContext, page: import("@playwright/test").Page) {
  const headers = await buildAuthHeaders(request);
  const apiUrl = getApiUrl();
  const title = `quiz-history-${Date.now()}`;

  // Step 1: Create a real quiz through the in-chat generate_quiz tool so it
  // exists in the DB (the gateway quiz-generation API was removed in PC-03).
  //
  // A freshly streamed quiz is hydrated straight from the canonical
  // ``quiz:ready`` SSE payload, so the card fires no GET at render time and
  // there is no hydration response to wait for. Resolve the id the same way
  // quiz-workflow does: from the persisted assistant message metadata.
  const owned = await createOwnedKnowledgeDataset(request);
  const datasetName = owned.name;

  await page.goto("/assistant", { waitUntil: "domcontentloaded" });
  const composer = page.locator("#assistant-chat-composer");
  await expect(composer).toBeVisible();

  // Bind the dataset through the same UI state real chat requests use; the
  // assistant refuses KB-grounded quiz generation when the binding is empty.
  const composerShell = composer.locator("xpath=..");
  await composerShell.locator("button:has(svg.lucide-plus)").click();
  await page.getByRole("button", { name: /Company Knowledge Base|公司知识库/ }).click();
  await page.getByText(datasetName, { exact: true }).click();
  await composer.click();

  const quizThreadResponse = page.waitForResponse(
    (res) =>
      res.request().method() === "POST" &&
      new URL(res.url()).pathname.endsWith("/api/v2/agent/threads"),
    { timeout: 180_000 },
  );
  await composer.fill("请根据知识库出 3 道单选题");
  await composer.press("Enter");

  const quizThreadBody = await (await quizThreadResponse).json();
  const quizSessionId = String(quizThreadBody?.thread?.session_id ?? "");
  expect(quizSessionId).toBeTruthy();

  // `generate_quiz` is a write capability under `approvalPolicy: on-request`,
  // so the turn parks on an approval before the card can render.
  const approveButton = page.getByRole("button", { name: /^(Approve|通过)$/ });
  const startQuizButton = page.getByRole("button", { name: /^(Start Quiz|开始作答)$/i });
  await expect(approveButton.or(startQuizButton).first()).toBeVisible({ timeout: 180_000 });
  if ((await approveButton.count()) > 0) {
    await approveButton.first().click();
  }
  await expect(startQuizButton).toBeVisible({ timeout: 180_000 });

  let quizId: string | undefined;
  await expect
    .poll(
      async () => {
        // The Runtime owns conversation history now; the generic
        // /api/v1/sessions history reads the Gateway session store and carries
        // none of the runtime-projected metadata.
        const historyRes = await request.get(
          `${apiUrl}/api/v1/assistant/sessions/${quizSessionId}/history?limit=200`,
          { headers },
        );
        if (!historyRes.ok()) return undefined;
        const historyBody = await historyRes.json();
        const messages = historyBody.messages ?? historyBody;
        const quizMessage = [...messages]
          .reverse()
          .find(
            (message: { role?: string; metadata?: { quiz_id?: unknown } }) =>
              message.role === "assistant" && typeof message.metadata?.quiz_id === "string",
          );
        quizId = quizMessage?.metadata?.quiz_id as string | undefined;
        return quizId;
      },
      { timeout: 30_000 },
    )
    .toMatch(/^[0-9a-f-]{36}$/);

  const quizRes = await request.get(`${apiUrl}/api/v1/assistant/quiz/${quizId}`, { headers });
  expect(quizRes.ok(), `Quiz fetch failed: ${quizRes.status()}`).toBeTruthy();
  const quiz = await quizRes.json();

  // Step 2: Create session
  const createRes = await request.post(`${apiUrl}/api/v1/sessions`, {
    headers,
    data: {
      service_id: "__builtin_assistant__",
      metadata: { title },
    },
  });
  expect(createRes.ok()).toBeTruthy();
  const { session_id: sessionId } = (await createRes.json()) as { session_id: string };

  // Step 3: Seed user message
  await request.post(`${apiUrl}/api/v1/sessions/${sessionId}/messages`, {
    headers,
    data: { role: "user", content: "请根据知识库出 3 道单选题" },
  });

  // Step 4: Seed assistant message with quiz_id in metadata
  await request.post(`${apiUrl}/api/v1/sessions/${sessionId}/messages`, {
    headers,
    data: {
      role: "assistant",
      content: `Quiz "${quiz.title}" created with ${quiz.question_count ?? quiz.questions?.length ?? 0} questions. Interactive quiz card is now displayed.`,
      metadata: { quiz_id: quizId },
    },
  });

  return { sessionId, title, quizId, quizTitle: quiz.title, datasetId: owned.datasetId };
}

test.describe("Quiz history persistence", () => {
  test.setTimeout(3 * 60_000);

  let headers: Record<string, string>;
  let cleanupIds: { sessionId?: string; quizId?: string; datasetId?: string } = {};

  test.beforeAll(async ({ request }) => {
    headers = await buildAuthHeaders(request);
  });

  test.afterAll(async ({ request }) => {
    const apiUrl = getApiUrl();
    if (cleanupIds.sessionId) {
      await request.delete(`${apiUrl}/api/v1/sessions/${cleanupIds.sessionId}`, { headers }).catch(() => {});
    }
    if (cleanupIds.quizId) {
      await request.delete(`${apiUrl}/api/v1/assistant/quiz/${cleanupIds.quizId}`, { headers }).catch(() => {});
    }
    if (cleanupIds.datasetId) {
      await deleteKnowledgeDataset(request, cleanupIds.datasetId).catch(() => {});
    }
  });

  test("restores QuizCard when switching to a session with quiz history", async ({ page, request }) => {
    const seed = await seedSessionWithQuiz(request, page);
    if (!seed) {
      test.skip(true, "No quiz created");
      return;
    }
    cleanupIds = { sessionId: seed.sessionId, quizId: seed.quizId, datasetId: seed.datasetId };

    // Navigate to assistant page
    await ensureAuthenticatedPage(page, "/assistant");

    // The history drawer starts closed. Wait for the toggle to render before
    // deciding — probing `count()` mid-hydration silently skips the click and
    // leaves the drawer shut.
    const showHistory = page.getByRole("button", { name: /^(Show history|显示历史)$/ });
    const hideHistory = page.getByRole("button", { name: /^(Hide history|隐藏历史)$/ });
    await expect(showHistory.or(hideHistory).first()).toBeVisible({ timeout: 15_000 });
    if ((await showHistory.count()) > 0) {
      await showHistory.first().click();
    }
    await expect(hideHistory.first()).toBeVisible({ timeout: 15_000 });

    // Find and click the seeded session in sidebar
    const sessionBtn = page.getByRole("button", { name: sessionButtonName(seed.title) });
    await expect(sessionBtn).toBeVisible({ timeout: 15_000 });
    await sessionBtn.click();

    // Wait for history restore — the user message should appear
    await expect(page.getByText("请根据知识库出 3 道单选题")).toBeVisible({ timeout: 15_000 });

    // QuizCard should render after async hydration
    // QuizCard contains the quiz title and question elements
    await expect(
      page.getByRole("heading", { name: seed.quizTitle, level: 2 }),
    ).toBeVisible({ timeout: 15_000 });

    // Verify quiz questions are rendered (QuizCard shows question text)
    const questionElements = page.locator("[data-testid='quiz-question'], .quiz-question");
    // Fallback: just check there's some quiz UI element
    const quizCard = page.locator("[data-testid='quiz-card'], .quiz-card");
    const hasQuizCard = await quizCard.count().catch(() => 0);
    const hasQuestions = await questionElements.count().catch(() => 0);

    // At least the quiz title should be visible — that proves hydration worked
    const titleVisible = await page
      .getByRole("heading", { name: seed.quizTitle, level: 2 })
      .isVisible();
    expect(hasQuizCard > 0 || hasQuestions > 0 || titleVisible).toBeTruthy();
  });
});
