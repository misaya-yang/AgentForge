/**
 * E2E tests for model_tester role — playground-only access.
 *
 * Validates that users with only `conversation:playground:access`
 * can log in, see only the Playground menu, and are blocked from
 * all other console pages.
 */
import { test, expect } from "@playwright/test";

const AUTH_EMAIL_DOMAIN = process.env.E2E_AUTH_EMAIL_DOMAIN || "example.com";
const MODEL_TESTER_PASSWORD = process.env.E2E_MODEL_TESTER_PASSWORD || "ModelTester-ChangeMe-2026!";
const TEST_USERS = [
  { email: `model_tester_1@${AUTH_EMAIL_DOMAIN}`, password: MODEL_TESTER_PASSWORD },
  { email: `model_tester_2@${AUTH_EMAIL_DOMAIN}`, password: MODEL_TESTER_PASSWORD },
  { email: `model_tester_3@${AUTH_EMAIL_DOMAIN}`, password: MODEL_TESTER_PASSWORD },
  { email: `model_tester_4@${AUTH_EMAIL_DOMAIN}`, password: MODEL_TESTER_PASSWORD },
  { email: `model_tester_5@${AUTH_EMAIL_DOMAIN}`, password: MODEL_TESTER_PASSWORD },
];

async function login(page, email: string, password: string) {
  await page.goto("/login", { waitUntil: "domcontentloaded" });
  // The login form accepts either the full email or the local part.
  const username = email.replace(`@${AUTH_EMAIL_DOMAIN}`, "");
  await page.fill('input#email', username);
  await page.fill('input#password', password);
  // `networkidle` can settle before the SPA has stored the token, so the next
  // navigation raced back to /login. Wait for the auth response and for the
  // route guard to actually let go of the login page instead.
  const [response] = await Promise.all([
    page.waitForResponse(
      (candidate) =>
        candidate.url().includes("/api/v1/auth/login") &&
        candidate.request().method() === "POST",
      { timeout: 30_000 }
    ),
    page.click('button[type="submit"]'),
  ]);
  if (!response.ok()) {
    throw new Error(`model_tester login failed for ${email}: ${response.status()}`);
  }
  await page.waitForURL((url) => !url.pathname.startsWith("/login"), { timeout: 30_000 });
}

for (const user of TEST_USERS) {
  test.describe(`model_tester user: ${user.email}`, () => {
    test("can log in and access playground", async ({ page }) => {
      await login(page, user.email, user.password);
      await page.goto("/playground");
      await page.waitForLoadState("networkidle");
      // Verify playground content is visible
      const bodyText = await page.locator("body").textContent();
      expect(bodyText).toMatch(
        /Model Debug|模型调试|New chat|Playground|模型体验|聊天|assistant/i
      );
    });

    test("sees only playground in sidebar", async ({ page }) => {
      await login(page, user.email, user.password);
      await page.goto("/playground");
      await page.waitForLoadState("networkidle");

      // Wait for sidebar to render
      await page.waitForSelector("nav, [class*='sidebar'], aside", { timeout: 10000 });

      // The playground surface is named "Model Debug" / "模型调试" in the
      // console navigation; older builds called it Playground / 模型体验.
      const playgroundVisible = await page
        .locator("text=/Model Debug|模型调试|模型体验|Playground|New chat/i")
        .first()
        .isVisible()
        .catch(() => false);
      expect(playgroundVisible).toBe(true);

      // Dashboard should NOT be visible in nav
      const dashboardVisible = await page.locator("nav >> text=/Dashboard|仪表盘/i").isVisible().catch(() => false);
      expect(dashboardVisible).toBe(false);

      // Services should NOT be visible
      const servicesVisible = await page.locator("nav >> text=/Services|服务/i").isVisible().catch(() => false);
      expect(servicesVisible).toBe(false);

      // Users should NOT be visible
      const usersVisible = await page.locator("nav >> text=/Users|用户/i").isVisible().catch(() => false);
      expect(usersVisible).toBe(false);
    });

    test("is redirected away from assistant", async ({ page }) => {
      await login(page, user.email, user.password);
      await page.goto("/assistant");
      await page.waitForLoadState("networkidle");
      await expect(page).toHaveURL(/\/playground/);
    });

    test("is blocked from dashboard (403)", async ({ page }) => {
      await login(page, user.email, user.password);
      // Try navigating to dashboard directly
      await page.goto("/dashboard");
      await page.waitForLoadState("networkidle");
      // Should show 403 or redirect away from dashboard
      const bodyText = await page.locator("body").textContent();
      const url = page.url();
      const is403 = bodyText?.includes("403") || bodyText?.includes("Access Denied") || bodyText?.includes("没有权限");
      const notDashboard = !url.includes("/dashboard");
      expect(is403 || notDashboard).toBe(true);
    });
  });
}
