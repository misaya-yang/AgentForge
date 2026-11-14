import { test, expect } from "@playwright/test";
import { mkdir } from "node:fs/promises";
import path from "node:path";
import { installDot1113Fixture } from "./support/dot1113";
import { seedClientPrefs } from "./support/helpers";

const directory = path.resolve("../reports/ui-dot_1113");
const surfaces = [
  ["workbench", "/workbench"], ["assistant", "/assistant"], ["knowledge", "/knowledge"],
  ["spaces-list", "/spaces"], ["spaces-grid", "/spaces"], ["spaces-editor", "/spaces/item/item-0"],
  ["connectors", "/settings/connectors"], ["connect-apps", "/workbench"], ["settings", "/settings"], ["login", "/login"],
] as const;
for (const theme of ["light", "dark"] as const) {
  for (const width of [1440, 1024, 390]) {
    for (const [name, route] of surfaces) {
      test(`${name} ${theme} ${width}`, async ({ page }) => {
        await installDot1113Fixture(page);
        await seedClientPrefs(page, { locale: "en-US", themeMode: theme, resolvedTheme: theme, darkMode: theme === "dark" });
        if (name === "login") await page.route("**/api/auth/session", handler => handler.fulfill({ status: 401, contentType: "application/json", body: '{"detail":"anonymous_fixture"}' }));
        await page.setViewportSize({ width, height: width === 390 ? 844 : 960 });
        await page.emulateMedia({ reducedMotion: "reduce" });
        await page.goto(route);
        await expect(page.locator("h1, h2").first()).toBeVisible();
        await expect(page.locator("body")).not.toContainText("Something went wrong");
        if (name === "spaces-grid") await page.getByRole("button", { name: "Grid", exact: true }).click();
        if (name === "spaces-editor") await expect(page.locator(".spaces-editor [contenteditable=true]").first()).toBeVisible();
        if (name === "connect-apps") {
          await page.getByRole("button", { name: "Connect apps", exact: true }).click();
          const dialog = page.getByRole("dialog", { name: "Connect apps" });
          await expect(dialog).toBeVisible();
          await expect(dialog).toHaveAttribute("closedby", "any");
          const rect = await dialog.boundingBox();
          expect(rect).not.toBeNull();
          expect(Math.abs(rect!.x + rect!.width / 2 - width / 2)).toBeLessThanOrEqual(2);
          expect(rect!.width).toBeLessThan(width);
          await expect(dialog.getByRole("searchbox")).toBeFocused();
        }
        await page.evaluate(() => document.fonts.ready);
        const overflow = await page.evaluate(() => document.documentElement.scrollWidth > window.innerWidth + 1);
        expect(overflow, `${name} must fit ${width}px`).toBe(false);
        await mkdir(directory, { recursive: true });
        await page.screenshot({ path: path.join(directory, `${name}-${theme}-${width}.png`), fullPage: true, animations: "disabled" });
      });
    }
  }
}
