import { fileURLToPath } from "node:url";
import { expect, test, type APIRequestContext } from "@playwright/test";
import { buildAuthHeaders, loginThroughApi } from "./support/helpers";

function sessionButtonName(title: string): RegExp {
  return new RegExp(`^${title.replace(/[.*+?^${}()|[\]\\]/g, "\\$&")}(?:\\s·|$)`);
}

function historyToggle(page: import("@playwright/test").Page, state: "show" | "hide") {
  const labels = state === "show"
    ? ["Show history", "显示历史"]
    : ["Hide history", "隐藏历史"];
  return page.locator(labels.map((label) => `button[aria-label="${label}"]`).join(", "));
}

async function seedAssistantSession(request: APIRequestContext) {
  const headers = await buildAuthHeaders(request);
  const title = `assistant-history-${Date.now()}`;
  const createResponse = await request.post(`${process.env.E2E_API_URL}/api/v1/sessions`, {
    headers,
    data: {
      service_id: "__builtin_assistant__",
      metadata: { title },
    },
  });
  expect(createResponse.ok()).toBeTruthy();
  const { session_id: sessionId } = (await createResponse.json()) as { session_id: string };

  await request.post(`${process.env.E2E_API_URL}/api/v1/sessions/${sessionId}/messages`, {
    headers,
    data: { role: "user", content: "History seed question" },
  });
  await request.post(`${process.env.E2E_API_URL}/api/v1/sessions/${sessionId}/messages`, {
    headers,
    data: { role: "assistant", content: "History seed answer" },
  });

  return { sessionId, title };
}

async function installApiSession(page: import("@playwright/test").Page, request: APIRequestContext) {
  const { token, user } = await loginThroughApi(request);
  await page.addInitScript(
    ({ authPayload }) => {
      localStorage.setItem("agent-gateway-auth", JSON.stringify(authPayload));
      sessionStorage.removeItem("agent-gateway-auth");
    },
    {
      authPayload: {
        state: {
          token,
          user,
          isAuthenticated: true,
          forcePasswordChange: false,
          rememberMe: true,
        },
        version: 0,
      },
    }
  );
  await page.route(/\/api\/v1\//, async (route) => {
    const requestUrl = new URL(route.request().url());
    const response = await request.fetch(`${process.env.E2E_API_URL}${requestUrl.pathname}${requestUrl.search}`, {
      headers: {
        ...route.request().headers(),
        authorization: `Bearer ${token}`,
      },
      method: route.request().method(),
      data: route.request().postDataBuffer(),
    });
    await route.fulfill({ response });
  });
}

test("assistant restores seeded history and keeps sidebar toggle functional", async ({ page, request }) => {
  const { title } = await seedAssistantSession(request);

  await installApiSession(page, request);
  await page.goto("/assistant");
  const sessionButton = page.getByRole("button", { name: sessionButtonName(title) });
  if (!(await sessionButton.isVisible())) {
    await historyToggle(page, "show").click();
  }
  await sessionButton.click();

  await expect(page.getByText("History seed question")).toBeVisible();
  await expect(page.getByText("History seed answer")).toBeVisible();

  const toggle = historyToggle(page, "hide");
  await toggle.click();
  await expect(historyToggle(page, "show")).toBeVisible();

  await page.reload();
  await expect(page.getByText("History seed question")).toBeVisible();
  await expect(historyToggle(page, "show")).toBeVisible();
});

test("assistant restores seeded history in the mobile history sheet", async ({ page, request }) => {
  await page.setViewportSize({ width: 390, height: 844 });
  const { title } = await seedAssistantSession(request);

  await installApiSession(page, request);
  await page.goto("/assistant");
  await historyToggle(page, "show").click();
  const historySheet = page.getByRole("dialog", { name: /history|历史/i });
  await expect(historySheet).toBeVisible();
  const bounds = await historySheet.boundingBox();
  expect(bounds).not.toBeNull();
  expect(bounds!.width).toBeLessThanOrEqual(390);

  await page.getByRole("button", { name: sessionButtonName(title) }).click();
  await expect(page.getByText("History seed question")).toBeVisible();
  await expect(page.getByText("History seed answer")).toBeVisible();
});

test("assistant outcome history survives reload and reopen without starting a run", async ({ page, request }) => {
  const headers = await buildAuthHeaders(request);
  const title = `as02-outcome-history-${Date.now()}`;
  const runId = "11111111-2222-4333-8444-555555555555";
  const created = await request.post(`${process.env.E2E_API_URL}/api/v1/sessions`, {
    headers,
    data: { service_id: "__builtin_assistant__", metadata: { title } },
  });
  expect(created.ok()).toBeTruthy();
  const { session_id: sessionId } = await created.json() as { session_id: string };
  for (const [prompt, content, status, uncertain] of [
    ["AS02 failed before text", "", "failed", false],
    ["AS02 failed with text", "AS02_PARTIAL_KEPT", "failed", false],
    ["AS02 cancelled", "", "cancelled", false],
    ["AS02 empty success", "", "succeeded", false],
    ["AS02 normal success", "AS02_SUCCESS_KEPT", "succeeded", false],
    ["AS02 uncertain", "", "failed", true],
  ] as const) {
    const user = await request.post(`${process.env.E2E_API_URL}/api/v1/sessions/${sessionId}/messages`, {
      headers, data: { role: "user", content: prompt },
    });
    expect(user.ok()).toBeTruthy();
    const assistant = await request.post(`${process.env.E2E_API_URL}/api/v1/sessions/${sessionId}/messages`, {
      headers,
      data: {
        role: "assistant", content,
        metadata: {
          process_summary: {
            run_id: runId, status, outcome_uncertain: uncertain,
            collapsed: true, steps: [], tools: [],
          },
          raw_error: "INTERNAL_ERROR_NOT_FOR_UI",
        },
      },
    });
    expect(assistant.ok()).toBeTruthy();
  }

  await installApiSession(page, request);
  let turnStarts = 0;
  page.on("request", (outgoing) => {
    if (outgoing.method() === "POST" && /\/api\/v2\/agent\/threads(?:\/[^/]+\/turns)?$/.test(outgoing.url())) {
      turnStarts += 1;
    }
  });
  await page.goto("/assistant");
  const sessionButton = page.getByRole("button", { name: sessionButtonName(title) });
  if (!(await sessionButton.isVisible())) {
    await historyToggle(page, "show").click();
  }
  await sessionButton.click();

  const verify = async () => {
    await expect(page.getByRole("status").filter({ hasText: /Run failed|运行失败/ })).toHaveCount(2);
    await expect(page.getByText("AS02_PARTIAL_KEPT")).toBeVisible();
    await expect(page.getByRole("status").filter({ hasText: /Cancelled|已取消/ })).toHaveCount(1);
    await expect(page.getByRole("status").filter({ hasText: /Completed without a text response|已完成，但无正文/ })).toHaveCount(1);
    await expect(page.getByText("AS02_SUCCESS_KEPT")).toBeVisible();
    await expect(page.getByRole("status").filter({ hasText: /Result unknown|结果未知/ })).toHaveCount(1);
    await expect(page.getByText(runId)).toHaveCount(3);
    await expect(page.getByText("INTERNAL_ERROR_NOT_FOR_UI")).toHaveCount(0);
  };
  await verify();
  await page.context().grantPermissions(["clipboard-read", "clipboard-write"]);
  await page.getByRole("button", { name: /Copy diagnostic ID|复制诊断 ID/ }).first().click();
  await expect.poll(() => page.evaluate(() => navigator.clipboard.readText())).toBe(runId);
  await page.reload();
  await verify();
  if (await historyToggle(page, "show").isVisible()) {
    await historyToggle(page, "show").click();
  }
  await page.getByRole("button", { name: /^(New chat|新对话)$/ }).click();
  if (await historyToggle(page, "show").isVisible()) {
    await historyToggle(page, "show").click();
  }
  await sessionButton.click();
  await verify();
  expect(turnStarts).toBe(0);
});

test("assistant session rename, search, and confirmed removal stay consistent", async ({ page, request }) => {
  const { sessionId, title } = await seedAssistantSession(request);
  const renamed = `${title}-renamed`;
  const headers = await buildAuthHeaders(request);
  await installApiSession(page, request);
  let turnStarts = 0;
  page.on("request", (outgoing) => {
    if (outgoing.method() === "POST" && /\/api\/v2\/agent\/threads\/[^/]+\/turns$/.test(outgoing.url())) {
      turnStarts += 1;
    }
  });

  try {
    await page.goto("/assistant");
    let session = page.getByRole("button", { name: sessionButtonName(title) });
    if (!(await session.isVisible())) await historyToggle(page, "show").click();
    await session.dblclick();
    const renameInput = page.locator("div.group input");
    await expect(renameInput).toBeVisible();
    await renameInput.fill(renamed);
    await renameInput.press("Enter");
    session = page.getByRole("button", { name: sessionButtonName(renamed) });
    await expect(session).toBeVisible();

    const search = page.getByPlaceholder(/Search conversations|搜索对话/);
    await search.fill(renamed);
    await expect(session).toHaveCount(1);
    await search.fill(`${renamed}-missing`);
    await expect(session).toHaveCount(0);
    await search.fill(renamed);
    await session.click();
    await expect(page.getByText("History seed answer")).toBeVisible();
    await session.hover();
    await page.getByRole("button", { name: new RegExp(`^(Delete|删除): ${renamed}$`) }).click();

    const dialog = page.getByRole("dialog", { name: /Delete this conversation|删除这个会话/ });
    await expect(dialog).toContainText(/share links|分享链接/);
    await dialog.getByRole("button", { name: /^(Cancel|取消)$/ }).click();
    await expect(session).toBeVisible();
    await session.hover();
    await page.getByRole("button", { name: new RegExp(`^(Delete|删除): ${renamed}$`) }).click();
    await dialog.getByRole("button", { name: /^(Delete|删除)$/ }).click();
    await expect(session).toHaveCount(0);
    await expect(page.getByText("History seed answer")).toHaveCount(0);

    const removed = await request.get(`${process.env.E2E_API_URL}/api/v1/sessions/${sessionId}`, { headers });
    expect(removed.status()).toBe(404);
    await page.reload();
    await expect(page.getByRole("button", { name: sessionButtonName(renamed) })).toHaveCount(0);
    expect(turnStarts).toBe(0);
  } finally {
    await request.delete(`${process.env.E2E_API_URL}/api/v1/sessions/${sessionId}`, { headers });
  }
});

test("a preadmission failure keeps its retry draft out of a fresh chat", async ({ page, request }) => {
  test.setTimeout(60_000);
  const headers = await buildAuthHeaders(request);
  const prompt = `R1_PREADMISSION_RETRY_${Date.now()}`;
  await installApiSession(page, request);
  let turnStarts = 0;
  await page.route(/\/api\/v2\/agent\/threads\/[^/]+\/turns$/, async (route) => {
    turnStarts += 1;
    await route.fulfill({ status: 503, contentType: "application/json", body: '{"detail":"controlled preadmission failure"}' });
  });

  let sessionId: string | undefined;
  try {
    await page.goto("/assistant");
    const composer = page.locator("#assistant-chat-composer");
    await composer.fill(prompt);
    const created = page.waitForResponse((response) =>
      response.request().method() === "POST" && new URL(response.url()).pathname === "/api/v1/sessions"
    );
    await page.getByRole("button", { name: /^(Send|发送)$/ }).click();
    const createResponse = await created;
    expect(createResponse.ok()).toBeTruthy();
    sessionId = (await createResponse.json() as { session_id: string }).session_id;
    await expect(composer).toHaveValue(prompt);
    await expect.poll(() => turnStarts).toBe(1);

    if (await historyToggle(page, "show").isVisible()) await historyToggle(page, "show").click();
    await page.getByRole("button", { name: /^(New chat|新对话)$/ }).click();
    await expect(composer).toHaveValue("");
    await page.reload();
    await expect(composer).toHaveValue("");

    if (await historyToggle(page, "show").isVisible()) await historyToggle(page, "show").click();
    await page.getByRole("button", { name: sessionButtonName(prompt.slice(0, 50)) }).click();
    await expect(composer).toHaveValue(prompt);
    expect(turnStarts).toBe(1);
  } finally {
    if (sessionId) {
      await request.delete(`${process.env.E2E_API_URL}/api/v1/sessions/${sessionId}`, { headers });
    }
  }
});

test("an unknown image outcome stays unknown after reload without retrying", async ({ page, request }) => {
  test.setTimeout(60_000);
  const headers = await buildAuthHeaders(request);
  const taskId = "imt_aaaaaaaaaaaaaaaaaaaa";
  await installApiSession(page, request);
  let imageRequests = 0;
  await page.route(/\/api\/v1\/assistant\/generate-image$/, async (route) => {
    imageRequests += 1;
    await route.fulfill({
      status: 502,
      contentType: "application/json",
      body: JSON.stringify({ detail: {
        error_code: "outcome_unknown", task_id: taskId,
        message: "image generation outcome unknown",
      } }),
    });
  });

  let sessionId: string | undefined;
  try {
    await page.goto("/assistant");
    await page.getByRole("button", { name: /Add files, images, or sources|添加文件、图片或资料来源/ }).click();
    await page.getByRole("button", { name: /Generate image|生成图片/ }).click();
    const composer = page.locator("#assistant-chat-composer");
    await composer.fill("R1 synthetic unknown image result");
    const created = page.waitForResponse((response) =>
      response.request().method() === "POST" && new URL(response.url()).pathname === "/api/v1/sessions"
    );
    await page.getByRole("button", { name: /^(Send|发送)$/ }).click();
    sessionId = (await (await created).json() as { session_id: string }).session_id;

    const verify = async () => {
      await expect(page.getByRole("status").filter({ hasText: /Result unknown|结果未知/ })).toBeVisible();
      await expect(page.getByText(taskId)).toBeVisible();
      await expect(page.getByText("image generation outcome unknown")).toHaveCount(0);
    };
    await verify();
    await page.reload();
    await verify();
    expect(imageRequests).toBe(1);
    if (await historyToggle(page, "show").isVisible()) await historyToggle(page, "show").click();
    await page.getByRole("button", { name: /^(New chat|新对话)$/ }).click();
    await expect(composer).toHaveValue("");
  } finally {
    if (sessionId) {
      await request.delete(`${process.env.E2E_API_URL}/api/v1/sessions/${sessionId}`, { headers });
    }
  }
});

test("an image result cannot overwrite a different conversation after navigation", async ({ page, request }) => {
  test.setTimeout(60_000);
  const headers = await buildAuthHeaders(request);
  await installApiSession(page, request);
  let releaseImage: (() => void) | undefined;
  const imageGate = new Promise<void>((resolve) => { releaseImage = resolve; });
  let imageRequests = 0;
  await page.route(/\/api\/v1\/assistant\/generate-image$/, async (route) => {
    imageRequests += 1;
    await imageGate;
    await route.fulfill({
      status: 502,
      contentType: "application/json",
      body: JSON.stringify({ detail: {
        error_code: "outcome_unknown", task_id: "imt_bbbbbbbbbbbbbbbbbbbb",
      } }),
    });
  });

  let sessionId: string | undefined;
  try {
    await page.goto("/assistant");
    await page.getByRole("button", { name: /Add files, images, or sources|添加文件、图片或资料来源/ }).click();
    await page.getByRole("button", { name: /Generate image|生成图片/ }).click();
    const composer = page.locator("#assistant-chat-composer");
    await composer.fill("R1 delayed image result");
    const created = page.waitForResponse((response) =>
      response.request().method() === "POST" && new URL(response.url()).pathname === "/api/v1/sessions"
    );
    await page.getByRole("button", { name: /^(Send|发送)$/ }).click();
    sessionId = (await (await created).json() as { session_id: string }).session_id;
    await expect.poll(() => imageRequests).toBe(1);

    if (await historyToggle(page, "show").isVisible()) await historyToggle(page, "show").click();
    await page.getByRole("button", { name: /^(New chat|新对话)$/ }).click();
    releaseImage();
    await expect(composer).toHaveValue("");
    await expect(page.getByRole("status").filter({ hasText: /Result unknown|结果未知/ })).toHaveCount(0);
    await expect.poll(async () => {
      const response = await request.get(`${process.env.E2E_API_URL}/api/v1/assistant/sessions/${sessionId}/history`, { headers });
      return response.ok() ? ((await response.json()) as { messages: unknown[] }).messages.length : 0;
    }).toBe(2);
    await expect(composer).toHaveValue("");
    expect(imageRequests).toBe(1);
  } finally {
    releaseImage?.();
    if (sessionId) {
      await request.delete(`${process.env.E2E_API_URL}/api/v1/sessions/${sessionId}`, { headers });
    }
  }
});


for (const failure of [
  { status: 409, code: "AI_PLATFORM_AGENT_RUNTIME_CAPABILITY_THREAD_RECREATE_REQUIRED" },
  { status: 422, code: "ATTACHMENT_UNAVAILABLE" },
]) {
  test(`R1 controlled ${failure.status} retains the selected upload and an actionable draft`, async ({ page, request }, info) => {
    test.setTimeout(55_000);
    const headers = await buildAuthHeaders(request);
    await installApiSession(page, request);
    let starts = 0, uploads = 0;
    let sessionId: string | undefined;
    page.on("request", r => { if (r.method() === "POST" && r.url().endsWith("/files/upload")) uploads += 1; });
    await page.route(/\/api\/v2\/agent\/threads\/[^/]+\/turns$/, async route => {
      starts += 1;
      await route.fulfill({ status: failure.status, contentType: "application/json", body: JSON.stringify({ detail: { code: failure.code, internal_error: "INTERNAL_DO_NOT_DISPLAY" } }) });
    });
    try {
      await page.goto("/assistant");
      const fresh = page.getByRole("heading", { name: /How can I help|今天有什么可以帮您/ });
      if (!(await fresh.isVisible())) {
        await page.getByRole("button", { name: /^(Show history|显示历史)$/ }).first().click();
        await page.getByRole("button", { name: /^(New chat|新对话)$/ }).first().click();
      }
      const composer = page.locator("#assistant-chat-composer");
      const prompt = `R1 boundary ${failure.status} draft ${Date.now()}`;
      await composer.fill(prompt);
      await page.locator('input[type="file"]').setInputFiles(fileURLToPath(new URL("./fixtures/r1-attachments/alpha.txt", import.meta.url)));
      const selected = page.getByRole("button", { name: /^(Included in next message|随下条消息发送)$/ });
      await expect(selected).toHaveCount(1);
      const created = page.waitForResponse(r => r.request().method() === "POST" && new URL(r.url()).pathname === "/api/v1/sessions");
      await page.getByRole("button", { name: /^(Send|发送)$/ }).click();
      sessionId = (await (await created).json()).session_id;
      await expect.poll(() => starts).toBe(1);
      await expect(composer).toHaveValue(prompt);
      if (failure.status === 422) {
        await expect(page.getByRole("log")).toContainText(/selected.*attachments?.*unavailable|选中.*附件.*不可用/i);
        await expect(page.getByRole("log")).toContainText(/remove.*upload|移除.*上传/i);
      }
      await expect(selected).toHaveCount(1);
      await expect(page.getByRole("log")).not.toContainText("INTERNAL_DO_NOT_DISPLAY");
      if (failure.status === 409) {
        await page.getByRole("button", { name: /New conversation with draft|保留草稿的新会话|保留草稿.*新/ }).click();
        await expect(composer).toHaveValue(prompt);
        await expect(selected).toHaveCount(1);
      }
      expect(starts).toBe(1); expect(uploads).toBe(1);
      await info.attach("controlled-preadmission-facts", { body: JSON.stringify({ status: failure.status, turnPosts: starts, uploads, acceptedRuns: 0, selectedAttachmentRetained: true, automaticRetry: false }), contentType: "application/json" });
    } finally {
      if (sessionId) await request.delete(`${process.env.E2E_API_URL}/api/v1/sessions/${sessionId}`, { headers });
    }
  });
}
