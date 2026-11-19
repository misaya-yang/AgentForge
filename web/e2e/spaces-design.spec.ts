import { test, expect } from "@playwright/test";
import { readFile } from "node:fs/promises";
import { installDot1113Fixture } from "./support/dot1113";
import { seedClientPrefs } from "./support/helpers";

test("phone editor keeps navigation compact, uses the dark theme, and anchors its AI menu", async ({ page }) => {
  await installDot1113Fixture(page);
  await seedClientPrefs(page, { locale: "en-US", themeMode: "dark", resolvedTheme: "dark", darkMode: true });
  await page.setViewportSize({ width: 390, height: 844 });
  await page.emulateMedia({ reducedMotion: "reduce" });
  await page.goto("/spaces/item/item-0");
  const editor = page.locator(".spaces-editor .bn-editor");
  await expect(editor).toBeVisible();
  await expect(page.locator(".spaces-navigation-trigger")).toHaveAttribute("aria-expanded", "false");
  await expect(page.locator("#spaces-navigation-content")).toHaveAttribute("inert", "");
  await expect(page.locator(".spaces-tree-trigger")).toHaveAttribute("aria-expanded", "false");
  expect((await editor.boundingBox())!.y).toBeLessThan(844);
  const background = await editor.evaluate(element => getComputedStyle(element).backgroundColor);
  expect(background).not.toBe("rgb(255, 255, 255)");
  await expect(page.locator(".spaces-blocknote")).toHaveAttribute("data-color-scheme", "dark");

  const ai = page.getByRole("button", { name: "AI", exact: true });
  const menu = page.locator(".spaces-ai");
  await expect(menu).toBeHidden();
  await ai.click();
  await expect(menu).toBeVisible();
  await expect(ai).toHaveAttribute("aria-expanded", "true");
  await expect(menu.getByRole("menuitem")).toHaveCount(5);
  const triggerRect = (await ai.boundingBox())!;
  const menuRect = (await menu.boundingBox())!;
  expect(menuRect.y).toBeGreaterThanOrEqual(triggerRect.y + triggerRect.height);
  expect(menuRect.x).toBeGreaterThanOrEqual(0);
  expect(menuRect.x + menuRect.width).toBeLessThanOrEqual(390);
  await page.keyboard.press("Escape");
  await expect(menu).toBeHidden();

  await page.locator(".spaces-tree-trigger").click();
  await expect(page.locator(".spaces-tree-trigger")).toHaveAttribute("aria-expanded", "true");
  const grip = page.locator(".spaces-drag svg").first();
  await expect(grip).toBeVisible();
  expect((await grip.boundingBox())!.width).toBeGreaterThan(8);
  await page.locator(".spaces-tree-trigger").click();
  await page.locator(".spaces-navigation-trigger").click();
  await expect(page.locator(".spaces-navigation-trigger")).toHaveAttribute("aria-expanded", "true");
  await page.locator(".spaces-sidebar").getByRole("link", { name: "All", exact: true }).click();
  await expect(page).toHaveURL(/\/spaces$/);
  await expect(page.locator(".spaces-navigation-trigger")).toHaveAttribute("aria-expanded", "false");
});

test("editor preserves revision controls and downloads Markdown and HTML exports", async ({ page }) => {
  await installDot1113Fixture(page);
  await seedClientPrefs(page, { locale: "en-US", themeMode: "light", resolvedTheme: "light", darkMode: false });
  await page.setViewportSize({ width: 1440, height: 960 });
  await page.route("**/api/v1/knowledge/space-items/item-0/export?*", route => {
    const html = new URL(route.request().url()).searchParams.get("format") === "html";
    return route.fulfill({ contentType: "application/json", body: JSON.stringify({
      filename: `Project handbook.${html ? "html" : "md"}`,
      format: html ? "html" : "markdown",
      body: html ? "<h1>Project handbook</h1>" : "# Project handbook",
    }) });
  });
  await page.goto("/spaces/item/item-0");
  await expect(page.locator(".spaces-editor .bn-editor")).toBeVisible();
  await expect(page.locator(".spaces-crumbs [aria-current=page]")).toHaveText("Project handbook");
  const revision = page.getByRole("button", { name: "Revision 1", exact: true });
  await revision.click();
  await expect(revision).toHaveAttribute("aria-pressed", "true");
  await expect(page.getByRole("button", { name: "Restore", exact: true })).toBeVisible();
  await expect(page.getByText("No backlinks yet", { exact: true })).toBeVisible();
  for (const [name, filename, body] of [
    ["Export Markdown", "Project handbook.md", "# Project handbook"],
    ["Export HTML", "Project handbook.html", "<h1>Project handbook</h1>"],
  ]) {
    const downloadEvent = page.waitForEvent("download");
    await page.getByRole("button", { name, exact: true }).click();
    const download = await downloadEvent;
    expect(download.suggestedFilename()).toBe(filename);
    expect(await readFile((await download.path())!, "utf8")).toBe(body);
  }
});
