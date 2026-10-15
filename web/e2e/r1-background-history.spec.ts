/** Existing real runs only: browser background and session switching never execute again. */
import { expect, test, type Page } from "@playwright/test";
import { buildAuthHeaders, ensureAuthenticatedPage } from "./support/helpers";

test.use({ channel: "chromium" });

async function openSession(page: Page, title: string, query: string) {
  const search = page.getByPlaceholder(/Search conversations|搜索对话/);
  if (!(await search.isVisible())) {
    await page.getByRole("button", { name: /^(Show history|显示历史)$/ }).first().click();
  }
  await search.fill(query);
  await page.getByRole("button", { name: title, exact: true }).click();
}

test("existing run survives background tab and conversation switch without execution", async ({ page, request }) => {
  const headers = await buildAuthHeaders(request);
  const sessionsResponse = await request.get(`${process.env.E2E_API_URL}/api/v1/assistant/sessions?limit=200`, { headers });
  expect(sessionsResponse.ok()).toBeTruthy();
  const sessions = (await sessionsResponse.json()).sessions as Array<{ session_id: string; metadata?: { title?: string } }>;
  const ttl = sessions.find(s => s.metadata?.title?.startsWith("R1-BOUNDARY-TTL-927"));
  const pdf = sessions.find(s => s.metadata?.title?.startsWith("R1-PDF-927"));
  expect(ttl && pdf).toBeTruthy();
  const historyPath = (id: string) => `${process.env.E2E_API_URL}/api/v1/assistant/sessions/${id}/history`;
  const before = await Promise.all([ttl!, pdf!].map(async session => {
    const response = await request.get(historyPath(session.session_id), { headers });
    expect(response.ok()).toBeTruthy();
    return (await response.json()).messages as Array<{ role: string; content: string; metadata?: { runtime_run_id?: string } }>;
  }));
  const identities = before.map(messages => messages.filter(m => m.role === "assistant").map(m => m.metadata?.runtime_run_id));
  const executionPosts: string[] = [];
  const watch = (r: import("@playwright/test").Request) => {
    if (r.method() === "POST" && /\/api\/(v2\/agent\/threads|v1\/assistant\/(chat|tools|tasks))/.test(r.url())) executionPosts.push(r.url());
  };
  page.on("request", watch);
  await ensureAuthenticatedPage(page, "/assistant");
  await openSession(page, ttl!.metadata!.title!, "R1-BOUNDARY-TTL-927");
  await expect(page.getByRole("log")).toContainText("capability approval expired");
  await openSession(page, pdf!.metadata!.title!, "R1-PDF-927");
  await expect(page.getByRole("log")).toContainText("PDF 已生成完毕");

  const otherTab = await page.context().newPage();
  otherTab.on("request", watch);
  await otherTab.goto("/dashboard");
  await expect(otherTab).toHaveURL(/\/dashboard/);
  await page.bringToFront();
  await expect(page.getByRole("log")).toContainText("PDF 已生成完毕");
  await openSession(page, ttl!.metadata!.title!, "R1-BOUNDARY-TTL-927");
  await expect(page.getByRole("log")).toContainText("capability approval expired");
  await page.reload();
  await expect(page.getByRole("log")).toContainText("capability approval expired");
  await otherTab.close();

  const after = await Promise.all([ttl!, pdf!].map(async session => {
    const response = await request.get(historyPath(session.session_id), { headers });
    expect(response.ok()).toBeTruthy();
    return (await response.json()).messages as Array<{ role: string; content: string; metadata?: { runtime_run_id?: string } }>;
  }));
  expect(after.map(messages => messages.filter(m => m.role === "assistant").map(m => m.metadata?.runtime_run_id))).toEqual(identities);
  expect(after.map(messages => messages.map(m => m.content))).toEqual(before.map(messages => messages.map(m => m.content)));
  expect(executionPosts).toEqual([]);
});
