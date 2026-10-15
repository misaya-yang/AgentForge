/** Real saved PDF + explicitly controlled image-history recovery. No provider calls. */
import fs from "node:fs/promises";
import { createHash } from "node:crypto";
import { expect, test, type Page } from "@playwright/test";
import { buildAuthHeaders, ensureAuthenticatedPage } from "./support/helpers";

// Full Chromium includes its PDF viewer; the headless shell only downloads PDFs.
test.use({ channel: "chromium" });

async function openHistory(page: Page, title: string, searchText: string) {
  await ensureAuthenticatedPage(page, "/assistant");
  const search = page.getByPlaceholder(/Search conversations|搜索对话/);
  if (!(await search.isVisible())) await page.getByRole("button", { name: /^(Show history|显示历史)$/ }).first().click();
  await search.fill(searchText);
  await page.getByRole("button", { name: title, exact: true }).click();
}

test("real saved PDF opens authenticated preview and downloads identical bytes at 390px", async ({ page, request }) => {
  await page.setViewportSize({ width: 390, height: 844 });
  const headers = await buildAuthHeaders(request);
  const sessions = await request.get(`${process.env.E2E_API_URL}/api/v1/assistant/sessions?limit=200`, { headers });
  expect(sessions.ok()).toBeTruthy();
  const session = (await sessions.json()).sessions.find((s: { metadata?: { title?: string } }) => s.metadata?.title?.startsWith("R1-PDF-927"));
  expect(session).toBeTruthy();
  const response = await request.get(`${process.env.E2E_API_URL}/api/v1/assistant/sessions/${session.session_id}/artifacts`, { headers });
  const pdf = (await response.json()).artifacts.find((a: { format: string }) => a.format === "pdf");
  expect(pdf).toBeTruthy();
  const content = await request.get(`${process.env.E2E_API_URL}/api/v1/assistant/artifacts/${pdf.artifact_id}/download`, { headers });
  expect(content.ok()).toBeTruthy();
  const expected = await content.body();
  expect(expected.subarray(0, 4).toString()).toBe("%PDF");
  await openHistory(page, session.metadata.title, "R1-PDF-927");
  await page.getByRole("button", { name: /^(Artifacts|产物).*1/ }).click();
  const download = page.getByRole("button", { name: /^(Download|下载)$/ }).first();
  await expect(download).toBeVisible();
  const event = page.waitForEvent("download");
  await download.click();
  const file = await event;
  const actual = await fs.readFile((await file.path())!);
  expect(createHash("sha256").update(actual).digest("hex")).toBe(createHash("sha256").update(expected).digest("hex"));
  const preview = page.getByRole("button", { name: /^(Open in new tab|新标签页打开)$/ }).first();
  await expect(preview).toBeVisible();
  const popupEvent = page.waitForEvent("popup");
  await preview.click();
  const popup = await popupEvent;
  await expect(popup).toHaveURL(/^blob:/);
  await popup.close();
  await page.reload();
  await page.getByRole("button", { name: /^(Artifacts|产物).*1/ }).click();
  await expect(page.getByText("R1-PDF-927.pdf", { exact: true }).first()).toBeVisible();
  expect(await page.evaluate(() => document.documentElement.scrollWidth)).toBeLessThanOrEqual(390);
});

test("controlled recovered image observes pending then unknown without any execution POST", async ({ page, request }) => {
  const headers = await buildAuthHeaders(request);
  const id = "3979c378-2057-41c8-ac75-a1ec51b8045b";
  const sessions = await request.get(`${process.env.E2E_API_URL}/api/v1/assistant/sessions?limit=200`, { headers });
  const session = (await sessions.json()).sessions.find((s: { session_id: string }) => s.session_id === id);
  expect(session).toBeTruthy();
  let reads = 0;
  let allowTerminal!: () => void;
  const terminalAllowed = new Promise<void>(resolve => { allowTerminal = resolve; });
  const executionPosts: string[] = [];
  page.on("request", r => {
    if (r.method() === "POST" && /\/api\/(v1\/agent\/images|v2\/agent\/threads)/.test(r.url())) executionPosts.push(r.url());
  });
  await page.route(`**/api/v1/assistant/sessions/${id}/history*`, async route => {
    const response = await route.fetch();
    const history = await response.json();
    const image = history.messages.find((m: { metadata?: { image_task_id?: string } }) => m.metadata?.image_task_id === "imt_d61ec6f47a444430a770");
    expect(image).toBeTruthy();
    if (++reads === 1) {
      image.metadata.image_generating = true;
      image.metadata.process_summary.status = "running";
      image.metadata.process_summary.outcome_uncertain = false;
    } else {
      await terminalAllowed;
    }
    await route.fulfill({ response, json: history });
  });
  await openHistory(page, session.metadata.title, "R1-IMAGE-PROOF-927D");
  await page.getByRole("textbox", { name: /Assistant message composer|助手消息输入框/ }).fill("Controlled image recovery draft; do not send");
  await expect(page.getByRole("button", { name: /^(Send|发送)$/ })).toBeDisabled();
  allowTerminal();
  await expect(page.getByText(/^(结果未知|Result unknown)$/).first()).toBeVisible();
  expect(reads).toBeGreaterThanOrEqual(2);
  await page.reload();
  await expect(page.getByText("imt_d61ec6f47a444430a770", { exact: false }).first()).toBeVisible();
  expect(executionPosts).toEqual([]);
});

test("controlled partial, empty, and unavailable artifact history never starts execution", async ({ page, request }) => {
  test.setTimeout(45_000);
  const headers = await buildAuthHeaders(request);
  const sessions = await request.get(`${process.env.E2E_API_URL}/api/v1/assistant/sessions?limit=200`, { headers });
  const session = (await sessions.json()).sessions.find((s: { metadata?: { title?: string } }) => s.metadata?.title?.startsWith("R1-PDF-927"));
  expect(session).toBeTruthy();
  const original = await request.get(`${process.env.E2E_API_URL}/api/v1/assistant/sessions/${session.session_id}/artifacts`, { headers });
  const pdf = (await original.json()).artifacts.find((a: { format: string }) => a.format === "pdf");
  expect(pdf).toBeTruthy();
  let mode: "partial" | "unavailable" | "real" = "partial";
  let emptyDownloadReads = 0;
  const executionPosts: string[] = [];
  page.on("request", r => {
    if (r.method() === "POST" && /\/api\/(v1\/assistant\/chat|v2\/agent\/threads)/.test(r.url())) executionPosts.push(r.url());
    if (r.method() === "GET" && r.url().includes("art_r1_empty")) emptyDownloadReads += 1;
  });
  await page.route(`**/api/v1/assistant/sessions/${session.session_id}/artifacts*`, async route => {
    if (mode === "unavailable") {
      await route.fulfill({ status: 503, contentType: "application/json", body: JSON.stringify({ detail: "TEMPORARY_UNAVAILABLE" }) });
      return;
    }
    const response = await route.fetch();
    if (mode === "real") {
      await route.fulfill({ response });
      return;
    }
    const body = await response.json();
    const emptyDocument = { ...pdf, artifact_id: "art_r1_empty_doc", filename: "unfinished.pdf", title: "Unfinished PDF", size_bytes: 0, ready: false };
    const emptyImage = { ...pdf, artifact_id: "art_r1_empty_image", type: "image", format: "png", filename: "unfinished.png", title: "Unfinished image", mime_type: "image/png", size_bytes: 0, ready: false };
    await route.fulfill({ response, json: { ...body, artifacts: [...body.artifacts, emptyDocument, emptyImage], total: body.artifacts.length + 2 } });
  });
  await openHistory(page, session.metadata.title, "R1-PDF-927");
  await page.getByRole("button", { name: /^(Artifacts|产物).*3/ }).click();
  await expect(page.getByText(/Empty or unfinished file|空文件|未完成的文件/)).toHaveCount(2);
  await expect(page.getByRole("button", { name: /^(Download|下载)$/ }).filter({ visible: true })).toHaveCount(3);
  expect(emptyDownloadReads).toBe(0);
  await page.route(`**/api/v1/assistant/artifacts/${pdf.artifact_id}/download`, async route => {
    await route.fulfill({ status: 503, contentType: "application/json", body: JSON.stringify({ detail: "TEMPORARY_UNAVAILABLE" }) });
  });
  await page.locator('button[aria-label="下载"]:not(:disabled), button[aria-label="Download"]:not(:disabled)').first().click();
  await expect(page.getByText(/Download unavailable|暂时无法下载/)).toBeVisible();
  await expect(page.getByText(/The file service is temporarily unavailable|文件服务暂时不可用/)).toBeVisible();
  await page.unroute(`**/api/v1/assistant/artifacts/${pdf.artifact_id}/download`);

  mode = "unavailable";
  await page.reload();
  await page.getByRole("button", { name: /^(Artifacts|产物)$/ }).click();
  await expect(page.getByRole("alert")).toContainText(/Files could not be checked|无法核对文件/);
  expect(executionPosts).toEqual([]);

  mode = "real";
  await page.reload();
  await page.getByRole("button", { name: /^(Artifacts|产物).*1/ }).click();
  await expect(page.getByText("R1-PDF-927.pdf", { exact: true }).first()).toBeVisible();
  expect(executionPosts).toEqual([]);
});

test("controlled second output failure preserves the first real PDF after reload", async ({ page, request }) => {
  const headers = await buildAuthHeaders(request);
  const sessions = await request.get(`${process.env.E2E_API_URL}/api/v1/assistant/sessions?limit=200`, { headers });
  const session = (await sessions.json()).sessions.find((s: { metadata?: { title?: string } }) => s.metadata?.title?.startsWith("R1-PDF-927"));
  expect(session).toBeTruthy();
  const historyResponse = await request.get(`${process.env.E2E_API_URL}/api/v1/assistant/sessions/${session.session_id}/history`, { headers });
  const originalHistory = await historyResponse.json();
  const assistant = [...originalHistory.messages].reverse().find((m: { role: string; metadata?: { runtime_run_id?: string } }) => m.role === "assistant" && m.metadata?.runtime_run_id);
  expect(assistant).toBeTruthy();
  const runId = assistant.metadata.runtime_run_id;
  const expectedBody = "PDF 已生成完毕";
  expect(assistant.content).toContain(expectedBody);
  const posts: string[] = [];
  page.on("request", r => {
    if (r.method() === "POST" && /\/api\/v2\/agent\/threads/.test(r.url())) posts.push(r.url());
  });
  await page.route(`**/api/v1/assistant/sessions/${session.session_id}/history*`, async route => {
    const history = structuredClone(originalHistory);
    const last = [...history.messages].reverse().find((m: { role: string; metadata?: { runtime_run_id?: string } }) => m.role === "assistant" && m.metadata?.runtime_run_id === runId);
    last.metadata.tool_calls = [...(last.metadata.tool_calls || []), { id: "controlled-second-output", name: "second_output", arguments: {}, status: "error" }];
    last.metadata.tool_results = [...(last.metadata.tool_results || []), { tool_call_id: "controlled-second-output", name: "second_output", error: "second output failed" }];
    last.metadata.process_summary = { ...(last.metadata.process_summary || {}), run_id: runId, status: "failed", steps: [], tools: [] };
    await route.fulfill({ status: 200, contentType: "application/json", body: JSON.stringify(history) });
  });
  await page.route(`**/api/v1/assistant/runs/${runId}`, async route => {
    const response = await route.fetch();
    const body = await response.json();
    await route.fulfill({ response, json: { ...body, run: { ...body.run, status: "failed", checkpoint: { phase: "failed" } } } });
  });
  await openHistory(page, session.metadata.title, "R1-PDF-927");
  await expect(page.getByRole("log")).toContainText(expectedBody);
  await expect(page.getByRole("status").filter({ hasText: /Run failed|运行失败/ })).toBeVisible();
  await page.getByRole("button", { name: /^(Artifacts|产物).*1/ }).click();
  await expect(page.getByText("R1-PDF-927.pdf", { exact: true }).first()).toBeVisible();
  await page.reload();
  await expect(page.getByRole("log")).toContainText(expectedBody);
  await expect(page.getByRole("status").filter({ hasText: /Run failed|运行失败/ })).toBeVisible();
  expect(posts).toEqual([]);
});
