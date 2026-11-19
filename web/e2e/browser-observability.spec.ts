import { expect, test } from "@playwright/test";
import { installClientAuth, seedClientPrefs } from "./support/helpers";

test("U-WEB-06 sampled browser errors and API traces use the owned same-origin collector without PII", async ({ page }, testInfo) => {
  await seedClientPrefs(page, { locale: "en-US" });
  await installClientAuth(page);
  await page.addInitScript(() => {
    window.__AI_GATEWAY_RUNTIME_CONFIG__ = { telemetryEndpoint: "/api/v1/telemetry/browser" };
  });
  await page.route("**/runtime-config.json", route => route.fulfill({ contentType: "application/json", body: JSON.stringify({ telemetryEndpoint: "/api/v1/telemetry/browser" }) }));
  const fixtureOrigin = new URL(testInfo.project.use.baseURL!).origin;
  await page.route("**/*", async route => {
    const url = new URL(route.request().url());
    if (["http:", "https:"].includes(url.protocol) && url.origin !== fixtureOrigin) return route.abort("blockedbyclient");
    await route.fallback();
  });
  const consoleErrors: string[] = [];
  const pageErrors: string[] = [];
  const externalRequests: string[] = [];
  page.on("console", message => { if (message.type() === "error") consoleErrors.push(message.text()); });
  page.on("pageerror", error => pageErrors.push(error.name));
  page.on("request", request => {
    const url = new URL(request.url());
    if (["http:", "https:"].includes(url.protocol) && url.origin !== fixtureOrigin) externalRequests.push(url.origin);
  });
  // Serve the fixture through real Vite HTTP, preserving browser origin classification.
  const batches: Array<Record<string, unknown>> = [];
  let trace = "";
  await page.route("**/api/v1/telemetry/browser/config", async route => {
    expect(route.request().headers().authorization).toBeUndefined();
    await route.fulfill({ contentType: "application/json", body: JSON.stringify({ enabled: true, sampled: true, sessionId: "0123456789abcdef0123456789abcdef", sampleRate: 0.01, ttlSeconds: 900, sourceMapsEnabled: false }) });
  });
  await page.route("**/api/v1/telemetry/browser", async route => {
    expect(route.request().method()).toBe("POST"); expect(route.request().headers()["x-csrf-token"]).toBe("e2e-client-csrf"); expect(route.request().headers().authorization).toBeUndefined();
    batches.push(route.request().postDataJSON()); await route.fulfill({ status: 202, body: "" });
  });
  await page.route("**/api/v1/users**", async route => {
    trace = route.request().headers().traceparent ?? ""; await route.fulfill({ contentType: "application/json", body: "{}" });
  });
  await page.goto("/e2e/fixtures/browser-observability.html?token=PRIVATE_QUERY");
  await expect(page).toHaveTitle("Browser observability fixture");
  await expect(page.getByRole("heading", { name: "Browser observability fixture" })).toBeVisible();
  expect(new URL(page.url()).pathname).toBe("/e2e/fixtures/browser-observability.html");
  await expect.poll(async () => { await page.getByRole("button", { name: "Send API probe" }).click(); return trace; }).toMatch(/^00-0123456789abcdef0123456789abcdef-[a-f0-9]{16}-01$/);
  await expect(page.getByRole("status")).toHaveText("API probe complete");
  await page.getByRole("button", { name: "Export diagnostic" }).click();
  await expect(page.getByRole("status")).toHaveText("Diagnostic exported");
  await expect.poll(() => batches.length).toBeGreaterThan(0);
  const wire = JSON.stringify(batches);
  expect(wire).not.toMatch(/PRIVATE_|private@example|prompt|email|tenant_id|user_id/);
  expect(batches.flatMap(batch => batch.records as Array<{ name: string }>).some(record => record.name === "TypeError")).toBeTruthy();
  const records = batches.flatMap(batch => batch.records as Array<{ name: string; frames?: unknown[]; span_id?: string; trace_id?: string; http_status?: number }>);
  expect(records.find(record => record.name === "TypeError")?.frames).toEqual([{ asset: "/assets/observability-fixture.js", line: 7, column: 11 }]);
  expect(records.some(record => record.name === "api_headers" && record.span_id === trace.split("-")[2] && record.trace_id === "0123456789abcdef0123456789abcdef" && record.http_status === 200)).toBeTruthy();
  expect(externalRequests).toEqual([]);
  expect(consoleErrors).toEqual([]);
  expect(pageErrors).toEqual([]);
  await expect(page.locator("vite-error-overlay")).toHaveCount(0);
  await page.screenshot({ path: testInfo.outputPath("observability-interaction.png"), fullPage: true });
});
