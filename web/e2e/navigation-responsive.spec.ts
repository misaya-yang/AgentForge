import { expect, test } from "@playwright/test";
import { installDot1113Fixture } from "./support/dot1113";
import { modKey, seedClientPrefs } from "./support/helpers";

test("knowledge root finishes loading without waiting for detail translations", async ({ page }) => {
  await installDot1113Fixture(page);
  await seedClientPrefs(page, { locale: "en-US" });
  const errors: string[] = [];
  page.on("pageerror", error => errors.push(error.message));
  const response = page.waitForResponse(value => new URL(value.url()).pathname === "/api/v1/knowledge/datasets" && value.request().method() === "GET");
  await page.goto("/knowledge");
  expect((await response).ok()).toBe(true);
  await expect(page.getByRole("heading", { name: "No Knowledge Bases Yet" })).toBeVisible();
  await expect(page.locator("#console-main")).not.toContainText("Loading...");
  await expect(page.locator("vite-error-overlay")).toHaveCount(0);
  expect(errors).toEqual([]);
});

test("phone navigation is closed on every feature route and closes after selection", async ({ page }) => {
  await installDot1113Fixture(page);
  await seedClientPrefs(page, { locale: "en-US" });
  await page.setViewportSize({ width: 390, height: 844 });
  await page.emulateMedia({ reducedMotion: "reduce" });
  const routes = ["/workbench", "/knowledge", "/settings", "/spaces"];
  for (const [index, route] of routes.entries()) {
    await page.goto(route);
    await expect(page.locator("#console-main").getByRole("heading").first()).toBeVisible();
    const sidebar = page.locator("#console-sidebar");
    await expect(sidebar).toHaveAttribute("aria-hidden", "true");
    await expect(sidebar).toHaveAttribute("inert", "");
    await expect.poll(async () => {
      const box = await sidebar.boundingBox();
      return box ? box.x + box.width : 0;
    }).toBeLessThanOrEqual(1);
    const toggle = page.getByRole("button", { name: "Toggle sidebar", exact: true });
    await toggle.click();
    await expect(toggle).toHaveAttribute("aria-expanded", "true");
    const navigation = page.getByRole("navigation", { name: "Primary navigation", exact: true });
    await expect(navigation).toBeInViewport();
    const destination = routes[(index + 1) % routes.length];
    await navigation.locator(`a[href="${destination}"]`).click();
    await expect(page).toHaveURL(new RegExp(`${destination}$`));
    await expect(sidebar).toHaveAttribute("aria-hidden", "true");
    await expect(page.getByRole("button", { name: "Toggle sidebar", exact: true })).toHaveAttribute("aria-expanded", "false");
  }
});

test("desktop sidebar collapse survives navigation between feature pages", async ({ page }) => {
  await installDot1113Fixture(page);
  await seedClientPrefs(page, { locale: "en-US" });
  await page.setViewportSize({ width: 1440, height: 960 });
  await page.emulateMedia({ reducedMotion: "reduce" });
  await page.goto("/workbench");
  await expect(page.locator("#console-main").getByRole("heading").first()).toBeVisible();
  await expect(page.getByRole("button", { name: "Quick navigation", exact: true })).toBeVisible();
  await page.keyboard.press(`${modKey()}+K`);
  const quickNavigation = page.getByRole("dialog", { name: "Quick navigation", exact: true });
  await expect(quickNavigation).toBeVisible();
  await expect(quickNavigation.getByRole("combobox")).toBeFocused();
  await page.keyboard.press("Escape");
  await expect(quickNavigation).not.toBeVisible();
  await page.getByRole("button", { name: "Collapse sidebar", exact: true }).click();
  const sidebar = page.locator("#console-sidebar");
  for (const route of ["/knowledge", "/settings", "/spaces"]) {
    await page.getByRole("navigation", { name: "Primary navigation", exact: true }).locator(`a[href="${route}"]`).click();
    await expect(page).toHaveURL(new RegExp(`${route}$`));
    await expect(page.locator("#console-main").getByRole("heading").first()).toBeVisible();
    await expect(page.getByRole("button", { name: "Expand sidebar", exact: true })).toBeVisible();
    await expect.poll(async () => (await sidebar.boundingBox())?.width).toBe(64);
  }
});
