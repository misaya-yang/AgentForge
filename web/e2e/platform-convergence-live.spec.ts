/**
 * Platform convergence release acceptance against the already-running stack.
 *
 * This file intentionally installs no Playwright routes. Every request reaches
 * the live Gateway, Runtime, capability worker, provider, and database.
 * Merely listing or skipping these tests is not release evidence; the release
 * run must opt in with RUN_PLATFORM_CONVERGENCE_LIVE=1.
 */

import { expect, test, type Page, type Response } from "@playwright/test";

import { ensureAuthenticatedPage } from "./support/helpers";

const liveEnabled = process.env.RUN_PLATFORM_CONVERGENCE_LIVE === "1";
if (!liveEnabled) {
  throw new Error(
    "Set RUN_PLATFORM_CONVERGENCE_LIVE=1 before collecting or running paid-provider release tests.",
  );
}
const requestedQwenModel = process.env.PLATFORM_CONVERGENCE_QWEN_MODEL?.trim();
const composerSelector = "#assistant-chat-composer";

interface LiveModel {
  id: string;
  name: string;
  provider: string;
}

interface LiveUser {
  roles?: string[];
  permissions?: string[];
}

interface ArchitectureStatusPayload {
  schema_version?: string;
  topology_revision?: string;
  mode?: string;
  groups?: unknown[];
}

interface BrowserGetResult<T> {
  status: number;
  ok: boolean;
  body: T | null;
}

async function authenticatedGet<T>(page: Page, path: string): Promise<BrowserGetResult<T>> {
  return page.evaluate(async (requestPath) => {
    const rawAuth = localStorage.getItem("agent-gateway-auth");
    const token = rawAuth
      ? (JSON.parse(rawAuth) as { state?: { token?: string } }).state?.token
      : undefined;
    if (!token) throw new Error("Missing authenticated live browser token");

    const response = await fetch(requestPath, {
      headers: {
        Accept: "application/json",
        Authorization: `Bearer ${token}`,
      },
    });
    const text = await response.text();
    let body: unknown = null;
    if (text) {
      try {
        body = JSON.parse(text);
      } catch {
        body = text;
      }
    }
    return { status: response.status, ok: response.ok, body };
  }, path) as Promise<BrowserGetResult<T>>;
}

async function openAssistantWithQwen(page: Page): Promise<LiveModel> {
  await page.addInitScript(() => localStorage.setItem("i18nextLng", "en-US"));
  await ensureAuthenticatedPage(page, "/assistant");
  await expect(page.locator(composerSelector)).toBeVisible();

  const modelsResponse = await authenticatedGet<{ models?: LiveModel[] }>(
    page,
    "/api/v1/assistant/models",
  );
  expect(modelsResponse.status, "live Assistant model catalog must be readable").toBe(200);
  const models = modelsResponse.body?.models ?? [];
  const qwen = requestedQwenModel
    ? models.find((model) => model.id === requestedQwenModel)
    : models.find((model) => model.id === "qwen3.7-plus") ??
      models.find((model) =>
        model.provider.toLowerCase().includes("dashscope") ||
        /qwen/i.test(`${model.id} ${model.name}`),
      );
  expect(
    qwen,
    requestedQwenModel
      ? `configured Qwen model ${requestedQwenModel} is not available to the E2E user`
      : "no configured Qwen/DashScope model is available to the E2E user",
  ).toBeTruthy();

  const model = qwen as LiveModel;
  await expect.poll(async () => {
    if (await page.getByRole("button", { name: "Select model", exact: true }).isVisible()) {
      return true;
    }
    for (const candidateModel of models) {
      if (await page.getByRole("button", {
        name: candidateModel.name,
        exact: true,
      }).first().isVisible().catch(() => false)) {
        return true;
      }
    }
    return false;
  }, { timeout: 30_000 }).toBe(true);
  let modelTrigger = page.getByRole("button", { name: "Select model", exact: true });
  for (const candidateModel of models) {
    const candidate = page.getByRole("button", {
      name: candidateModel.name,
      exact: true,
    }).first();
    if (await candidate.isVisible().catch(() => false)) {
      modelTrigger = candidate;
      break;
    }
  }
  await expect(modelTrigger).toBeVisible();
  if (!(await modelTrigger.innerText()).includes(model.name)) {
    await modelTrigger.click();
    await page.getByRole("menuitem").filter({ hasText: model.name }).first().click();
    modelTrigger = page.getByRole("button", { name: model.name, exact: true }).first();
  }
  await expect(modelTrigger).toContainText(model.name);
  return model;
}

function isRuntimeTurnResponse(response: Response): boolean {
  const pathname = new URL(response.url()).pathname;
  return response.request().method() === "POST" &&
    /^\/api\/v2\/agent\/threads\/[^/]+\/turns$/.test(pathname);
}

function isRuntimeEventStream(response: Response): boolean {
  const pathname = new URL(response.url()).pathname;
  return response.request().method() === "GET" &&
    /^\/api\/v2\/agent\/threads\/[^/]+\/events$/.test(pathname);
}

async function beginQwenTurn(page: Page, message: string, modelId: string): Promise<void> {
  const turnResponsePromise = page.waitForResponse(isRuntimeTurnResponse, { timeout: 30_000 });
  const eventsResponsePromise = page.waitForResponse(isRuntimeEventStream, { timeout: 30_000 });
  const composer = page.locator(composerSelector);
  await expect(composer).toBeEnabled();
  await composer.fill(message);
  await composer.press("Enter");

  const turnResponse = await turnResponsePromise;
  expect(turnResponse.ok(), "Runtime V2 turn start must succeed").toBeTruthy();
  expect(turnResponse.request().postDataJSON()).toMatchObject({ model_id: modelId });

  const eventsResponse = await eventsResponsePromise;
  expect(eventsResponse.ok(), "Runtime V2 event cursor must open").toBeTruthy();
  expect(eventsResponse.headers()["content-type"] ?? "").toContain("text/event-stream");
}

function isCancellationResponse(response: Response): boolean {
  if (response.request().method() !== "POST") return false;
  const pathname = new URL(response.url()).pathname;
  return /^\/api\/v1\/assistant\/tasks\/[^/]+\/cancel$/.test(pathname) ||
    /^\/api\/v2\/agent\/threads\/[^/]+\/turns\/[^/]+:interrupt$/.test(pathname);
}

function waitForAcceptedCancellation(page: Page): Promise<Response> {
  return new Promise((resolve, reject) => {
    const timer = setTimeout(() => {
      page.off("response", onResponse);
      reject(new Error("No accepted task cancel or Runtime V2 interrupt was observed"));
    }, 30_000);

    const finish = (response: Response) => {
      clearTimeout(timer);
      page.off("response", onResponse);
      resolve(response);
    };

    const onResponse = (response: Response) => {
      if (!isCancellationResponse(response) || !response.ok()) return;
      const pathname = new URL(response.url()).pathname;
      if (pathname.endsWith(":interrupt")) {
        finish(response);
        return;
      }
      void response.json().then((payload: { cancelled?: boolean }) => {
        if (payload.cancelled === true) finish(response);
      }).catch(() => undefined);
    };

    page.on("response", onResponse);
  });
}

function isRuntimeApprovalDecision(response: Response): boolean {
  const pathname = new URL(response.url()).pathname;
  return response.request().method() === "POST" &&
    /^\/api\/v2\/agent\/threads\/[^/]+\/approvals\/[^/]+\/decision$/.test(pathname);
}

async function decideApproval(page: Page, approved: boolean): Promise<void> {
  const dialog = page.getByRole("alertdialog").filter({ hasText: "Approval required" }).last();
  await expect(dialog).toBeVisible();
  const responsePromise = page.waitForResponse(isRuntimeApprovalDecision, { timeout: 30_000 });
  await dialog.getByRole("button", { name: approved ? "Approve" : "Reject" }).click();
  const response = await responsePromise;
  expect(response.ok(), "Runtime V2 approval decision must succeed").toBeTruthy();
  expect(response.request().postDataJSON()).toMatchObject({ approved });
  await expect(dialog).toBeHidden();
}

test.describe("Platform convergence live acceptance", () => {
  test("cancels an active Qwen Runtime V2 stream and restores its terminal state", async ({
    page,
  }) => {
    test.setTimeout(4 * 60_000);
    const qwen = await openAssistantWithQwen(page);
    const marker = `platform-live-cancel-${Date.now()}`;
    const prompt =
      `${marker}: answer directly without tools. Produce a numbered list from 1 to 1000, ` +
      "with one complete sentence per item, so the response remains streaming until I stop it.";

    await beginQwenTurn(page, prompt, qwen.id);
    const stop = page.getByRole("button", { name: "Stop generating" });
    await expect(stop).toBeVisible();
    await expect(page.getByRole("button", { name: "Thinking" }).last()).toBeVisible();

    const cancellationPromise = waitForAcceptedCancellation(page);
    await stop.click();
    const cancellation = await cancellationPromise;
    expect(cancellation.ok(), "server-side cancel/interrupt must be accepted").toBeTruthy();
    await expect(stop).toBeHidden({ timeout: 45_000 });

    await page.reload({ waitUntil: "domcontentloaded" });
    const log = page.getByRole("log", { name: "Assistant conversation log" });
    await expect(log).toContainText(marker, { timeout: 30_000 });
    await page.getByRole("button", { name: "Activity" }).last().click();
    await expect(page.getByText(/cancelled\s*·/i).last()).toBeVisible();
  });

  test("restores a pending tool approval, rejects it, then approves a fresh tool call", async ({
    page,
  }) => {
    test.setTimeout(7 * 60_000);
    const qwen = await openAssistantWithQwen(page);
    const rejectMarker = `platform-live-reject-${Date.now()}`;
    const approveMarker = `platform-live-approve-${Date.now()}`;

    await beginQwenTurn(
      page,
      `Please verify the Python workspace by using execute_python_code to run ` +
        `print("${rejectMarker}"). I will review the execution when the approval dialog appears.`,
      qwen.id,
    );
    await expect(
      page.getByRole("alertdialog").filter({ hasText: "Approval required" }).last(),
    ).toBeVisible({ timeout: 90_000 });

    await page.reload({ waitUntil: "domcontentloaded" });
    const log = page.getByRole("log", { name: "Assistant conversation log" });
    await expect(log).toContainText(rejectMarker, { timeout: 30_000 });
    await expect(
      page.getByRole("alertdialog").filter({ hasText: "Approval required" }).last(),
    ).toBeVisible({ timeout: 30_000 });
    await decideApproval(page, false);
    await expect(page.getByRole("button", { name: "Stop generating" })).toBeHidden({
      timeout: 60_000,
    });
    await expect(page.locator(composerSelector)).toBeEnabled({ timeout: 60_000 });

    await beginQwenTurn(
      page,
      `Please verify the Python workspace again by using execute_python_code to run ` +
        `print("${approveMarker}"). After the approved execution succeeds, reply "approval complete".`,
      qwen.id,
    );
    await expect(
      page.getByRole("alertdialog").filter({ hasText: "Approval required" }).last(),
    ).toBeVisible({ timeout: 90_000 });
    await decideApproval(page, true);
    await expect(page.getByRole("button", { name: "Stop generating" })).toBeHidden({
      timeout: 120_000,
    });

    await page.reload({ waitUntil: "domcontentloaded" });
    await expect(log).toContainText(rejectMarker, { timeout: 30_000 });
    await expect(log).toContainText(approveMarker);
    await expect(page.getByRole("alertdialog")).toHaveCount(0);
    await expect(page.locator(composerSelector)).toBeEnabled();
  });

  test("shows live architecture status to platform admins and enforces 403 otherwise", async ({
    page,
  }) => {
    test.setTimeout(90_000);
    await page.addInitScript(() => localStorage.setItem("i18nextLng", "en-US"));
    await ensureAuthenticatedPage(page, "/services");

    const me = await authenticatedGet<LiveUser>(page, "/api/v1/auth/me");
    expect(me.status).toBe(200);
    const subjects = new Set(
      [...(me.body?.roles ?? []), ...(me.body?.permissions ?? [])].map((value) =>
        value.trim().toLowerCase(),
      ),
    );
    const isPlatformAdmin = ["platform_admin", "superadmin", "super_admin"].some((role) =>
      subjects.has(role),
    );
    const status = await authenticatedGet<ArchitectureStatusPayload>(
      page,
      "/api/v1/admin/architecture-status",
    );

    if (isPlatformAdmin) {
      expect(status.status).toBe(200);
      expect(status.body).toMatchObject({
        schema_version: "ai-gateway/architecture-status/v1",
      });
      expect(status.body?.topology_revision).toBeTruthy();
      expect(status.body?.mode).toMatch(/^(compact|full|scale)$/);
      expect(status.body?.groups?.length).toBeGreaterThan(0);
      await expect(page.getByRole("heading", { name: "Platform architecture" })).toBeVisible();
      await expect(page.getByText(/Mode (compact|full|scale) · topology /)).toBeVisible();
      return;
    }

    expect(status.status, "non-platform-admin API access must fail closed").toBe(403);
    await expect(page.getByRole("heading", { name: "Platform architecture" })).toHaveCount(0);
    const servicesHeading = page.getByRole("heading", { name: /Services|服务/ }).first();
    if (await servicesHeading.isVisible().catch(() => false)) {
      await expect(
        page.getByText("Platform architecture status is available to platform administrators."),
      ).toBeVisible();
    }
  });
});
