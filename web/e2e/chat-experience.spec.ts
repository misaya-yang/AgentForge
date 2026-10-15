import { expect, test, type Page, type Route } from "@playwright/test";
import {
  assertInpBudget,
  assertNoBlockingA11yIssues,
  ensureAuthenticatedPage,
  installClientAuth,
  installTelemetryCollector,
  modKey,
  readTelemetryEvents,
  seedClientPrefs,
  toSseBody,
  wrapAssistantSseAsAgentV2,
} from "./support/helpers";
import { buildTimeline } from "../src/pages/assistant/components/buildTimeline";
import type { ChatMessage as AssistantChatMessage } from "../src/pages/assistant/types";
import {
  cancelStreamTurn,
  completeStreamTurn,
  createStreamTurnState,
  failStreamTurn,
  reduceNormalizedStreamEvent,
} from "../src/features/chat/stream/reducer";
import { splitStreamingMarkdownBlocks } from "../src/components/streamingMarkdown";

const ASSISTANT_COMPOSER_ID = "assistant-chat-composer";
const PLAYGROUND_COMPOSER_ID = "playground-chat-composer";
const MOCK_ASSISTANT_SERVICE_ID = "__builtin_assistant__";
const MOCK_ASSISTANT_MODEL_ID = "gpt-4o";
const MOCK_PLAYGROUND_SERVICE_ID = "e2e-mock-playground";
const MOCK_PLAYGROUND_THREAD_ID = "e2e-mock-thread";
const MOCK_PLAYGROUND_TOOL_ID = "pg-tool-1";
const LAST_MODEL_STORAGE_KEY = "assistant.lastModelId.v1";

test("R1 controlled stream disconnect reconnects to the same turn without duplicate execution", async ({ page }) => {
  await seedClientPrefs(page, { locale: "en-US" });
  await installClientAuth(page, {
    user_id: "e2e-r1-reconnect", email: "r1-reconnect@example.com", display_name: "R1 reconnect",
  });
  await installAssistantHarness(page, async route => {
    await route.fulfill({ status: 500, body: "unexpected harness stream" });
  });
  let threadCreates = 0, turnStarts = 0, interrupts = 0, cursorReads = 0;
  await page.route("**/api/v2/agent/threads", async route => {
    if (route.request().method() !== "POST") return route.fallback();
    threadCreates += 1;
    const sessionId = (route.request().postDataJSON() as { session_id?: string }).session_id;
    await route.fulfill(jsonResponse({ thread: { id: "e2e-runtime-thread", thread_id: "e2e-runtime-thread", session_id: sessionId, last_sequence: 0 } }));
  });
  await page.route("**/api/v2/agent/threads/*/turns", async route => {
    if (route.request().method() !== "POST") return route.fallback();
    turnStarts += 1;
    await route.fulfill(jsonResponse({ turn: { id: "e2e-turn", events_url: "/api/v2/agent/threads/e2e-runtime-thread/events?turn_id=e2e-turn" } }));
  });
  await page.route("**/api/v2/agent/threads/*/turns/*", async route => {
    if (route.request().url().includes(":interrupt")) interrupts += 1;
    await route.fulfill(jsonResponse({ status: "accepted" }));
  });
  await page.route("**/api/v2/agent/threads/*/events**", async route => {
    cursorReads += 1;
    if (cursorReads === 1) {
      await route.fulfill({ status: 503, contentType: "application/json", body: JSON.stringify({ detail: "controlled disconnect" }) });
      return;
    }
    expect(new URL(route.request().url()).searchParams.get("after_sequence")).toBe("0");
    await route.fulfill({ status: 200, headers: { "content-type": "text/event-stream" }, body: wrapAssistantSseAsAgentV2(toSseBody([
      { event_type: "run_started", data: { run_id: "e2e-turn", thread_id: "e2e-runtime-thread" } },
      { event_type: "text_delta", data: "R1_RECONNECTED_ONCE" },
      { event_type: "run_finished", data: { status: "succeeded" } },
    ])) });
  });
  await ensureAuthenticatedPage(page, "/assistant");
  await page.locator(`#${ASSISTANT_COMPOSER_ID}`).fill("R1 controlled network reconnect");
  await page.getByRole("button", { name: "Send", exact: true }).click();
  await expect(page.getByRole("log", { name: "Assistant conversation log" })).toContainText("R1_RECONNECTED_ONCE");
  await expect.poll(() => cursorReads).toBe(2);
  expect({ threadCreates, turnStarts, interrupts }).toEqual({ threadCreates: 1, turnStarts: 1, interrupts: 0 });
});

// Controlled activity input, not a real provider/Worker throughput claim.
for (const viewport of [{ width: 1280, height: 800 }, { width: 390, height: 844 }]) {
  test(`R1 controlled 1000 activity events remain unique, scrollable and stoppable at ${viewport.width}px`, async ({ page }, testInfo) => {
    await page.setViewportSize(viewport);
    await seedClientPrefs(page, { locale: "en-US" });
    await installClientAuth(page, {
      user_id: "e2e-r1-activity-sample", email: "r1-activity@example.com", display_name: "R1 activity sample",
    });
    let starts = 0;
    let interrupts = 0;
    await installAssistantHarness(page, async route => {
      await route.fulfill({ status: 500, body: "controlled stream override missing" });
    });
    await page.route("**/api/v2/agent/threads/*/turns", async route => {
      starts += 1;
      await route.fulfill(jsonResponse({ turn: {
        id: "e2e-turn", events_url: "/api/v2/agent/threads/e2e-runtime-thread/events?turn_id=e2e-turn",
      } }));
    });
    await page.route("**/api/v2/agent/threads/*/turns/*", async route => {
      if (route.request().url().includes(":interrupt")) interrupts += 1;
      await route.fulfill(jsonResponse({ status: "accepted" }));
    });
    const frames: Array<{ event_type: string; data: unknown }> = [
      { event_type: "run_started", data: { run_id: "11111111-1111-4111-8111-111111111111", thread_id: "e2e-runtime-thread", timestamp: Date.now() } },
      { event_type: "text_delta", data: Array.from({ length: 150 }, (_, i) => `R1 history paragraph ${i + 1}.\n\n`).join("") },
    ];
    for (let i = 1; i <= 100; i++) {
      const id = `r1-sample-${String(i).padStart(3, "0")}`;
      const name = `r1_sample_${String(i).padStart(3, "0")}`;
      frames.push({ event_type: "tool_call_start", data: { tool_call_id: id, tool_name: name, arguments: {}, timestamp: Date.now() } });
      for (let j = 1; j <= (i === 100 ? 7 : 8); j++) {
        frames.push({ event_type: "queue_state", data: { tool_id: id, state: `sample-${i}-q${j}` } });
      }
      if (i !== 100) frames.push({ event_type: "tool_call_result", data: { tool_call_id: id, tool_name: name, status: "succeeded", success: true, result: { summary: `R1 result ${i}` }, timestamp: Date.now() } });
    }
    expect(frames).toHaveLength(1000);
    await page.addInitScript(({ frames }) => {
      const nativeFetch = window.fetch.bind(window);
      const state = window as typeof window & { r1ReleaseActivity?: () => void; r1ActivityFrames?: number };
      let opened = false;
      window.fetch = async (input, init) => {
        const url = typeof input === "string" ? input : input instanceof Request ? input.url : String(input);
        if (!url.includes("/api/v2/agent/threads/") || !url.includes("/events")) return nativeFetch(input, init);
        if (opened) throw new Error("controlled cursor must not reconnect before Stop");
        opened = true;
        const encoder = new TextEncoder();
        let closed = false;
        return new Response(new ReadableStream({
          start(controller) {
            const emit = (from: number, to: number) => {
              if (closed) return;
              for (let index = from; index < to; index++) {
                const sequence = index + 1;
                const event = frames[index];
                const envelope = {
                  schema_version: "agent-event/v2", thread_id: "e2e-runtime-thread", sequence,
                  event: { id: `evt-${sequence}`, key: `evt-${sequence}`, type: event.event_type, item_id: null, turn_id: "e2e-turn", status: null, payload: event },
                  timestamp: new Date().toISOString(),
                };
                controller.enqueue(encoder.encode(`data: ${JSON.stringify(envelope)}\n\n`));
              }
              state.r1ActivityFrames = to;
            };
            emit(0, 500);
            state.r1ReleaseActivity = () => { emit(500, 1000); state.r1ReleaseActivity = undefined; };
            init?.signal?.addEventListener("abort", () => {
              if (!closed) { closed = true; controller.close(); }
            }, { once: true });
          },
          cancel() { closed = true; },
        }), { headers: { "content-type": "text/event-stream" } });
      };
    }, { frames });
    await ensureAuthenticatedPage(page, "/assistant");
    const composer = page.locator(`#${ASSISTANT_COMPOSER_ID}`);
    await composer.fill("R1 controlled activity1000; no real provider or tool execution");
    if (viewport.width === 1280) await page.getByRole("button", { name: "Send", exact: true }).dblclick();
    else await composer.press("Enter");
    const log = page.getByRole("log", { name: "Assistant conversation log" });
    await expect(log.getByText("R1 history paragraph 150.", { exact: true })).toBeVisible();
    const openActivity = () => log.getByRole("button", { name: /Activity|Thinking/ }).last().click();
    await openActivity();
    const rows = page.locator(".act-scroll .act-step-new").filter({ hasText: /r1_sample_/ });
    await expect(rows).toHaveCount(50);
    await expect(rows.last()).toContainText("Queue: sample-50-q7");
    if (viewport.width === 390) await page.getByRole("button", { name: "Close", exact: true }).click();
    await log.getByText("R1 history paragraph 150.", { exact: true }).click();
    const mainScroll = page.locator(".assistant-v2 .overflow-y-auto").filter({ has: log }).first();
    const beforeWheel = await mainScroll.evaluate(el => el.scrollTop);
    const mainBox = await mainScroll.boundingBox();
    expect(mainBox).toBeTruthy();
    await page.mouse.move(mainBox!.x + mainBox!.width / 2, mainBox!.y + mainBox!.height / 2);
    await page.mouse.wheel(0, -900);
    await expect.poll(() => mainScroll.evaluate(el => el.scrollTop)).toBeLessThan(beforeWheel - 200);
    if (viewport.width === 390) await openActivity();
    const activityScroll = page.locator(".act-scroll");
    await testInfo.attach("r1-activity-initial-geometry.json", { body: Buffer.from(JSON.stringify(await activityScroll.evaluate(el => ({ height: el.clientHeight, content: el.scrollHeight, box: el.getBoundingClientRect().toJSON() })))), contentType: "application/json" });
    // Wait for the existing opening animation before targeting the wheel.
    await expect.poll(() => activityScroll.evaluate(el => {
      const box = el.getBoundingClientRect();
      return box.top >= 0 && box.bottom <= window.innerHeight + 1;
    })).toBe(true);
    const activityBox = await activityScroll.boundingBox();
    expect(activityBox).toBeTruthy();
    await page.mouse.move(activityBox!.x + activityBox!.width / 2, activityBox!.y + activityBox!.height / 2);
    await page.mouse.wheel(0, 600);
    await expect.poll(() => activityScroll.evaluate(el => el.scrollTop)).toBeGreaterThan(200);
    const mainPosition = await mainScroll.evaluate(el => el.scrollTop);
    const activityPosition = await activityScroll.evaluate(el => el.scrollTop);
    await page.evaluate(() => (window as typeof window & { r1ReleaseActivity?: () => void }).r1ReleaseActivity?.());
    await expect(rows).toHaveCount(100);
    await expect(rows.last()).toContainText("Queue: sample-100-q7");
    for (let i = 1; i <= 100; i++) await expect(activityScroll.getByText(`r1_sample_${String(i).padStart(3, "0")}`, { exact: true })).toHaveCount(1);
    await expect(activityScroll.locator(".act-running-dot")).toHaveCount(1);
    expect(Math.abs(await mainScroll.evaluate(el => el.scrollTop) - mainPosition)).toBeLessThanOrEqual(2);
    expect(Math.abs(await activityScroll.evaluate(el => el.scrollTop) - activityPosition)).toBeLessThanOrEqual(2);
    if (viewport.width === 390) await page.getByRole("button", { name: "Close", exact: true }).click();
    const stop = page.getByRole("button", { name: "Stop generating", exact: true });
    if (viewport.width === 390) await stop.click();
    else await stop.press("Enter");
    await expect.poll(() => interrupts, { timeout: 15000 }).toBe(1);
    await expect(page.getByRole("button", { name: "Stop generating", exact: true })).toBeHidden();
    if (viewport.width === 390) await openActivity();
    await expect(page.getByText(/^cancelled ·/)).toBeVisible();
    await expect(activityScroll.locator(".act-running-dot")).toHaveCount(0);
    await expect(rows.last()).toContainText("Result unknown");
    expect(starts).toBe(1);
    expect(await page.evaluate(() => document.documentElement.scrollWidth)).toBeLessThanOrEqual(viewport.width);
    await testInfo.attach("r1-activity1000-facts.json", { body: Buffer.from(JSON.stringify({ controlled: true, viewport, frames: 1000, unique_tool_rows: 100, starts, interrupts, mainPosition, activityPosition })), contentType: "application/json" });
  });
}

const translateDefault = (
  key: string,
  options?: Record<string, unknown>
): string => String(options?.defaultValue ?? key);

test("assistant timeline tolerates transient tool entries without names", () => {
  const message = {
    id: "assistant-transient-tool-name",
    role: "assistant",
    content: "",
    createdAt: new Date(0).toISOString(),
    toolCalls: [
      {
        id: "todo-call-transient",
        name: undefined,
        arguments: { todos: [{ content: "Review tool stream", status: "pending" }] },
        status: "running",
      },
    ],
    processSummary: {
      collapsed: true,
      status: "running",
      steps: [],
      tools: [
        {
          id: "process-tool-transient",
          name: undefined,
          status: "running",
        },
      ],
    },
  } as unknown as AssistantChatMessage;

  const timeline = buildTimeline(message, translateDefault);

  expect(timeline.steps).toHaveLength(2);
  expect(timeline.steps.map((step) => step.title)).toEqual([
    "External tool",
    "External tool",
  ]);
});

test("assistant stream terminal state is first-wins", () => {
  const completed = completeStreamTurn(createStreamTurnState(0), 10);
  expect(cancelStreamTurn(completed, 20).status).toBe("completed");
  expect(failStreamTurn(completed, "late transport error", 20).status).toBe(
    "completed"
  );

  const failed = failStreamTurn(createStreamTurnState(0), "provider failed", 10);
  expect(cancelStreamTurn(failed, 20).status).toBe("failed");
  expect(completeStreamTurn(failed, 20).status).toBe("failed");
});

test("late stream deltas do not reopen a completed turn", () => {
  const completed = completeStreamTurn(createStreamTurnState(0), 10);
  const lateDelta = reduceNormalizedStreamEvent(
    completed,
    { type: "message_delta", content: "late" },
    { autoToolCallCounter: 0 },
    20,
  );
  expect(lateDelta.state.status).toBe("completed");
  expect(lateDelta.state.content).toBe(completed.content);

  const lateTool = reduceNormalizedStreamEvent(
    completed,
    { type: "tool_call_end", metadata: { tool_call_id: "t1" } },
    { autoToolCallCounter: 0 },
    20,
  );
  expect(lateTool.state.status).toBe("completed");
});

test("streaming markdown splits completed blocks without breaking code or math", () => {
  const blocks = splitStreamingMarkdownBlocks(
    "Intro\n\n```ts\nconst x = 1;\n\nconst y = 2;\n```\n\n$$\na + b\n\nc + d\n$$\n\nTail"
  );

  expect(blocks).toHaveLength(4);
  expect(blocks[1]).toContain("const y = 2");
  expect(blocks[2]).toContain("c + d");
  expect(blocks[3]).toBe("Tail");
});

test("streaming markdown keeps loose list continuations in one parse block", () => {
  const blocks = splitStreamingMarkdownBlocks(
    "Intro\n\n1. first paragraph\n\n   continuation of first item\n\n2. second item\n\nTail"
  );

  expect(blocks).toHaveLength(3);
  expect(blocks[1]).toContain("continuation of first item");
  expect(blocks[1]).toContain("2. second item");
});

test("streaming markdown keeps reference syntax in one parse block", () => {
  const text = "See [the guide][docs].\n\nMore context.\n\n[docs]: https://example.com";

  expect(splitStreamingMarkdownBlocks(text)).toEqual([text]);
});

type MockAssistantSession = {
  session_id: string;
  service_id: string;
  user_id: string;
  tenant_id: string;
  created_at: string;
  updated_at: string;
  metadata?: Record<string, unknown>;
  config?: Record<string, unknown>;
};

type MockPlaygroundSession = {
  session_id: string;
  service_id: string;
  created_at: string;
  updated_at: string;
  metadata?: Record<string, unknown>;
};

type MockAssistantModel = {
  id: string;
  name: string;
  provider: string;
  context_window: number;
  max_output_tokens: number;
  supports_vision: boolean;
  supports_tools: boolean;
};

type MockAssistantMessage = {
  role: "user" | "assistant";
  content: string;
  timestamp?: string;
  metadata?: Record<string, unknown>;
};

type MockAssistantArtifact = {
  artifact_id: string;
  type: string;
  format: string;
  title: string;
  filename: string;
  mime_type?: string;
  size_bytes?: number;
  source?: string;
  download_url?: string;
  created_at?: string;
};

function jsonResponse(body: unknown) {
  return {
    status: 200,
    contentType: "application/json",
    body: JSON.stringify(body),
  };
}

function pathSegmentAfter(url: string, segment: string): string {
  const parts = new URL(url).pathname.split("/");
  const index = parts.lastIndexOf(segment);
  return index >= 0 ? parts[index + 1] || "" : "";
}

function buildMockPlaygroundSession(
  sessionId: string,
  metadata?: Record<string, unknown>
): MockPlaygroundSession {
  const now = new Date().toISOString();
  return {
    session_id: sessionId,
    service_id: MOCK_PLAYGROUND_SERVICE_ID,
    created_at: now,
    updated_at: now,
    metadata,
  };
}

function buildMockAssistantSession(
  sessionId: string,
  metadata?: Record<string, unknown>,
  config?: Record<string, unknown>
): MockAssistantSession {
  const now = new Date().toISOString();
  return {
    session_id: sessionId,
    service_id: MOCK_ASSISTANT_SERVICE_ID,
    user_id: "e2e-user",
    tenant_id: "default",
    created_at: now,
    updated_at: now,
    metadata,
    config,
  };
}

export async function installAssistantHarness(
  page: Page,
  fulfillStream: (route: Route) => Promise<void>,
  options: {
    models?: MockAssistantModel[];
    defaultModelId?: string;
    availableProviders?: string[];
    preloadedSessions?: MockAssistantSession[];
    historyBySessionId?: Record<string, MockAssistantMessage[]>;
    artifactsBySessionId?: Record<string, MockAssistantArtifact[]>;
    createSessionDelayMs?: number;
    postCreateSessionListDelayMs?: number;
    releaseModels?: () => Promise<void>;
    onCreateShare?: (
      sessionId: string,
      payload: { include_artifacts?: boolean }
    ) => void;
  } = {}
) {
  let sessionCounter = 0;
  const sessions = new Map<string, MockAssistantSession>(
    (options.preloadedSessions || []).map((session) => [session.session_id, session])
  );
  const historyBySessionId = options.historyBySessionId || {};
  const artifactsBySessionId = options.artifactsBySessionId || {};
  const models = options.models ?? [
    {
      id: MOCK_ASSISTANT_MODEL_ID,
      name: "GPT-4o",
      provider: "openai",
      context_window: 128000,
      max_output_tokens: 4096,
      supports_vision: true,
      supports_tools: true,
    },
  ];
  const defaultModelId = options.defaultModelId ?? models[0]?.id ?? "";
  const availableProviders = options.availableProviders ?? [
    ...new Set(models.map((model) => model.provider)),
  ];

  await page.route("**/api/v1/assistant/models", async (route) => {
    if (options.releaseModels) {
      await options.releaseModels();
    }
    await route.fulfill(
      jsonResponse({
        models,
      })
    );
  });

  await page.route("**/api/v1/assistant/datasets", async (route) => {
    await route.fulfill(jsonResponse({ datasets: [] }));
  });

  await page.route("**/api/v1/assistant/config", async (route) => {
    await route.fulfill(
      jsonResponse({
        default_model_id: defaultModelId,
        available_providers: availableProviders,
        kb_enabled: false,
        web_search_enabled: true,
      })
    );
  });

  // The Local Node control plane moved off the /assistant prefix during the
  // Runtime cutover. Mock both paths: an unmocked 401 here trips the global
  // auth interceptor and redirects the page to /login mid-assertion.
  await page.route("**/api/v1/assistant/local-nodes*", async (route) => {
    await route.fulfill(jsonResponse({ devices: [] }));
  });

  await page.route("**/api/v1/local-nodes*", async (route) => {
    await route.fulfill(jsonResponse({ devices: [] }));
  });

  // Keep mocked Assistant tests independent from a running Gateway.  Without
  // this route, a live container can reject the synthetic E2E bearer token and
  // the global auth interceptor redirects the page to /login mid-assertion.
  await page.route("**/api/v1/connectors/available", async (route) => {
    await route.fulfill(jsonResponse([]));
  });

  await page.route("**/api/v1/assistant/sessions/*/artifacts", async (route) => {
    const sessionId = pathSegmentAfter(route.request().url(), "sessions");
    const artifacts = artifactsBySessionId[sessionId] || [];
    await route.fulfill(jsonResponse({ artifacts, total: artifacts.length }));
  });

  await page.route("**/api/v1/assistant/sessions/*/share", async (route) => {
    const sessionId = pathSegmentAfter(route.request().url(), "sessions");
    const payload =
      (route.request().postDataJSON() as { include_artifacts?: boolean } | null) || {};
    options.onCreateShare?.(sessionId, payload);
    const artifacts = payload.include_artifacts ? artifactsBySessionId[sessionId] || [] : [];
    await route.fulfill(
      jsonResponse({
        share_code: "e2e-share",
        share_url: "/share/e2e-share",
        title: sessions.get(sessionId)?.metadata?.title || null,
        message_count: historyBySessionId[sessionId]?.length || 0,
        artifact_count: artifacts.length,
        created_at: new Date().toISOString(),
        expires_at: null,
      })
    );
  });

  const listAssistantSessions = async (route: Route) => {
    if (sessionCounter > 0 && options.postCreateSessionListDelayMs) {
      await new Promise((resolve) =>
        setTimeout(resolve, options.postCreateSessionListDelayMs)
      );
    }
    await route.fulfill(jsonResponse(Array.from(sessions.values())));
  };

  await page.route("**/api/v1/sessions?*", async (route) => {
    const requestUrl = new URL(route.request().url());
    if (requestUrl.searchParams.get("service_id") !== MOCK_ASSISTANT_SERVICE_ID) {
      await route.fallback();
      return;
    }
    await listAssistantSessions(route);
  });

  await page.route("**/api/v1/sessions", async (route) => {
    if (route.request().method() === "GET") {
      await listAssistantSessions(route);
      return;
    }

    const payload =
      (route.request().postDataJSON() as {
        metadata?: Record<string, unknown>;
        config?: Record<string, unknown>;
      } | null) || null;
    const sessionId = `e2e-assistant-session-${++sessionCounter}`;
    const session = buildMockAssistantSession(sessionId, payload?.metadata, payload?.config);
    sessions.set(sessionId, session);
    if (options.createSessionDelayMs) {
      await new Promise((resolve) => setTimeout(resolve, options.createSessionDelayMs));
    }
    await route.fulfill(jsonResponse({ session_id: sessionId }));
  });

  await page.route("**/api/v1/sessions/*/history?*", async (route) => {
    const sessionId = pathSegmentAfter(route.request().url(), "sessions");
    await route.fulfill(jsonResponse(historyBySessionId[sessionId] || []));
  });

  // The Assistant reads history from its own prefix and reconciles once at
  // turn terminal. Leaving this unmocked returns 401, and the global auth
  // interceptor then redirects the page to /login after every mocked stream.
  await page.route("**/api/v1/assistant/sessions/*/history*", async (route) => {
    const sessionId = pathSegmentAfter(route.request().url(), "sessions");
    const messages = historyBySessionId[sessionId] || [];
    await route.fulfill(
      jsonResponse({ session_id: sessionId, messages, total: messages.length }),
    );
  });

  await page.route("**/api/v1/sessions/*", async (route) => {
    const request = route.request();
    const sessionId = request.url().split("/").pop() || "";

    if (request.method() === "GET") {
      const session = sessions.get(sessionId);
      await route.fulfill(jsonResponse(session || {}));
      return;
    }

    const existing = sessions.get(sessionId);
    if (!existing) {
      await route.fulfill(jsonResponse({}));
      return;
    }

    const payload =
      (request.postDataJSON() as {
        metadata?: Record<string, unknown>;
        config?: Record<string, unknown>;
      } | null) || null;
    const nextSession: MockAssistantSession = {
      ...existing,
      updated_at: new Date().toISOString(),
      metadata: {
        ...(existing.metadata || {}),
        ...(payload?.metadata || {}),
      },
      config: {
        ...(existing.config || {}),
        ...(payload?.config || {}),
      },
    };
    sessions.set(sessionId, nextSession);
    await route.fulfill(jsonResponse(nextSession));
  });

  await page.route("**/api/v1/assistant/chat/stream", fulfillStream);

  const fulfillV2Events = async (route: Route) => {
    let captured:
      | {
          status?: number;
          headers?: Record<string, string>;
          body?: string;
        }
      | undefined;
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
    await fulfillStream(capturingRoute);
    await route.fulfill({
      status: captured?.status ?? 200,
      headers: {
        ...(captured?.headers || {}),
        "content-type": "text/event-stream",
      },
      body: wrapAssistantSseAsAgentV2(captured?.body || toSseBody([])),
    });
  };

  await page.route("**/api/v2/agent/threads/*/events**", fulfillV2Events);
  await page.route("**/api/v2/agent/threads/*/turns", async (route) => {
    if (route.request().method() !== "POST") {
      await route.fallback();
      return;
    }
    await route.fulfill(
      jsonResponse({
        turn: {
          id: "e2e-turn",
          events_url: "/api/v2/agent/threads/e2e-runtime-thread/events?turn_id=e2e-turn",
        },
      }),
    );
  });
  // Fired from the stream generator's `finally` on a non-terminal EOF. It is
  // best-effort in the client, but an unmocked 401 still trips the global auth
  // interceptor and redirects the page to /login.
  await page.route("**/api/v2/agent/threads/*/turns/*", async (route) => {
    await route.fulfill(jsonResponse({ status: "accepted" }));
  });

  await page.route("**/api/v2/agent/threads/*/approvals/*/decision", async (route) => {
    await route.fulfill(
      jsonResponse({
        schema_version: "agent-approval/v2",
        approval: { status: "approved", approved: true },
      }),
    );
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
      jsonResponse({
        thread: {
          schema_version: "agent-thread/v2",
          id: "e2e-runtime-thread",
          thread_id: "e2e-runtime-thread",
          session_id: sessionId,
          import_status: "not_required",
          last_sequence: 0,
          runtime: { owner: "agent_runtime", source: "native" },
        },
      }),
    );
  });
}

test("composer sends with the cached last model before the catalog resolves", async ({
  page,
}) => {
  test.setTimeout(20_000);

  let releaseModels!: () => void;
  const modelsGate = new Promise<void>((resolve) => {
    releaseModels = resolve;
  });
  let modelsRequested = false;
  page.on("request", (request) => {
    if (new URL(request.url()).pathname === "/api/v1/assistant/models") {
      modelsRequested = true;
    }
  });

  await seedClientPrefs(page, { locale: "en-US" });
  await installClientAuth(page, {
    user_id: "e2e-assistant-composer-unlock-user",
    email: "assistant-composer-unlock@example.com",
    display_name: "Composer Unlock",
  });
  await page.addInitScript(
    ({ key, modelId }) => {
      localStorage.setItem(key, modelId);
    },
    { key: LAST_MODEL_STORAGE_KEY, modelId: MOCK_ASSISTANT_MODEL_ID }
  );

  await installAssistantHarness(
    page,
    async (route) => {
      await route.fulfill({
        status: 200,
        headers: { "content-type": "text/event-stream" },
        body: toSseBody([
          { event_type: "started", data: { request_id: "composer-unlock-e2e" } },
          { event_type: "text_delta", data: "Catalog-free first message" },
          { event_type: "done", data: { duration_ms: 50, total_tokens: 4 } },
          { event_type: "run_finished", data: { run_id: "composer-unlock-e2e" } },
        ]),
      });
    },
    {
      releaseModels: () => modelsGate,
    }
  );

  await page.goto("/assistant");
  await expect.poll(() => modelsRequested, { timeout: 10_000 }).toBeTruthy();

  const composer = page.locator(`#${ASSISTANT_COMPOSER_ID}`);
  await composer.fill("send before catalog");
  await expect(composer).toBeEnabled();
  const sendButton = page.locator('button[aria-label="Send"]');
  await expect(sendButton).toBeEnabled();

  await composer.press("Enter");
  await expect(page.getByText("Catalog-free first message")).toBeVisible({
    timeout: 2_000,
  });

  releaseModels();
  await expect(page.getByText("GPT-4o")).toBeVisible({ timeout: 2_000 });
  await composer.fill("still unlocked after catalog");
  await expect(sendButton).toBeEnabled();
});

test("composer stays locked when the catalog loads empty and no model is cached", async ({
  page,
}) => {
  test.setTimeout(20_000);

  await seedClientPrefs(page, { locale: "en-US" });
  await installClientAuth(page, {
    user_id: "e2e-assistant-no-model-cache-user",
    email: "assistant-no-model-cache@example.com",
    display_name: "No Model Cache",
  });
  await installAssistantHarness(page, async () => {}, {
    models: [],
    defaultModelId: "",
  });

  await page.goto("/assistant");

  const composer = page.locator(`#${ASSISTANT_COMPOSER_ID}`);
  await expect(composer).toBeDisabled();
  await expect(page.locator('button[aria-label="Send"]')).toBeDisabled();
});

async function installPlaygroundHarness(page: Page) {
  let sessionCounter = 0;
  let streamHits = 0;
  const sessions = new Map<string, MockPlaygroundSession>();

  const listSessions = async (route: Route) => {
    const requestUrl = new URL(route.request().url());
    const requestedServiceId = requestUrl.searchParams.get("service_id");
    const items = Array.from(sessions.values()).filter((session) =>
      requestedServiceId ? session.service_id === requestedServiceId : true
    );
    await route.fulfill(jsonResponse(items));
  };

  await page.route("**/api/v1/proxy", async (route) => {
    await route.fulfill(
      jsonResponse({
        services: [
          {
            service_id: MOCK_PLAYGROUND_SERVICE_ID,
            service_name: "Playwright Mock Agent",
            enabled: true,
            service_type: "langgraph",
            metadata: {
              adapter_type: "langgraph",
              proxy_mode: "transparent",
            },
          },
        ],
      })
    );
  });

  await page.route("**/api/v1/sessions?*", listSessions);
  await page.route("**/api/v1/sessions", async (route) => {
    if (route.request().method() === "GET") {
      await listSessions(route);
      return;
    }

    const payload =
      (route.request().postDataJSON() as { metadata?: Record<string, unknown> } | null) || null;
    const sessionId = `e2e-playground-session-${++sessionCounter}`;
    const session = buildMockPlaygroundSession(sessionId, payload?.metadata);
    sessions.set(sessionId, session);
    await route.fulfill(jsonResponse(session));
  });

  await page.route("**/api/v1/sessions/*", async (route) => {
    const sessionId = route.request().url().split("/").pop() || "";
    const existing = sessions.get(sessionId);
    const payload =
      (route.request().postDataJSON() as { metadata?: Record<string, unknown> } | null) || null;

    if (!existing) {
      await route.fulfill(jsonResponse({}));
      return;
    }

    const nextSession: MockPlaygroundSession = {
      ...existing,
      updated_at: new Date().toISOString(),
      metadata: {
        ...(existing.metadata || {}),
        ...(payload?.metadata || {}),
      },
    };
    sessions.set(sessionId, nextSession);
    await route.fulfill(jsonResponse(nextSession));
  });

  await page.route("**/api/v1/sessions/*/messages", async (route) => {
    await route.fulfill(jsonResponse({ ok: true }));
  });

  await page.route(
    `**/api/v1/proxy/${MOCK_PLAYGROUND_SERVICE_ID}/threads`,
    async (route) => {
      await route.fulfill(jsonResponse({ thread_id: MOCK_PLAYGROUND_THREAD_ID }));
    }
  );

  await page.route(
    `**/api/v1/proxy/${MOCK_PLAYGROUND_SERVICE_ID}/runs/wait`,
    async (route) => {
      await route.fulfill(
        jsonResponse({
          messages: [
            {
              type: "tool",
              role: "tool",
              tool_call_id: MOCK_PLAYGROUND_TOOL_ID,
              name: "web_search",
              content: "ok",
            },
            {
              type: "AIMessage",
              role: "assistant",
              content: "Playground mock response",
              tool_calls: [
                {
                  id: MOCK_PLAYGROUND_TOOL_ID,
                  name: "web_search",
                  args: { query: "playwright" },
                },
              ],
            },
          ],
        })
      );
    }
  );

  await page.route(
    `**/api/v1/proxy/${MOCK_PLAYGROUND_SERVICE_ID}/threads/${MOCK_PLAYGROUND_THREAD_ID}/runs/wait`,
    async (route) => {
      await route.fulfill(
        jsonResponse({
          messages: [
            {
              type: "tool",
              role: "tool",
              tool_call_id: MOCK_PLAYGROUND_TOOL_ID,
              name: "web_search",
              content: "ok",
            },
            {
              type: "AIMessage",
              role: "assistant",
              content: "Playground mock response",
              tool_calls: [
                {
                  id: MOCK_PLAYGROUND_TOOL_ID,
                  name: "web_search",
                  args: { query: "playwright" },
                },
              ],
            },
          ],
        })
      );
    }
  );

  return {
    recordStreamHit() {
      streamHits += 1;
    },
    getStreamHits() {
      return streamHits;
    },
  };
}

async function selectFirstPlaygroundService(page: Page) {
  const serviceSelect = page.locator('button[role="combobox"]').first();
  await expect(serviceSelect).toBeVisible();
  await serviceSelect.click();
  const firstOption = page.locator('[role="option"]').first();
  const hasOptions = await firstOption
    .waitFor({ state: "visible", timeout: 5000 })
    .then(() => true)
    .catch(() => false);
  test.skip(!hasOptions, "No playground services configured");
  await firstOption.click();
}

test("assistant stream path keeps a11y and performance budget", async ({ page }) => {
  await ensureAuthenticatedPage(page, "/assistant");
  await installTelemetryCollector(page);
  const composer = page.locator(`#${ASSISTANT_COMPOSER_ID}`);
  await expect(composer).toBeVisible();
  await expect(composer).toBeEnabled();

  await page.keyboard.press(`${modKey()}+K`);
  await expect(composer).toBeFocused();

  await assertNoBlockingA11yIssues(page, [
    `#${ASSISTANT_COMPOSER_ID}`,
    '[role="log"]',
  ]);

  // Say what the turn should do. Handed a bare token the model reaches for a
  // tool to make sense of it — observed: `execute_python_code` — and the turn
  // then parks on an approval instead of completing.
  const prompt = `e2e-assistant-${Date.now()}: reply with exactly "ok" and use no tools`;
  await composer.fill(prompt);
  await composer.press("Enter");
  await expect(page.getByText(prompt)).toBeVisible();

  const log = page.locator('[role="log"]');
  await expect(log).toBeVisible();
  const approve = page.getByRole("button", { name: /^(Approve|通过)$/ });
  await expect
    .poll(
      async () => {
        // A live model may still request a write capability; grant it rather
        // than time out on a run that is deliberately waiting for an operator.
        if ((await approve.count()) > 0) {
          await approve.first().click();
        }
        const events = await readTelemetryEvents(page);
        return events.find((event) => event.event === "chat.stream.finished")?.payload?.outcome;
      },
      { timeout: 120_000 }
    )
    .toBe("completed");
  await expect(log).not.toContainText("The response stream was interrupted");
  await expect(composer).toBeEnabled();
  await assertInpBudget(page);
});

test("assistant applies persisted locale + theme before interaction", async ({ page }) => {
  await seedClientPrefs(page, {
    locale: "en-US",
    themeMode: "dark",
    resolvedTheme: "dark",
    darkMode: true,
  });

  await ensureAuthenticatedPage(page, "/assistant");

  await expect(page.locator("html")).toHaveAttribute("lang", "en-US");
  await expect(page.locator("html")).toHaveAttribute("data-theme", "dark");
  const colorScheme = await page.evaluate(() => document.documentElement.style.colorScheme);
  expect(colorScheme).toBe("dark");
});

test("assistant route admits permitted users", async ({ page }) => {
  await installClientAuth(page, {
    user_id: "e2e-assistant-route-user",
    email: "assistant-route@example.com",
    display_name: "Assistant Route",
    roles: ["user"],
    permissions: ["console:dashboard:view", "conversation:playground:access"],
    effective_permissions: ["console:dashboard:view", "conversation:playground:access"],
  });
  await installAssistantHarness(page, async (route) => {
    await route.fulfill({
      status: 200,
      headers: { "content-type": "text/event-stream" },
      body: toSseBody([{ event_type: "done", data: { duration_ms: 0 } }]),
    });
  });

  await page.goto("/assistant");
  await expect(page).toHaveURL(/\/assistant/);
  await expect(page.locator(`#${ASSISTANT_COMPOSER_ID}`)).toBeVisible();
  await expect(page.locator('a[href="/assistant"]')).toBeVisible();
});

test("assistant shows the active Confluence connector count", async ({ page }) => {
  await seedClientPrefs(page, { locale: "en-US" });
  await installClientAuth(page, {
    user_id: "e2e-connector-count-user",
    email: "connector-count@example.com",
    display_name: "Connector Count",
    roles: ["user"],
    permissions: ["console:dashboard:view", "conversation:playground:access"],
    effective_permissions: ["console:dashboard:view", "conversation:playground:access"],
  });
  await installAssistantHarness(page, async (route) => {
    await route.fulfill({
      status: 200,
      headers: { "content-type": "text/event-stream" },
      body: toSseBody([{ event_type: "done", data: { duration_ms: 0 } }]),
    });
  });
  await page.route("**/api/v1/connectors/available", async (route) => {
    await route.fulfill(jsonResponse([
      { provider: "confluence", display_name: "Confluence", description: "Atlassian wiki", enabled: true, connected: true },
      { provider: "github", display_name: "GitHub", description: "Code and issues", enabled: true, connected: true },
    ]));
  });

  await page.goto("/assistant");
  await page.locator("button:has(svg.lucide-plus)").last().click();
  await expect(page.getByRole("button", { name: /connectors|连接器/i })).toContainText("2");
});

test("assistant route keeps model testers playground-only", async ({ page }) => {
  await installClientAuth(page, {
    user_id: "e2e-model-tester-route-user",
    email: "model-tester-route@example.com",
    display_name: "Model Tester Route",
    roles: ["model_tester"],
    permissions: ["conversation:playground:access"],
    effective_permissions: ["conversation:playground:access"],
  });
  await installPlaygroundHarness(page);

  await page.goto("/assistant");
  await expect(page).toHaveURL(/\/playground/);
  await expect(page.locator('a[href="/assistant"]')).toHaveCount(0);
  await expect(page.locator(`#${PLAYGROUND_COMPOSER_ID}`)).toBeVisible();
});

test("assistant disables sending when no models are available", async ({ page }) => {
  await seedClientPrefs(page, { locale: "en-US" });
  await installClientAuth(page, {
    user_id: "e2e-assistant-no-model-user",
    email: "assistant-no-model@example.com",
    display_name: "Assistant No Model",
    roles: ["user"],
    permissions: ["console:dashboard:view", "conversation:playground:access"],
    effective_permissions: ["console:dashboard:view", "conversation:playground:access"],
  });
  let streamHits = 0;
  await installAssistantHarness(
    page,
    async (route) => {
      streamHits += 1;
      await route.fulfill({
        status: 200,
        headers: { "content-type": "text/event-stream" },
        body: toSseBody([{ event_type: "done", data: { duration_ms: 0 } }]),
      });
    },
    { models: [], defaultModelId: "", availableProviders: [] }
  );

  await page.goto("/assistant");
  await expect(page).toHaveURL(/\/assistant/);

  const composer = page.locator(`#${ASSISTANT_COMPOSER_ID}`);
  await expect(composer).toBeVisible();
  await expect(composer).toBeDisabled();
  await expect(composer).toHaveAttribute("placeholder", "No models available");

  const sendButton = page.locator('button[aria-keyshortcuts*="Enter"]').last();
  await expect(sendButton).toBeDisabled();
  await page.keyboard.press("Enter");
  expect(streamHits).toBe(0);
});

test("assistant ignores empty and duplicate same-tick Enter submissions", async ({ page }) => {
  await seedClientPrefs(page, { locale: "en-US" });
  await installClientAuth(page, {
    user_id: "e2e-assistant-send-guard-user",
    email: "assistant-send-guard@example.com",
    display_name: "Assistant Send Guard",
  });
  let streamHits = 0;
  await installAssistantHarness(page, async (route) => {
    streamHits += 1;
    await route.fulfill({
      status: 200,
      headers: { "content-type": "text/event-stream" },
      body: toSseBody([
        { event_type: "text_delta", data: "Guarded response" },
        { event_type: "done", data: { duration_ms: 10 } },
      ]),
    });
  });

  await page.goto("/assistant");
  const composer = page.locator(`#${ASSISTANT_COMPOSER_ID}`);
  await expect(composer).toBeEnabled();

  await composer.press("Enter");
  expect(streamHits).toBe(0);

  await composer.fill("send once");
  await composer.evaluate((element) => {
    element.dispatchEvent(new KeyboardEvent("keydown", { key: "Enter", bubbles: true }));
    element.dispatchEvent(new KeyboardEvent("keydown", { key: "Enter", bubbles: true }));
  });

  await expect.poll(() => streamHits).toBe(1);
  await expect(page.getByText("send once", { exact: true })).toHaveCount(1);
});

test("assistant mobile history uses a bounded overlay sheet", async ({ page }) => {
  await page.setViewportSize({ width: 390, height: 844 });
  await seedClientPrefs(page, { locale: "en-US" });
  await installClientAuth(page, {
    user_id: "e2e-assistant-mobile-history-user",
    email: "assistant-mobile-history@example.com",
    display_name: "Assistant Mobile History",
  });
  const session = buildMockAssistantSession("mobile-history-session", {
    title: "Mobile history conversation",
  });
  await installAssistantHarness(
    page,
    async (route) => {
      await route.fulfill({
        status: 200,
        headers: { "content-type": "text/event-stream" },
        body: toSseBody([{ event_type: "done", data: { duration_ms: 0 } }]),
      });
    },
    { preloadedSessions: [session] },
  );

  await page.goto("/assistant");
  const historyToggle = page.getByRole("button", { name: /show history/i });
  await expect(historyToggle).toBeVisible();
  await historyToggle.click();

  const historySheet = page.getByRole("dialog", { name: /history/i });
  await expect(historySheet).toBeVisible();
  const bounds = await historySheet.boundingBox();
  expect(bounds).not.toBeNull();
  expect(bounds!.width).toBeLessThanOrEqual(390);
  expect(bounds!.width).toBeGreaterThan(300);

  await page.keyboard.press("Escape");
  await expect(historySheet).toBeHidden();
});

test("assistant deletes a session through the runtime cleanup route", async ({ page }) => {
  test.setTimeout(30_000);
  await seedClientPrefs(page, { locale: "en-US" });
  await installClientAuth(page, {
    user_id: "e2e-assistant-delete-user",
    email: "assistant-delete@example.com",
    display_name: "Assistant Delete",
  });
  const title = "Delete through assistant runtime";
  const session = buildMockAssistantSession("delete-runtime-session", { title });
  await installAssistantHarness(
    page,
    async (route) => {
      await route.fulfill({
        status: 200,
        headers: { "content-type": "text/event-stream" },
        body: toSseBody([{ event_type: "done", data: { duration_ms: 0 } }]),
      });
    },
    { preloadedSessions: [session] }
  );
  let deleteHits = 0;
  await page.route(
    "**/api/v1/assistant/sessions/delete-runtime-session",
    async (route) => {
      deleteHits += 1;
      await route.fulfill(jsonResponse({ status: "deleted" }));
    }
  );

  await page.goto("/assistant");
  await page.getByLabel("Show history").click();
  await page.getByRole("button", { name: `Delete: ${title}` }).click();

  await expect(page.getByRole("button", { name: `Delete: ${title}` })).toHaveCount(0);
  expect(deleteHits).toBe(1);
});

test("assistant emits stream telemetry lifecycle on mocked stream", async ({ page }) => {
  await installAssistantHarness(page, async (route) => {
    const body = toSseBody([
      { event_type: "started", data: { request_id: "e2e-mock" } },
      { event_type: "thinking_start", data: {} },
      { event_type: "thinking_delta", data: "Checking the request." },
      { event_type: "thinking_end", data: { content: "Checking the request." } },
      { event_type: "text_delta", data: "Mocked " },
      {
        event_type: "tool_call_start",
        data: {
          tool_call_id: "tool-1",
          tool_name: "search_docs",
          arguments: '{"query":"mock"}',
        },
      },
      {
        event_type: "tool_call_result",
        data: {
          tool_call_id: "tool-1",
          tool_name: "search_docs",
          result: "ok",
          success: true,
        },
      },
      { event_type: "text_delta", data: "stream response" },
      { event_type: "done", data: { duration_ms: 120, total_tokens: 42 } },
      { event_type: "run_finished", data: { run_id: "run-e2e-mock" } },
    ]);
    await route.fulfill({
      status: 200,
      headers: { "content-type": "text/event-stream" },
      body,
    });
  });

  await ensureAuthenticatedPage(page, "/assistant");
  await installTelemetryCollector(page);

  const composer = page.locator(`#${ASSISTANT_COMPOSER_ID}`);
  await composer.fill(`e2e-mock-${Date.now()}`);
  await composer.press("Enter");

  await expect(page.getByText("Mocked stream response")).toBeVisible();

  const events = await readTelemetryEvents(page);
  const names = events.map((event) => event.event);
  expect(names).toContain("chat.stream.started");
  expect(names).toContain("chat.stream.first_token");
  expect(names).toContain("chat.stream.first_text_token");
  expect(names).toContain("chat.stream.finished");

  const firstToken = events.find((event) => event.event === "chat.stream.first_token");
  const firstTextToken = events.find(
    (event) => event.event === "chat.stream.first_text_token"
  );
  expect(Number(firstTextToken?.payload?.firstTextTokenMs)).toBeGreaterThanOrEqual(
    Number(firstToken?.payload?.firstTokenMs)
  );

  const finishedEvent = events.find((event) => event.event === "chat.stream.finished");
  expect(finishedEvent?.payload?.outcome).toBe("completed");
});

test("assistant preserves reasoning when provider omits thinking start and end", async ({ page }) => {
  const reasoningSummary = "Provider reasoning summary without lifecycle markers.";
  // The shared storage state pins i18nextLng=zh-CN, so English locators below
  // need an explicit locale like every other test in this file.
  await seedClientPrefs(page, { locale: "en-US" });
  await installAssistantHarness(page, async (route) => {
    await route.fulfill({
      status: 200,
      headers: { "content-type": "text/event-stream" },
      body: toSseBody([
        { event_type: "run_started", data: { run_id: "run-reasoning-summary" } },
        { event_type: "thinking_delta", data: reasoningSummary },
        { event_type: "text_delta", data: "Reasoning persisted." },
        { event_type: "run_finished", data: { run_id: "run-reasoning-summary" } },
      ]),
    });
  });

  await ensureAuthenticatedPage(page, "/assistant");
  const composer = page.locator(`#${ASSISTANT_COMPOSER_ID}`);
  await composer.fill(`reasoning-summary-${Date.now()}`);
  await composer.press("Enter");

  await expect(page.getByText("Reasoning persisted.")).toBeVisible();
  await page.getByRole("button", { name: /Activity/ }).last().click();
  await expect(page.getByText(reasoningSummary)).toBeVisible();
  await expect(page.getByText(/1 step/)).toBeVisible();
});

test("assistant new-session stream is not blocked by delayed sidebar refresh", async ({ page }) => {
  test.setTimeout(20_000);
  const createSessionDelayMs = 250;
  const sessionListDelayMs = 5_000;
  let initialListResponses = 0;
  let sessionCreateRequested = false;
  let chatRequestedAt: number | null = null;
  let postCreateListStartedAt: number | null = null;
  let postCreateListFinishedAt: number | null = null;

  page.on("request", (request) => {
    const path = new URL(request.url()).pathname;
    if (path === "/api/v1/sessions" && request.method() === "POST") {
      sessionCreateRequested = true;
    } else if (
      path === "/api/v1/sessions" &&
      request.method() === "GET" &&
      sessionCreateRequested
    ) {
      postCreateListStartedAt ??= Date.now();
    } else if (
      path === "/api/v1/assistant/chat/stream" ||
      (path.startsWith("/api/v2/agent/threads/") && path.endsWith("/events"))
    ) {
      // The turn streams through the V2 events cursor now; keying this probe
      // on the retired V1 path left `chatRequestedAt` null forever.
      chatRequestedAt ??= Date.now();
    }
  });
  page.on("response", (response) => {
    const request = response.request();
    const path = new URL(request.url()).pathname;
    if (path !== "/api/v1/sessions" || request.method() !== "GET") return;
    if (sessionCreateRequested) {
      postCreateListFinishedAt ??= Date.now();
    } else {
      initialListResponses += 1;
    }
  });

  await seedClientPrefs(page, { locale: "en-US" });
  await installClientAuth(page, {
    user_id: "e2e-assistant-session-latency-user",
    email: "assistant-session-latency@example.com",
    display_name: "Assistant Session Latency",
  });
  await installAssistantHarness(
    page,
    async (route) => {
      await route.fulfill({
        status: 200,
        headers: { "content-type": "text/event-stream" },
        body: toSseBody([
          { event_type: "started", data: { request_id: "session-latency-e2e" } },
          { event_type: "text_delta", data: "Sidebar-independent response" },
          { event_type: "done", data: { duration_ms: 50, total_tokens: 4 } },
          { event_type: "run_finished", data: { run_id: "session-latency-e2e" } },
        ]),
      });
    },
    {
      createSessionDelayMs,
      postCreateSessionListDelayMs: sessionListDelayMs,
    }
  );

  await page.goto("/assistant");
  await expect.poll(() => initialListResponses).toBeGreaterThan(0);
  await installTelemetryCollector(page);

  const composer = page.locator(`#${ASSISTANT_COMPOSER_ID}`);
  await composer.fill("new session latency regression");
  const sentAt = Date.now();
  await composer.press("Enter");

  await expect(page.getByText("Sidebar-independent response")).toBeVisible({ timeout: 2_000 });
  await expect.poll(() => chatRequestedAt, { timeout: 2_000 }).not.toBeNull();
  await expect.poll(() => postCreateListStartedAt, { timeout: 2_000 }).not.toBeNull();
  expect(chatRequestedAt! - sentAt).toBeLessThan(2_000);
  expect(postCreateListFinishedAt).toBeNull();

  await expect
    .poll(
      async () => {
        const events = await readTelemetryEvents(page);
        return events.find((event) => event.event === "chat.stream.first_token") || null;
      },
      { timeout: 2_000 }
    )
    .not.toBeNull();
  const telemetryEvents = await readTelemetryEvents(page);
  const firstTokenPayload = telemetryEvents.find(
    (event) => event.event === "chat.stream.first_token"
  )?.payload;
  const streamFirstTokenMs = Number(firstTokenPayload?.firstTokenMs);
  const interactionToFirstTokenMs = Number(firstTokenPayload?.interactionToFirstTokenMs);
  expect(Number.isFinite(streamFirstTokenMs)).toBeTruthy();
  expect(Number.isFinite(interactionToFirstTokenMs)).toBeTruthy();
  expect(interactionToFirstTokenMs - streamFirstTokenMs).toBeLessThan(80);

  await expect
    .poll(() => postCreateListFinishedAt, { timeout: sessionListDelayMs + 3_000 })
    .not.toBeNull();
  expect(postCreateListFinishedAt! - postCreateListStartedAt!).toBeGreaterThanOrEqual(
    sessionListDelayMs - 100
  );
});

test("assistant fails closed on run_error and preserves partial text", async ({ page }) => {
  await seedClientPrefs(page, { locale: "en-US" });
  await installClientAuth(page, {
    user_id: "e2e-assistant-run-error-user",
    email: "assistant-run-error@example.com",
    display_name: "Assistant Run Error",
  });
  await installAssistantHarness(page, async (route) => {
    await route.fulfill({
      status: 200,
      headers: { "content-type": "text/event-stream" },
      body: toSseBody([
        { event_type: "run_started", data: { run_id: "run-error-e2e" } },
        { event_type: "text_delta", data: "Useful partial answer." },
        {
          event_type: "run_error",
          data: {
            run_id: "run-error-e2e",
            error: "provider_failed",
          },
        },
      ]),
    });
  });

  await page.goto("/assistant");
  await installTelemetryCollector(page);
  const composer = page.locator(`#${ASSISTANT_COMPOSER_ID}`);
  await composer.fill("trigger run error");
  await composer.press("Enter");

  const log = page.locator('[role="log"]');
  await expect(log).toContainText("Useful partial answer.");
  await expect(log).toContainText("The response stream was interrupted");
  await expect(composer).toBeEnabled();
  await expect
    .poll(async () => {
      const events = await readTelemetryEvents(page);
      return events.find((event) => event.event === "chat.stream.finished")?.payload?.outcome;
    })
    .toBe("failed");
});

test("assistant treats EOF before a terminal event as interrupted", async ({ page }) => {
  await seedClientPrefs(page, { locale: "en-US" });
  await installClientAuth(page, {
    user_id: "e2e-assistant-eof-user",
    email: "assistant-eof@example.com",
    display_name: "Assistant EOF",
  });
  await installAssistantHarness(page, async (route) => {
    await route.fulfill({
      status: 200,
      headers: { "content-type": "text/event-stream" },
      body: toSseBody([
        { event_type: "run_started", data: { run_id: "run-eof-e2e" } },
        { event_type: "text_delta", data: "Partial before EOF." },
      ]),
    });
  });

  await page.goto("/assistant");
  await installTelemetryCollector(page);
  const composer = page.locator(`#${ASSISTANT_COMPOSER_ID}`);
  await composer.fill("trigger incomplete stream");
  await composer.press("Enter");

  const log = page.locator('[role="log"]');
  await expect(log).toContainText("Partial before EOF.");
  await expect(log).toContainText("The response stream was interrupted");
  await expect(composer).toBeEnabled();
  await expect
    .poll(async () => {
      const events = await readTelemetryEvents(page);
      return events.find((event) => event.event === "chat.stream.finished")?.payload?.outcome;
    })
    .toBe("failed");
});

test("assistant treats done followed by EOF before a run terminal as interrupted", async ({ page }) => {
  await seedClientPrefs(page, { locale: "en-US" });
  await installClientAuth(page, {
    user_id: "e2e-assistant-done-eof-user",
    email: "assistant-done-eof@example.com",
    display_name: "Assistant Done EOF",
  });
  await installAssistantHarness(page, async (route) => {
    await route.fulfill({
      status: 200,
      headers: { "content-type": "text/event-stream" },
      body: toSseBody([
        { event_type: "run_started", data: { run_id: "run-done-eof-e2e" } },
        { event_type: "text_delta", data: "Partial before transport done." },
        {
          event_type: "done",
          data: { run_id: "run-done-eof-e2e", duration_ms: 42, total_tokens: 12 },
        },
      ]),
    });
  });

  await page.goto("/assistant");
  await installTelemetryCollector(page);
  const composer = page.locator(`#${ASSISTANT_COMPOSER_ID}`);
  await composer.fill("trigger transport done without run terminal");
  await composer.press("Enter");

  const log = page.locator('[role="log"]');
  await expect(log).toContainText("Partial before transport done.");
  await expect(log).toContainText("The response stream was interrupted");
  await expect(composer).toBeEnabled();
  await expect
    .poll(async () => {
      const events = await readTelemetryEvents(page);
      return events.find((event) => event.event === "chat.stream.finished")?.payload?.outcome;
    })
    .toBe("failed");
});

test("assistant renders the complete sub-agent status contract", async ({ page }) => {
  await page.setViewportSize({ width: 390, height: 844 });
  await seedClientPrefs(page, {
    locale: "en-US",
    themeMode: "dark",
    resolvedTheme: "dark",
    darkMode: true,
  });
  await installClientAuth(page, {
    user_id: "e2e-subagent-status-user",
    email: "subagent-status@example.com",
    display_name: "Sub-agent Status",
  });
  await installAssistantHarness(page, async (route) => {
    const started = (agentId: string, description: string) => ({
      event_type: "subagent_started",
      data: {
        agent_id: agentId,
        agent_type: "task",
        description,
        prompt: `Handle ${description}`,
        profile_id: `profile-${agentId}`,
        profile_name: `${description} agent`,
        delegation_id: "status-contract-batch",
      },
    });
    const finished = (
      agentId: string,
      status: string,
      options: {
        error?: string;
        limitations?: string[];
        resultSummary?: string;
        evidenceSummary?: string;
      } = {},
    ) => ({
      event_type: "subagent_finished",
      data: {
        agent_id: agentId,
        status,
        result_summary: options.resultSummary,
        error: options.error,
        duration_ms: 42,
        turns: 1,
        tool_calls: 1,
        result: {
          status,
          limitations: options.limitations || [],
          evidence: options.evidenceSummary
            ? [{ evidence_id: `evidence-${agentId}`, tool_name: "search_docs", summary: options.evidenceSummary }]
            : [],
          usage: { model_turns: 1, tool_calls: 1 },
        },
      },
    });

    await route.fulfill({
      status: 200,
      headers: { "content-type": "text/event-stream" },
      body: toSseBody([
        { event_type: "started", data: { request_id: "subagent-status-contract" } },
        started("sub-running", "Running research"),
        {
          event_type: "subagent_step",
          data: { agent_id: "sub-running", step: "Turn 1/3", status: "running" },
        },
        started("sub-completed", "Completed research"),
        {
          event_type: "subagent_step",
          data: { agent_id: "sub-completed", step: "Verify sources", status: "running" },
        },
        {
          event_type: "subagent_tool_start",
          data: { agent_id: "sub-completed", tool_name: "search_docs", call_id: "search-1" },
        },
        {
          event_type: "subagent_tool_result",
          data: {
            agent_id: "sub-completed",
            call_id: "search-1",
            success: true,
            summary: "Two sources verified",
          },
        },
        finished("sub-completed", "completed", {
          resultSummary: "Completed with verified evidence.",
          evidenceSummary: "Two sources verified",
        }),
        started("sub-failed", "Failed research"),
        finished("sub-failed", "failed", { error: "Provider unavailable" }),
        started("sub-cancelled", "Cancelled research"),
        finished("sub-cancelled", "cancelled", { error: "Cancelled by parent" }),
        started("sub-blocked", "Blocked research"),
        finished("sub-blocked", "blocked", {
          error: "Approval required",
          limitations: ["Blocked on operator approval"],
        }),
        started("sub-partial", "Partial research"),
        finished("sub-partial", "partial", {
          resultSummary: "Available evidence is incomplete.",
          limitations: ["Partial data only"],
        }),
        started("sub-unknown", "Unknown terminal research"),
        finished("sub-unknown", "future_terminal"),
        { event_type: "text_delta", data: "Sub-agent status contract rendered." },
        { event_type: "done", data: { duration_ms: 80 } },
      ]),
    });
  });

  await page.goto("/assistant");
  const composer = page.locator(`#${ASSISTANT_COMPOSER_ID}`);
  await composer.fill(`subagent-status-${Date.now()}`);
  await composer.press("Enter");
  // `RightPanelChip` sets an explicit `aria-label` of "<label> (<count>)",
  // which wins over the visible "Agents 7" content when the accessible
  // name is computed.
  const agentsLauncher = page.getByRole("button", { name: /^Agents \(7\)$/ });
  await agentsLauncher.click();
  await expect(page.getByRole("dialog", { name: "Sub-agent workbench" })).toBeVisible();
  await expect(page.getByTestId("subagent-workbench")).toBeVisible();
  await expect(page.getByRole("button", { name: "Close sub-agent workbench" })).toBeFocused();
  await expect(page.getByTestId("subagent-workbench")).toContainText("1 active · 6 completed");
  await page.waitForTimeout(400); // Let the drawer opacity transition reach its final contrast.
  await assertNoBlockingA11yIssues(page, ['[data-testid="subagent-workbench"]']);
  if (process.env.SUBAGENT_MOBILE_SCREENSHOT) {
    await page.screenshot({ path: process.env.SUBAGENT_MOBILE_SCREENSHOT, fullPage: false });
  }

  const statusCases = [
    ["sub-running", "running", "Running"],
    ["sub-completed", "completed", "Completed"],
    ["sub-failed", "failed", "Failed"],
    ["sub-cancelled", "cancelled", "Cancelled"],
    ["sub-blocked", "blocked", "Blocked"],
    ["sub-partial", "partial", "Partial"],
  ] as const;
  for (const [agentId, status, label] of statusCases) {
    const card = page.locator(`[data-subagent-id="${agentId}"]`);
    await expect(card).toHaveAttribute("data-status", status);
    await expect(card.getByLabel(`Status: ${label}`, { exact: true })).toBeVisible();
  }

  await expect(page.locator('[data-subagent-id="sub-running"]')).toContainText("Turn 1/3");
  await page.locator('[data-subagent-id="sub-blocked"] button').first().click();
  await expect(page.locator('[data-subagent-id="sub-blocked"]')).toContainText(
    "Blocked on operator approval",
  );
  await page.locator('[data-subagent-id="sub-partial"] button').first().click();
  await expect(page.locator('[data-subagent-id="sub-partial"]')).toContainText(
    "Partial data only",
  );
  await page.locator('[data-subagent-id="sub-completed"] button').first().click();
  await expect(page.locator('[data-subagent-id="sub-completed"]')).toContainText(
    "Two sources verified",
  );

  const unknownCard = page.locator('[data-subagent-id="sub-unknown"]');
  await expect(unknownCard).toHaveAttribute("data-status", "failed");
  await unknownCard.locator("button").first().click();
  await expect(unknownCard).toContainText(
    "Unsupported sub-agent status: future_terminal",
  );

  await page.keyboard.press("Escape");
  await expect(page.getByRole("dialog", { name: "Sub-agent workbench" })).toBeHidden();
  await expect(agentsLauncher).toBeFocused();

});

test("desktop sub-agent workbench follows three parallel children through out-of-order fan-in", async ({ page }) => {
  await seedClientPrefs(page, {
    locale: "en-US",
    themeMode: "dark",
    resolvedTheme: "dark",
    darkMode: true,
  });
  await installClientAuth(page, {
    user_id: "e2e-subagent-mobile-user",
    email: "subagent-mobile@example.com",
    display_name: "Sub-agent Parallel QA",
  });
  await installAssistantHarness(page, async (route) => {
    await route.fulfill({ status: 500, body: "The browser stream fixture was not installed" });
  });

  const frames: Array<{ delayMs: number; event: Record<string, unknown> }> = [
    { delayMs: 20, event: { event_type: "started", data: { request_id: "parallel-mobile" } } },
    ...[0, 1, 2].map((index) => ({
      delayMs: 60 + index * 20,
      event: {
        event_type: "subagent_started",
        data: {
          agent_id: `parallel-${index}`,
          agent_type: "task",
          description: [
            "Analyze controlling employment law authorities",
            "Inspect SEC filing risk disclosures",
            "Cross-check cited evidence and limitations",
          ][index],
          prompt: "private assignment containing sk-not-for-display-123456789",
          profile_id: ["legal-primary", "finance-filings", "evidence-reviewer"][index],
          profile_name: ["Legal primary-source analyst", "SEC filing analyst", "Evidence reviewer"][index],
          source_plugin: "community-doublecheck",
          delegation_id: "parallel-realistic-1",
          dispatch_index: index,
          attempt_id: "attempt-mobile-1",
        },
      },
    })),
    {
      delayMs: 130,
      event: { event_type: "subagent_step", data: { agent_id: "parallel-0", step: "Checking controlling cases", status: "running" } },
    },
    {
      delayMs: 150,
      event: { event_type: "subagent_tool_start", data: { agent_id: "parallel-1", call_id: "filing-1", tool_name: "filing_lookup", arguments: { api_key: "secret-raw-args" } } },
    },
    // Reconnect duplicates must not create extra children or tool rows.
    {
      delayMs: 170,
      event: { event_type: "subagent_started", data: { agent_id: "parallel-1", agent_type: "task", description: "Inspect SEC filing risk disclosures", delegation_id: "parallel-realistic-1", dispatch_index: 1 } },
    },
    {
      delayMs: 190,
      event: { event_type: "subagent_tool_start", data: { agent_id: "parallel-1", call_id: "filing-1", tool_name: "filing_lookup" } },
    },
    {
      delayMs: 210,
      event: { event_type: "subagent_text_delta", data: { agent_id: "parallel-0", text: "private chain-of-thought sk-never-render-123456789" } },
    },
    {
      delayMs: 1_500,
      event: {
        event_type: "subagent_finished",
        data: {
          agent_id: "parallel-2",
          status: "completed",
          result_summary: "Evidence cross-check completed.",
          duration_ms: 1_320,
          turns: 3,
          tool_calls: 2,
          result: {
            evidence: [{ evidence_id: "court-opinion-1", tool_name: "court_opinion_lookup", status: "completed", summary: "Controlling opinion verified" }],
            limitations: [],
            usage: { model_turns: 3, tool_calls: 2 },
            structured_payload: {
              verdict: "verified",
              confidence: "high",
              api_key: "structured-secret-raw",
            },
          },
          effective_execution: { model_id: "qwen3.7-plus", extensions: 1 },
        },
      },
    },
    // A replayed conflicting terminal and late step cannot rewrite/resurrect it.
    { delayMs: 1_540, event: { event_type: "subagent_finished", data: { agent_id: "parallel-2", status: "failed", error: "conflicting replay" } } },
    { delayMs: 1_560, event: { event_type: "subagent_step", data: { agent_id: "parallel-2", step: "Late replayed turn", status: "running" } } },
    { delayMs: 1_580, event: { event_type: "subagent_future_event", data: { agent_id: "parallel-2", instruction: "ignore unknown event" } } },
    {
      delayMs: 2_700,
      event: {
        event_type: "subagent_finished",
        data: {
          agent_id: "parallel-0",
          status: "failed",
          error: "Primary authority service unavailable",
          duration_ms: 2_520,
          result: { limitations: ["Could not retrieve the controlling opinion"] },
        },
      },
    },
    {
      delayMs: 3_900,
      event: {
        event_type: "subagent_finished",
        data: {
          agent_id: "parallel-1",
          status: "cancelled",
          error: "Cancelled by parent stream",
          duration_ms: 3_700,
          result: { limitations: ["Parent run stopped before synthesis"] },
        },
      },
    },
    { delayMs: 4_100, event: { event_type: "text_delta", data: "Parallel research concluded with mixed outcomes." } },
    { delayMs: 4_180, event: { event_type: "done", data: { duration_ms: 4_180 } } },
  ];

  await page.addInitScript(({ streamFrames }) => {
    const nativeFetch = window.fetch.bind(window);
    // The Assistant streams through the V2 events cursor; the legacy
    // /api/v1/assistant/chat/stream path is never requested any more. Accept
    // both and re-envelope legacy frames so these overrides keep working.
    const isAgentStreamUrl = (value: string) =>
      value.includes("/api/v1/assistant/chat/stream") ||
      (value.includes("/api/v2/agent/threads/") && value.includes("/events"));
    let envelopeSequence = 0;
    const asAgentV2Frame = (event: unknown) => {
      envelopeSequence += 1;
      const record = (event || {}) as Record<string, unknown>;
      return {
        schema_version: "agent-event/v2",
        thread_id: "e2e-runtime-thread",
        sequence: envelopeSequence,
        event: {
          id: `evt-${envelopeSequence}`,
          key: `evt-${envelopeSequence}`,
          type: typeof record.event_type === "string" ? record.event_type : "item",
          item_id: null,
          turn_id: null,
          status: null,
          payload: record,
        },
        timestamp: new Date().toISOString(),
      };
    };
    window.fetch = async (input: RequestInfo | URL, init?: RequestInit) => {
      const url = typeof input === "string"
        ? input
        : input instanceof Request
          ? input.url
          : String(input);
      if (!isAgentStreamUrl(url)) return nativeFetch(input, init);
      const encoder = new TextEncoder();
      return new Response(new ReadableStream({
        start(controller) {
          let closed = false;
          const close = () => {
            if (closed) return;
            closed = true;
            controller.enqueue(encoder.encode("data: [DONE]\n\n"));
            controller.close();
          };
          for (const frame of streamFrames) {
            window.setTimeout(() => {
              if (!closed) controller.enqueue(encoder.encode(`data: ${JSON.stringify(asAgentV2Frame(frame.event))}\n\n`));
            }, frame.delayMs);
          }
          window.setTimeout(close, Math.max(...streamFrames.map((frame) => frame.delayMs)) + 80);
          init?.signal?.addEventListener("abort", close, { once: true });
        },
      }), { headers: { "content-type": "text/event-stream" } });
    };
  }, { streamFrames: frames });

  await page.goto("/assistant");
  const composer = page.locator(`#${ASSISTANT_COMPOSER_ID}`);
  await composer.fill("Run a parallel legal, finance, and evidence review");
  await composer.press("Enter");

  const launcher = page.getByRole("button", { name: "Open 3 sub-agents" });
  await expect(launcher).toContainText("3 active");
  await launcher.click();
  const dialog = page.getByTestId("subagent-workbench");
  await expect(dialog).toBeVisible();
  await expect(dialog).toContainText("3 active · 0 completed");
  await expect(dialog.locator('[data-subagent-id="parallel-1"]')).toContainText("Using filing lookup");
  await expect(dialog).not.toContainText("private chain-of-thought");
  await expect(dialog).not.toContainText("sk-never-render");
  await expect(dialog).not.toContainText("secret-raw-args");

  await expect(dialog).toContainText("2 active · 1 completed", { timeout: 2_200 });
  await expect(dialog.locator('[data-subagent-id="parallel-2"]')).toHaveAttribute("data-status", "completed");
  await expect(dialog.locator('[data-subagent-id="parallel-2"]')).not.toContainText("conflicting replay");
  await expect(dialog.locator('[data-subagent-id="parallel-2"]')).not.toContainText("Late replayed turn");
  await page.waitForTimeout(400);
  if (process.env.SUBAGENT_DESKTOP_SCREENSHOT) {
    await page.screenshot({ path: process.env.SUBAGENT_DESKTOP_SCREENSHOT, fullPage: false });
  }

  await dialog.locator('[data-subagent-id="parallel-2"] button').first().click();
  await expect(dialog.locator('[data-subagent-id="parallel-2"]')).toContainText("Controlling opinion verified");
  await expect(dialog.locator('[data-subagent-id="parallel-2"]')).toContainText("Structured result");
  await dialog.locator('[data-subagent-id="parallel-2"] details summary').click();
  await expect(dialog).not.toContainText("structured-secret-raw");
  await expect(dialog).toContainText("1 active · 2 completed", { timeout: 3_200 });
  await expect(dialog).toContainText("0 active · 3 completed", { timeout: 4_500 });
  await expect(dialog.locator('[data-subagent-id="parallel-0"]')).toHaveAttribute("data-status", "failed");
  await expect(dialog.locator('[data-subagent-id="parallel-1"]')).toHaveAttribute("data-status", "cancelled");
  await expect(dialog).toContainText("All child terminals received; parent synthesis can continue.");
  await expect(dialog).toContainText("Child steering and per-agent cancellation are not available");
  await page.waitForTimeout(400);
  await assertNoBlockingA11yIssues(page, ['[data-testid="subagent-workbench"]']);
});

test("assistant restores todo_write after transient nameless tool events", async ({ page }) => {
  await seedClientPrefs(page, { locale: "en-US" });
  await installClientAuth(page, {
    user_id: "e2e-assistant-tool-order-user",
    email: "assistant-tool-order@example.com",
    display_name: "Assistant Tool Order",
  });
  const pageErrors: string[] = [];
  page.on("pageerror", (error) => pageErrors.push(error.message));

  await installAssistantHarness(page, async (route) => {
    const common = {
      run_id: "qwen-todo-run",
      thread_id: "qwen-todo-thread",
      session_id: "qwen-todo-session",
    };
    await route.fulfill({
      status: 200,
      headers: { "content-type": "text/event-stream" },
      body: toSseBody([
        { event_type: "started", data: { request_id: "qwen-todo-request" } },
        {
          event_type: "tool_call_end",
          data: {
            ...common,
            tool_call_id: "qwen-todo-call",
            timestamp: 1000,
          },
        },
        {
          event_type: "tool_call_result",
          data: {
            ...common,
            tool_call_id: "qwen-todo-call",
            result: { updated: 1 },
            success: true,
            timestamp: 1001,
          },
        },
        {
          event_type: "tool_call_start",
          data: {
            ...common,
            tool_call_id: "qwen-todo-call",
            name: "todo_write",
            tool_name: "todo_write",
            arguments: {
              todos: [{ content: "Review tool stream", status: "in_progress" }],
            },
            step_id: "qwen-todo-step",
            timestamp: 1002,
          },
        },
        {
          event_type: "tool_call_result",
          data: {
            ...common,
            tool_call_id: "qwen-todo-call",
            name: "todo_write",
            tool_name: "todo_write",
            result: { updated: 1 },
            success: true,
            timestamp: 1003,
          },
        },
        { event_type: "text_delta", data: "Todo list updated." },
        { event_type: "done", data: { duration_ms: 120, total_tokens: 42 } },
      ]),
    });
  });

  await ensureAuthenticatedPage(page, "/assistant");
  const composer = page.locator(`#${ASSISTANT_COMPOSER_ID}`);
  await composer.fill(`qwen-todo-${Date.now()}`);
  await composer.press("Enter");

  await expect(page.getByText("Todo list updated.")).toBeVisible();
  await expect(page.getByText("Failed to render message")).toHaveCount(0);
  await page.getByRole("button", { name: /Activity/ }).last().click();
  await expect(page.getByText("todo_write")).toBeVisible();
  expect(pageErrors).not.toContainEqual(
    expect.stringContaining("Cannot read properties of undefined")
  );
});

test("assistant treats protocol-completed todo_read result without success as completed", async ({ page }) => {
  await seedClientPrefs(page, { locale: "en-US" });
  await installClientAuth(page, {
    user_id: "e2e-assistant-tool-status-user",
    email: "assistant-tool-status@example.com",
    display_name: "Assistant Tool Status",
  });

  await installAssistantHarness(page, async (route) => {
    const common = {
      run_id: "qwen-todo-read-run",
      thread_id: "qwen-todo-read-thread",
      session_id: "qwen-todo-read-session",
    };
    await route.fulfill({
      status: 200,
      headers: { "content-type": "text/event-stream" },
      body: toSseBody([
        { event_type: "run_started", data: { ...common, timestamp: 1000 } },
        {
          event_type: "step_started",
          data: {
            ...common,
            step_id: "qwen-todo-read-step",
            title: "Execute tool: todo_read",
            timestamp: 1001,
          },
        },
        {
          event_type: "tool_call_start",
          data: {
            ...common,
            tool_call_id: "qwen-todo-read-call",
            name: "todo_read",
            tool_name: "todo_read",
            arguments: {},
            step_id: "qwen-todo-read-step",
            timestamp: 1002,
          },
        },
        {
          event_type: "tool_call_result",
          data: {
            ...common,
            tool_call_id: "qwen-todo-read-call",
            name: "todo_read",
            tool_name: "todo_read",
            status: "completed",
            result_preview: '{"todos":[]}',
            duration_ms: 8,
            timestamp: 1003,
          },
        },
        {
          event_type: "tool_call_end",
          data: {
            ...common,
            tool_call_id: "qwen-todo-read-call",
            name: "todo_read",
            status: "completed",
            duration_ms: 8,
            timestamp: 1004,
          },
        },
        {
          event_type: "step_finished",
          data: {
            ...common,
            step_id: "qwen-todo-read-step",
            status: "completed",
            duration_ms: 9,
            timestamp: 1005,
          },
        },
        { event_type: "text_delta", data: "Todo list is empty." },
        {
          event_type: "run_finished",
          data: { ...common, status: "succeeded", timestamp: 1006 },
        },
        { event_type: "done", data: { duration_ms: 10, total_tokens: 32 } },
      ]),
    });
  });

  await ensureAuthenticatedPage(page, "/assistant");
  const composer = page.locator(`#${ASSISTANT_COMPOSER_ID}`);
  await composer.fill(`qwen-todo-read-${Date.now()}`);
  await composer.press("Enter");

  await expect(page.getByText("Todo list is empty.")).toBeVisible();
  await page.getByRole("button", { name: /Activity/ }).last().click();
  await expect(page.getByText("Execute tool: todo_read")).toBeVisible();
  await expect(page.getByText("Step failed", { exact: true })).toHaveCount(0);
});

test("assistant keeps explicit todo_read failure as an error", async ({ page }) => {
  await seedClientPrefs(page, { locale: "en-US" });
  await installClientAuth(page, {
    user_id: "e2e-assistant-tool-error-user",
    email: "assistant-tool-error@example.com",
    display_name: "Assistant Tool Error",
  });

  await installAssistantHarness(page, async (route) => {
    await route.fulfill({
      status: 200,
      headers: { "content-type": "text/event-stream" },
      body: toSseBody([
        {
          event_type: "tool_call_start",
          data: {
            tool_call_id: "qwen-todo-read-error-call",
            name: "todo_read",
            tool_name: "todo_read",
            arguments: {},
            timestamp: 1000,
          },
        },
        {
          event_type: "tool_call_result",
          data: {
            tool_call_id: "qwen-todo-read-error-call",
            name: "todo_read",
            tool_name: "todo_read",
            status: "error",
            success: false,
            error: "todo_read denied",
            timestamp: 1001,
          },
        },
        {
          event_type: "tool_call_end",
          data: {
            tool_call_id: "qwen-todo-read-error-call",
            name: "todo_read",
            status: "error",
            error: "todo_read denied",
            timestamp: 1002,
          },
        },
        { event_type: "text_delta", data: "Tool request failed safely." },
        { event_type: "done", data: { duration_ms: 10, total_tokens: 20 } },
      ]),
    });
  });

  await ensureAuthenticatedPage(page, "/assistant");
  const composer = page.locator(`#${ASSISTANT_COMPOSER_ID}`);
  await composer.fill(`qwen-todo-read-error-${Date.now()}`);
  await composer.press("Enter");

  await expect(page.getByText("Tool request failed safely.")).toBeVisible();
  await page.getByRole("button", { name: /Activity/ }).last().click();
  await expect(page.getByText("todo_read denied", { exact: true })).toBeVisible();
});

test("assistant activity surfaces agent run state approvals context and artifacts", async ({ page }) => {
  await seedClientPrefs(page, { locale: "en-US" });
  await installClientAuth(page, {
    user_id: "e2e-assistant-agent-state-user",
    email: "assistant-agent-state@example.com",
    display_name: "Assistant Agent State",
    roles: ["user"],
    permissions: ["console:dashboard:view", "conversation:playground:access"],
    effective_permissions: ["console:dashboard:view", "conversation:playground:access"],
  });
  await installAssistantHarness(page, async (route) => {
    await route.fulfill({
      status: 200,
      headers: { "content-type": "text/event-stream" },
      body: toSseBody([
        { event_type: "run_started", data: { run_id: "f010-run", timestamp: 1000 } },
        {
          event_type: "task_planning",
          data: {
            goal: "Prepare deployment review",
            tasks: [
              {
                id: "plan",
                description: "Review requested change",
                type: "analysis",
                dependencies: [],
              },
              {
                id: "execute",
                description: "Draft final answer",
                type: "writing",
                dependencies: ["plan"],
              },
            ],
            parallel_groups: [],
          },
        },
        {
          event_type: "working_memory_update",
          data: {
            goal: "Prepare deployment review",
            tasks: [
              {
                id: "plan",
                description: "Review requested change",
                status: "completed",
                result: "Checklist ready",
              },
              {
                id: "execute",
                description: "Draft final answer",
                status: "in_progress",
              },
            ],
            collected_info: [
              { key: "context", value: "policy", source: "mock" },
            ],
            notes: ["Using remembered operator preference"],
            progress: { total: 2, completed: 1, failed: 0, percentage: 50 },
          },
        },
        {
          event_type: "tool_call_start",
          data: {
            tool_call_id: "approval-tool",
            tool_name: "shell",
            arguments: { cmd: "pnpm -C web build" },
            timestamp: 1100,
          },
        },
        {
          event_type: "approval_required",
          data: {
            tool_id: "approval-tool",
            tool_name: "shell",
            approval_id: "approval-1",
            reason: "Needs operator approval before running command",
          },
        },
        {
          event_type: "context_budget",
          data: {
            used_tokens: 2048,
            model_context_window: 128000,
            dropped_history_messages: 1,
          },
        },
        {
          event_type: "context_compacted",
          data: { compacted: true, dropped_history_messages: 1 },
        },
        {
          event_type: "artifact_created",
          data: {
            artifact_id: "artifact-f010",
            type: "document",
            format: "md",
            title: "Run report",
            filename: "run-report.md",
            mime_type: "text/markdown",
            size_bytes: 2048,
            download_url: "/mock-artifacts/run-report.md",
          },
        },
        { event_type: "text_delta", data: "Run state ready." },
        { event_type: "done", data: { duration_ms: 120, total_tokens: 42 } },
      ]),
    });
  });

  await page.goto("/assistant");

  const composer = page.locator(`#${ASSISTANT_COMPOSER_ID}`);
  await expect(composer).toBeVisible();
  await composer.fill(`e2e-agent-state-${Date.now()}`);
  await composer.press("Enter");

  await expect(page.getByText("Run state ready.")).toBeVisible();
  await expect(page.getByText("Run report").first()).toBeVisible();
  await expect(page.getByRole("button", { name: /^Approve$/i })).toBeVisible();
  await expect(page.getByRole("button", { name: /^Reject$/i })).toBeVisible();
  await expect(page.getByText("Needs operator approval before running command")).toBeVisible();

  await page.getByRole("button", { name: /Activity/ }).last().click();
  await expect(page.getByText("Review requested change")).toBeVisible();
  await expect(page.getByText("Draft final answer")).toBeVisible();
  await expect(page.getByText("Context used")).toBeVisible();
  await expect(page.getByText("Context compacted")).toBeVisible();
});

test("assistant restores session artifacts into unique artifact and share counts", async ({ page }) => {
  await seedClientPrefs(page, { locale: "en-US" });
  await installClientAuth(page, {
    user_id: "e2e-assistant-session-artifacts-user",
    email: "assistant-session-artifacts@example.com",
    display_name: "Assistant Session Artifacts",
    roles: ["user"],
    permissions: ["console:dashboard:view", "conversation:playground:access"],
    effective_permissions: ["console:dashboard:view", "conversation:playground:access"],
  });

  const sessionId = "e2e-restored-artifacts-session";
  const artifactId = "artifact-f011";
  const now = new Date().toISOString();
  const title = `restored-artifacts-${Date.now()}`;
  const shareRequests: Array<{ sessionId: string; includeArtifacts?: boolean }> = [];

  await installAssistantHarness(
    page,
    async (route) => {
      await route.fulfill({
        status: 200,
        headers: { "content-type": "text/event-stream" },
        body: toSseBody([{ event_type: "done", data: { duration_ms: 0 } }]),
      });
    },
    {
      preloadedSessions: [
        {
          ...buildMockAssistantSession(sessionId, { title }),
          created_at: now,
          updated_at: now,
        },
      ],
      historyBySessionId: {
        [sessionId]: [
          {
            role: "user",
            content: "Recover my generated plan",
            timestamp: now,
          },
          {
            role: "assistant",
            content: "Here is the recovered plan.",
            timestamp: now,
            metadata: { artifact_ids: [artifactId] },
          },
        ],
      },
      artifactsBySessionId: {
        [sessionId]: [
          {
            artifact_id: artifactId,
            type: "document",
            format: "md",
            title: "Recovered plan",
            filename: "recovered-plan.md",
            mime_type: "text/markdown",
            size_bytes: 1024,
            source: "ai",
            download_url: "/mock-artifacts/recovered-plan.md",
            created_at: now,
          },
        ],
      },
      onCreateShare: (createdSessionId, payload) => {
        shareRequests.push({
          sessionId: createdSessionId,
          includeArtifacts: payload.include_artifacts,
        });
      },
    }
  );

  await page.goto("/assistant");
  await page.getByRole("button", { name: /show history/i }).first().click();
  await page.getByRole("button", { name: title, exact: true }).click();

  await expect(page.getByText("Recover my generated plan")).toBeVisible();
  await expect(page.getByText("Here is the recovered plan.")).toBeVisible();
  await expect(page.getByText("Recovered plan").first()).toBeVisible();

  const artifactsChip = page.getByRole("button", { name: /^Artifacts \(1\)$/ });
  await expect(artifactsChip).toBeVisible();
  await artifactsChip.click();
  await expect(page.getByText("recovered-plan.md").first()).toBeVisible();

  await page.getByRole("button", { name: /^Share$/ }).click();
  const shareDialog = page.locator(".fixed.inset-0").filter({ hasText: "Share Conversation" }).first();
  await expect(shareDialog.getByText("Share Conversation")).toBeVisible();
  await expect(shareDialog.getByText("Artifacts")).toBeVisible();
  await expect(shareDialog.getByText("(1)")).toBeVisible();

  await shareDialog.getByRole("button", { name: "Create Share Link" }).click();
  await expect(shareDialog.getByText("Share link is ready!")).toBeVisible();
  expect(shareRequests).toEqual([{ sessionId, includeArtifacts: true }]);
});

test("assistant restores a pending approval after refresh", async ({ page }) => {
  await seedClientPrefs(page, { locale: "en-US" });
  await installClientAuth(page, {
    user_id: "e2e-assistant-reconnect-user",
    email: "assistant-reconnect@example.com",
    display_name: "Assistant Reconnect",
  });

  const sessionId = "e2e-reconnect-session";
  const runId = "e2e-reconnect-run";
  const approvalId = "e2e-reconnect-approval";
  const title = `reconnect-approval-${Date.now()}`;
  const now = new Date().toISOString();
  const consoleErrors: string[] = [];
  const requestFailures: string[] = [];
  let runStatusHits = 0;
  let runStatusFails = false, runSucceeded = false;

  page.on("console", (message) => {
    if (message.type() === "error") consoleErrors.push(message.text());
  });
  page.on("requestfailed", (request) => {
    requestFailures.push(`${request.method()} ${new URL(request.url()).pathname}`);
  });
  await installAssistantHarness(
    page,
    async (route) => {
      await route.fulfill({
        status: 200,
        headers: { "content-type": "text/event-stream" },
        body: toSseBody([{ event_type: "done", data: { duration_ms: 0 } }]),
      });
    },
    {
      preloadedSessions: [
        buildMockAssistantSession(sessionId, {
          title,
          assistant_active_run: {
            run_id: runId,
            updated_at: now,
          },
        }),
      ],
      historyBySessionId: {
        [sessionId]: [
          { role: "user", content: "Run the guarded command", timestamp: now },
          {
            role: "assistant",
            content: "Awaiting operator approval.",
            timestamp: now,
            metadata: {
              tool_calls: [
                { id: "guarded-shell", name: "shell", arguments: {}, status: "completed" },
              ],
            },
          },
        ],
      },
    }
  );
  await page.route(`**/api/v1/assistant/runs/${runId}`, async (route) => {
    runStatusHits += 1;
    if (runStatusFails) return route.fulfill(jsonResponse({}));
    await route.fulfill(
      jsonResponse({
        run: {
          run_id: runId,
          session_id: sessionId, harness_thread_id: "e2e-runtime-thread",
          status: runSucceeded ? "succeeded" : "running",
          checkpoint: runSucceeded
            ? { phase: "completed" }
            : {
                phase: "approval_pending",
                approval_id: approvalId,
                pending_tool: { tool_id: "guarded-shell", tool_name: "shell" },
              },
        },
      })
    );
  });
  await page.goto("/assistant");
  await page.getByRole("button", { name: /show history/i }).first().click();
  await page.getByRole("button", { name: title, exact: true }).click();
  await expect(page.getByText("Awaiting operator approval.")).toBeVisible();

  const hitsBeforeRefresh = runStatusHits;
  await page.reload();
  await expect.poll(() => runStatusHits).toBeGreaterThan(hitsBeforeRefresh);
  await page.getByRole("button", { name: /Activity/ }).last().click();
  await expect(page.getByText("Approval required: shell")).toBeVisible();
  await expect(page.getByRole("button", { name: "Approve" }).first()).toBeVisible();
  await expect(page.getByRole("button", { name: "Reject" }).first()).toBeVisible();
  runStatusFails = true;
  await page.getByRole("button", { name: "Approve" }).first().click();
  await expect(page.locator(`#${ASSISTANT_COMPOSER_ID}`)).toBeDisabled();
  runStatusFails = false;
  runSucceeded = true;
  const hitsBeforeTerminalRefresh = runStatusHits;
  await page.reload();
  await expect.poll(() => runStatusHits).toBeGreaterThan(hitsBeforeTerminalRefresh);
  await page.getByRole("button", { name: /Activity/ }).last().click();
  await expect(page.getByRole("button", { name: "Approve" })).toHaveCount(0);
  await expect(page.getByRole("button", { name: "Reject" })).toHaveCount(0);
  await expect(page.getByText(/^completed ·/)).toBeVisible();
  await expect(page.locator(`#${ASSISTANT_COMPOSER_ID}`)).toBeEnabled();
  expect(consoleErrors).toEqual([]);
  expect(requestFailures).toEqual([]);
});

test("assistant escape cancels delayed stream", async ({ page }) => {
  await seedClientPrefs(page, { locale: "en-US" });
  await installClientAuth(page, {
    user_id: "e2e-assistant-cancel-user",
    email: "assistant-cancel@example.com",
    display_name: "Assistant Cancel",
  });
  let releaseStream: () => void = () => {};
  const streamGate = new Promise<void>((resolve) => {
    releaseStream = resolve;
  });
  await installAssistantHarness(page, async (route) => {
    await streamGate;
    try {
      await route.fulfill({
        status: 200,
        headers: { "content-type": "text/event-stream" },
        body: toSseBody([
          { event_type: "started", data: {} },
          { event_type: "text_delta", data: "late response" },
          { event_type: "done", data: { duration_ms: 2500 } },
        ]),
      });
    } catch {
      // Request can be aborted by Escape; ignore fulfill failure.
    }
  });

  await ensureAuthenticatedPage(page, "/assistant");
  await installTelemetryCollector(page);

  const composer = page.locator(`#${ASSISTANT_COMPOSER_ID}`);
  await composer.fill(`e2e-cancel-${Date.now()}`);
  await composer.press("Enter");

  const stopButton = page.locator('button[aria-keyshortcuts="Escape"]').first();
  await expect(stopButton).toBeVisible();
  try {
    await page.getByRole("button", { name: /Activity/ }).last().click();
    await expect(page.getByText(/^Running… ·/)).toBeVisible();
    await page.keyboard.press("Escape");
  } finally {
    releaseStream();
  }

  await expect(stopButton).toBeHidden();
  await expect(page.locator('[role="log"]')).toBeVisible();
  await expect(page.getByText(/^cancelled ·/)).toBeVisible();
  await expect(page.getByText(/^completed ·/)).toHaveCount(0);

  const events = await readTelemetryEvents(page);
  expect(
    events.some(
      (event) =>
        event.event === "chat.shortcut.triggered" &&
        event.payload?.action === "stop_stream"
    )
  ).toBeTruthy();
  expect(
    events.some(
      (event) =>
        event.event === "chat.stream.finished" &&
        event.payload?.outcome === "cancelled"
    )
  ).toBeTruthy();
});

test("assistant stop closes tool exchange through the turn interrupt and allows follow-up", async ({ page }) => {
  await seedClientPrefs(page, { locale: "en-US" });
  await installClientAuth(page, {
    user_id: "e2e-assistant-tool-cancel-user",
    email: "assistant-tool-cancel@example.com",
    display_name: "Assistant Tool Cancel",
  });

  // Stop is a V2 contract now: `projectAgentV2Event` deliberately nulls the
  // legacy `task_id`, so the client no longer calls
  // /api/v1/assistant/tasks/<id>/cancel. It closes the events cursor and the
  // stream generator's `finally` interrupts the turn instead. Count that.
  let interruptHits = 0;
  await installAssistantHarness(page, async (route) => {
    await route.fulfill({
      status: 500,
      contentType: "text/plain",
      body: "stream override was not installed",
    });
  });
  await page.addInitScript(() => {
    const nativeFetch = window.fetch.bind(window);
    type MockFrame = { delayMs: number; event: unknown };
    // The Assistant streams through the V2 events cursor; the legacy
    // /api/v1/assistant/chat/stream path is never requested any more. Accept
    // both and re-envelope legacy frames so these overrides keep working.
    const isAgentStreamUrl = (value: string) =>
      value.includes("/api/v1/assistant/chat/stream") ||
      (value.includes("/api/v2/agent/threads/") && value.includes("/events"));
    let envelopeSequence = 0;
    const asAgentV2Frame = (event: unknown) => {
      envelopeSequence += 1;
      const record = (event || {}) as Record<string, unknown>;
      return {
        schema_version: "agent-event/v2",
        thread_id: "e2e-runtime-thread",
        sequence: envelopeSequence,
        event: {
          id: `evt-${envelopeSequence}`,
          key: `evt-${envelopeSequence}`,
          type: typeof record.event_type === "string" ? record.event_type : "item",
          item_id: null,
          turn_id: null,
          status: null,
          payload: record,
        },
        timestamp: new Date().toISOString(),
      };
    };
    let streamNumber = 0;
    window.fetch = async (input: RequestInfo | URL, init?: RequestInit) => {
      const url = typeof input === "string"
        ? input
        : input instanceof Request
          ? input.url
          : String(input);
      if (!isAgentStreamUrl(url)) {
        return nativeFetch(input, init);
      }
      streamNumber += 1;
      const encoder = new TextEncoder();
      const frames: MockFrame[] = streamNumber === 1
        ? [
            {
              delayMs: 0,
              event: {
                event_type: "run_started",
                data: {
                  run_id: "run-stop-paired",
                  session_id: "session-stop-paired",
                  timestamp: Date.now(),
                },
              },
            },
            {
              delayMs: 50,
              event: {
                event_type: "tool_call_start",
                data: {
                  tool_call_id: "tool-stop-paired",
                  tool_name: "web_fetch",
                  arguments: { url: "https://example.com" },
                  status: "running",
                },
              },
            },
            // No terminal frame: a slow tool call keeps the cursor open until
            // the operator stops it, which is exactly the state Stop has to
            // resolve.
          ]
        : [
            {
              delayMs: 0,
              event: {
                event_type: "run_started",
                data: {
                  run_id: "run-after-stop",
                  session_id: "session-stop-paired",
                  task_id: "task-after-stop",
                  timestamp: Date.now(),
                },
              },
            },
            { delayMs: 30, event: { event_type: "text_delta", data: "FOLLOWUP_OK" } },
            {
              delayMs: 50,
              event: {
                event_type: "run_finished",
                data: { run_id: "run-after-stop", status: "succeeded" },
              },
            },
          ];

      const hasTerminalFrame = streamNumber !== 1;
      return new Response(new ReadableStream({
        start(controller) {
          let closed = false;
          const close = () => {
            if (closed) return;
            closed = true;
            controller.enqueue(encoder.encode("data: [DONE]\n\n"));
            controller.close();
          };
          const emit = (frame: MockFrame) => {
            if (closed) return;
            controller.enqueue(
              encoder.encode(`data: ${JSON.stringify(asAgentV2Frame(frame.event))}\n\n`)
            );
          };
          for (const frame of frames) {
            window.setTimeout(() => emit(frame), frame.delayMs);
          }
          if (hasTerminalFrame) {
            window.setTimeout(close, Math.max(...frames.map((frame) => frame.delayMs)) + 30);
          }
          // Without a terminal frame the cursor stays open until the client
          // aborts it, which is what Stop does.
          init?.signal?.addEventListener("abort", close, { once: true });
        },
      }), { headers: { "content-type": "text/event-stream" } });
    };
  });

  // Registered after the harness so it wins the same `turns/*` pattern.
  await page.route("**/api/v2/agent/threads/*/turns/*", async (route) => {
    if (route.request().url().includes(":interrupt")) interruptHits += 1;
    await route.fulfill(jsonResponse({ status: "accepted" }));
  });

  await ensureAuthenticatedPage(page, "/assistant");
  const composer = page.locator(`#${ASSISTANT_COMPOSER_ID}`);
  await composer.fill("Start a slow tool and let me stop it");
  await composer.press("Enter");

  await expect(page.getByRole("button", { name: "Stop generating" })).toBeVisible();
  await page.getByRole("button", { name: /Activity/ }).last().click();
  await expect(page.getByText("web_fetch")).toBeVisible();
  await page.getByRole("button", { name: "Stop generating" }).click();

  // `stopStreaming` holds the cursor open for a grace window before aborting,
  // so the interrupt lands a couple of seconds after the click.
  await expect.poll(() => interruptHits, { timeout: 15_000 }).toBe(1);
  await expect(page.getByRole("button", { name: "Stop generating" })).toBeHidden();
  await expect(page.getByText(/^cancelled ·/)).toBeVisible();
  await expect(page.getByText(/^completed ·/)).toHaveCount(0);

  await composer.fill("continue after stop");
  await composer.press("Enter");
  await expect(page.getByText("FOLLOWUP_OK")).toBeVisible();
  // Sending closes the drawer, and the run state lives in its header — reopen
  // it on the follow-up turn before asserting that run settled as completed.
  await page.getByRole("button", { name: /Activity/ }).last().click();
  await expect(page.getByText(/^completed ·/)).toBeVisible({ timeout: 30_000 });
});

test("assistant new chat invalidates late events from the previous stream", async ({ page }) => {
  await seedClientPrefs(page, { locale: "en-US" });
  await installClientAuth(page, {
    user_id: "e2e-assistant-stream-epoch-user",
    email: "assistant-stream-epoch@example.com",
    display_name: "Assistant Stream Epoch",
  });
  await installAssistantHarness(page, async (route) => {
    await route.fulfill({
      status: 500,
      contentType: "application/json",
      body: JSON.stringify({ error: "browser stream interceptor was not installed" }),
    });
  });
  await page.addInitScript(() => {
    const nativeFetch = window.fetch.bind(window);
    // The Assistant streams through the V2 events cursor; the legacy
    // /api/v1/assistant/chat/stream path is never requested any more. Accept
    // both and re-envelope legacy frames so these overrides keep working.
    const isAgentStreamUrl = (value: string) =>
      value.includes("/api/v1/assistant/chat/stream") ||
      (value.includes("/api/v2/agent/threads/") && value.includes("/events"));
    let envelopeSequence = 0;
    const asAgentV2Frame = (event: unknown) => {
      envelopeSequence += 1;
      const record = (event || {}) as Record<string, unknown>;
      return {
        schema_version: "agent-event/v2",
        thread_id: "e2e-runtime-thread",
        sequence: envelopeSequence,
        event: {
          id: `evt-${envelopeSequence}`,
          key: `evt-${envelopeSequence}`,
          type: typeof record.event_type === "string" ? record.event_type : "item",
          item_id: null,
          turn_id: null,
          status: null,
          payload: record,
        },
        timestamp: new Date().toISOString(),
      };
    };
    let streamNumber = 0;
    window.fetch = async (input: RequestInfo | URL, init?: RequestInit) => {
      const url =
        typeof input === "string"
          ? input
          : input instanceof Request
            ? input.url
            : String(input);
      if (!isAgentStreamUrl(url)) {
        return nativeFetch(input, init);
      }
      streamNumber += 1;
      (window as typeof window & { __assistantStreamCount?: number }).__assistantStreamCount =
        streamNumber;
      const current = streamNumber;
      const encoder = new TextEncoder();
      return new Response(
        new ReadableStream({
          start(controller) {
            const emit = (event: unknown) =>
              controller.enqueue(
                encoder.encode(`data: ${JSON.stringify(asAgentV2Frame(event))}\n\n`)
              );
            emit({ event_type: "started", data: { stream: current } });
            if (current === 1) {
              // Intentionally ignore AbortSignal to emulate a proxy/client
              // that continues delivering buffered frames after navigation.
              window.setTimeout(() => {
                try {
                  emit({ event_type: "text_delta", data: "stale previous response" });
                  emit({ event_type: "run_finished", data: { run_id: "stale-run" } });
                  controller.enqueue(encoder.encode("data: [DONE]\n\n"));
                  controller.close();
                } catch {
                  // Browser stream cancellation is also an acceptable result;
                  // the epoch guard covers transports that do deliver late.
                }
              }, 450);
              return;
            }
            emit({ event_type: "text_delta", data: "fresh current response" });
            emit({ event_type: "run_finished", data: { run_id: "fresh-run" } });
            controller.enqueue(encoder.encode("data: [DONE]\n\n"));
            controller.close();
          },
        }),
        { headers: { "content-type": "text/event-stream" } }
      );
    };
  });

  await ensureAuthenticatedPage(page, "/assistant");
  const composer = page.locator(`#${ASSISTANT_COMPOSER_ID}`);
  await composer.fill("start stale stream");
  await composer.press("Enter");
  await expect(page.locator('button[aria-keyshortcuts="Escape"]').first()).toBeVisible();
  await expect
    .poll(() =>
      page.evaluate(
        () =>
          (window as typeof window & { __assistantStreamCount?: number })
            .__assistantStreamCount || 0
      )
    )
    .toBe(1);

  await page.keyboard.press(`${modKey()}+Shift+N`);
  await expect(composer).toBeEnabled();
  await composer.fill("start fresh stream");
  await composer.press("Enter");

  await expect(page.getByText("fresh current response")).toBeVisible();
  await page.waitForTimeout(650);
  await expect(page.getByText("stale previous response")).toHaveCount(0);
  await expect(composer).toBeEnabled();
});

test("playground stream path keeps a11y and performance budget", async ({ page }) => {
  await ensureAuthenticatedPage(page, "/playground");
  await selectFirstPlaygroundService(page);

  const composer = page.locator(`#${PLAYGROUND_COMPOSER_ID}`);
  await expect(composer).toBeVisible();

  await page.keyboard.press(`${modKey()}+K`);
  await expect(composer).toBeFocused();

  await assertNoBlockingA11yIssues(page, [
    `#${PLAYGROUND_COMPOSER_ID}`,
    '[role="log"]',
  ]);

  const prompt = `e2e-playground-${Date.now()}`;
  await composer.fill(prompt);
  await composer.press("Enter");
  await expect(page.getByText(prompt)).toBeVisible();

  await expect(page.locator('[role="log"]')).toBeVisible();
  await assertInpBudget(page);
});

test("playground mobile history uses a bounded overlay sheet", async ({ page }) => {
  await page.setViewportSize({ width: 390, height: 844 });
  await seedClientPrefs(page, { locale: "en-US" });
  await installClientAuth(page, {
    user_id: "e2e-playground-mobile-history-user",
    email: "playground-mobile-history@example.com",
    display_name: "Playground Mobile History",
  });
  await installPlaygroundHarness(page);

  await page.goto("/playground");
  const historyToggle = page.getByRole("button", { name: /show history/i });
  await expect(historyToggle).toBeVisible();
  await historyToggle.click();

  const historySheet = page.getByRole("dialog", { name: /history/i });
  await expect(historySheet).toBeVisible();
  const bounds = await historySheet.boundingBox();
  expect(bounds).not.toBeNull();
  expect(bounds!.width).toBeLessThanOrEqual(390);
  expect(bounds!.width).toBeGreaterThan(300);

  await page.keyboard.press("Escape");
  await expect(historySheet).toBeHidden();
});

test("playground handles mocked stream with tool call lifecycle", async ({ page }) => {
  const harness = await installPlaygroundHarness(page);
  const mockPlaygroundStream = async (route: Route) => {
    harness.recordStreamHit();
    await route.fulfill({
      status: 200,
      headers: { "content-type": "text/event-stream" },
      body: toSseBody([
        {
          request_id: "pg-e2e",
          chunk_index: 1,
          event_type: "text_delta",
          content: { type: "text", data: "Playground " },
        },
        {
          request_id: "pg-e2e",
          chunk_index: 2,
          event_type: "tool_call_start",
            tool_call: {
            tool_call_id: MOCK_PLAYGROUND_TOOL_ID,
            name: "web_search",
            arguments: '{"query":"playwright"}',
            status: "running",
          },
          content: { type: "tool_call", data: "" },
        },
        {
          request_id: "pg-e2e",
          chunk_index: 3,
          event_type: "tool_result",
          tool_call: {
            tool_call_id: MOCK_PLAYGROUND_TOOL_ID,
            name: "web_search",
            arguments: '{"query":"playwright"}',
            status: "completed",
          },
          content: { type: "tool_result", data: "ok" },
        },
        {
          request_id: "pg-e2e",
          chunk_index: 4,
          event_type: "text_delta",
          content: { type: "text", data: "mock response" },
        },
        {
          request_id: "pg-e2e",
          chunk_index: 5,
          event_type: "stream_end",
          content: { type: "text", data: "" },
          metadata: { usage: { total_tokens: 33 } },
        },
      ]),
    });
  };

  await page.route("**/api/v1/stream", mockPlaygroundStream);
  await page.route("**/api/v1/proxy/**/runs/stream", mockPlaygroundStream);
  await page.route("**/api/v1/proxy/**/threads/**/runs/stream", mockPlaygroundStream);

  await ensureAuthenticatedPage(page, "/playground");
  await selectFirstPlaygroundService(page);

  const composer = page.locator(`#${PLAYGROUND_COMPOSER_ID}`);
  await composer.fill(`e2e-pg-mock-${Date.now()}`);
  await composer.press("Enter");

  await expect(page.getByText("Playground mock response")).toBeVisible();
  await expect(page.getByText("web_search")).toBeVisible();
  expect(harness.getStreamHits()).toBeGreaterThan(0);
});

test("playground renders native langgraph tool events without heavy assistant card", async ({ page }) => {
  const harness = await installPlaygroundHarness(page);
  let requestedStreamMode: unknown;
  let waitHits = 0;
  await seedClientPrefs(page, {
    locale: "zh-CN",
    themeMode: "dark",
    resolvedTheme: "dark",
    darkMode: true,
  });

  const nativeLangGraphStream = async (route: Route) => {
    harness.recordStreamHit();
    requestedStreamMode = route.request().postDataJSON()?.stream_mode;
    await route.fulfill({
      status: 200,
      headers: { "content-type": "text/event-stream" },
      body: [
        'event: metadata\ndata: {"run_id":"pg-native-e2e"}\n\n',
        `event: messages\ndata: [{"content":[],"type":"AIMessageChunk","tool_call_chunks":[{"id":"${MOCK_PLAYGROUND_TOOL_ID}","name":"classify_query","index":0,"args":"{\\"query\\":"}],"usage_metadata":{"input_tokens":21,"output_tokens":4,"total_tokens":25}}]\n\n`,
        'event: messages\ndata: [{"content":[],"type":"AIMessageChunk","tool_call_chunks":[{"index":0,"args":"\\"agent\\""}]}]\n\n',
        'event: messages\ndata: [{"content":[],"type":"AIMessageChunk","tool_call_chunks":[{"index":0,"args":",\\"top_k\\":3}"}]}]\n\n',
        `event: messages/complete\ndata: [{"content":"router: agent","type":"tool","role":"tool","tool_call_id":"${MOCK_PLAYGROUND_TOOL_ID}","name":"classify_query","status":"success"}]\n\n`,
        'event: messages\ndata: [{"content":"Agent ","type":"AIMessageChunk","role":"assistant"}]\n\n',
        'event: messages\ndata: [{"content":"native ","type":"AIMessageChunk","role":"assistant"}]\n\n',
        'event: messages\ndata: [{"content":"response","type":"AIMessageChunk","role":"assistant"}]\n\n',
        'event: messages/complete\ndata: [{"content":"Agent native response","type":"ai","role":"assistant"}]\n\n',
        "event: end\ndata: {}\n\n",
      ].join(""),
    });
  };
  const nativeWaitResponse = {
    messages: [
      {
        type: "tool",
        role: "tool",
        tool_call_id: MOCK_PLAYGROUND_TOOL_ID,
        name: "classify_query",
        content: "router: agent",
      },
      {
        type: "AIMessage",
        role: "assistant",
        content: "Agent native response",
        tool_calls: [
          {
            id: MOCK_PLAYGROUND_TOOL_ID,
            name: "classify_query",
            args: { query: "agent" },
          },
        ],
      },
    ],
  };

  await page.route("**/api/v1/proxy/**/runs/stream", nativeLangGraphStream);
  await page.route("**/api/v1/proxy/**/threads/**/runs/stream", nativeLangGraphStream);
  await page.route("**/api/v1/proxy/**/runs/wait", async (route) => {
    waitHits += 1;
    await route.fulfill(jsonResponse(nativeWaitResponse));
  });
  await page.route("**/api/v1/proxy/**/threads/**/runs/wait", async (route) => {
    waitHits += 1;
    await route.fulfill(jsonResponse(nativeWaitResponse));
  });

  await ensureAuthenticatedPage(page, "/playground");
  await selectFirstPlaygroundService(page);

  const composer = page.locator(`#${PLAYGROUND_COMPOSER_ID}`);
  await composer.fill(`e2e-pg-native-${Date.now()}`);
  await composer.press("Enter");

  await expect(page.getByText("Agent native response")).toBeVisible();
  await expect(page.getByText("classify_query")).toBeVisible();
  await expect(page.getByText("unknown_tool")).toHaveCount(0);
  await expect(page.locator('[data-message-supplemental="timeline"]')).toHaveCount(0);

  const assistantSurface = page
    .locator('[data-message-role="assistant"] [data-message-surface="assistant"]')
    .last();
  const surfaceStyles = await assistantSurface.evaluate((node) => {
    const styles = window.getComputedStyle(node);
    return {
      backgroundColor: styles.backgroundColor,
      boxShadow: styles.boxShadow,
    };
  });

  expect(surfaceStyles.backgroundColor).toBe("rgba(0, 0, 0, 0)");
  expect(surfaceStyles.boxShadow).toBe("none");
  expect(requestedStreamMode).toEqual(["messages-tuple", "updates", "custom"]);
  expect(waitHits).toBe(0);
  expect(harness.getStreamHits()).toBeGreaterThan(0);
});
