import fs from "node:fs/promises";
import path from "node:path";

import { expect, test, type Locator, type Page, type Route } from "@playwright/test";
import {
  assertNoBlockingA11yIssues,
  installClientAuth,
  seedClientPrefs,
} from "./support/helpers";

const AGENT_ID = "11111111-1111-4111-8111-111111111111";
const COPIED_AGENT_ID = "12121212-1212-4121-8121-121212121212";
const VERSION_ID = "22222222-2222-4222-8222-222222222222";
const DATASET_ID = "33333333-3333-4333-8333-333333333333";
const NOW = "2026-07-18T08:00:00.000Z";

const baseSpec = {
  schema_version: "agent-spec/v1",
  identity: {
    icon_url: null,
    theme_color: "#7B7BE8",
    welcome_message: "How can I help with this support request?",
    suggested_prompts: ["Classify a billing issue"],
  },
  instructions: "Classify support requests and explain the selected queue.",
  model: {
    model_id: "qwen3.7-plus",
    provider_id: "dashscope",
    temperature: 0.3,
    max_tokens: 4096,
  },
  capabilities: [
    {
      type: "native",
      resource_id: "lookup_account",
      resource_version: null,
      schema_hash: null,
      config: { risk: "low" },
    },
  ],
  knowledge: [
    {
      dataset_id: DATASET_ID,
      retrieval_config: {
        mode: "auto",
        top_k: 5,
        threshold: 0.4,
        include_images: false,
      },
    },
  ],
  memory: { mode: "session" },
};

const versionSpec = {
  ...baseSpec,
  model: { ...baseSpec.model, model_id: "qwen3.7-max" },
  capabilities: [],
  knowledge: [],
};

const agents = [
  {
    tenant_id: "tenant-a",
    agent_id: AGENT_ID,
    slug: "support-triage",
    name: "Support Triage",
    description: "Classifies and routes customer support tickets.",
    owner_id: "alex",
    status: "draft",
    caller_role: "owner",
    draft_revision: 8,
    created_at: NOW,
    updated_at: NOW,
  },
  {
    tenant_id: "tenant-a",
    agent_id: "44444444-4444-4444-8444-444444444444",
    slug: "research-analyst",
    name: "Research Analyst",
    description: "Deep research and source-grounded summaries.",
    owner_id: "maya",
    status: "active",
    caller_role: "editor",
    draft_revision: 12,
    created_at: NOW,
    updated_at: "2026-07-17T08:00:00.000Z",
  },
  {
    tenant_id: "tenant-a",
    agent_id: "55555555-5555-4555-8555-555555555555",
    slug: "policy-copilot",
    name: "Policy Copilot",
    description: "Answers policy questions with citations.",
    owner_id: "jordan",
    status: "draft",
    caller_role: "viewer",
    draft_revision: 5,
    created_at: NOW,
    updated_at: "2026-07-16T08:00:00.000Z",
  },
] as const;

interface HarnessOptions {
  listState?: "populated" | "empty" | "error" | "forbidden" | "slow";
  callerRole?: "owner" | "editor" | "viewer";
  degradedCatalog?: boolean;
  locale?: "en-US" | "zh-CN";
  createFailure?: boolean;
  previewFailure?: { code: string; message: string; status: number };
}

interface HarnessState {
  draftRevision: number;
  currentSpec: typeof baseSpec;
  conflictNext: boolean;
  validationNext: boolean;
  networkNext: boolean;
  draftSessions: number;
  versionSessions: number;
  createdAgents: number;
  copiedAgents: number;
  archivedAgents: number;
  metadataPatchRequests: number;
  currentName: string;
  currentDescription: string;
  createdSpec: typeof baseSpec | null;
  previewSessions: Record<string, {
    pin: { session_id: string; agent_id: string; agent_version_id: string | null; draft_revision: number | null; channel: "preview"; request_id: string };
    messages: Array<{ role: "user" | "assistant"; content: string; metadata: { runtime_run_id: string } }>;
  }>;
  previewRuns: Record<string, { sessionId: string; status: "succeeded" }>;
  previewRunCount: number;
  previewPinReads: string[];
  previewHistoryReads: string[];
  previewArtifactReads: string[];
}

function json(body: unknown, status = 200) {
  return { status, contentType: "application/json", body: JSON.stringify(body) };
}

async function fulfillAgentApi(route: Route, state: HarnessState, options: HarnessOptions) {
  const request = route.request();
  const url = new URL(request.url());
  const path = url.pathname;
  const method = request.method();

  if (path === "/api/v1/agents" && method === "GET") {
    if (options.listState === "slow") await new Promise((resolve) => setTimeout(resolve, 2500));
    if (options.listState === "error") return route.fulfill(json({ detail: { code: "AGENT_STORAGE_UNAVAILABLE", message: "Storage is temporarily unavailable." } }, 503));
    if (options.listState === "forbidden") return route.fulfill(json({ detail: { code: "AGENT_FORBIDDEN", message: "Agent access is required." } }, 403));
    if (options.listState === "empty") return route.fulfill(json({ items: [], next_cursor: null }));
    const search = (url.searchParams.get("search") || "").toLowerCase();
    const status = url.searchParams.get("status");
    const owner = url.searchParams.get("owner_id");
    const items = agents.map((agent) => agent.agent_id === AGENT_ID
      ? {
          ...agent,
          name: state.currentName,
          description: state.currentDescription,
          ...(state.archivedAgents > 0 ? { status: "archived" as const } : {}),
        }
      : agent).filter((agent) =>
      (!search || `${agent.name} ${agent.description}`.toLowerCase().includes(search))
      && (!status || agent.status === status)
      && (!owner || agent.owner_id === owner)
    );
    return route.fulfill(json({ items, next_cursor: null }));
  }

  if (path === "/api/v1/agents" && method === "POST") {
    if (options.createFailure) return route.fulfill(json({ detail: { code: "AGENT_STORAGE_UNAVAILABLE", message: "Draft storage is temporarily unavailable." } }, 503));
    state.createdAgents += 1;
    const body = request.postDataJSON();
    state.createdSpec = body.spec;
    return route.fulfill(json({ request_id: "create-request", agent: { ...agents[0], name: body.name, description: body.description, draft_revision: 1 } }, 201));
  }

  if (path === `/api/v1/agents/${AGENT_ID}/copy` && method === "POST") {
    state.copiedAgents += 1;
    return route.fulfill(json({ request_id: "copy-request", agent: { ...agents[0], agent_id: COPIED_AGENT_ID, slug: "support-triage-copy", name: "Support Triage copy", draft_revision: 1 } }, 201));
  }

  if (path === `/api/v1/agents/${AGENT_ID}/archive` && method === "POST") {
    state.archivedAgents += 1;
    return route.fulfill(json({ request_id: "archive-request", agent: { ...agents[0], status: "archived" } }));
  }

  if (path === `/api/v1/agents/${AGENT_ID}` && method === "GET") {
    return route.fulfill(json({ ...agents[0], name: state.currentName, description: state.currentDescription, caller_role: options.callerRole || "owner", draft_revision: state.draftRevision, draft: { revision: state.draftRevision, schema_version: "agent-spec/v1", spec_hash: "a".repeat(64), updated_at: NOW } }));
  }

  if (path === `/api/v1/agents/${COPIED_AGENT_ID}` && method === "GET") {
    return route.fulfill(json({ ...agents[0], agent_id: COPIED_AGENT_ID, slug: "support-triage-copy", name: "Support Triage copy", draft_revision: 1, caller_role: "owner", draft: { revision: 1, schema_version: "agent-spec/v1", spec_hash: "a".repeat(64), updated_at: NOW } }));
  }

  if (path === `/api/v1/agents/${AGENT_ID}` && method === "PATCH") {
    state.metadataPatchRequests += 1;
    const body = request.postDataJSON();
    if (typeof body.name === "string") state.currentName = body.name;
    if (typeof body.description === "string") state.currentDescription = body.description;
    return route.fulfill(json({ request_id: "metadata-save", agent: { ...agents[0], ...body, caller_role: options.callerRole || "owner", draft_revision: state.draftRevision } }));
  }

  if (path === `/api/v1/agents/${AGENT_ID}/draft` && method === "GET") {
    return route.fulfill(json({ tenant_id: "tenant-a", draft_id: "draft-a", agent_id: AGENT_ID, revision: state.draftRevision, schema_version: "agent-spec/v1", spec: state.currentSpec, spec_hash: "a".repeat(64), updated_by: "alex", created_at: NOW, updated_at: NOW }));
  }

  if (path === `/api/v1/agents/${COPIED_AGENT_ID}/draft` && method === "GET") {
    return route.fulfill(json({ tenant_id: "tenant-a", draft_id: "draft-copy", agent_id: COPIED_AGENT_ID, revision: 1, schema_version: "agent-spec/v1", spec: state.currentSpec, spec_hash: "a".repeat(64), updated_by: "alex", created_at: NOW, updated_at: NOW }));
  }

  if (path === `/api/v1/agents/${AGENT_ID}/draft` && method === "PUT") {
    await new Promise((resolve) => setTimeout(resolve, 600));
    if (state.networkNext) {
      state.networkNext = false;
      return route.fulfill(json({ detail: { code: "AGENT_STORAGE_UNAVAILABLE", message: "Draft storage is temporarily unavailable." } }, 503));
    }
    if (state.conflictNext) {
      state.conflictNext = false;
      state.draftRevision += 1;
      return route.fulfill(json({ detail: { code: "AGENT_DRAFT_CONFLICT", message: "Draft revision is stale", current_revision: state.draftRevision, request_id: "conflict-request" } }, 409));
    }
    if (state.validationNext) {
      state.validationNext = false;
      return route.fulfill(json({ detail: { code: "AGENT_VALIDATION_FAILED", message: "Draft validation failed", errors: [{ field: "description", code: "DESCRIPTION_INVALID", message: "Description needs a clearer purpose." }] } }, 422));
    }
    const body = request.postDataJSON();
    state.currentSpec = body.spec;
    if (typeof body.name === "string") state.currentName = body.name;
    if (typeof body.description === "string") state.currentDescription = body.description;
    state.draftRevision += 1;
    return route.fulfill(json({ request_id: "draft-save", draft: { tenant_id: "tenant-a", draft_id: "draft-a", agent_id: AGENT_ID, revision: state.draftRevision, schema_version: "agent-spec/v1", spec: state.currentSpec, spec_hash: "b".repeat(64), updated_by: "alex", created_at: NOW, updated_at: "2026-07-18T09:00:00.000Z" } }));
  }

  if (path === `/api/v1/agents/${AGENT_ID}/versions` && method === "GET") {
    return route.fulfill(json([{ tenant_id: "tenant-a", agent_version_id: VERSION_ID, agent_id: AGENT_ID, version_number: 7, schema_version: "agent-spec/v1", spec: versionSpec, spec_hash: "c".repeat(64), source_draft_id: "draft-a", source_draft_revision: 7, created_by: "alex", created_at: NOW }]));
  }

  if (path === `/api/v1/agents/${COPIED_AGENT_ID}/versions` && method === "GET") {
    return route.fulfill(json([]));
  }

  if (path === `/api/v1/agents/${AGENT_ID}/preview/sessions` && method === "POST") {
    if (options.previewFailure) return route.fulfill(json({ detail: { ...options.previewFailure, request_id: "preview-failure-request" } }, options.previewFailure.status));
    state.draftSessions += 1;
    const sessionId = `draft-session-${state.draftSessions}`;
    state.previewSessions[sessionId] = {
      pin: { session_id: sessionId, agent_id: AGENT_ID, agent_version_id: null, draft_revision: state.draftRevision, channel: "preview", request_id: "draft-session-request" },
      messages: [],
    };
    return route.fulfill(json({ ...state.previewSessions[sessionId].pin, publication_id: null, runtime_fingerprint: "sha256:draft" }, 201));
  }

  if (path === `/api/v1/agents/${AGENT_ID}/versions/${VERSION_ID}/preview/sessions` && method === "POST") {
    state.versionSessions += 1;
    const sessionId = `version-session-${state.versionSessions}`;
    state.previewSessions[sessionId] = {
      pin: { session_id: sessionId, agent_id: AGENT_ID, agent_version_id: VERSION_ID, draft_revision: null, channel: "preview", request_id: "version-session-request" },
      messages: [],
    };
    return route.fulfill(json({ ...state.previewSessions[sessionId].pin, publication_id: null, runtime_fingerprint: "sha256:version" }, 201));
  }

  const previewPin = path.match(new RegExp(`^/api/v1/agents/${AGENT_ID}/preview/sessions/([^/]+)$`));
  if (previewPin && method === "GET") {
    const session = state.previewSessions[previewPin[1]];
    if (!session) throw new Error(`Preview recovery requested an unknown session: ${previewPin[1]}`);
    state.previewPinReads.push(previewPin[1]);
    return route.fulfill(json(session.pin));
  }

  if (path.endsWith("/preview/chat/stream") && method === "POST") {
    const body = request.postDataJSON();
    const session = state.previewSessions[body.session_id];
    if (!session) throw new Error(`Preview stream requested an unknown session: ${body.session_id}`);
    const versionTarget = path.includes(`/versions/${VERSION_ID}/`);
    if (session.pin.agent_version_id !== (versionTarget ? VERSION_ID : null)) {
      throw new Error(`Preview stream target does not match pinned session: ${body.session_id}`);
    }
    if (!versionTarget && body.draft_revision !== session.pin.draft_revision) {
      throw new Error(`Preview stream revision does not match pinned session: ${body.session_id}`);
    }
    state.previewRunCount += 1;
    const runId = `00000000-0000-4000-8000-${String(state.previewRunCount).padStart(12, "0")}`;
    const answer = "Billing issue · duplicate charge. Route this request to Billing review.";
    state.previewRuns[runId] = { sessionId: body.session_id, status: "succeeded" };
    session.messages.push(
      { role: "user", content: body.message, metadata: { runtime_run_id: runId } },
      { role: "assistant", content: answer, metadata: { runtime_run_id: runId } },
    );
    const stream = [
      { event_type: "run_started", data: { run_id: runId } },
      { event_type: "text_delta", data: { content: "Billing issue · duplicate charge. " } },
      { event_type: "text_delta", data: { content: "Route this request to Billing review." } },
      { event_type: "tool_call_start", data: { tool_name: "lookup_account", status: "allowed" } },
      { event_type: "context_retrieved", data: { dataset_name: "Refund policy", citation_count: 2 } },
      { event_type: "run_finished", data: { status: "succeeded" } },
    ].map((event) => `data: ${JSON.stringify(event)}\n\n`).join("") + "data: [DONE]\n\n";
    return route.fulfill({ status: 200, contentType: "text/event-stream", body: stream });
  }

  throw new Error(`Unhandled Agent API request: ${method} ${path}`);
}

async function installHarness(page: Page, options: HarnessOptions = {}): Promise<HarnessState> {
  await installClientAuth(page, {
    user_id: "agent-studio-user",
    email: "agent-studio@example.com",
    display_name: "Agent Studio User",
    roles: ["user"],
    permissions: ["console:dashboard:view", "conversation:playground:access", "console:eval:view"],
    effective_permissions: ["console:dashboard:view", "conversation:playground:access", "console:eval:view"],
  });
  await seedClientPrefs(page, { locale: options.locale ?? "en-US", themeMode: "dark", resolvedTheme: "dark", darkMode: true });
  const state: HarnessState = {
    draftRevision: 8,
    currentSpec: structuredClone(baseSpec),
    conflictNext: false,
    validationNext: false,
    networkNext: false,
    draftSessions: 0,
    versionSessions: 0,
    createdAgents: 0,
    copiedAgents: 0,
    archivedAgents: 0,
    metadataPatchRequests: 0,
    currentName: agents[0].name,
    currentDescription: agents[0].description,
    createdSpec: null,
    previewSessions: {},
    previewRuns: {},
    previewRunCount: 0,
    previewPinReads: [],
    previewHistoryReads: [],
    previewArtifactReads: [],
  };

  await page.route("**/api/v1/**", async (route) => {
    const request = route.request();
    const url = new URL(request.url());
    const path = url.pathname;
    if (path === "/api/v1/auth/me") return route.fulfill(json({ user_id: "agent-studio-user", email: "agent-studio@example.com", display_name: "Agent Studio User", roles: ["user"], permissions: ["console:dashboard:view", "conversation:playground:access", "console:eval:view"], effective_permissions: ["console:dashboard:view", "conversation:playground:access", "console:eval:view"], tier: "normal", force_password_change: false }));
    if (path.startsWith("/api/v1/agents")) return fulfillAgentApi(route, state, options);
    const history = path.match(/^\/api\/v1\/assistant\/sessions\/([^/]+)\/history$/);
    if (history && request.method() === "GET") {
      const session = state.previewSessions[history[1]];
      if (!session) throw new Error(`History requested outside a pinned Preview session: ${history[1]}`);
      state.previewHistoryReads.push(history[1]);
      return route.fulfill(json({ session_id: history[1], messages: session.messages, total: session.messages.length }));
    }
    const artifacts = path.match(/^\/api\/v1\/assistant\/sessions\/([^/]+)\/artifacts$/);
    if (artifacts && request.method() === "GET") {
      if (!state.previewSessions[artifacts[1]]) throw new Error(`Artifacts requested outside a pinned Preview session: ${artifacts[1]}`);
      state.previewArtifactReads.push(artifacts[1]);
      return route.fulfill(json({ artifacts: [], total: 0 }));
    }
    const runStatus = path.match(/^\/api\/v1\/assistant\/runs\/([^/]+)$/);
    if (runStatus && request.method() === "GET") {
      const run = state.previewRuns[runStatus[1]];
      if (!run) throw new Error(`Run status requested outside a pinned Preview session: ${runStatus[1]}`);
      return route.fulfill(json({ run: { run_id: runStatus[1], session_id: run.sessionId, status: run.status, harness_thread_id: null } }));
    }
    if (path === "/api/v1/assistant/models") return route.fulfill(json({ models: [{ id: "qwen3.7-plus", name: "qwen3.7-plus", provider: "dashscope", context_window: 131072, max_output_tokens: 8192, supports_vision: true, supports_tools: true }] }));
    if (path === "/api/v1/assistant/datasets") return route.fulfill(json({ datasets: [] }));
    if (path === "/api/v1/assistant/local-nodes" && request.method() === "GET") {
      return route.fulfill(json({ devices: [] }));
    }
    if (path === "/api/v1/assistant/config") return route.fulfill(json({ default_model_id: "qwen3.7-plus", available_providers: ["dashscope"], kb_enabled: false, web_search_enabled: false }));
    if (path === "/api/v1/sessions" && request.method() === "GET") return route.fulfill(json([]));
    if (path === "/api/v1/assistant/tools") return route.fulfill(json({ tools: [{ name: "lookup_account", description: "Look up the current support account.", category: "support", risk_level: "low" }] }));
    if (path === "/api/v1/mcp/servers") {
      if (options.degradedCatalog) return route.fulfill(json({ detail: { message: "MCP catalog unavailable" } }, 503));
      return route.fulfill(json({ servers: [{ server_id: "66666666-6666-4666-8666-666666666666", name: "Support MCP", enabled: true }], total: 1 }));
    }
    if (path.endsWith("/tools") && path.includes("/api/v1/mcp/servers/")) return route.fulfill(json({ tools: [{ tool_id: "77777777-7777-4777-8777-777777777777", server_id: "66666666-6666-4666-8666-666666666666", runtime_name: "support_ticket_lookup", description: "Read a support ticket.", snapshot_id: "88888888-8888-4888-8888-888888888888", schema_hash: "d".repeat(64), risk_level: "low", enabled: true }], total: 1 }));
    if (path.endsWith("/connections") && path.includes("/api/v1/mcp/servers/")) return route.fulfill(json({ connections: [{ connection_id: "99999999-9999-4999-8999-999999999999", principal_type: "service_account", enabled: true, credential_configured: true }], total: 1 }));
    if (path === "/api/v1/skills") return route.fulfill(json({ skills: [{ name: "support-writing", title: "Support writing", description: "Write concise support replies.", version: "1.0.0", version_id: "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa", content_hash: "e".repeat(64), enabled: true }], total: 1 }));
    if (path === "/api/v1/connectors/available") return route.fulfill(json([{ provider: "confluence", display_name: "Confluence", description: "Search support policies.", enabled: true, connected: true }]));
    if (path === "/api/v1/connectors/mine") return route.fulfill(json([{ provider: "confluence", status: "connected" }]));
    if (path === "/api/v1/connectors/confluence/principals") return route.fulfill(json({ principals: [{ grant_id: "bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb", principal_type: "service_account", scopes: ["read"], allowed_channels: ["preview"], enabled: true }], total: 1 }));
    if (path === "/api/v1/knowledge/datasets") return route.fulfill(json([{ dataset_id: DATASET_ID, name: "Refund policy", description: "Approved refund and billing policy.", visibility: "tenant", embedding_provider: "dashscope", embedding_model: "text-embedding-v4" }]));
    if (path === "/api/v1/eval/datasets") return route.fulfill(json({ datasets: [], total: 0, limit: 200, offset: 0 }));
    if (path === "/api/v1/setup/state") return route.fulfill(json({ configured: true, missing: [], mode: "environment", default_model: null }));
    // The Local Node control plane moved off the /assistant prefix during the
    // Runtime cutover and is polled on console pages.
    if (path === "/api/v1/local-nodes" || path === "/api/v1/assistant/local-nodes") {
      return route.fulfill(json({ devices: [] }));
    }
    throw new Error(`Unhandled API request: ${request.method()} ${path}`);
  });
  return state;
}

function watchHappyPath(page: Page) {
  const errors: string[] = [];
  const badResponses: string[] = [];
  page.on("pageerror", (error) => errors.push(String(error)));
  page.on("console", (message) => {
    if (message.type() === "error" && !/favicon/i.test(message.text())) errors.push(message.text());
  });
  page.on("response", (response) => {
    if (response.status() >= 400 && response.url().includes("/api/v1/")) badResponses.push(`${response.status()} ${response.url()}`);
  });
  return () => {
    expect(errors).toEqual([]);
    expect(badResponses).toEqual([]);
  };
}

async function expectNoHorizontalOverflow(page: Page) {
  expect(await page.evaluate(() => document.documentElement.scrollWidth <= document.documentElement.clientWidth + 1)).toBe(true);
}

async function captureEvidence(
  page: Page,
  name: string,
  options: { animations?: "allow" | "disabled"; fullPage?: boolean } = {},
) {
  const viewportLabel = name.match(/-(\d+)x(\d+)$/);
  if (viewportLabel) {
    expect(page.viewportSize(), `Evidence ${name} must use its labelled viewport`).toEqual({
      width: Number(viewportLabel[1]),
      height: Number(viewportLabel[2]),
    });
  }
  const evidenceDir = path.resolve(process.cwd(), "../reports/agent-studio/as-05/matrix");
  await fs.mkdir(evidenceDir, { recursive: true });
  await page.screenshot({
    path: path.join(evidenceDir, `${name}.png`),
    fullPage: options.fullPage ?? true,
    animations: options.animations ?? "disabled",
  });
}

async function tabTo(page: Page, target: Locator, options: { reverse?: boolean; limit?: number } = {}) {
  const key = options.reverse ? "Shift+Tab" : "Tab";
  await expect(target).toHaveCount(1);
  await expect(target).toBeVisible();
  for (let index = 0; index < (options.limit ?? 40); index += 1) {
    if (await target.evaluate((element) => element === document.activeElement)) return;
    await page.keyboard.press(key);
  }
  throw new Error(`Keyboard focus did not reach ${await target.getAttribute("aria-label") || await target.textContent() || "target"}`);
}

test.describe("Agent Studio directory", () => {
  test("renders populated, filtered, keyboard-accessible desktop and mobile states", async ({ page }) => {
    const assertHappy = watchHappyPath(page);
    await page.setViewportSize({ width: 1440, height: 900 });
    await installHarness(page);
    await page.goto("/agents", { waitUntil: "domcontentloaded" });
    await expect(page.getByTestId("agents-page").getByRole("heading", { name: "Agents", exact: true })).toBeVisible();
    await expect(page.getByRole("link", { name: "Support Triage" })).toBeVisible();
    await expect(page.getByText("3 agents")).toBeVisible();
    await captureEvidence(page, "directory-populated-desktop-1440x900");
    await page.getByLabel("Search agents").fill("Policy");
    await expect(page.getByRole("link", { name: "Policy Copilot" })).toBeVisible();
    await expect(page.getByRole("link", { name: "Support Triage" })).toHaveCount(0);
    await page.getByLabel("Search agents").fill("");
    await expect(page.getByRole("link", { name: "Support Triage" })).toBeVisible();
    await page.getByLabel("Search agents").fill("no matching agent");
    await expect(page.getByText("No agents match these filters.")).toBeVisible();
    await captureEvidence(page, "directory-filtered-empty-desktop-1440x900");
    await page.getByLabel("Search agents").fill("");
    const createButton = page.getByRole("button", { name: "Create agent" });
    await tabTo(page, createButton);
    await expect(createButton).toBeFocused();
    await page.keyboard.press("Enter");
    await expect(page).toHaveURL(/\/agents\/new$/);
    await page.goBack({ waitUntil: "domcontentloaded" });
    await expect(page.getByTestId("agents-page")).toBeVisible();
    await assertNoBlockingA11yIssues(page, ["main"]);
    await expectNoHorizontalOverflow(page);

    await page.setViewportSize({ width: 390, height: 844 });
    await expect(page.locator(".agent-list-mobile")).toBeVisible();
    await expect(page.locator(".agent-table-desktop")).toBeHidden();
    await captureEvidence(page, "directory-populated-mobile-390x844");
    await page.getByLabel("Search agents").fill("no matching agent");
    await expect(page.getByText("No agents match these filters.")).toBeVisible();
    await captureEvidence(page, "directory-filtered-empty-mobile-390x844");
    await page.getByLabel("Search agents").fill("");
    await expectNoHorizontalOverflow(page);
    assertHappy();
  });

  test("shows loading and empty states without inventing data", async ({ page }) => {
    await page.setViewportSize({ width: 1440, height: 900 });
    await installHarness(page, { listState: "slow" });
    await page.goto("/agents", { waitUntil: "domcontentloaded" });
    await expect(page.getByRole("status", { name: "Loading agents" })).toBeVisible();
    await captureEvidence(page, "directory-loading-desktop-1440x900");
    await page.setViewportSize({ width: 390, height: 844 });
    await captureEvidence(page, "directory-loading-mobile-390x844");
    await expect(page.getByRole("link", { name: "Support Triage" })).toBeVisible();
  });

  test("shows retryable API and permission states", async ({ page }) => {
    await page.setViewportSize({ width: 1440, height: 900 });
    await installHarness(page, { listState: "error" });
    await page.goto("/agents", { waitUntil: "domcontentloaded" });
    await expect(page.getByText("Agents could not be loaded")).toBeVisible();
    await expect(page.getByRole("button", { name: "Retry" })).toBeVisible();
    await captureEvidence(page, "directory-api-error-desktop-1440x900");
    await page.setViewportSize({ width: 390, height: 844 });
    await captureEvidence(page, "directory-api-error-mobile-390x844");
  });

  test("shows a bounded permission state", async ({ page }) => {
    await page.setViewportSize({ width: 1440, height: 900 });
    await installHarness(page, { listState: "forbidden" });
    await page.goto("/agents", { waitUntil: "domcontentloaded" });
    await expect(page.getByText("You don't have access to Agent Studio")).toBeVisible();
    await expect(page.getByRole("button", { name: "Create agent" })).toHaveCount(0);
    await captureEvidence(page, "directory-permission-desktop-1440x900");
    await page.setViewportSize({ width: 390, height: 844 });
    await captureEvidence(page, "directory-permission-mobile-390x844");
  });

  test("shows a true empty state and clear-filter state", async ({ page }) => {
    await page.setViewportSize({ width: 1440, height: 900 });
    await installHarness(page, { listState: "empty" });
    await page.goto("/agents", { waitUntil: "domcontentloaded" });
    await expect(page.getByText("Create your first agent to configure a reusable runtime.")).toBeVisible();
    await expect(page.getByLabel("Agent directory").getByRole("button", { name: "Create blank agent" })).toBeVisible();
    await expect(page.getByRole("button", { name: "Support triage template" })).toBeVisible();
    await expect(page.getByRole("button", { name: "Knowledge guide template" })).toBeVisible();
    await captureEvidence(page, "directory-empty-desktop-1440x900");
    await page.setViewportSize({ width: 390, height: 844 });
    await captureEvidence(page, "directory-empty-mobile-390x844");
    await page.getByLabel("Search agents").fill("missing");
    await expect(page.getByText("No agents match these filters.")).toBeVisible();
    await page.getByRole("button", { name: "Clear filters" }).click();
    await expect(page.getByText("Create your first agent to configure a reusable runtime.")).toBeVisible();
  });

  test("renders the Agent directory, creation flow, and Studio chrome in Chinese", async ({ page }) => {
    await installHarness(page, { locale: "zh-CN" });
    await page.goto("/agents", { waitUntil: "domcontentloaded" });
    await expect(page.getByText("创建、配置、测试并发布可复用的 Agent。").first()).toBeVisible();
    await expect(page.getByRole("button", { name: "创建 Agent" })).toBeVisible();
    await page.goto("/agents/new", { waitUntil: "domcontentloaded" });
    await expect(page.getByRole("heading", { name: "创建 Agent" })).toBeVisible();
    await expect(page.getByRole("button", { name: /空白 Agent/ })).toBeVisible();
    await page.goto(`/agents/${AGENT_ID}`, { waitUntil: "domcontentloaded" });
    await expect(page.getByRole("button", { name: "保存草稿" }).first()).toBeVisible();
    await expect(page.getByText("全部更改已保存")).toBeVisible();
  });

  test("copies an editable Agent and confirms owner-only archive", async ({ page }) => {
    const state = await installHarness(page);
    await page.goto("/agents", { waitUntil: "domcontentloaded" });
    await page.getByRole("button", { name: "Actions for Support Triage" }).click();
    await page.getByRole("menuitem", { name: "Copy agent" }).click();
    await expect(page).toHaveURL(new RegExp(`/agents/${COPIED_AGENT_ID}$`));
    expect(state.copiedAgents).toBe(1);

    await page.goto("/agents", { waitUntil: "domcontentloaded" });
    await page.getByRole("button", { name: "Actions for Support Triage" }).click();
    await page.getByRole("menuitem", { name: "Archive" }).click();
    await expect(page.getByRole("dialog", { name: "Archive Support Triage?" })).toBeVisible();
    const confirmArchiveButton = page.getByRole("button", { name: "Archive Agent" });
    await tabTo(page, confirmArchiveButton);
    await page.keyboard.press("Enter");
    await expect(page.getByRole("row", { name: /Support Triage/ }).getByText("Archived")).toBeVisible();
    expect(state.archivedAgents).toBe(1);
  });

  test("exposes role-correct Copy and Archive actions on mobile", async ({ page }) => {
    const state = await installHarness(page);
    await page.setViewportSize({ width: 390, height: 844 });
    await page.goto("/agents", { waitUntil: "domcontentloaded" });

    await page.getByRole("button", { name: "Actions for Support Triage" }).click();
    await page.getByRole("menuitem", { name: "Copy agent" }).click();
    await expect(page).toHaveURL(new RegExp(`/agents/${COPIED_AGENT_ID}$`));
    expect(state.copiedAgents).toBe(1);

    await page.goto("/agents", { waitUntil: "domcontentloaded" });
    await page.getByRole("button", { name: "Actions for Support Triage" }).click();
    await page.getByRole("menuitem", { name: "Archive" }).click();
    await expect(page.getByRole("dialog", { name: "Archive Support Triage?" })).toBeVisible();
    await page.getByRole("button", { name: "Archive Agent" }).click();
    await expect(page.locator(".agent-mobile-row").filter({ hasText: "Support Triage" }).getByText("Archived")).toBeVisible();
    expect(state.archivedAgents).toBe(1);

    await page.getByRole("button", { name: "Actions for Research Analyst" }).click();
    let activeMenu = page.getByRole("menu").last();
    await expect(activeMenu).toBeVisible();
    await expect(activeMenu.getByRole("menuitem", { name: "Copy agent" })).toHaveAttribute("aria-disabled", "false");
    await expect(activeMenu.getByRole("menuitem", { name: "Archive" })).toHaveAttribute("aria-disabled", "true");
    await page.keyboard.press("Escape");

    await page.getByRole("button", { name: "Actions for Policy Copilot" }).click();
    activeMenu = page.getByRole("menu").last();
    await expect(activeMenu).toBeVisible();
    await expect(activeMenu.getByRole("menuitem", { name: "Copy agent" })).toHaveAttribute("aria-disabled", "true");
    await expect(activeMenu.getByRole("menuitem", { name: "Archive" })).toHaveAttribute("aria-disabled", "true");
    const overlay = activeMenu.locator("xpath=ancestor::*[contains(@class, 'ant-dropdown')][1]");
    await expect(overlay).toBeVisible();
    await expect.poll(async () => overlay.evaluate((element) => {
      const style = window.getComputedStyle(element);
      const rect = element.getBoundingClientRect();
      return style.opacity === "1" && style.visibility !== "hidden" && rect.width > 0 && rect.height > 0;
    })).toBe(true);
    const overlayBox = await overlay.boundingBox();
    expect(overlayBox).not.toBeNull();
    expect(overlayBox!.x).toBeGreaterThanOrEqual(0);
    expect(overlayBox!.x + overlayBox!.width).toBeLessThanOrEqual(390);
    await expectNoHorizontalOverflow(page);
    await captureEvidence(page, "directory-role-actions-mobile-390x844", { animations: "allow", fullPage: false });
  });

  test("removes Agent navigation and routes when the runtime feature flag is off", async ({ page }) => {
    await installHarness(page);
    await page.addInitScript(() => {
      window.__AI_GATEWAY_RUNTIME_CONFIG__ = { agentStudioEnabled: "false" };
    });
    await page.goto("/agents", { waitUntil: "domcontentloaded" });
    await expect(page.getByRole("link", { name: "Agents" })).toHaveCount(0);
    await expect(page.getByTestId("agents-page")).toHaveCount(0);
    await page.goto("/assistant", { waitUntil: "domcontentloaded" });
    await expect(page.locator("#assistant-chat-composer")).toBeVisible();
    await expect(page).toHaveURL(/\/assistant$/);
  });
});

test.describe("Create agent", () => {
  test("completes the three-step Draft-only creation flow", async ({ page }) => {
    const assertHappy = watchHappyPath(page);
    await page.setViewportSize({ width: 1440, height: 900 });
    const state = await installHarness(page);
    await page.goto("/agents/new", { waitUntil: "domcontentloaded" });
    await expect(page.getByRole("heading", { name: "Create agent" })).toBeVisible();
    const blankTemplate = page.getByRole("button", { name: /^Blank agent\b/ });
    await tabTo(page, blankTemplate);
    await page.keyboard.press("Space");
    await expect(blankTemplate).toHaveAttribute("aria-pressed", "true");
    await assertNoBlockingA11yIssues(page, ["main"]);
    await captureEvidence(page, "create-blank-identity-desktop-1440x900");
    await page.getByLabel("Name").fill("Billing Guide");
    await page.getByLabel("Description").fill("Routes billing questions to the correct policy and queue.");
    await page.getByLabel("Icon URL").fill("http://example.com/icon.png?size=small");
    await expect(page.getByText("Use public HTTPS without credentials, query, or fragment.")).toBeVisible();
    await expect(page.getByRole("button", { name: "Continue" })).toBeDisabled();
    await page.getByLabel("Icon URL").fill("https://example.com/icon.png");
    const continueButton = page.getByRole("button", { name: "Continue" });
    await tabTo(page, continueButton);
    await page.keyboard.press("Enter");
    // No concrete model id is preselected: the server applies its deployment default.
    await expect(page.getByRole("region", { name: "Behavior" }).getByRole("combobox", { name: "Model" })).toHaveValue("");
    await expect(page.getByLabel("Agent instructions")).not.toHaveValue("");
    await captureEvidence(page, "create-behavior-desktop-1440x900");
    await page.getByRole("button", { name: "Continue" }).click();
    await expect(page.getByRole("heading", { name: "Start" })).toBeVisible();
    await expect(page.getByText("Nothing is published until you create a version.")).toBeVisible();
    await captureEvidence(page, "create-start-desktop-1440x900");
    await page.getByRole("button", { name: "Create agent" }).click();
    await expect(page).toHaveURL(new RegExp(`/agents/${AGENT_ID}$`));
    expect(state.createdAgents).toBe(1);
    expect(state.createdSpec?.instructions.trim()).toBeTruthy();
    assertHappy();
  });

  test("creates from a controlled template without copying credentials or resources", async ({ page }) => {
    await page.setViewportSize({ width: 1440, height: 900 });
    const state = await installHarness(page);
    await page.goto("/agents/new?template=support", { waitUntil: "domcontentloaded" });
    await expect(page.getByRole("button", { name: /Support triage/ })).toHaveAttribute("aria-pressed", "true");
    await expect(page.getByLabel("Welcome message")).toHaveValue("Describe the support request you want me to triage.");
    await captureEvidence(page, "create-controlled-template-desktop-1440x900");
    await page.getByLabel("Name").fill("Template Support");
    await page.getByLabel("Description").fill("A controlled support starter without copied authority.");
    await page.getByRole("button", { name: "Save as draft" }).click();
    await expect(page).toHaveURL(new RegExp(`/agents/${AGENT_ID}$`));
    expect(state.createdSpec?.instructions).toContain("Classify support requests");
    expect(state.createdSpec?.capabilities).toEqual([]);
    expect(state.createdSpec?.knowledge).toEqual([]);
    expect(JSON.stringify(state.createdSpec)).not.toMatch(/"(?:api[_-]?key|access[_-]?token|refresh[_-]?token|secret|oauth)[^"]*"/i);
  });

  test("preserves creation fields when the Draft API fails", async ({ page }) => {
    await page.setViewportSize({ width: 1440, height: 900 });
    await installHarness(page, { createFailure: true });
    await page.goto("/agents/new", { waitUntil: "domcontentloaded" });
    await page.getByLabel("Name").fill("Retryable Agent");
    await page.getByLabel("Description").fill("Keep this form intact after a server failure.");
    await page.getByRole("button", { name: "Save as draft" }).click();
    await expect(page.getByText("Agent could not be created")).toBeVisible();
    await expect(page.getByText("Draft storage is temporarily unavailable.")).toBeVisible();
    await expect(page.getByLabel("Name")).toHaveValue("Retryable Agent");
    await captureEvidence(page, "create-api-failure-desktop-1440x900");
  });
});

test.describe("Agent Studio workbench", () => {
  const previewFailures = [
    { name: "configuration", failure: { code: "AGENT_DRAFT_REVISION_MISMATCH", message: "The saved Draft revision changed.", status: 409 }, expected: "Saved configuration changed: The saved Draft revision changed." },
    { name: "resource", failure: { code: "AGENT_CAPABILITY_UNAVAILABLE", message: "A bound capability is unavailable.", status: 422 }, expected: "Capability unavailable: A bound capability is unavailable." },
    { name: "permission", failure: { code: "AGENT_FORBIDDEN", message: "Preview permission is required.", status: 403 }, expected: "Permission denied: Preview permission is required." },
    { name: "provider", failure: { code: "AGENT_PROVIDER_UNAVAILABLE", message: "The configured provider is unavailable.", status: 503 }, expected: "Provider unavailable: The configured provider is unavailable." },
    { name: "runtime", failure: { code: "AGENT_RUNTIME_FAILED", message: "The isolated runtime failed.", status: 500 }, expected: "Runtime failed: The isolated runtime failed." },
  ] as const;

  for (const scenario of previewFailures) {
    test(`labels ${scenario.name} Preview failures without exposing internals`, async ({ page }) => {
      await installHarness(page, { previewFailure: scenario.failure });
      await page.goto(`/agents/${AGENT_ID}`, { waitUntil: "domcontentloaded" });
      await page.locator(".agent-preview-header").getByRole("button", { name: "New session" }).click();
      await expect(page.getByRole("alert").filter({ hasText: scenario.expected })).toBeVisible();
    });
  }

  test("keeps a server-default Draft editable and saveable", async ({ page }) => {
    const state = await installHarness(page);
    state.currentSpec = {
      ...state.currentSpec,
      model: { ...state.currentSpec.model, model_id: "" },
    };
    await page.goto(`/agents/${AGENT_ID}`, { waitUntil: "domcontentloaded" });

    await expect(page.getByText("Server default").first()).toBeVisible();
    await page.getByLabel("Description").fill("Keep using the deployment default model.");
    const saveDraft = page.locator(".agent-studio-actions").getByRole("button", { name: "Save draft" });
    await expect(saveDraft).toBeEnabled();
    await saveDraft.click();

    await expect(page.locator(".agent-save-state")).toHaveText("Saved");
    expect(state.currentSpec.model.model_id).toBe("");
  });

  test("saves Draft edits and surfaces validation and 409 conflict states", async ({ page }) => {
    const state = await installHarness(page);
    await page.setViewportSize({ width: 1440, height: 900 });
    await page.goto(`/agents/${AGENT_ID}`, { waitUntil: "domcontentloaded" });
    await expect(page.getByRole("heading", { name: "Support Triage" })).toBeVisible();
    await captureEvidence(page, "studio-clean-desktop-1440x900");
    await page.getByLabel("Name").fill("Support Triage Pro");
    await expect(page.getByText("Unsaved changes")).toBeVisible();
    await captureEvidence(page, "studio-dirty-desktop-1440x900");
    const headerSave = page.locator(".agent-studio-actions").getByRole("button", { name: "Save draft" });
    await headerSave.click();
    await expect(page.getByText("Saving…")).toBeVisible();
    await captureEvidence(page, "studio-saving-desktop-1440x900");
    await expect(page.locator(".agent-save-state")).toHaveText("Saved");
    await expect(headerSave).toBeDisabled();
    await expect(page.getByText("Draft · revision 9")).toBeVisible();
    expect(state.currentName).toBe("Support Triage Pro");
    expect(state.metadataPatchRequests).toBe(0);
    await captureEvidence(page, "studio-saved-desktop-1440x900");

    state.validationNext = true;
    await page.getByLabel("Description").fill("Needs validation");
    await headerSave.click();
    await expect(page.getByText("Description needs a clearer purpose.")).toBeVisible();
    await expect(page.getByLabel("Description")).toBeFocused();
    expect(state.currentDescription).toBe(agents[0].description);
    expect(state.metadataPatchRequests).toBe(0);
    await captureEvidence(page, "studio-field-error-desktop-1440x900");

    state.conflictNext = true;
    await page.getByLabel("Description").fill("Local conflicting edit");
    await headerSave.click();
    await expect(page.getByText("Draft changed in another session")).toBeVisible();
    await expect(page.getByText("Reload revision 10 or copy your changes before continuing.")).toBeVisible();
    await expect(page.getByText("Local fields: Description")).toBeVisible();
    expect(state.currentDescription).toBe(agents[0].description);
    expect(state.metadataPatchRequests).toBe(0);
    await captureEvidence(page, "studio-conflict-desktop-1440x900");
    await page.getByRole("button", { name: "Reload and reapply" }).click();
    await expect(page.getByLabel("Description")).toHaveValue("Local conflicting edit");
    await expect(page.getByText("Unsaved changes")).toBeVisible();
    await page.getByRole("button", { name: "Discard" }).click();
    await expect(page.getByText("All changes saved")).toBeVisible();

    state.networkNext = true;
    await page.getByLabel("Description").fill("Keep this local edit through a retryable network error.");
    await headerSave.click();
    await expect(page.getByText("Draft could not be saved")).toBeVisible();
    await expect(page.getByLabel("Description")).toHaveValue("Keep this local edit through a retryable network error.");
    expect(state.currentDescription).toBe(agents[0].description);
    expect(state.metadataPatchRequests).toBe(0);
    await captureEvidence(page, "studio-network-error-desktop-1440x900");
    await page.getByRole("button", { name: "Retry save" }).click();
    await expect(page.locator(".agent-save-state")).toHaveText("Saved");
  });

  test("preserves edits made while an earlier Draft batch is saving", async ({ page }) => {
    const state = await installHarness(page);
    await page.goto(`/agents/${AGENT_ID}`, { waitUntil: "domcontentloaded" });
    const headerSave = page.locator(".agent-studio-actions").getByRole("button", { name: "Save draft" });
    await page.getByLabel("Description").fill("First save batch");
    await headerSave.click();
    await expect(page.getByText("Saving…")).toBeVisible();
    await page.getByLabel("Description").fill("Second batch stays local");
    await expect(page.locator(".agent-save-state")).toHaveText("Unsaved changes");
    await expect(page.getByLabel("Description")).toHaveValue("Second batch stays local");
    expect(state.currentDescription).toBe("First save batch");
    await headerSave.click();
    await expect(page.locator(".agent-save-state")).toHaveText("Saved");
    await expect(page.getByLabel("Description")).toHaveValue("Second batch stays local");
    expect(state.currentDescription).toBe("Second batch stays local");
  });

  test("runs isolated Draft and Version Preview sessions with tool and Knowledge events", async ({ page }) => {
    const assertHappy = watchHappyPath(page);
    await page.setViewportSize({ width: 1440, height: 900 });
    const state = await installHarness(page);
    await page.goto(`/agents/${AGENT_ID}`, { waitUntil: "domcontentloaded" });
    await page.locator(".agent-preview-header").getByRole("button", { name: "New session" }).click();
    await expect(page.getByText("New isolated session · Draft r8")).toBeVisible();
    await page.getByLabel("Message this agent").fill("Classify this request: I was charged twice.");
    await page.getByRole("button", { name: "Send Preview message" }).click();
    await expect(page.getByText("Billing issue · duplicate charge. Route this request to Billing review.")).toBeVisible();
    await expect(page.getByText("Platform tool · lookup_account")).toBeVisible();
    await expect(page.getByText("Knowledge · Refund policy")).toBeVisible();
    await captureEvidence(page, "preview-draft-events-desktop-1440x900");
    expect(state.draftSessions).toBe(1);

    await page.getByLabel("Name").fill("Unsaved name");
    await expect(page.getByText("Unsaved form changes are not running. Preview uses the saved Draft r8.")).toBeVisible();
    page.once("dialog", async (dialog) => {
      expect(dialog.message()).toBe("Switching the Preview target creates a new isolated session. Continue?");
      await dialog.accept();
    });
    await page.getByLabel("Preview target").click();
    await page.locator(".ant-select-dropdown .ant-select-item-option").filter({ hasText: "Version 7" }).click();
    await expect(page.getByText("Unsaved form changes are not running. Preview uses the saved Version 7.")).toBeVisible();
    await expect(page.locator(".agent-effective-summary").getByText("Model qwen3.7-max")).toBeVisible();
    await expect(page.locator(".agent-effective-summary").getByText("0 configured capabilities")).toBeVisible();
    await page.locator(".agent-preview-header").getByRole("button", { name: "New session" }).click();
    await expect(page.getByText("New isolated session · Version 7")).toBeVisible();
    await captureEvidence(page, "preview-version-desktop-1440x900");
    expect(state.versionSessions).toBe(1);
    page.once("dialog", async (dialog) => {
      await dialog.accept();
    });
    await page.getByLabel("Preview target").click();
    await page.locator(".ant-select-dropdown .ant-select-item-option").filter({ hasText: "Draft r8" }).click();
    await expect(page.getByText("Billing issue · duplicate charge. Route this request to Billing review.")).toBeVisible();
    await page.getByRole("button", { name: "Clear session" }).click();
    await expect(page.getByText("Start an isolated Preview session")).toBeVisible();
    await assertNoBlockingA11yIssues(page, ["main"]);
    assertHappy();
  });

  test("restores only the pinned Preview session after refresh", async ({ page }) => {
    const assertHappy = watchHappyPath(page);
    const state = await installHarness(page);
    await page.goto(`/agents/${AGENT_ID}`, { waitUntil: "domcontentloaded" });
    await page.locator(".agent-preview-header").getByRole("button", { name: "New session" }).click();
    await page.getByLabel("Message this agent").fill("Classify my duplicate charge.");
    await page.getByRole("button", { name: "Send Preview message" }).click();
    await expect(page.getByText("Billing issue · duplicate charge. Route this request to Billing review.")).toBeVisible();
    await expect(page.locator(".agent-preview-header").getByRole("button", { name: "New session" })).toBeEnabled();

    await page.reload({ waitUntil: "domcontentloaded" });
    await expect(page.getByText("Classify my duplicate charge.")).toBeVisible();
    await expect(page.getByText("Billing issue · duplicate charge. Route this request to Billing review.")).toBeVisible();
    await expect(page.getByText("New isolated session · Draft r8")).toBeVisible();
    expect(state.draftSessions).toBe(1);
    expect(state.previewPinReads.length).toBeGreaterThan(0);
    expect(state.previewPinReads.every((id) => id === "draft-session-1")).toBe(true);
    expect(state.previewHistoryReads.length).toBeGreaterThan(0);
    expect(state.previewHistoryReads.every((id) => id === "draft-session-1")).toBe(true);
    expect(state.previewArtifactReads.every((id) => id === "draft-session-1")).toBe(true);
    expect(state.previewArtifactReads.length).toBeGreaterThan(0);
    assertHappy();
  });

  test("preserves selections when one catalog is degraded and keeps Viewer read-only", async ({ page }) => {
    await page.setViewportSize({ width: 1440, height: 900 });
    await installHarness(page, { callerRole: "viewer", degradedCatalog: true });
    await page.goto(`/agents/${AGENT_ID}`, { waitUntil: "domcontentloaded" });
    await expect(page.getByText("MCP tools catalog is unavailable. Existing selections are preserved.")).toBeVisible();
    await expect(page.getByText("Viewer access")).toBeVisible();
    await expect(page.locator(".agent-studio-actions").getByRole("button", { name: "Save draft" })).toBeDisabled();
    await expect(page.locator(".agent-preview-header").getByRole("button", { name: "New session" })).toBeEnabled();
    await captureEvidence(page, "studio-degraded-viewer-desktop-1440x900");
  });

  test("keeps Editor configuration and Preview enabled without owner-only archive", async ({ page }) => {
    const state = await installHarness(page, { callerRole: "editor" });
    await page.goto(`/agents/${AGENT_ID}`, { waitUntil: "domcontentloaded" });
    await expect(page.getByText("Editor")).toBeVisible();
    await page.getByLabel("Description").fill("Editor can save this Draft.");
    const saveDraft = page.locator(".agent-studio-actions").getByRole("button", { name: "Save draft" });
    await expect(saveDraft).toBeEnabled();
    await saveDraft.click();
    await expect(page.locator(".agent-save-state")).toHaveText("Saved");
    await expect(page.getByText("Draft · revision 9")).toBeVisible();
    expect(state.currentDescription).toBe("Editor can save this Draft.");
    expect(state.draftRevision).toBe(9);
    expect(state.metadataPatchRequests).toBe(0);

    const newSession = page.locator(".agent-preview-header").getByRole("button", { name: "New session" });
    await expect(newSession).toBeEnabled();
    await newSession.click();
    await expect(page.getByText("New isolated session · Draft r9")).toBeVisible();
    await page.getByLabel("Message this agent").fill("Run an Editor Preview.");
    await page.getByRole("button", { name: "Send Preview message" }).click();
    await expect(page.getByText("Billing issue · duplicate charge. Route this request to Billing review.")).toBeVisible();
    expect(state.draftSessions).toBe(1);

    await page.goto("/agents", { waitUntil: "domcontentloaded" });
    await page.getByRole("button", { name: "Actions for Research Analyst" }).click();
    await expect(page.getByRole("menuitem", { name: "Copy agent" })).toHaveAttribute("aria-disabled", "false");
    await expect(page.getByRole("menuitem", { name: "Archive" })).toHaveAttribute("aria-disabled", "true");
  });

  test("uses Configure/Preview tabs and an accessible section drawer on mobile", async ({ page }) => {
    await page.setViewportSize({ width: 390, height: 844 });
    await installHarness(page);
    await page.goto(`/agents/${AGENT_ID}`, { waitUntil: "domcontentloaded" });
    const trigger = page.locator(".agent-mobile-section-trigger");
    await tabTo(page, trigger);
    await expect(trigger).toBeFocused();
    await page.keyboard.press("Enter");
    const drawer = page.getByRole("dialog", { name: "Agent sections" });
    await expect(drawer).toBeVisible();
    for (let index = 0; index < 10; index += 1) {
      await page.keyboard.press("Tab");
      expect(await drawer.evaluate((element) => element.contains(document.activeElement))).toBe(true);
    }
    await page.keyboard.press("Shift+Tab");
    expect(await drawer.evaluate((element) => element.contains(document.activeElement))).toBe(true);
    await captureEvidence(page, "studio-mobile-section-drawer-390x844");
    await page.keyboard.press("Escape");
    await expect(drawer).toBeHidden();
    await expect(trigger).toBeFocused();
    await page.keyboard.press("Space");
    await expect(drawer).toBeVisible();
    const capabilitiesButton = drawer.getByRole("button", { name: "Capabilities" });
    await tabTo(page, capabilitiesButton);
    await expect(capabilitiesButton).toBeFocused();
    await page.keyboard.press("Enter");
    await expect(drawer).toBeHidden();
    await expect(trigger).toBeFocused();
    await expect(page.getByRole("heading", { name: "Capabilities" })).toBeVisible();
    await page.getByRole("tab", { name: "MCP tools" }).click();
    await expect(page.getByRole("checkbox", { name: "Enable Support ticket lookup" })).toBeVisible();
    await page.getByRole("tab", { name: "Skills" }).click();
    await expect(page.getByText("Support writing")).toBeVisible();
    await page.getByRole("tab", { name: "Connectors" }).click();
    await expect(page.getByText("Confluence")).toBeVisible();
    await page.getByRole("tab", { name: "Platform tools" }).click();
    await captureEvidence(page, "studio-mobile-configure-390x844");
    await page.getByRole("button", { name: "Test in Preview" }).click();
    await expect(page.getByTestId("agent-preview-panel")).toBeVisible();
    await expect(page.locator(".agent-config-canvas")).toBeHidden();
    await expect(page.getByLabel("Message this agent")).toBeFocused();
    await captureEvidence(page, "studio-mobile-preview-390x844");
    await expectNoHorizontalOverflow(page);
    await assertNoBlockingA11yIssues(page, ["main"]);
  });

  test("keeps configuration and Preview reachable at the tablet breakpoint", async ({ page }) => {
    await page.setViewportSize({ width: 1024, height: 768 });
    await installHarness(page);
    await page.goto(`/agents/${AGENT_ID}`, { waitUntil: "domcontentloaded" });
    await expect(page.locator(".agent-studio-sections")).toBeVisible();
    await expect(page.locator(".agent-config-canvas")).toBeVisible();
    await expect(page.getByTestId("agent-preview-panel")).toBeVisible();
    await expectNoHorizontalOverflow(page);
    await captureEvidence(page, "studio-tablet-1024x768");
  });

  test("honors reduced-motion preferences", async ({ page }) => {
    await page.emulateMedia({ reducedMotion: "reduce" });
    await installHarness(page);
    await page.goto(`/agents/${AGENT_ID}`, { waitUntil: "domcontentloaded" });
    const duration = await page.locator(".agent-studio").evaluate((element) => getComputedStyle(element).animationDuration);
    expect(parseFloat(duration)).toBeLessThanOrEqual(0.001);
  });
});
