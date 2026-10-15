import { expect, test } from "@playwright/test";
import { clearAuthState, readE2ETestUser } from "./support/helpers";

test.use({ storageState: { cookies: [], origins: [] } });

const loginButtonName = /sign in|log in|login|登\s*录/i;

test("login succeeds with generated E2E credentials", async ({ page }) => {
  const testUser = await readE2ETestUser();
  const emailInput = page.locator("#email");
  const passwordInput = page.locator("#password");

  await page.goto("/login");
  await emailInput.fill(testUser.email);
  await passwordInput.fill(testUser.password);
  await page.getByRole("button", { name: loginButtonName }).click();

  await expect(page).toHaveURL(/\/dashboard/);
  await expect(page.locator(".ant-layout-content")).toBeVisible();
});

test("login shows readable error for invalid credentials without crashing", async ({ page }) => {
  const emailInput = page.locator("#email");
  const passwordInput = page.locator("#password");
  await clearAuthState(page);

  await page.goto("/login");
  await emailInput.fill(`missing-${Date.now()}`);
  await passwordInput.fill("definitely-wrong-password");
  await page.getByRole("button", { name: loginButtonName }).click();

  await expect(page.locator("body")).toContainText(/invalid credentials|invalid email or password|用户名或密码错误|邮箱或密码错误/i);
  await expect(page).toHaveURL(/\/login/);
});
