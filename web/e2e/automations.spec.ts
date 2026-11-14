import { test, expect } from "@playwright/test";
import { installDot1113Fixture } from "./support/dot1113";
import { seedClientPrefs } from "./support/helpers";

test("natural-language creation stays a draft until the reviewed revision is confirmed", async ({ page }) => {
  await installDot1113Fixture(page);
  await seedClientPrefs(page, { locale: "en-US" });
  const rule = { automation_id: "automation-fixture", title: "Weekday review", revision: 1, status: "draft", next_run_at: null,
    definition: { title: "Weekday review", model_id: "fixture-model", trigger: { kind: "cron", expression: "0 9 * * 1-5", timezone: "America/New_York" }, actions: [{ kind: "notify", message: "Review the launch checklist" }] } };
  let generated = false;
  let confirmations = 0;
  await page.route("**/api/v1/automations**", async route => {
    const request = route.request();
    const path = new URL(request.url()).pathname;
    const respond = (value: unknown, status = 200) => route.fulfill({ status, contentType: "application/json", body: JSON.stringify(value) });
    if (path.endsWith("/draft")) { expect(request.postDataJSON()).toEqual({ prompt: "Every weekday at 9am send me a checklist reminder" }); generated = true; return respond({ automation: rule }); }
    if (path.endsWith("/confirm")) {
      confirmations++;
      expect(request.headers()["if-match"]).toBe('"1"');
      expect(request.postDataJSON()).toEqual({ confirm: true });
      rule.status = "active"; rule.revision = 2;
      return respond({ automation: rule });
    }
    return respond(path.endsWith("/notifications") ? { notifications: [] } : { automations: generated ? [rule] : [] });
  });
  await page.goto("/automations");
  await page.getByLabel("What would you like to automate?").fill("Every weekday at 9am send me a checklist reminder");
  await page.getByRole("button", { name: "Create draft", exact: true }).first().click();
  const review = page.getByRole("dialog");
  await expect(review).toBeVisible();
  await expect(review).toHaveAttribute("closedby", "any");
  await expect(review).toContainText("America/New_York");
  await expect(review).toContainText("fixture-model");
  await expect(review).toContainText("Review the launch checklist");
  expect(confirmations).toBe(0);
  await review.getByRole("button", { name: "Confirm and activate", exact: true }).click();
  await expect(review).not.toBeVisible();
  await expect(page.getByText("Active", { exact: true })).toBeVisible();
  expect(confirmations).toBe(1);
});

test("a stale confirmation keeps the draft review open and never reports activation", async ({ page }) => {
  await installDot1113Fixture(page);
  await seedClientPrefs(page, { locale: "en-US" });
  const rule = { automation_id: "stale-fixture", title: "Stale review", revision: 3, status: "draft", next_run_at: null,
    definition: { title: "Stale review", model_id: "fixture-model", trigger: { kind: "webhook" }, actions: [{ kind: "notify", message: "Waiting for a signed webhook" }] } };
  let confirmations = 0;
  await page.route("**/api/v1/automations**", route => {
    const path = new URL(route.request().url()).pathname;
    if (path.endsWith("/confirm")) { confirmations++; return route.fulfill({ status: 409, contentType: "application/json", body: '{"detail":{"code":"AUTOMATION_DRAFT_CHANGED"}}' }); }
    return route.fulfill({ contentType: "application/json", body: JSON.stringify(path.endsWith("/notifications") ? { notifications: [] } : { automations: [rule] }) });
  });
  await page.goto("/automations");
  await page.getByRole("button", { name: "Review draft", exact: true }).click();
  const review = page.getByRole("dialog");
  await review.getByRole("button", { name: "Confirm and activate", exact: true }).click();
  await expect(review.getByRole("alert")).toBeVisible();
  await expect(review).toBeVisible();
  await expect(page.getByText("Active", { exact: true })).toHaveCount(0);
  expect(confirmations).toBe(1);
  await page.keyboard.press("Escape");
  await expect(review).not.toBeVisible();
});
