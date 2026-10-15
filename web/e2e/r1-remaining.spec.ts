/** R1 controlled upload failure and real saved Office download. No provider calls. */
import fs from "node:fs/promises";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { createHash } from "node:crypto";
import { expect, test } from "@playwright/test";
import { buildAuthHeaders, ensureAuthenticatedPage } from "./support/helpers";

const here = path.dirname(fileURLToPath(import.meta.url));
const files = ["alpha.txt", "beta.txt", "gamma.txt"].map(name => path.join(here, "fixtures/r1-attachments", name));
const SELECTED = /^(Included in next message|随下条消息发送)$/;

test("controlled one-file upload 503 retries only that file and leaves originals intact", async ({ page }) => {
  const uploads: Record<string, number> = {};
  let deleteRequests = 0;
  await page.route("**/api/v1/files/**", async route => {
    if (route.request().method() === "DELETE") deleteRequests += 1;
    if (!route.request().url().endsWith("/upload")) return route.continue();
    const body = route.request().postDataBuffer()?.toString() || "";
    const name = ["alpha.txt", "beta.txt", "gamma.txt"].find(name => body.includes(`filename="${name}"`));
    expect(name).toBeTruthy();
    uploads[name!] = (uploads[name!] || 0) + 1;
    if (name === "beta.txt" && uploads[name] === 1) {
      return route.fulfill({ status: 503, contentType: "application/json", body: '{"detail":"controlled upload unavailable"}' });
    }
    return route.continue();
  });
  await ensureAuthenticatedPage(page, "/assistant");
  const newChat = page.getByRole("button", { name: /^(New chat|新对话)$/ });
  if ((await newChat.count()) === 0) await page.getByRole("button", { name: /^(Show history|显示历史)$/ }).click();
  await newChat.click();
  await page.getByRole("textbox", { name: /助手消息输入框|Assistant message input/ }).fill("R1 attachment retry draft; do not send");
  await page.locator('input[type="file"]').setInputFiles(files);
  await expect(page.getByRole("button", { name: /^(Retry|重试)$/ })).toBeVisible();
  await expect(page.getByRole("button", { name: SELECTED })).toHaveCount(2);
  await expect(page.getByRole("button", { name: /^(Send|发送)$/ })).toBeEnabled();
  await page.getByRole("button", { name: /^(Retry|重试)$/ }).click();
  await expect(page.getByRole("button", { name: SELECTED })).toHaveCount(3);
  await expect(page.getByRole("button", { name: /^(Retry|重试)$/ })).toHaveCount(0);
  expect(uploads).toEqual({ "alpha.txt": 1, "beta.txt": 2, "gamma.txt": 1 });
  await page.getByRole("button", { name: SELECTED }).nth(2).click();
  await expect(page.locator('[aria-label="本轮资料范围"]')).not.toContainText("gamma.txt");
  await page.getByRole("button", { name: /^(Delete: gamma.txt|删除: gamma.txt)$/ }).click();
  expect(deleteRequests).toBe(0);
  expect(await fs.readFile(files[2], "utf8")).toBe("R1 attachment GAMMA = violet\n");
});

test("real Office artifact downloads from the browser after reopening saved history", async ({ page, request }) => {
  const headers = await buildAuthHeaders(request);
  const sessionsResponse = await request.get(`${process.env.E2E_API_URL}/api/v1/assistant/sessions?limit=200`, { headers });
  expect(sessionsResponse.ok()).toBeTruthy();
  const sessions = (await sessionsResponse.json()).sessions as Array<{ session_id: string; metadata?: { title?: string } }>;
  const session = sessions.find(item => item.metadata?.title?.startsWith("请使用 mcp_docgen__generate_document"));
  expect(session, "the earlier real Office fixture must exist").toBeTruthy();
  const artifactsResponse = await request.get(`${process.env.E2E_API_URL}/api/v1/assistant/sessions/${session!.session_id}/artifacts`, { headers });
  expect(artifactsResponse.ok()).toBeTruthy();
  const artifacts = (await artifactsResponse.json()).artifacts as Array<{ artifact_id: string; format: string; filename: string; size_bytes: number }>;
  const office = artifacts.find(item => item.format === "docx");
  expect(office).toBeTruthy();
  const content = await request.get(`${process.env.E2E_API_URL}/api/v1/assistant/artifacts/${office!.artifact_id}/download`, { headers });
  expect(content.ok()).toBeTruthy();
  const expectedBytes = await content.body();
  expect(expectedBytes.length).toBe(office!.size_bytes);
  await ensureAuthenticatedPage(page, "/assistant");
  const search = page.getByPlaceholder(/Search conversations|搜索对话/);
  if (!(await search.isVisible())) await page.getByRole("button", { name: /^(Show history|显示历史)$/ }).click();
  await search.fill("R1 Off");
  await page.getByRole("button", { name: session!.metadata!.title, exact: true }).click();
  const download = page.getByRole("button", { name: /^(Download|下载)$/ });
  await expect(download).toBeVisible();
  const event = page.waitForEvent("download");
  await download.click();
  const downloaded = await event;
  const downloadedPath = await downloaded.path();
  expect(downloadedPath).toBeTruthy();
  const actual = await fs.readFile(downloadedPath!);
  expect(actual.subarray(0, 2).toString()).toBe("PK");
  expect(createHash("sha256").update(actual).digest("hex")).toBe(createHash("sha256").update(expectedBytes).digest("hex"));
  expect(downloaded.suggestedFilename()).toMatch(/\.docx$/);
  await page.reload();
  await expect(page.getByText("R1 Office 验收", { exact: true }).first()).toBeVisible();
});
