import { test, expect } from "@playwright/test";
import AxeBuilder from "@axe-core/playwright";
import { consoleRouteInventory } from "../src/config/routeInventory";
import { installClientAuth } from "./support/helpers";

// Run against the configured owned deployment/fixture server. No account,
// service, dataset or credential mutation is permitted by this read-only gate.
for (const [route, fixture] of consoleRouteInventory) {
  test(`WCAG 2.2 AA route ${route}`, async ({ page }) => {
    if (route === "/login") await page.route("**/api/auth/session", handler => handler.fulfill({ status: 401, contentType: "application/json", body: JSON.stringify({ detail: "anonymous_fixture" }) }));
    else await installClientAuth(page, { roles: ["admin"], permissions: ["admin:*"], effective_permissions: ["admin:*"] });
    await page.goto(fixture);
    if (route !== "/") await expect.poll(() => new URL(page.url()).pathname).toBe(fixture);
    await expect(page.locator("#root")).not.toBeEmpty();
    await expect(page.locator("h1, h2, [role=alert], main, form").first()).toBeVisible();
    await expect(page.locator("body")).not.toContainText("Something went wrong");
    await expect(page.locator("body")).not.toContainText("Loading...");
    if (route === "/login") await page.locator("details summary").first().click();
    const result = await new AxeBuilder({ page }).withTags(["wcag2a", "wcag2aa", "wcag21aa", "wcag22aa"]).analyze();
    // All AA violations fail, including moderate label/structure issues.
    expect(result.violations, JSON.stringify(result.violations)).toEqual([]);
  });
}
