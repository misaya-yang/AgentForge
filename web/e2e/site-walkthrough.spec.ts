import { expect, test, type Page, type TestInfo } from "@playwright/test";

const sidebarNavSelector = "aside a[href^='/'], [role='complementary'] a[href^='/']";
const standaloneProtectedRoutes = ["/knowledge/create"];

interface DataBackedRoutes {
  protectedRoutes: string[];
  publicRoutes: string[];
}

async function collectDataBackedRoutes(page: Page): Promise<DataBackedRoutes> {
  return page.evaluate(async () => {
    const routes: DataBackedRoutes = { protectedRoutes: [], publicRoutes: [] };
    const rawAuth = window.localStorage.getItem("agent-gateway-auth");
    const token = rawAuth ? JSON.parse(rawAuth)?.state?.token : "";
    if (!token) return routes;

    const headers = { Authorization: `Bearer ${token}` };
    const fetchJson = async (path: string): Promise<unknown> => {
      const response = await window.fetch(path, { headers });
      if (!response.ok) return null;
      try {
        return await response.json();
      } catch {
        return null;
      }
    };

    const readString = (value: unknown, key: string): string => {
      if (!value || typeof value !== "object") return "";
      const candidate = (value as Record<string, unknown>)[key];
      return typeof candidate === "string" ? candidate : "";
    };

    const firstRecord = (payload: unknown, listKey?: string): unknown => {
      if (Array.isArray(payload)) return payload[0];
      if (!payload || typeof payload !== "object" || !listKey) return null;
      const items = (payload as Record<string, unknown>)[listKey];
      return Array.isArray(items) ? items[0] : null;
    };

    const addRoute = (collection: string[], route: string) => {
      if (route) collection.push(route);
    };

    const currentUser = await fetchJson("/api/v1/auth/me");
    const userId = readString(currentUser, "user_id");
    addRoute(routes.protectedRoutes, userId ? `/users/${encodeURIComponent(userId)}/edit` : "");

    const firstDataset = firstRecord(await fetchJson("/api/v1/knowledge/datasets"));
    const datasetId = readString(firstDataset, "dataset_id");
    addRoute(routes.protectedRoutes, datasetId ? `/knowledge/${encodeURIComponent(datasetId)}` : "");

    const firstShare = firstRecord(await fetchJson("/api/v1/assistant/shares?limit=5"), "shares");
    const shareCode = readString(firstShare, "share_code");
    addRoute(routes.publicRoutes, shareCode ? `/share/${encodeURIComponent(shareCode)}` : "");

    return routes;
  });
}

function normalizeRouteForFile(route: string): string {
  if (!route || route === "/") return "home";
  return route.replace(/^\//, "").replace(/[/?#:&=]+/g, "-");
}

async function collectVisibleRoutes(page: Page): Promise<string[]> {
  const navLinks = page.locator(sidebarNavSelector);
  const routes = new Set<string>(["/dashboard"]);
  await expect(navLinks.first()).toBeVisible({ timeout: 15_000 });
  const navCount = await navLinks.count();
  expect(navCount).toBeGreaterThan(0);

  for (let index = 0; index < navCount; index += 1) {
    const href = await navLinks.nth(index).getAttribute("href");
    if (href && href.startsWith("/")) {
      routes.add(href);
    }
  }

  return [...routes];
}

test("walk main pages and detect obvious UI breakage", async ({ page }, testInfo: TestInfo) => {
  test.setTimeout(5 * 60_000);

  const consoleErrors: string[] = [];
  const pageErrors: string[] = [];
  const badResponses: string[] = [];

  page.on("response", (response) => {
    const status = response.status();
    if (status < 400) return;

    const url = response.url();
    if (/favicon\.ico/i.test(url)) return;

    badResponses.push(`${status} ${url}`);
  });

  page.on("console", (message) => {
    if (message.type() !== "error") return;
    const text = message.text();
    if (!/favicon|NO_COLOR/i.test(text)) {
      consoleErrors.push(text);
    }
  });

  page.on("pageerror", (error) => {
    pageErrors.push(String(error));
  });

  await page.goto("/dashboard", { waitUntil: "domcontentloaded" });
  await expect(page).toHaveURL(/\/dashboard/);

  const dataBackedRoutes = await collectDataBackedRoutes(page);
  const protectedRoutes = [
    ...new Set([
      ...(await collectVisibleRoutes(page)),
      ...standaloneProtectedRoutes,
      ...dataBackedRoutes.protectedRoutes,
    ]),
  ];
  const publicRoutes = [...new Set(dataBackedRoutes.publicRoutes)];
  const mainContent = page.locator(".ant-layout-content, main").first();

  for (const route of protectedRoutes) {
    await page.goto(route, { waitUntil: "domcontentloaded" });
    await expect(page).toHaveURL(new RegExp(route.replace(/[.*+?^${}()|[\]\\]/g, "\\$&")));
    await expect(page.locator(sidebarNavSelector).first()).toBeVisible({ timeout: 15_000 });
    await expect(mainContent).toBeVisible();
    await expect(page.getByText(/something went wrong|application error|runtime error/i)).toHaveCount(0);

    const screenshotPath = testInfo.outputPath(`walkthrough-${normalizeRouteForFile(route)}.png`);
    await page.screenshot({ path: screenshotPath });
  }

  for (const route of publicRoutes) {
    await page.goto(route, { waitUntil: "domcontentloaded" });
    await expect(page).toHaveURL(new RegExp(route.replace(/[.*+?^${}()|[\]\\]/g, "\\$&")));
    await expect(page.locator("body")).toBeVisible();
    await expect(page.getByText(/something went wrong|application error|runtime error/i)).toHaveCount(0);

    const screenshotPath = testInfo.outputPath(`walkthrough-${normalizeRouteForFile(route)}.png`);
    await page.screenshot({ path: screenshotPath });
  }

  const uniqueBadResponses = [...new Set(badResponses)];
  expect(pageErrors, `Page runtime errors:\n${pageErrors.join("\n")}`).toEqual([]);
  expect(consoleErrors, `Console errors:\n${consoleErrors.join("\n")}`).toEqual([]);
  expect(uniqueBadResponses, `Network responses >= 400:\n${uniqueBadResponses.join("\n")}`).toEqual([]);
});
