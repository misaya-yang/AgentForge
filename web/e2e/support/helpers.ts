import fs from "node:fs/promises";
import path from "node:path";
import { fileURLToPath } from "node:url";
import AxeBuilder from "@axe-core/playwright";
import { expect, type APIRequestContext, type Page, type Route } from "@playwright/test";

const helperDir = path.dirname(fileURLToPath(import.meta.url));
const ARTIFACT_DIR = path.resolve(helperDir, "../../.playwright");
const USER_FILE = path.join(ARTIFACT_DIR, "e2e-user.json");
const AUTH_STORAGE_KEY = "agent-gateway-auth";
const APP_STORAGE_KEY = "agent-gateway-storage";

export interface E2ETestUser {
  email: string;
  password: string;
}

export interface E2EClientAuthUser {
  user_id: string;
  email: string;
  display_name: string;
  department: string | null;
  roles: string[];
  permissions: string[];
  effective_permissions: string[];
  tier: string;
  force_password_change: boolean;
}

export async function installClientAuth(
  page: Page,
  overrides: Partial<E2EClientAuthUser> = {}
) {
  const permissions = overrides.permissions || [
    "console:dashboard:view",
    "conversation:playground:access",
  ];
  const user: E2EClientAuthUser = {
    user_id: "e2e-client-user",
    email: "e2e-client@example.com",
    display_name: "E2E Client",
    department: null,
    roles: ["user"],
    permissions,
    effective_permissions: overrides.effective_permissions || permissions,
    tier: "normal",
    force_password_change: false,
    ...overrides,
  };
  const authState = {
    state: {
      token: "e2e-client-token",
      user,
      isAuthenticated: true,
      forcePasswordChange: user.force_password_change,
      rememberMe: true,
    },
    version: 0,
  };

  await page.route("**/api/v1/auth/me", async (route) => {
    await route.fulfill({
      status: 200,
      contentType: "application/json",
      body: JSON.stringify(user),
    });
  });
  await page.addInitScript(
    ({ authStorageKey, authPayload }) => {
      localStorage.setItem(authStorageKey, JSON.stringify(authPayload));
      sessionStorage.removeItem(authStorageKey);
    },
    { authStorageKey: AUTH_STORAGE_KEY, authPayload: authState }
  );
}

export function getApiUrl(): string {
  const apiUrl = process.env.E2E_API_URL;
  if (!apiUrl) {
    throw new Error("Missing E2E_API_URL");
  }
  return apiUrl;
}

export function modKey(): "Meta" | "Control" {
  return process.platform === "darwin" ? "Meta" : "Control";
}

export async function readE2ETestUser(): Promise<E2ETestUser> {
  const raw = await fs.readFile(USER_FILE, "utf-8");
  return JSON.parse(raw) as E2ETestUser;
}

export async function ensureAuthenticatedPage(page: Page, destination: string) {
  const loginButtonName = /sign in|log in|login|登\s*录/i;

  await page.goto(destination);

  const loginFormVisible = await page
    .locator("#email")
    .isVisible()
    .catch(() => false);

  if (!loginFormVisible && !page.url().includes("/login")) {
    return;
  }

  const testUser = await readE2ETestUser();
  await page.locator("#email").fill(testUser.email);
  await page.locator("#password").fill(testUser.password);
  await page.getByRole("button", { name: loginButtonName }).click();
  await page.waitForURL(/\/(dashboard|assistant|playground)/, { timeout: 15000 });

  if (!page.url().includes(destination)) {
    await page.goto(destination);
  }
}

export async function loginThroughApi(request: APIRequestContext): Promise<{
  token: string;
  user: Record<string, unknown>;
}> {
  const testUser = await readE2ETestUser();
  const response = await request.post(`${getApiUrl()}/api/v1/auth/login`, {
    data: {
      email: testUser.email,
      password: testUser.password,
    },
  });

  if (!response.ok()) {
    throw new Error(`E2E login failed (${response.status()}): ${await response.text()}`);
  }

  const payload = (await response.json()) as Record<string, unknown>;
  return {
    token: String(payload.access_token || ""),
    user: (payload.user as Record<string, unknown>) || {},
  };
}

export async function buildAuthHeaders(request: APIRequestContext): Promise<Record<string, string>> {
  const { token } = await loginThroughApi(request);
  return {
    Authorization: `Bearer ${token}`,
  };
}

export async function seedClientPrefs(
  page: Page,
  prefs: {
    locale?: "zh-CN" | "en-US";
    themeMode?: "light" | "dark" | "system";
    resolvedTheme?: "light" | "dark";
    darkMode?: boolean;
  }
) {
  await page.addInitScript(
    ({ appStorageKey, locale, themeMode, resolvedTheme, darkMode }) => {
      if (locale) {
        localStorage.setItem("i18nextLng", locale);
      }
      if (themeMode && resolvedTheme) {
        localStorage.setItem(
          appStorageKey,
          JSON.stringify({
            state: {
              themeMode,
              resolvedTheme,
              darkMode:
                typeof darkMode === "boolean"
                  ? darkMode
                  : resolvedTheme === "dark",
            },
            version: 3,
          })
        );
      }
    },
    {
      appStorageKey: APP_STORAGE_KEY,
      locale: prefs.locale,
      themeMode: prefs.themeMode,
      resolvedTheme: prefs.resolvedTheme,
      darkMode: prefs.darkMode,
    }
  );
}

export async function clearAuthState(page: Page) {
  await page.addInitScript(({ authStorageKey, appStorageKey }) => {
    localStorage.removeItem(authStorageKey);
    sessionStorage.removeItem(authStorageKey);
    localStorage.removeItem(appStorageKey);
  }, { authStorageKey: AUTH_STORAGE_KEY, appStorageKey: APP_STORAGE_KEY });
}

export async function assertNoBlockingA11yIssues(page: Page, include: string[]) {
  const scanner = include.reduce(
    (builder, selector) => builder.include(selector),
    new AxeBuilder({ page })
  );
  const result = await scanner.analyze();
  const violations = result.violations.filter((violation) =>
    ["serious", "critical"].includes(violation.impact || "")
  );
  expect(violations).toEqual([]);
}

export async function assertInpBudget(page: Page, p75ThresholdMs = 200) {
  const p75 = await page.evaluate(
    () => window.__AI_GATEWAY_PERF__?.getInteractionP75?.() ?? 0
  );
  expect(p75).toBeLessThan(p75ThresholdMs);
}

export async function installTelemetryCollector(page: Page) {
  await page.evaluate(() => {
    const key = "__E2E_TELEMETRY_EVENTS__";
    (window as Window & { __E2E_TELEMETRY_EVENTS__?: unknown[] })[key] = [];
    window.addEventListener("ai-gateway:telemetry", (event) => {
      const detail = (event as CustomEvent).detail;
      (
        window as Window & { __E2E_TELEMETRY_EVENTS__?: unknown[] }
      ).__E2E_TELEMETRY_EVENTS__?.push(detail);
    });
  });
}

export async function readTelemetryEvents(page: Page): Promise<
  Array<{ event: string; payload?: Record<string, unknown> }>
> {
  return page.evaluate(() => {
    return (
      (
        window as Window & {
          __E2E_TELEMETRY_EVENTS__?: Array<{
            event: string;
            payload?: Record<string, unknown>;
          }>;
        }
      ).__E2E_TELEMETRY_EVENTS__ || []
    );
  });
}

export function toSseBody(events: Array<Record<string, unknown>>): string {
  return `${events.map((event) => `data: ${JSON.stringify(event)}\n\n`).join("")}data: [DONE]\n\n`;
}

export function wrapAssistantSseAsAgentV2(
  body: string,
  threadId = "e2e-runtime-thread",
): string {
  const chunks: string[] = [];
  let sequence = 1;
  for (const block of body.replace(/\r\n/g, "\n").split("\n\n")) {
    const line = block.split("\n").find((entry) => entry.startsWith("data:"));
    if (!line) continue;
    const jsonStr = line.slice(5).trim();
    if (!jsonStr || jsonStr === "[DONE]") continue;
    let parsed: Record<string, unknown>;
    try {
      parsed = JSON.parse(jsonStr) as Record<string, unknown>;
    } catch {
      continue;
    }
    if (parsed.schema_version === "agent-event/v2") {
      chunks.push(`data: ${JSON.stringify(parsed)}\n\n`);
      continue;
    }
    const eventType = typeof parsed.event_type === "string" ? parsed.event_type : "item";
    chunks.push(
      `data: ${JSON.stringify({
        schema_version: "agent-event/v2",
        thread_id: threadId,
        sequence,
        event: {
          id: `evt-${sequence}`,
          key: `evt-${sequence}`,
          type: eventType,
          item_id: null,
          turn_id: null,
          status: null,
          payload: parsed,
        },
        timestamp: new Date().toISOString(),
      })}\n\n`,
    );
    sequence += 1;
  }
  return chunks.join("");
}

/**
 * Install the Agent Runtime V2 thread/turn/events routes that the Assistant
 * actually calls, driven by an existing V1-style SSE fulfiller.
 *
 * The Assistant streams through `/api/v2/agent/threads/{id}/events`; the legacy
 * `/api/v1/assistant/chat/stream` route it used to call is never requested any
 * more, so a spec that only mocks the V1 path renders no assistant turn at all.
 */
export async function installAgentV2StreamRoutes(
  page: Page,
  fulfillV1Stream: (route: Route) => Promise<void>,
  threadId = "e2e-runtime-thread",
): Promise<void> {
  const json = (body: unknown) => ({
    status: 200,
    contentType: "application/json",
    body: JSON.stringify(body),
  });

  await page.route("**/api/v2/agent/threads", async (route) => {
    if (route.request().method() !== "POST") {
      await route.fallback();
      return;
    }
    const sessionId =
      (route.request().postDataJSON() as { session_id?: string } | null)?.session_id ||
      "e2e-session";
    await route.fulfill(
      json({
        thread: {
          schema_version: "agent-thread/v2",
          id: threadId,
          thread_id: threadId,
          session_id: sessionId,
          import_status: "not_required",
          last_sequence: 0,
          runtime: { owner: "agent_runtime", source: "native" },
        },
      }),
    );
  });

  await page.route("**/api/v2/agent/threads/*/turns", async (route) => {
    if (route.request().method() !== "POST") {
      await route.fallback();
      return;
    }
    await route.fulfill(
      json({
        turn: {
          id: "e2e-turn",
          events_url: `/api/v2/agent/threads/${threadId}/events?turn_id=e2e-turn`,
        },
      }),
    );
  });

  // Best-effort interrupt from the stream generator's `finally`. An unmocked
  // 401 here trips the global auth interceptor and redirects to /login.
  await page.route("**/api/v2/agent/threads/*/turns/*", async (route) => {
    await route.fulfill(json({ status: "accepted" }));
  });

  await page.route("**/api/v2/agent/threads/*/approvals/*/decision", async (route) => {
    await route.fulfill(
      json({
        schema_version: "agent-approval/v2",
        approval: { status: "approved", approved: true },
      }),
    );
  });

  await page.route("**/api/v2/agent/threads/*/events**", async (route) => {
    let captured: { status?: number; headers?: Record<string, string>; body?: string } | undefined;
    const capturingRoute = {
      request: () => route.request(),
      fulfill: async (response: Parameters<Route["fulfill"]>[0]) => {
        captured = {
          status: response.status,
          headers: (response.headers as Record<string, string> | undefined) || {},
          body: typeof response.body === "string" ? response.body : undefined,
        };
      },
    } as Route;
    await fulfillV1Stream(capturingRoute);
    await route.fulfill({
      status: captured?.status ?? 200,
      headers: { ...(captured?.headers || {}), "content-type": "text/event-stream" },
      body: wrapAssistantSseAsAgentV2(captured?.body || toSseBody([]), threadId),
    });
  });
}

/**
 * Create a knowledge dataset the calling user personally owns, seeded with one
 * text document, and wait until it is retrievable.
 *
 * Specs must not reach for `datasets[0]` off `/api/v1/assistant/datasets`. That
 * list is permission-scoped, and the E2E account is an admin, so it happily
 * returns other people's private datasets. The Agent capability worker does
 * NOT inherit the caller's roles — it presents the bare tenant/user identity —
 * so `search_knowledge_base` against an admin-only-visible dataset comes back
 * `read_capability_downstream_rejected` and the assistant reports that it
 * cannot reach the knowledge base.
 */
export async function createOwnedKnowledgeDataset(
  request: APIRequestContext,
  options: { name?: string; content?: string } = {},
): Promise<{ datasetId: string; name: string }> {
  const apiUrl = getApiUrl();
  const headers = { ...(await buildAuthHeaders(request)), "content-type": "application/json" };
  const name = options.name ?? `e2e-owned-kb-${Date.now()}`;
  const content =
    options.content ??
    [
      "AI Gateway 平台核心概念speed-run。",
      "第一，网关负责统一鉴权、配额与审计，所有模型调用都必须经过网关。",
      "第二，知识库以数据集为单位管理，数据集包含文档，文档切分为段落后建立索引。",
      "第三，检索采用向量召回加分数阈值过滤，top_k 控制返回的段落数量。",
      "第四，智能体运行时以线程为单位隔离会话，写能力需要操作员审批。",
    ].join("\n");

  const created = await request.post(`${apiUrl}/api/v1/knowledge/datasets`, {
    headers,
    data: {
      name,
      description: "E2E owned dataset for capability-plane retrieval",
      embedding_provider: "local",
      embedding_model: "hash-384",
      embedding_dimension: 384,
    },
  });
  expect(created.ok(), `Create dataset failed: ${created.status()}`).toBeTruthy();
  const body = await created.json();
  const datasetId: string = body.id ?? body.dataset_id ?? body.data?.id;
  expect(datasetId, "dataset id missing from create response").toBeTruthy();

  const document = await request.post(`${apiUrl}/api/v1/knowledge/${datasetId}/documents/text`, {
    headers,
    data: { title: "E2E 核心概念", content },
  });
  expect(document.ok(), `Create document failed: ${document.status()}`).toBeTruthy();

  // Indexing is asynchronous; poll retrieval instead of guessing a sleep.
  await expect
    .poll(
      async () => {
        const retrieved = await request.post(`${apiUrl}/api/v1/knowledge/${datasetId}/retrieve`, {
          headers,
          data: { query: "核心概念", top_k: 3 },
        });
        if (!retrieved.ok()) return 0;
        const payload = await retrieved.json();
        const results = payload.results ?? payload.data?.results ?? [];
        return Array.isArray(results) ? results.length : 0;
      },
      { timeout: 90_000, intervals: [1_000] },
    )
    .toBeGreaterThan(0);

  return { datasetId, name };
}

/**
 * Delete a dataset, absorbing the retryable index-lease conflict.
 *
 * Deletion takes the same index lifecycle lease as ingestion, so a delete
 * issued while a document is still being indexed loses the race and comes back
 * 409. That is expected contention, not a failure.
 */
export async function deleteKnowledgeDataset(
  request: APIRequestContext,
  datasetId: string,
  reason = "E2E cleanup",
): Promise<void> {
  const apiUrl = getApiUrl();
  const headers = { ...(await buildAuthHeaders(request)), "content-type": "application/json" };
  const { password } = await readE2ETestUser();
  for (let attempt = 0; attempt < 15; attempt += 1) {
    const response = await request.delete(`${apiUrl}/api/v1/knowledge/datasets/${datasetId}`, {
      headers,
      data: { password, reason },
    });
    if (response.status() !== 409) return;
    await new Promise((resolve) => setTimeout(resolve, 1_000));
  }
}
