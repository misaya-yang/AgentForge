import fs from "node:fs/promises";
import path from "node:path";

import { expect, test, type Page } from "@playwright/test";

const liveEnabled =
  process.env.E2E_LIVE_AGENT_STUDIO === "1" &&
  process.env.E2E_DOCKER_LIVE_STACK === "1";
const liveDisabledEnabled = process.env.E2E_LIVE_AGENT_STUDIO_DISABLED === "1";

async function deleteCreatedAgent(page: Page, agentId: string): Promise<void> {
  const responseStatus = await page.evaluate(async (id) => {
    const rawAuth = localStorage.getItem("agent-gateway-auth");
    const token = rawAuth
      ? (JSON.parse(rawAuth) as { state?: { token?: string } }).state?.token
      : undefined;
    if (!token) return 0;
    const response = await fetch(`/api/v1/agents/${id}`, {
      method: "DELETE",
      headers: { Authorization: `Bearer ${token}` },
    });
    return response.status;
  }, agentId);
  expect(responseStatus).toBe(200);
}

async function readPreviewFacts(page: Page, agentId: string, sessionId: string, runId: string) {
  return page.evaluate(async ({ agentId: id, sessionId: sid, runId: rid }) => {
    const rawAuth = localStorage.getItem("agent-gateway-auth");
    const token = rawAuth
      ? (JSON.parse(rawAuth) as { state?: { token?: string } }).state?.token
      : undefined;
    if (!token) throw new Error("Missing authenticated live browser token");
    const headers = { Authorization: `Bearer ${token}` };
    const [pinResponse, runResponse, historyResponse] = await Promise.all([
      fetch(`/api/v1/agents/${id}/preview/sessions/${sid}`, { headers }),
      fetch(`/api/v1/assistant/runs/${rid}`, { headers }),
      fetch(`/api/v1/assistant/sessions/${sid}/history`, { headers }),
    ]);
    if (![pinResponse, runResponse, historyResponse].every((response) => response.ok)) {
      throw new Error("Preview pin, run, or history read failed");
    }
    const pin = await pinResponse.json() as { session_id: string; draft_revision: number | null; agent_version_id: string | null; channel: string };
    const runPayload = await runResponse.json() as { run: { run_id: string; session_id: string; status: string } };
    const history = await historyResponse.json() as { messages: Array<{ role: string; metadata?: { runtime_run_id?: string } }> };
    return {
      pin,
      run: runPayload.run,
      historyRunIds: history.messages.map((message) => message.metadata?.runtime_run_id).filter(Boolean),
    };
  }, { agentId, sessionId, runId });
}

test.describe("Agent Studio live stack", () => {
  test.skip(
    !liveEnabled,
    "Use playwright.live.config.ts with E2E_LIVE_AGENT_STUDIO=1 against the authenticated Docker stack.",
  );

  test("creates, saves, previews, renders responsively, and cleans up a real Draft", async ({ page }) => {
    page.setDefaultTimeout(15_000);
    await page.addInitScript(() => localStorage.setItem("i18nextLng", "en-US"));
    const evidenceDir = path.resolve(process.cwd(), "../reports/agent-studio/as-05");
    await fs.mkdir(evidenceDir, { recursive: true });
    const uniqueName = `AS05 Live ${Date.now()}`;
    const savedName = `${uniqueName} updated`;
    const savedDescription = "Persists Agent metadata and Draft spec in one live transaction.";
    let agentId: string | null = null;

    try {
      await page.setViewportSize({ width: 1440, height: 900 });
      await page.goto("/agents", { waitUntil: "domcontentloaded" });
      await expect(page.getByTestId("agents-page")).toBeVisible();
      await page.getByRole("button", { name: "Create agent" }).click();
      await page.getByLabel("Name").fill(uniqueName);
      await page.getByLabel("Description").fill("Validates the real AS-05 local Agent Studio runtime path.");
      await page.getByRole("button", { name: "Continue" }).click();
      await expect(page.getByRole("region", { name: "Behavior" })).toBeVisible();
      await page.getByRole("button", { name: "Continue" }).click();
      await expect(page.getByRole("heading", { name: "Start" })).toBeVisible();
      await page.getByRole("button", { name: "Create agent" }).click();
      await expect(page).toHaveURL(/\/agents\/[0-9a-f-]{36}$/);
      agentId = new URL(page.url()).pathname.split("/").pop() || null;
      expect(agentId).toBeTruthy();

      await expect(page.getByRole("heading", { name: uniqueName })).toBeVisible();
      await expect(page.getByText(/Draft · revision \d+/)).toBeVisible();
      await page.getByLabel("Name").fill(savedName);
      await page.getByLabel("Description").fill(savedDescription);
      await page.getByLabel("Welcome message").fill("Welcome to the AS-05 live Preview.");
      const saveDraftButton = page.locator(".agent-studio-actions").getByRole("button", { name: "Save draft" });
      await expect(saveDraftButton).toBeEnabled();
      await saveDraftButton.click();
      await expect(page.locator(".agent-save-state")).toHaveText("Saved");
      await expect(saveDraftButton).toBeDisabled();
      await page.reload({ waitUntil: "domcontentloaded" });
      await expect(page.getByRole("heading", { name: savedName })).toBeVisible();
      await expect(page.getByLabel("Description")).toHaveValue(savedDescription);
      await expect(page.getByLabel("Welcome message")).toHaveValue("Welcome to the AS-05 live Preview.");
      await page.locator(".agent-preview-header").getByRole("button", { name: "New session" }).click();
      const startedSession = page.getByText(/New isolated session · Draft r\d+/);
      await expect(startedSession).toBeVisible();
      await page.getByLabel("Message this agent").fill("Explain why Transformer training can be parallelized in three sentences.");
      await page.getByLabel("Send Preview message").click();
      const assistantResponse = page.locator(".agent-preview-message-assistant p").last();
      await expect(assistantResponse).toBeVisible();
      await expect(assistantResponse).not.toHaveText(/Generating|completed without a text response/);
      await expect(assistantResponse).not.toHaveText("Agent E2E stub response");
      await expect(page.locator(".agent-preview-error")).toHaveCount(0);
      await expect(page.getByTestId("agent-preview-panel")).toHaveAttribute(
        "aria-busy",
        "false",
        { timeout: 90_000 },
      );
      await page.screenshot({
        path: path.join(evidenceDir, "studio-desktop.png"),
        fullPage: true,
      });

      await page.setViewportSize({ width: 1024, height: 768 });
      await expect(page.locator(".agent-config-canvas")).toBeVisible();
      await expect(page.getByTestId("agent-preview-panel")).toBeVisible();
      await page.screenshot({
        path: path.join(evidenceDir, "studio-tablet.png"),
        fullPage: true,
      });

      await page.setViewportSize({ width: 390, height: 844 });
      await page.getByRole("tab", { name: "Preview" }).click();
      await expect(page.getByTestId("agent-preview-panel")).toBeVisible();
      await expect(page.locator(".agent-config-canvas")).toBeHidden();
      await page.screenshot({
        path: path.join(evidenceDir, "studio-mobile.png"),
        fullPage: true,
      });
    } finally {
      if (agentId) await deleteCreatedAgent(page, agentId);
    }
  });

  test("keeps an r1 Preview run pinned while saving r2 and restoring after refresh", async ({ page }) => {
    test.setTimeout(180_000);
    page.setDefaultTimeout(20_000);
    await page.addInitScript(() => localStorage.setItem("i18nextLng", "en-US"));
    let agentId: string | null = null;
    try {
      await page.goto("/agents", { waitUntil: "domcontentloaded" });
      await page.getByRole("button", { name: "Create agent" }).click();
      await page.getByLabel("Name").fill(`AS05 Pin ${Date.now()}`);
      await page.getByLabel("Description").fill("Live Preview revision pin acceptance.");
      await page.getByRole("button", { name: "Continue" }).click();
      await page.getByRole("button", { name: "Continue" }).click();
      await page.getByRole("button", { name: "Create agent" }).click();
      await expect(page).toHaveURL(/\/agents\/[0-9a-f-]{36}$/);
      agentId = new URL(page.url()).pathname.split("/").pop() || null;
      expect(agentId).toBeTruthy();

      const sessionResponse = page.waitForResponse((response) =>
        response.request().method() === "POST"
        && new URL(response.url()).pathname === `/api/v1/agents/${agentId}/preview/sessions`,
      );
      await page.locator(".agent-preview-header").getByRole("button", { name: "New session" }).click();
      const r1 = await (await sessionResponse).json() as { session_id: string; draft_revision: number };
      const prompt = "Write 120 numbered paragraphs, each explaining one distinct reason Transformer training batches can run in parallel. Continue through paragraph 120.";
      const streamResponse = page.waitForResponse((response) =>
        response.request().method() === "POST"
        && new URL(response.url()).pathname === `/api/v1/agents/${agentId}/preview/chat/stream`,
      );
      await page.getByLabel("Message this agent").fill(prompt);
      await page.getByLabel("Send Preview message").click();
      const runId = await (await streamResponse).headerValue("x-run-id");
      expect(runId).toMatch(/^[0-9a-f-]{36}$/);
      const answer = page.locator(".agent-preview-message-assistant p").last();
      await expect(answer).not.toHaveText("Generating…", { timeout: 60_000 });
      await expect(page.getByTestId("agent-preview-panel")).toHaveAttribute("aria-busy", "true");

      await page.getByLabel("Description").fill("Saved r2 while the r1 answer was streaming.");
      await page.locator(".agent-studio-actions").getByRole("button", { name: "Save draft" }).click();
      await expect(page.locator(".agent-save-state")).toHaveText("Saved");
      const r1Facts = await readPreviewFacts(page, agentId!, r1.session_id, runId!);
      expect(r1Facts.pin).toMatchObject({ session_id: r1.session_id, draft_revision: r1.draft_revision, agent_version_id: null, channel: "preview" });
      expect(r1Facts.run).toMatchObject({ run_id: runId, session_id: r1.session_id });
      expect(r1Facts.historyRunIds).toContain(runId);

      await page.reload({ waitUntil: "domcontentloaded" });
      await expect(page.getByText(prompt)).toBeVisible();
      await expect(page.getByText(`Earlier draft r${r1.draft_revision} · view only`)).toBeVisible();
      const restoredFacts = await readPreviewFacts(page, agentId!, r1.session_id, runId!);
      expect(restoredFacts.pin.draft_revision).toBe(r1.draft_revision);
      expect(restoredFacts.historyRunIds).toContain(runId);
      await expect(page.locator(".agent-preview-header").getByRole("button", { name: "Preview current draft" })).toBeEnabled({ timeout: 90_000 });

      const nextSessionResponse = page.waitForResponse((response) =>
        response.request().method() === "POST"
        && new URL(response.url()).pathname === `/api/v1/agents/${agentId}/preview/sessions`,
      );
      await page.locator(".agent-preview-header").getByRole("button", { name: "Preview current draft" }).click();
      const r2 = await (await nextSessionResponse).json() as { session_id: string; draft_revision: number };
      expect(r2.session_id).not.toBe(r1.session_id);
      expect(r2.draft_revision).toBe(r1.draft_revision + 1);
    } finally {
      if (agentId && !page.isClosed()) await deleteCreatedAgent(page, agentId);
    }
  });
});

test.describe("Agent Studio live rollback flag", () => {
  test.skip(
    !liveDisabledEnabled,
    "Set E2E_LIVE_AGENT_STUDIO_DISABLED=1 after starting the frontend with the flag disabled.",
  );

  test("removes Agent surfaces while preserving the existing Assistant", async ({ page }) => {
    await page.goto("/agents", { waitUntil: "domcontentloaded" });
    await expect(page.getByRole("link", { name: "Agents" })).toHaveCount(0);
    await expect(page.getByTestId("agents-page")).toHaveCount(0);

    await page.goto("/assistant", { waitUntil: "domcontentloaded" });
    await expect(page.locator("#assistant-chat-composer")).toBeVisible();
    await expect(page).toHaveURL(/\/assistant$/);
  });
});
