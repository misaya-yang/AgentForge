/** Real saved R1 artifacts and owner/guest sharing; no provider or tool execution. */
import { expect, test, type Page } from "@playwright/test";
import { createHash } from "node:crypto";
import { buildAuthHeaders, ensureAuthenticatedPage } from "./support/helpers";

const API = () => process.env.E2E_API_URL!;
const SHARE = /^(Share|分享)$/;
const CLOSE = /^(Close|关闭)$/;
const REVOKE = /^(Revoke|撤销)$/;
async function openHistory(page: Page, title: string, prefix: string) {
  await ensureAuthenticatedPage(page, "/assistant");
  const search = page.getByPlaceholder(/Search conversations|搜索对话/);
  if (!(await search.isVisible())) await page.getByRole("button", { name: /^(Show history|显示历史)$/ }).click();
  await search.fill(prefix);
  await page.getByRole("button", { name: title, exact: true }).click();
}

test("real PDF share matches preview, file exclusion and owner revocation at 390px", async ({ page, request, browser }, info) => {
  await page.setViewportSize({ width: 390, height: 844 });
  const headers = await buildAuthHeaders(request);
  const sessions = (await (await request.get(`${API()}/api/v1/assistant/sessions?limit=200`, { headers })).json()).sessions;
  const session = sessions.find((s: { metadata?: { title?: string } }) => s.metadata?.title?.startsWith("R1-PDF-927"));
  expect(session).toBeTruthy();
  let executionPosts = 0;
  page.on("request", r => { if (r.method() === "POST" && /\/api\/v2\/agent\/threads/.test(r.url())) executionPosts += 1; });
  await openHistory(page, session.metadata.title, "R1-PDF-927");
  const trigger = page.getByRole("button", { name: SHARE, exact: true });
  await trigger.click();
  const dialog = page.getByRole("dialog", { name: /Share Conversation|分享对话/ });
  await expect(dialog).toContainText("R1-PDF-927.pdf");
  await dialog.getByRole("button", { name: CLOSE }).press("Escape");
  await expect(dialog).not.toBeVisible();
  await expect(trigger).toBeFocused();
  const guest = await browser.newContext({ storageState: { cookies: [], origins: [] }, viewport: { width: 390, height: 844 } });
  const visitor = await guest.newPage();
  const facts: unknown[] = [];
  try {
    for (const include of [false, true]) {
      await trigger.click();
      await dialog.getByRole("checkbox").setChecked(include);
      const create = dialog.getByRole("button", { name: /Create Share Link|创建分享链接/ });
      await expect(create).toBeEnabled();
      const created = page.waitForResponse(r => r.request().method() === "POST" && r.url().endsWith(`/sessions/${session.session_id}/share`));
      await create.press("Enter");
      const response = await created;
      expect(response.ok()).toBeTruthy();
      const share = await response.json();
      await visitor.goto(`${process.env.E2E_BASE_URL}${share.share_url}`);
      const download = visitor.getByRole("link", { name: /^(Download|下载)$/ });
      await expect(download).toHaveCount(include ? 1 : 0);
      const publicFile = await guest.request.get(`${API()}/api/v1/assistant/shares/${share.share_code}/artifact/art_0bacdcbb7d5481e3`);
      expect(publicFile.status()).toBe(include ? 200 : 404);
      if (include) expect(createHash("sha256").update(await publicFile.body()).digest("hex")).toBe("461b7290028c728896001bfb0e353cb6fc6652e207ee811987aff2d02115df28");
      await expect.poll(() => visitor.evaluate(() => document.documentElement.scrollWidth <= innerWidth)).toBeTruthy();
      await dialog.getByRole("link", { name: share.share_code, exact: true }).locator("..").getByRole("button", { name: REVOKE }).click();
      await expect(dialog.getByRole("link", { name: share.share_code, exact: true })).toHaveCount(0);
      await visitor.reload();
      await expect(visitor.getByRole("alert")).toContainText(/revoked|撤销|不存在/);
      expect((await guest.request.get(`${API()}/api/v1/assistant/shares/${share.share_code}/artifact/art_0bacdcbb7d5481e3`)).status()).toBe(404);
      facts.push({ tier: "real_saved_artifact", share_code: share.share_code, include, public_status: publicFile.status(), revoked: true });
      await dialog.getByRole("button", { name: CLOSE }).click();
    }
    expect(executionPosts).toBe(0);
    await info.attach("real-sharing-facts", { body: JSON.stringify({ facts, executionPosts }), contentType: "application/json" });
  } finally { await guest.close(); }
});

test("real Quiz links can be previewed before taking, found after refresh and revoked", async ({ page, request, browser }, info) => {
  await page.setViewportSize({ width: 390, height: 844 });
  const headers = await buildAuthHeaders(request);
  const sessions = (await (await request.get(`${API()}/api/v1/assistant/sessions?limit=200`, { headers })).json()).sessions;
  const session = sessions.find((s: { metadata?: { title?: string } }) => s.metadata?.title?.startsWith("持久恢复验收 DR-C4"));
  expect(session).toBeTruthy();
  await openHistory(page, session.metadata.title, "DR-C4");
  const trigger = page.getByRole("button", { name: /^(Share Quiz|分享测验)$/ });
  await trigger.click();
  const dialog = page.getByRole("dialog", { name: /Share Quiz|分享测验/ });
  await expect(dialog).toContainText(/2\s*\+\s*3/);
  await dialog.getByRole("button", { name: CLOSE }).press("Escape");
  await expect(dialog).not.toBeVisible();
  await expect(trigger).toBeFocused();
  await trigger.press("Enter");
  const create = dialog.getByRole("button", { name: /Generate Share Link|生成分享链接/ });
  await expect(create).toBeEnabled();
  const created = page.waitForResponse(r => r.request().method() === "POST" && r.url().endsWith("/artifact-shares"));
  await create.press("Enter");
  const response = await created;
  expect(response.ok()).toBeTruthy();
  const share = await response.json();
  expect(new Date(share.expires_at).getTime() - Date.now()).toBeGreaterThan(6 * 86400000);
  const guest = await browser.newContext({ storageState: { cookies: [], origins: [] } });
  try {
    const publicQuiz = await guest.request.get(`${API()}/api/v1/quiz/shared/${share.share_code}`);
    expect(publicQuiz.ok()).toBeTruthy();
    expect(JSON.stringify(await publicQuiz.json())).not.toMatch(/correct_answer|explanation|answer_keys/);
    await page.reload();
    await trigger.click();
    await expect(dialog.getByRole("link", { name: share.share_code, exact: true })).toBeVisible();
    await dialog.getByRole("link", { name: share.share_code, exact: true }).locator("..").getByRole("button", { name: REVOKE }).click();
    await expect(dialog.getByRole("link", { name: share.share_code, exact: true }).locator("..")).toContainText(/revoked|撤销/i);
    expect((await guest.request.get(`${API()}/api/v1/quiz/shared/${share.share_code}`)).status()).toBe(404);
    expect((await guest.request.post(`${API()}/api/v1/quiz/public/${share.share_code}/attempts/start`)).status()).toBe(404);
    await info.attach("real-quiz-management-facts", { body: JSON.stringify({ tier: "real_saved_quiz", share_code: share.share_code, preview: true, anonymous_no_answers: true, expiry_days: 7, refresh_list: true, revoked: true }), contentType: "application/json" });
  } finally { await guest.close(); }
});

// The dedicated link's cutoff was shortened once by the primary's controlled
// expiry script; this is real HTTP410 rendering, not a naturally elapsed week.
test("controlled expired dedicated share gives English and Chinese next-step guidance", async ({ browser }, info) => {
  const { seedClientPrefs } = await import("./support/helpers");
  for (const locale of ["en-US", "zh-CN"] as const) {
    const context = await browser.newContext({ storageState: { cookies: [], origins: [] }, viewport: { width: 390, height: 844 } });
    try {
      const page = await context.newPage();
      await seedClientPrefs(page, { locale });
      await page.goto(`${process.env.E2E_BASE_URL}/share/1j5qyuji`);
      await expect(page.getByRole("alert")).toContainText(locale === "en-US" ? /expired/ : /过期/);
      await expect(page.getByRole("alert")).toContainText(locale === "en-US" ? /owner.*new link/ : /所有者.*新链接/);
      await expect(page.getByRole("link", { name: /^(Download|下载)$/ })).toHaveCount(0);
      await info.attach(`expiry-${locale}`, { body: JSON.stringify({ tier: "controlled_expiry_real_http", locale, share_code: "1j5qyuji", status: 410, next_step: true }), contentType: "application/json" });
    } finally { await context.close(); }
  }
});
