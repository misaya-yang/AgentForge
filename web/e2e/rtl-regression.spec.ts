/**
 * Regression tests for post-demo fixes:
 * - Task 3: Arabic RTL rendering (dir="rtl" on assistant messages)
 * - Task 2: Citation language following (visual check)
 *
 * Uses mock API routes to verify UI behavior without a live backend.
 */
import { expect, test, type Page, type Route } from "@playwright/test";
import { installAgentV2StreamRoutes, seedClientPrefs } from "./support/helpers";

const MOCK_SERVICE_ID = "__builtin_assistant__";
const MOCK_SESSION_ID = "rtl-test-session-001";

function jsonResponse(body: unknown) {
  return {
    status: 200,
    contentType: "application/json",
    body: JSON.stringify(body),
  };
}

const ARABIC_RESPONSE = `توضح سياسة النشر أن الرجوع إلى الإصدار السابق يجب أن يتم بعد فشل التحقق الصحي.

## الأدلة من الوثائق

يجب إيقاف الترحيل التلقائي عند ظهور أخطاء في قاعدة البيانات، ثم تشغيل خطة الرجوع الموثقة. [1]

## المصادر

[1] Deployment Runbook - Rollback Procedure

جميع المعلومات المقدمة مستمدة من مصادر إسلامية موثقة.`;

const ENGLISH_RESPONSE = `The rollback procedure should start after the health check fails.

## Evidence from the runbook

The deployment runbook says to stop automated migration, restore the previous image, and verify service health before reopening traffic. [1]

## Sources

[1] Deployment Runbook - Rollback Procedure

All information is based on the provided knowledge base materials.`;

const MIXED_RESPONSE = `# ما هي خطوات الرجوع؟

The rollback checklist should be followed before reopening traffic. يجب التحقق من صحة الخدمة أولاً.

## Sources
[1] Deployment Runbook - Rollback Procedure`;

async function installMockRoutes(page: Page, responseText: string) {
  // Mock models
  await page.route("**/api/v1/assistant/models", async (route) => {
    await route.fulfill(jsonResponse({
      models: [{ id: "gpt-4o", name: "GPT-4o", provider: "openai", context_window: 128000 }],
    }));
  });

  // Mock sessions list
  await page.route("**/api/v1/sessions?*", async (route) => {
    await route.fulfill(jsonResponse([]));
  });

  // Mock create session
  await page.route("**/api/v1/sessions", async (route) => {
    if (route.request().method() === "POST") {
      const now = new Date().toISOString();
      await route.fulfill(jsonResponse({
        session_id: MOCK_SESSION_ID,
        service_id: MOCK_SERVICE_ID,
        user_id: "e2e-user",
        tenant_id: "default",
        created_at: now,
        updated_at: now,
      }));
    } else {
      await route.fallback();
    }
  });

  // Mock stream response. The Assistant streams through the V2 events cursor,
  // so the same payload is served on both the legacy and current routes.
  const fulfillStream = async (route: Route) => {
    const sseChunks = [
      `data: ${JSON.stringify({ event_type: "text_delta", data: responseText, timestamp: Date.now() / 1000 })}\n\n`,
      `data: ${JSON.stringify({ event_type: "done", data: { usage: { prompt_tokens: 100, completion_tokens: 200 } }, timestamp: Date.now() / 1000 })}\n\n`,
    ];
    await route.fulfill({
      status: 200,
      contentType: "text/event-stream",
      body: sseChunks.join(""),
    });
  };
  await page.route("**/api/v1/assistant/chat/stream", fulfillStream);
  await installAgentV2StreamRoutes(page, fulfillStream);

  // Mock services
  await page.route("**/api/v1/services*", async (route) => {
    await route.fulfill(jsonResponse([]));
  });
}

test.describe("RTL Rendering Regression", () => {

  test("Arabic-majority response gets dir=rtl", async ({ page }) => {
    await installMockRoutes(page, ARABIC_RESPONSE);
    await seedClientPrefs(page, { locale: "en-US", themeMode: "light", resolvedTheme: "light" });
    await page.goto("/assistant");

    // Type an Arabic question and send
    const composer = page.locator("textarea, [contenteditable]").first();
    await composer.waitFor({ timeout: 10_000 });
    await composer.fill("ما حكم الربا في الإسلام؟");
    await page.keyboard.press("Enter");

    // Wait for the assistant response to render
    const assistantMessage = page.locator("[dir='rtl']");
    await expect(assistantMessage).toBeVisible({ timeout: 15_000 });

    // Verify the RTL div has the expected class
    const dirAttr = await assistantMessage.getAttribute("dir");
    expect(dirAttr).toBe("rtl");

    // Verify text-right class is applied
    const className = await assistantMessage.getAttribute("class");
    expect(className).toContain("text-right");

    // Screenshot for visual verification
    await page.screenshot({ path: "test-results/rtl-arabic-response.png", fullPage: true });
  });

  test("English response does NOT get dir=rtl", async ({ page }) => {
    await installMockRoutes(page, ENGLISH_RESPONSE);
    await seedClientPrefs(page, { locale: "en-US", themeMode: "light", resolvedTheme: "light" });
    await page.goto("/assistant");

    const composer = page.locator("textarea, [contenteditable]").first();
    await composer.waitFor({ timeout: 10_000 });
    await composer.fill("What is the ruling on interest in Islam?");
    await page.keyboard.press("Enter");

    // Wait for response
    const assistantCopy = page.locator(".assistant-copy").first();
    await expect(assistantCopy).toBeVisible({ timeout: 15_000 });

    // Should NOT have dir="rtl"
    const dirAttr = await assistantCopy.getAttribute("dir");
    expect(dirAttr).toBeNull();

    await page.screenshot({ path: "test-results/ltr-english-response.png", fullPage: true });
  });

  test("mixed content leans toward dominant script direction", async ({ page }) => {
    await installMockRoutes(page, MIXED_RESPONSE);
    await seedClientPrefs(page, { locale: "en-US", themeMode: "light", resolvedTheme: "light" });
    await page.goto("/assistant");

    const composer = page.locator("textarea, [contenteditable]").first();
    await composer.waitFor({ timeout: 10_000 });
    await composer.fill("What is the rollback procedure?");
    await page.keyboard.press("Enter");

    const assistantCopy = page.locator(".assistant-copy").first();
    await expect(assistantCopy).toBeVisible({ timeout: 15_000 });

    await page.screenshot({ path: "test-results/mixed-script-response.png", fullPage: true });
  });
});

test.describe("Citation Language Regression", () => {

  test("Arabic response has Arabic Sources heading", async ({ page }) => {
    await installMockRoutes(page, ARABIC_RESPONSE);
    await seedClientPrefs(page, { locale: "en-US", themeMode: "light", resolvedTheme: "light" });
    await page.goto("/assistant");

    const composer = page.locator("textarea, [contenteditable]").first();
    await composer.waitFor({ timeout: 10_000 });
    await composer.fill("ما حكم الربا؟");
    await page.keyboard.press("Enter");

    // The response should contain Arabic "المصادر" heading
    const sourcesHeading = page.getByText("المصادر");
    await expect(sourcesHeading).toBeVisible({ timeout: 15_000 });

    // Should also contain the Arabic closing phrase
    const closingPhrase = page.getByText("جميع المعلومات المقدمة مستمدة من مصادر إسلامية موثقة");
    await expect(closingPhrase).toBeVisible();
  });

  test("English response has English Sources heading", async ({ page }) => {
    await installMockRoutes(page, ENGLISH_RESPONSE);
    await seedClientPrefs(page, { locale: "en-US", themeMode: "light", resolvedTheme: "light" });
    await page.goto("/assistant");

    const composer = page.locator("textarea, [contenteditable]").first();
    await composer.waitFor({ timeout: 10_000 });
    await composer.fill("What is riba?");
    await page.keyboard.press("Enter");

    const sourcesHeading = page.getByText("Sources");
    await expect(sourcesHeading).toBeVisible({ timeout: 15_000 });

    const closingPhrase = page.getByText("All information is based on the provided knowledge base materials");
    await expect(closingPhrase).toBeVisible();
  });
});
