import fs from "node:fs/promises";
import path from "node:path";

import { expect, test, type Page } from "@playwright/test";

const liveEnabled = process.env.E2E_LIVE_AGENT_PUBLISH === "1";
const expectModelUnavailable = process.env.E2E_EXPECT_AGENT_MODEL_UNAVAILABLE === "1";
const liveEvalDatasetName = "AS06 Live Release Dataset";

interface LiveEvalDataset {
  dataset_id: string;
  name: string;
  version: string;
}

async function ensureLiveEvalDataset(page: Page): Promise<LiveEvalDataset> {
  return page.evaluate(async (datasetName) => {
    const rawAuth = localStorage.getItem("agent-gateway-auth");
    const token = rawAuth
      ? (JSON.parse(rawAuth) as { state?: { token?: string } }).state?.token
      : undefined;
    if (!token) throw new Error("Missing authenticated live browser token");
    const headers = {
      Authorization: `Bearer ${token}`,
      "content-type": "application/json",
    };
    const listResponse = await fetch("/api/v1/eval/datasets?limit=200&offset=0", { headers });
    if (!listResponse.ok) throw new Error(`Eval Dataset list failed: ${listResponse.status}`);
    const listPayload = await listResponse.json() as { datasets?: LiveEvalDataset[] };
    let dataset = (listPayload.datasets ?? []).find((item) => item.name === datasetName);
    if (!dataset) {
      const createResponse = await fetch("/api/v1/eval/datasets", {
        method: "POST",
        headers,
        body: JSON.stringify({
          name: datasetName,
          description: "Stable local AS-06 release-evidence fixture.",
          version: "as06-live-v1",
          schema: {},
          metadata: { purpose: "agent-studio-as06-live" },
        }),
      });
      if (!createResponse.ok) {
        throw new Error(`Eval Dataset create failed: ${createResponse.status}`);
      }
      dataset = await createResponse.json() as LiveEvalDataset;
    }
    const examplesResponse = await fetch(
      `/api/v1/eval/datasets/${dataset.dataset_id}/examples?limit=1&offset=0`,
      { headers },
    );
    if (!examplesResponse.ok) {
      throw new Error(`Eval example list failed: ${examplesResponse.status}`);
    }
    const examplesPayload = await examplesResponse.json() as { total?: number };
    if ((examplesPayload.total ?? 0) === 0) {
      const importResponse = await fetch(
        `/api/v1/eval/datasets/${dataset.dataset_id}/examples:import`,
        {
          method: "POST",
          headers,
          body: JSON.stringify({
            mode: "skip_duplicates",
            examples: [{
              case_id: "as06-release-identity-001",
              split: "regression",
              input: { message: "Confirm the saved release identity." },
              expected_output: { contains: "release" },
              expected_trajectory: {},
              assertions: [],
              metadata: { critical: true, owner: "agent-studio-as06" },
            }],
          }),
        },
      );
      if (!importResponse.ok) {
        throw new Error(`Eval example import failed: ${importResponse.status}`);
      }
    }
    return dataset;
  }, liveEvalDatasetName);
}

async function createAndCancelEvaluation(
  page: Page,
  agentId: string,
  datasetId: string,
) {
  return page.evaluate(async ({ id, evalDatasetId }) => {
    const rawAuth = localStorage.getItem("agent-gateway-auth");
    const token = rawAuth
      ? (JSON.parse(rawAuth) as { state?: { token?: string } }).state?.token
      : undefined;
    if (!token) throw new Error("Missing authenticated live browser token");
    const headers = {
      Authorization: `Bearer ${token}`,
      "content-type": "application/json",
    };
    const draftResponse = await fetch(`/api/v1/agents/${id}/draft`, { headers });
    if (!draftResponse.ok) throw new Error(`Draft lookup failed: ${draftResponse.status}`);
    const draft = await draftResponse.json() as { revision: number };
    const createResponse = await fetch(`/api/v1/agents/${id}/evals`, {
      method: "POST",
      headers,
      body: JSON.stringify({
        draft_revision: draft.revision,
        dataset_id: evalDatasetId,
        channel: "hosted",
        auth_mode: "private",
        channel_policy: {
          attachments: false,
          high_risk_tools: false,
          allowed_origins: [],
        },
      }),
    });
    if (!createResponse.ok) {
      throw new Error(`Queued evaluation create failed: ${createResponse.status}`);
    }
    const queued = await createResponse.json() as {
      evaluation_id: string;
      status: string;
      events: Array<{ status: string }>;
    };
    const cancelResponse = await fetch(
      `/api/v1/agents/${id}/evals/${queued.evaluation_id}/cancel`,
      { method: "POST", headers },
    );
    if (!cancelResponse.ok) {
      throw new Error(`Queued evaluation cancel failed: ${cancelResponse.status}`);
    }
    const cancelled = await cancelResponse.json() as {
      status: string;
      dataset_id: string | null;
      dataset_version: string | null;
      dataset_manifest_hash: string | null;
      events: Array<{ status: string }>;
    };
    return { queued, cancelled };
  }, { id: agentId, evalDatasetId: datasetId });
}

async function cleanupPriorLiveAgents(page: Page) {
  return page.evaluate(async () => {
    const rawAuth = localStorage.getItem("agent-gateway-auth");
    const token = rawAuth
      ? (JSON.parse(rawAuth) as { state?: { token?: string } }).state?.token
      : undefined;
    if (!token) throw new Error("Missing authenticated live browser token");
    const headers = {
      Authorization: `Bearer ${token}`,
      "content-type": "application/json",
    };
    const response = await fetch("/api/v1/agents?limit=100&search=AS06%20Live", { headers });
    if (!response.ok) throw new Error(`Prior live Agent lookup failed: ${response.status}`);
    const payload = await response.json() as {
      items?: Array<{ agent_id: string; name: string }>;
    };
    for (const agent of payload.items ?? []) {
      if (!agent.name.startsWith("AS06 Live ")) continue;
      await fetch(`/api/v1/agents/${agent.agent_id}/archive`, {
        method: "POST",
        headers,
        body: JSON.stringify({ disable_publications: true }),
      });
      await fetch(`/api/v1/agents/${agent.agent_id}`, {
        method: "DELETE",
        headers: { Authorization: `Bearer ${token}` },
      });
    }
  });
}

async function releaseCleanup(page: Page, agentId: string) {
  return page.evaluate(async (id) => {
    const rawAuth = localStorage.getItem("agent-gateway-auth");
    const token = rawAuth
      ? (JSON.parse(rawAuth) as { state?: { token?: string } }).state?.token
      : undefined;
    if (!token) return { archive: 0, remove: 0 };
    const headers = {
      Authorization: `Bearer ${token}`,
      "content-type": "application/json",
    };
    const archive = await fetch(`/api/v1/agents/${id}/archive`, {
      method: "POST",
      headers,
      body: JSON.stringify({ disable_publications: true }),
    });
    const remove = await fetch(`/api/v1/agents/${id}`, {
      method: "DELETE",
      headers: { Authorization: `Bearer ${token}` },
    });
    return { archive: archive.status, remove: remove.status };
  }, agentId);
}

async function releaseSnapshot(page: Page, agentId: string) {
  return page.evaluate(async (id) => {
    const rawAuth = localStorage.getItem("agent-gateway-auth");
    const token = rawAuth
      ? (JSON.parse(rawAuth) as { state?: { token?: string } }).state?.token
      : undefined;
    if (!token) throw new Error("Missing authenticated live browser token");
    const headers = { Authorization: `Bearer ${token}` };
    const [publicationsResponse, eventsResponse, versionsResponse, evaluationsResponse] = await Promise.all([
      fetch(`/api/v1/agents/${id}/publications`, { headers }),
      fetch(`/api/v1/agents/${id}/publish-events`, { headers }),
      fetch(`/api/v1/agents/${id}/versions`, { headers }),
      fetch(`/api/v1/agents/${id}/evals`, { headers }),
    ]);
    if (![publicationsResponse, eventsResponse, versionsResponse, evaluationsResponse].every((item) => item.ok)) {
      throw new Error("Live release evidence API failed");
    }
    const publications = await publicationsResponse.json() as Array<{ publication_id: string; version_id: string; version_number: number; public_id: string; channel: string }>;
    const events = await eventsResponse.json() as Array<{ operation: string; to_version_id: string }>;
    const versions = await versionsResponse.json() as Array<{
      agent_version_id: string;
      version_number: number;
      spec?: { model?: { model_id?: string }; knowledge?: Array<{ dataset_id: string }> };
    }>;
    const evaluationsPayload = await evaluationsResponse.json() as {
      evaluations: Array<{
        status: string;
        dataset_id: string | null;
        dataset_version: string | null;
        dataset_manifest_hash: string | null;
        events: Array<{ status: string }>;
      }>;
    };
    return {
      pointerVersionId: publications[0]?.version_id ?? null,
      pointerVersionNumber: publications[0]?.version_number ?? null,
      publicationId: publications.find((item) => item.channel === "hosted")?.publication_id ?? null,
      hostedPublicId: publications.find((item) => item.channel === "hosted")?.public_id ?? null,
      eventOperations: events.map((item) => item.operation),
      versions: versions.map((item) => ({
        id: item.agent_version_id,
        number: item.version_number,
        configuredModel: item.spec?.model?.model_id ?? "",
        knowledgeIds: (item.spec?.knowledge || []).map((binding) => binding.dataset_id),
      })),
      evaluations: evaluationsPayload.evaluations,
    };
  }, agentId);
}

async function screenshot(page: Page, name: string) {
  const dimensions = name.match(/-(\d+)x(\d+)$/);
  if (dimensions) {
    expect(page.viewportSize()).toEqual({
      width: Number(dimensions[1]),
      height: Number(dimensions[2]),
    });
  }
  const directory = path.resolve(process.cwd(), "../reports/agent-studio/as-06-screenshots");
  await fs.mkdir(directory, { recursive: true });
  await page.screenshot({
    path: path.join(directory, `${name}.png`),
    fullPage: false,
    animations: "disabled",
  });
}

async function sendHostedMessage(page: Page, message: string) {
  await page.getByPlaceholder("Message this agent").fill(message);
  const streamed = page.waitForResponse((response) =>
    response.request().method() === "POST" && new URL(response.url()).pathname.endsWith("/chat/stream"),
  );
  await page.getByRole("button", { name: "Send message" }).click();
  const response = await streamed;
  expect(response.status()).toBe(200);
  const answer = page.locator(".agent-public-message.is-assistant p").last();
  await expect(answer).not.toHaveText(/^(?:Generating…)?$/, { timeout: 90_000 });
  await expect(page.locator(".agent-public-error")).toHaveCount(0);
  return response;
}

test.describe("Agent publish live stack", () => {
  test.skip(!liveEnabled, "Set E2E_LIVE_AGENT_PUBLISH=1 for the authenticated local stack.");

  test("publishes and rolls back only when resources are ready, otherwise fails before evidence", async ({ page }) => {
    test.setTimeout(180_000);
    await page.addInitScript(() => localStorage.setItem("i18nextLng", "en-US"));
    const consoleErrors: string[] = [];
    const badResponses: string[] = [];
    page.on("pageerror", (error) => consoleErrors.push(String(error)));
    page.on("console", (message) => {
      const expectedResourceError = expectModelUnavailable
        && /Failed to load resource: the server responded with a status of 503/.test(message.text());
      if (message.type() === "error" && !/favicon/i.test(message.text()) && !expectedResourceError) {
        consoleErrors.push(message.text());
      }
    });
    page.on("response", (response) => {
      if (response.status() >= 400 && response.url().includes("/api/v1/")) {
        const pathname = new URL(response.url()).pathname;
        const expectedUnavailable = expectModelUnavailable
          && response.status() === 503
          && pathname === `/api/v1/agents/${agentId}/evals`;
        if (!expectedUnavailable) badResponses.push(`${response.status()} ${pathname}`);
      }
    });

    const uniqueName = `AS06 Live ${Date.now()}`;
    let agentId: string | null = null;
    let pinnedHostedPage: Page | null = null;
    let newHostedPage: Page | null = null;
    try {
      await page.setViewportSize({ width: 1440, height: 900 });
      await page.goto("/agents", { waitUntil: "domcontentloaded" });
      await cleanupPriorLiveAgents(page);
      await page.reload({ waitUntil: "domcontentloaded" });
      await page.getByRole("button", { name: "Create agent" }).click();
      await page.getByLabel("Name").fill(uniqueName);
      await page.getByLabel("Description").fill("Exercises the local immutable release transaction.");
      await page.getByRole("button", { name: "Continue" }).click();
      await page.getByRole("button", { name: "Continue" }).click();
      await page.getByRole("button", { name: "Create agent" }).click();
      await expect(page).toHaveURL(/\/agents\/[0-9a-f-]{36}$/);
      agentId = new URL(page.url()).pathname.split("/").pop() || null;
      expect(agentId).toBeTruthy();

      const evalDataset = await ensureLiveEvalDataset(page);
      if (!expectModelUnavailable) {
        const lifecycle = await createAndCancelEvaluation(page, agentId!, evalDataset.dataset_id);
        expect(lifecycle.queued.status).toBe("queued");
        expect(lifecycle.queued.events.map((event) => event.status)).toEqual(["queued"]);
        expect(lifecycle.cancelled.status).toBe("cancelled");
        expect(lifecycle.cancelled.dataset_id).toBe(evalDataset.dataset_id);
        expect(lifecycle.cancelled.dataset_version).toBe(evalDataset.version);
        expect(lifecycle.cancelled.dataset_manifest_hash).toMatch(/^[0-9a-f]{64}$/);
        expect(lifecycle.cancelled.events.map((event) => event.status)).toEqual([
          "queued",
          "cancelled",
        ]);
      }

      await page.goto(`/agents/${agentId}/evals`, { waitUntil: "domcontentloaded" });
      await page.getByLabel("Dataset").click();
      await page.getByText(
        `${liveEvalDatasetName} · ${evalDataset.version}`,
        { exact: true },
      ).click();
      await page.getByRole("button", { name: "Run evaluation" }).click();
      if (expectModelUnavailable) {
        await expect(page.getByRole("alert").filter({
          hasText: "AGENT_RUNTIME_MODEL_UNAVAILABLE: Agent model is unavailable",
        })).toBeVisible();
        const evidence = await releaseSnapshot(page, agentId);
        expect(evidence.pointerVersionId).toBeNull();
        expect(evidence.pointerVersionNumber).toBeNull();
        expect(evidence.eventOperations).toEqual([]);
        expect(evidence.versions).toEqual([]);
        expect(evidence.evaluations).toEqual([]);
        await screenshot(page, "live-eval-model-unavailable-1440x900");
        expect(consoleErrors).toEqual([]);
        expect(badResponses).toEqual([]);
        return;
      }
      await expect(page.getByTestId("agent-publish-sheet")).toBeVisible();
      const firstEvidence = await releaseSnapshot(page, agentId);
      const passedEvaluation = firstEvidence.evaluations.find((item) => item.status === "passed");
      expect(passedEvaluation?.dataset_id).toBe(evalDataset.dataset_id);
      expect(passedEvaluation?.dataset_version).toBe(evalDataset.version);
      expect(passedEvaluation?.dataset_manifest_hash).toMatch(/^[0-9a-f]{64}$/);
      expect(passedEvaluation?.events.map((event) => event.status)).toEqual([
        "queued",
        "running",
        "passed",
      ]);
      await page.getByRole("button", { name: "Close" }).click();

      await page.getByRole("button", { name: "Overview", exact: true }).click();
      await page.getByLabel("Description").fill("Changed after the first exact evaluation.");
      await page.locator(".agent-studio-actions").getByRole("button", { name: "Save draft" }).click();
      await expect(page.locator(".agent-save-state")).toHaveText("Saved");
      await page.getByRole("button", { name: "Eval & Publish", exact: true }).click();
      await expect(page.getByTestId("agent-eval-stale").first()).toBeVisible();
      await screenshot(page, "live-eval-stale-1440x900");

      await page.getByRole("button", { name: "Run evaluation" }).click();
      await expect(page.getByTestId("agent-publish-sheet")).toBeVisible();
      await page.getByLabel("Release reason").fill("Promote live revision one.");
      await page.getByRole("button", { name: "Publish Version" }).click();
      await expect(page.getByTestId("agent-publish-sheet").getByText("Version published")).toBeVisible();
      await page.getByRole("button", { name: "Close" }).click();

      await page.getByRole("button", { name: "Overview", exact: true }).click();
      await page.getByLabel("Welcome message").fill("Welcome to immutable Version two.");
      await page.locator(".agent-studio-actions").getByRole("button", { name: "Save draft" }).click();
      await expect(page.locator(".agent-save-state")).toHaveText("Saved");
      await page.getByRole("button", { name: "Eval & Publish", exact: true }).click();
      await page.getByRole("button", { name: "Run evaluation" }).click();
      await expect(page.getByTestId("agent-publish-sheet")).toBeVisible();
      await page.getByLabel("Release reason").fill("Promote live revision two.");
      await page.getByRole("button", { name: "Publish Version" }).click();
      await expect(page.getByTestId("agent-publish-sheet").getByText("Version published")).toBeVisible();
      await page.setViewportSize({ width: 390, height: 844 });
      await screenshot(page, "live-publish-v2-390x844");
      await page.getByRole("button", { name: "Close" }).click();

      await page.setViewportSize({ width: 1440, height: 900 });
      const beforeRollback = await releaseSnapshot(page, agentId!);
      const versionTwoEvidence = beforeRollback.versions.find((item) => item.number === 2);
      expect(beforeRollback.pointerVersionId).toBe(versionTwoEvidence?.id);
      expect(beforeRollback.hostedPublicId).toBeTruthy();
      expect(beforeRollback.publicationId).toBeTruthy();
      pinnedHostedPage = await page.context().newPage();
      await pinnedHostedPage.addInitScript(() => localStorage.setItem("i18nextLng", "en-US"));
      await pinnedHostedPage.goto(`/a/${beforeRollback.hostedPublicId}`, { waitUntil: "domcontentloaded" });
      const versionTwoSessionResponse = pinnedHostedPage.waitForResponse((response) =>
        response.request().method() === "POST"
        && new URL(response.url()).pathname === `/api/v1/public/agents/${beforeRollback.hostedPublicId}/sessions`,
      );
      await sendHostedMessage(pinnedHostedPage, "Confirm this published Agent can answer a support question.");
      const versionTwoSession = await (await versionTwoSessionResponse).json() as { session_id: string; agent_version_id: string; runtime_fingerprint: string };
      expect(versionTwoSession.agent_version_id).toBe(versionTwoEvidence?.id);
      expect(versionTwoSession.runtime_fingerprint).toBeTruthy();

      await page.goto(`/agents/${agentId}/versions`, { waitUntil: "domcontentloaded" });
      await expect(page.getByText("Current target · v2")).toBeVisible();
      const versionOne = page.locator(".agent-version-history > article").filter({ hasText: "v1" });
      await versionOne.getByRole("button", { name: "Roll back HOSTED" }).click();
      await page.getByLabel("Rollback reason").fill("Return to the first known healthy live Version.");
      const rollbackResponse = page.waitForResponse((response) =>
        response.request().method() === "POST"
        && new URL(response.url()).pathname === `/api/v1/publications/${beforeRollback.publicationId}/rollback`,
      );
      await page.getByRole("button", { name: "Roll back channel" }).click();
      expect((await rollbackResponse).ok(), "Rollback API must commit before the channel pointer can update").toBe(true);
      await expect(page.getByText("Current target · v1")).toBeVisible();
      await expect(page.getByText("Rolled back channel", { exact: true })).toBeVisible();

      const evidence = await releaseSnapshot(page, agentId);
      expect(
        evidence.versions.map((item) => item.number).sort((left, right) => left - right),
      ).toEqual([1, 2]);
      const versionOneEvidence = evidence.versions.find((item) => item.number === 1);
      expect(evidence.pointerVersionId).toBe(versionOneEvidence?.id);
      expect(evidence.pointerVersionNumber).toBe(1);
      expect(evidence.eventOperations.slice().reverse()).toEqual([
        "promote",
        "promote",
        "rollback",
      ]);
      const oldPinnedResponse = await sendHostedMessage(pinnedHostedPage, "Continue in the existing Version two conversation.");
      expect(await oldPinnedResponse.headerValue("x-session-id")).toBe(versionTwoSession.session_id);
      newHostedPage = await page.context().newPage();
      await newHostedPage.addInitScript(() => localStorage.setItem("i18nextLng", "en-US"));
      await newHostedPage.goto(`/a/${evidence.hostedPublicId}`, { waitUntil: "domcontentloaded" });
      const versionOneSessionResponse = newHostedPage.waitForResponse((response) =>
        response.request().method() === "POST"
        && new URL(response.url()).pathname === `/api/v1/public/agents/${evidence.hostedPublicId}/sessions`,
      );
      await sendHostedMessage(newHostedPage, "Confirm the rolled-back Agent still answers a support question.");
      const versionOneSession = await (await versionOneSessionResponse).json() as { session_id: string; agent_version_id: string; runtime_fingerprint: string };
      expect(versionOneSession.agent_version_id).toBe(versionOneEvidence?.id);
      expect(versionOneSession.session_id).not.toBe(versionTwoSession.session_id);
      expect(versionOneSession.runtime_fingerprint).toBeTruthy();
      expect(versionOneEvidence?.configuredModel).toBeDefined();
      expect(versionTwoEvidence?.configuredModel).toBeDefined();
      expect(versionOneEvidence?.knowledgeIds).toEqual(expect.any(Array));
      expect(versionTwoEvidence?.knowledgeIds).toEqual(expect.any(Array));
      await screenshot(page, "live-rollback-audit-1440x900");
      expect(consoleErrors).toEqual([]);
      expect(badResponses).toEqual([]);
    } finally {
      await pinnedHostedPage?.close();
      await newHostedPage?.close();
      if (agentId && !page.isClosed()) {
        const cleanup = await releaseCleanup(page, agentId);
        expect(cleanup).toEqual({ archive: 200, remove: 200 });
      }
    }
  });
});
