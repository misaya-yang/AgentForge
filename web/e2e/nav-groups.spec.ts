/**
 * Sidebar nav regrouping e2e: Use / Build / Govern groups.
 *
 * Dashboard stays ungrouped on top, then 使用 (assistant + agents),
 * 构建 (knowledge + playground + eval), 治理 (services + users +
 * tasks + settings). The model_tester role renders a flat rail with only
 * the playground item and no headings.
 */
import { expect, test, type Page } from "@playwright/test";
import { installFirstRunHarness } from "./support/first-run";

function watchRuntimeFailures(page: Page) {
  const consoleErrors: string[] = [];
  const pageErrors: string[] = [];
  const badResponses: string[] = [];

  page.on("console", (message) => {
    if (message.type() === "error" && !/favicon|NO_COLOR/i.test(message.text())) {
      consoleErrors.push(message.text());
    }
  });
  page.on("pageerror", (error) => {
    pageErrors.push(String(error));
  });
  page.on("response", (response) => {
    const url = response.url();
    if (response.status() >= 400 && /\/api\/v1\//.test(url)) {
      badResponses.push(`${response.status()} ${url}`);
    }
  });

  return () => {
    expect(pageErrors, `Page runtime errors:\n${pageErrors.join("\n")}`).toEqual([]);
    expect(consoleErrors, `Console errors:\n${consoleErrors.join("\n")}`).toEqual([]);
    expect([...new Set(badResponses)], `API responses >= 400:\n${badResponses.join("\n")}`).toEqual([]);
  };
}

test.describe("sidebar nav groups", () => {
  test("regroups the sidebar into Use / Build / Govern with dashboard first", async ({
    page,
  }) => {
    const assertNoRuntimeFailures = watchRuntimeFailures(page);
    await installFirstRunHarness(page, { configured: true });

    await page.goto("/dashboard", { waitUntil: "domcontentloaded" });

    // Group headings in order.
    await expect(page.locator(".app-nav-group-label")).toHaveText(["Use", "Build", "Govern"]);

    // Full item order: dashboard ungrouped first, then the grouped sections,
    // with no trailing items.
    await expect(page.locator("nav .app-nav-label")).toHaveText([
      "Dashboard",
      "AI Assistant",
      "Agents",
      "Knowledge Base",
      "Model Debug",
      "Eval Console",
      "Services",
      "Users",
      "Tasks",
      "Settings",
    ]);

    assertNoRuntimeFailures();
  });

  test("renders a flat rail without group headings for the model_tester role", async ({
    page,
  }) => {
    const assertNoRuntimeFailures = watchRuntimeFailures(page);
    await installFirstRunHarness(page, {
      configured: true,
      roles: ["model_tester"],
      permissions: ["conversation:playground:access"],
    });

    await page.goto("/playground", { waitUntil: "domcontentloaded" });

    await expect(page.locator(".app-nav-group-label")).toHaveCount(0);
    await expect(page.locator("nav .app-nav-label")).toHaveText(["Model Debug"]);

    assertNoRuntimeFailures();
  });
});
