import { test, expect } from "@playwright/test";
import { mkdir, writeFile } from "node:fs/promises";
import path from "node:path";
import { installDot1113Fixture } from "./support/dot1113";

type Metrics = { lcp: number; cls: number; durations: Record<string, number>; supported: boolean };
const output = path.resolve("../reports/perf-dot_1113/cwv");
for (const route of ["workbench", "assistant", "knowledge", "spaces"]) {
  for (let run = 1; run <= 5; run++) {
    test(`${route} CWV sample ${run}`, async ({ page }) => {
      await installDot1113Fixture(page);
      await page.setViewportSize({ width: 1440, height: 900 });
      await page.addInitScript(() => {
        const state: Metrics = { lcp: 0, cls: 0, durations: {}, supported: PerformanceObserver.supportedEntryTypes.includes("event") };
        (window as unknown as { cwv: Metrics }).cwv = state;
        new PerformanceObserver(list => { for (const entry of list.getEntries()) state.lcp = entry.startTime; }).observe({ type: "largest-contentful-paint", buffered: true });
        new PerformanceObserver(list => { for (const entry of list.getEntries() as Array<PerformanceEntry & { value: number; hadRecentInput: boolean }>) if (!entry.hadRecentInput) state.cls += entry.value; }).observe({ type: "layout-shift", buffered: true });
        if (state.supported) new PerformanceObserver(list => {
          for (const entry of list.getEntries() as Array<PerformanceEntry & { interactionId: number }>) if (entry.interactionId) state.durations[String(entry.interactionId)] = Math.max(state.durations[String(entry.interactionId)] ?? 0, entry.duration);
        }).observe({ type: "event", buffered: true, durationThreshold: 16 } as PerformanceObserverInit);
      });
      await page.goto(`/${route}`);
      await expect(page.locator('script[src*="/@vite/client"]'), "CWV requires production assets; use playwright.perf.config.ts with an existing E2E_BASE_URL").toHaveCount(0);
      await expect(page.locator("main")).toBeVisible();
      await expect(page.locator("main h1, main h2").first()).toBeVisible();
      await expect(page.locator("body")).not.toContainText("Something went wrong");
      await page.evaluate(() => document.fonts.ready);
      // Allow the final painted content to enter buffered LCP before real inputs freeze it.
      await page.evaluate(() => new Promise<void>(resolve => requestAnimationFrame(() => requestAnimationFrame(() => resolve()))));
      const button = page.getByRole("button", { name: "Connect apps", exact: true });
      for (let interaction = 0; interaction < 3; interaction++) {
        await button.click();
        await expect(page.getByRole("dialog", { name: "Connect apps" })).toBeVisible();
        const search = page.getByRole("dialog").getByRole("searchbox");
        await search.pressSequentially("github");
        await search.fill("");
        await page.keyboard.press("Escape");
        await expect(page.getByRole("dialog", { name: "Connect apps" })).not.toBeVisible();
      }
      await page.waitForTimeout(300); // PerformanceObserver event delivery, not a readiness substitute.
      const measured = await page.evaluate(() => {
        const cwv = (window as unknown as { cwv: Metrics }).cwv;
        return { ...cwv, interactionCount: (performance as Performance & { interactionCount?: number }).interactionCount };
      });
      expect(measured.supported, "Event Timing must be measured, never substituted").toBe(true);
      expect(measured.interactionCount ?? Object.keys(measured.durations).length).toBeGreaterThan(0);
      expect(measured.lcp).toBeGreaterThan(0);
      // Entries below 16ms are censored by Event Timing. Record the conservative upper bound.
      const inp = Math.max(16, ...Object.values(measured.durations));
      const record = { route: `/${route}`, sample: run, lcp_ms: measured.lcp, inp_ms: inp, cls: measured.cls, interaction_count: measured.interactionCount, method: "cold-context production navigation; fixture API; Event Timing max per interaction (<50 interactions); 16ms lower reporting bound" };
      await mkdir(output, { recursive: true });
      await writeFile(path.join(output, `${route}-${run}.json`), JSON.stringify(record, null, 2) + "\n");
      expect(record.lcp_ms).toBeLessThan(2500);
      expect(record.inp_ms).toBeLessThan(200);
      expect(record.cls).toBeLessThan(.1);
    });
  }
}
