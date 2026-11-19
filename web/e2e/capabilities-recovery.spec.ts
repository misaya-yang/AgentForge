import { expect, test } from "@playwright/test";
import { installDot1113Fixture } from "./support/dot1113";

for (const area of ["skills", "mcp"] as const) {
  test(`${area} read failures remain explicit until an authoritative retry succeeds`, async ({ page }) => {
    page.setDefaultTimeout(15_000);
    await installDot1113Fixture(page);
    await page.route("**/api/v1/sessions**", route => new URL(route.request().url()).pathname === "/api/v1/sessions"
      ? route.fulfill({ json: [] }) : route.fallback());
    let unavailable = true;
    await page.route("**/api/v1/skills**", route => {
      if (new URL(route.request().url()).pathname !== "/api/v1/skills") return route.fallback();
      return route.fulfill({ status: area === "skills" && unavailable ? 503 : 200, json: { skills: [], total: 0 } });
    });
    await page.route("**/api/v1/assistant/mcp/**", route => {
      const path = new URL(route.request().url()).pathname;
      if (path.endsWith("/servers")) return route.fulfill({ status: area === "mcp" && unavailable ? 503 : 200, json: { servers: [] } });
      if (path.endsWith("/tools")) return route.fulfill({ json: { tools: [], total: 0 } });
      return route.fallback();
    });
    await page.goto("/assistant");
    const customize = page.getByRole("button", { name: "Customize", exact: true });
    if (!(await customize.isVisible())) await page.locator('button[aria-label="Show history"]').click();
    await customize.click();
    const dialog = page.getByRole("dialog", { name: "Customize AI Assistant" });
    if (area === "mcp") await dialog.getByRole("tab", { name: "MCP Connections", exact: true }).click();
    const emptyText = area === "skills" ? "No skills available" : "No MCP servers configured";
    await expect(dialog.getByRole("alert")).toContainText("could not be loaded");
    await expect(dialog.getByText(emptyText, { exact: true })).toHaveCount(0);
    unavailable = false;
    await dialog.getByRole("button", { name: "Retry", exact: true }).click();
    await expect(dialog.getByRole("alert")).toHaveCount(0);
    await expect(dialog.getByText(emptyText, { exact: true })).toBeVisible();
  });
}
