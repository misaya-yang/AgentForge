/**
 * First-run onboarding e2e: setup banner + dashboard checklist.
 *
 * Runs in the open-source suite (mocked APIs, no live backend).
 * The setup banner lives in AppLayout and the checklist on top of
 * /dashboard; both are driven by GET /api/v1/setup/state.
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

test.describe("first-run onboarding", () => {
  test("shows the setup banner and dashboard checklist when nothing is configured", async ({
    page,
  }) => {
    const assertNoRuntimeFailures = watchRuntimeFailures(page);
    await installFirstRunHarness(page, { configured: false });

    await page.goto("/dashboard", { waitUntil: "domcontentloaded" });

    // Banner with a link into the Services page.
    await expect(page.getByText(/No model service is configured yet/)).toBeVisible();
    const configureLink = page.getByRole("link", { name: "Configure model service" });
    await expect(configureLink).toBeVisible();
    await expect(configureLink).toHaveAttribute("href", "/services");

    // Dashboard checklist with the three first-run steps.
    await expect(page.getByText("First-run setup")).toBeVisible();
    await expect(page.getByText("Configure a model service", { exact: true })).toBeVisible();
    await expect(page.getByText("Create a knowledge base", { exact: true })).toBeVisible();
    await expect(page.getByText("Start your first conversation", { exact: true })).toBeVisible();
    await expect(page.getByRole("link", { name: "Go" })).toHaveCount(3);

    assertNoRuntimeFailures();
  });

  test("hides banner and checklist once a provider is configured", async ({ page }) => {
    const assertNoRuntimeFailures = watchRuntimeFailures(page);
    await installFirstRunHarness(page, { configured: true });

    await page.goto("/dashboard", { waitUntil: "domcontentloaded" });

    await expect(page.getByText(/No model service is configured yet/)).toHaveCount(0);
    await expect(page.getByText("First-run setup")).toHaveCount(0);

    assertNoRuntimeFailures();
  });

  test("does not query or show setup details without Services permission", async ({ page }) => {
    const setupRequests: string[] = [];
    page.on("request", (request) => {
      if (new URL(request.url()).pathname === "/api/v1/setup/state") {
        setupRequests.push(request.url());
      }
    });
    await installFirstRunHarness(page, {
      configured: false,
      permissions: ["console:dashboard:view"],
    });

    await page.goto("/dashboard", { waitUntil: "domcontentloaded" });

    await expect(page.getByText(/No model service is configured yet/)).toHaveCount(0);
    await expect(page.getByText("First-run setup")).toHaveCount(0);
    expect(setupRequests).toEqual([]);
  });

  test("dismisses the banner and persists the dismissal across reloads", async ({ page }) => {
    const assertNoRuntimeFailures = watchRuntimeFailures(page);
    await installFirstRunHarness(page, { configured: false });

    await page.goto("/dashboard", { waitUntil: "domcontentloaded" });
    await expect(page.getByText(/No model service is configured yet/)).toBeVisible();

    await page.getByRole("button", { name: "Dismiss" }).click();
    await expect(page.getByText(/No model service is configured yet/)).toHaveCount(0);
    expect(
      await page.evaluate(() => localStorage.getItem("setup-banner-dismissed:first-run-user")),
    ).toBe("1");
    expect(await page.evaluate(() => localStorage.getItem("setup-banner-dismissed"))).toBeNull();

    // The checklist is independent of the banner dismissal.
    await expect(page.getByText("First-run setup")).toBeVisible();

    // Reload — the banner stays dismissed, the checklist stays visible.
    await page.reload({ waitUntil: "domcontentloaded" });
    await expect(page.getByText(/No model service is configured yet/)).toHaveCount(0);
    await expect(page.getByText("First-run setup")).toBeVisible();

    assertNoRuntimeFailures();
  });
});
