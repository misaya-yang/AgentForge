import fs from "node:fs/promises";
import path from "node:path";

import { expect, test, type Locator, type Page, type Route } from "@playwright/test";

import {
  assertNoBlockingA11yIssues,
  installClientAuth,
  seedClientPrefs,
} from "./support/helpers";
import type {
  AgentPublication,
  AgentPublishEvent,
  AgentReleaseEvaluation,
  AgentReleaseStatus,
  AgentSpec,
  AgentVersion,
} from "../src/types/agents";

const AGENT_ID = "11111111-1111-4111-8111-111111111111";
const DRAFT_ID = "12121212-1212-4121-8121-121212121212";
const DATASET_ID = "13131313-1313-4131-8131-131313131313";
const VERSION_ONE_ID = "21111111-2111-4111-8111-211111111111";
const VERSION_TWO_ID = "22222222-2222-4222-8222-222222222222";
const VERSION_THREE_ID = "23333333-2333-4333-8333-233333333333";
const PUBLICATION_ID = "31111111-3111-4111-8111-311111111111";
const NOW = "2026-07-19T08:00:00.000Z";

const spec: AgentSpec = {
  schema_version: "agent-spec/v1",
  identity: {
    icon_url: null,
    theme_color: "#6D5CE7",
    welcome_message: "How can I help with this request?",
    suggested_prompts: ["Classify this billing request"],
  },
  instructions: "Classify support requests and explain the selected queue.",
  model: {
    model_id: "qwen3.7-plus",
    provider_id: "dashscope",
    temperature: 0.2,
    max_tokens: 4096,
  },
  capabilities: [],
  knowledge: [],
  memory: { mode: "session" },
};

type Role = "owner" | "editor" | "viewer";
type EvalStatus = "queued" | "running" | "passed" | "failed" | "cancelled" | "stale";

interface HarnessOptions {
  role?: Role;
  withDataset?: boolean;
  statuses?: EvalStatus[];
  blockingPassed?: boolean;
  failFirstPublish?: boolean;
  theme?: "light" | "dark";
}

interface HarnessState {
  role: Role;
  draftRevision: number;
  currentSpec: AgentSpec;
  evaluations: AgentReleaseEvaluation[];
  versions: AgentVersion[];
  publications: AgentPublication[];
  events: AgentPublishEvent[];
  publishCalls: number;
  rollbackCalls: number;
  publishKeys: string[];
  publishRunIds: Array<string | null>;
  successfulPublishKeys: Set<string>;
  rollbackKeys: string[];
  selectedEvalDatasetIds: Array<string | null>;
  successfulRollbackKeys: Set<string>;
  failedPublish: boolean;
}

function response(body: unknown, status = 200) {
  return { status, contentType: "application/json", body: JSON.stringify(body) };
}

function eventId(index: number, status: string) {
  return `4${index}${status.length}`.padEnd(8, "0") + "-4000-4000-8000-" + String(index).padStart(12, "0");
}

function evaluation(status: EvalStatus, index: number, blockingPassed = false): AgentReleaseEvaluation {
  const evaluationId = `5${index}000000-5000-4000-8000-${String(index).padStart(12, "0")}`;
  const finalStatus = status === "stale" ? "passed" : status;
  const lifecycle: AgentReleaseStatus[] = status === "queued"
    ? ["queued"]
    : status === "running"
      ? ["queued", "running"]
      : ["queued", "running", finalStatus];
  const blocking = status === "failed" || blockingPassed
    ? [{ code: "RESOURCE_REVOKED", field: "capabilities[0]", message: "The bound resource is no longer authorized." }]
    : [];
  return {
    tenant_id: "tenant-a",
    evaluation_id: evaluationId,
    agent_id: AGENT_ID,
    draft_id: DRAFT_ID,
    draft_revision: 8,
    spec_hash: "a".repeat(64),
    runtime_fingerprint: { spec: "sha256:" + "a".repeat(64), resources: "sha256:" + "b".repeat(64) },
    runtime_fingerprint_hash: "b".repeat(64),
    release_identity_hash: "c".repeat(64),
    evaluation_identity_hash: "e".repeat(64),
    profile_id: "offline_v1",
    profile_version: "1",
    dataset_id: null,
    dataset_version: null,
    dataset_manifest_hash: null,
    experiment_run_id: null,
    channel: "hosted",
    auth_mode: "private",
    channel_policy: { attachments: false, high_risk_tools: false, allowed_origins: [] },
    channel_policy_hash: "d".repeat(64),
    status,
    stale: status === "stale",
    stale_reasons: status === "stale" ? ["runtime_fingerprint_changed"] : [],
    validation_snapshot: { schema_version: "agent-release-validation/v1", valid: blocking.length === 0 },
    gate_snapshot: {
      schema_version: "agent-release-gate/v1",
      status: status === "queued" || status === "running"
        ? status
        : status === "failed" || blocking.length > 0
        ? "failed"
        : status === "cancelled"
          ? "cancelled"
          : "passed",
      profile_id: "offline_v1",
      profile_version: "1",
      execution_scope: "provider_free_integrity",
      model_quality_evaluated: false,
      blocking_findings: blocking,
      non_blocking_findings: [{ code: "MODEL_QUALITY_NOT_EVALUATED", field: "profile", message: "No approved production model-quality profile is configured." }],
      metrics: { validation_duration_ms: 18, provider_cost_cents: 0 },
    },
    events: lifecycle.map((item, sequence) => ({
      event_id: eventId(index * 10 + sequence, item),
      evaluation_id: evaluationId,
      sequence: sequence + 1,
      status: item,
      summary: {},
      created_at: NOW,
    })),
    created_by: "owner-user",
    created_at: NOW,
    completed_at: NOW,
  };
}

function version(id: string, number: number, revision: number, evaluationId: string | null): AgentVersion {
  return {
    tenant_id: "tenant-a",
    agent_version_id: id,
    agent_id: AGENT_ID,
    version_number: number,
    schema_version: "agent-spec/v1",
    spec,
    spec_hash: String(number).repeat(64),
    source_draft_id: DRAFT_ID,
    source_draft_revision: revision,
    release_evaluation_id: evaluationId,
    release_identity_hash: evaluationId ? "c".repeat(64) : null,
    created_by: "owner-user",
    created_at: NOW,
  };
}

function publication(versionId = VERSION_TWO_ID, versionNumber = 2): AgentPublication {
  return {
    tenant_id: "tenant-a",
    publication_id: PUBLICATION_ID,
    agent_id: AGENT_ID,
    channel: "hosted",
    public_id: "support-triage-hosted",
    version_id: versionId,
    version_number: versionNumber,
    version_spec_hash: String(versionNumber).repeat(64),
    auth_mode: "private",
    policy: { attachments: false, high_risk_tools: false, allowed_origins: [] },
    status: "active",
    created_by: "owner-user",
    updated_by: "owner-user",
    created_at: NOW,
    updated_at: NOW,
  };
}

function publishEvent(
  id: string,
  operation: "promote" | "rollback",
  fromVersionId: string | null,
  toVersionId: string,
  reason: string,
): AgentPublishEvent {
  return {
    event_id: id,
    publication_id: PUBLICATION_ID,
    agent_id: AGENT_ID,
    from_version_id: fromVersionId,
    to_version_id: toVersionId,
    actor_id: "owner-user",
    reason,
    validation_snapshot: { authorization_rechecked: true, resources_healthy: true },
    operation,
    release_evaluation_id: operation === "promote" ? "51000000-5000-4000-8000-000000000001" : null,
    request_hash: "f".repeat(64),
    created_at: NOW,
  };
}

function releaseDiff(evaluationId: string) {
  const section = (changed: boolean, paths: string[] = []) => ({
    changed,
    before_hash: "1".repeat(64),
    after_hash: "2".repeat(64),
    changed_paths: paths,
  });
  return {
    evaluation_id: evaluationId,
    draft_revision: 8,
    publication_id: PUBLICATION_ID,
    current_version_id: VERSION_TWO_ID,
    current_version_number: 2,
    diff: {
      schema_version: "agent-release-diff/v1",
      changed_sections: ["identity", "prompt", "model", "capabilities", "skills", "knowledge"],
      sections: {
        identity: section(true, ["identity.welcome_message"]),
        prompt: { ...section(true, ["instructions"]), before_length: 38, after_length: 58 },
        model: section(true, ["model"]),
        capabilities: section(true, ["capabilities"]),
        skills: section(true, ["capabilities.skills"]),
        knowledge: section(true, ["knowledge"]),
        memory: section(false),
      },
    },
  };
}

async function handleAgentApi(
  route: Route,
  state: HarnessState,
  options: HarnessOptions,
) {
  const request = route.request();
  const url = new URL(request.url());
  const pathname = url.pathname;
  const method = request.method();

  if (pathname === `/api/v1/agents/${AGENT_ID}` && method === "GET") {
    return route.fulfill(response({
      tenant_id: "tenant-a",
      agent_id: AGENT_ID,
      slug: "support-triage",
      name: "Support Triage",
      description: "Classifies and routes support requests.",
      owner_id: "owner-user",
      status: "active",
      caller_role: state.role,
      draft_revision: state.draftRevision,
      draft: { revision: state.draftRevision, schema_version: "agent-spec/v1", spec_hash: "a".repeat(64), updated_at: NOW },
      created_at: NOW,
      updated_at: NOW,
    }));
  }

  if (pathname === `/api/v1/agents/${AGENT_ID}/draft` && method === "GET") {
    return route.fulfill(response({
      tenant_id: "tenant-a",
      draft_id: DRAFT_ID,
      agent_id: AGENT_ID,
      revision: state.draftRevision,
      schema_version: "agent-spec/v1",
      spec: state.currentSpec,
      spec_hash: "a".repeat(64),
      updated_by: "owner-user",
      created_at: NOW,
      updated_at: NOW,
    }));
  }

  if (pathname === `/api/v1/agents/${AGENT_ID}/draft` && method === "PUT") {
    const body = request.postDataJSON() as { spec: Record<string, unknown> };
    state.currentSpec = body.spec as AgentSpec;
    state.draftRevision += 1;
    state.evaluations = state.evaluations.map((item) => ({
      ...item,
      status: "stale",
      stale: true,
    }));
    return route.fulfill(response({
      request_id: "draft-save-request",
      draft: {
        tenant_id: "tenant-a",
        draft_id: DRAFT_ID,
        agent_id: AGENT_ID,
        revision: state.draftRevision,
        schema_version: "agent-spec/v1",
        spec: state.currentSpec,
        spec_hash: "9".repeat(64),
        updated_by: "owner-user",
        created_at: NOW,
        updated_at: "2026-07-19T09:00:00.000Z",
      },
    }));
  }

  if (pathname === `/api/v1/agents/${AGENT_ID}/versions` && method === "GET") {
    return route.fulfill(response(state.versions));
  }

  if (pathname === `/api/v1/agents/${AGENT_ID}/evals` && method === "GET") {
    return route.fulfill(response({ evaluations: state.evaluations }));
  }

  if (pathname === `/api/v1/agents/${AGENT_ID}/evals` && method === "POST") {
    if (state.role !== "owner") {
      return route.fulfill(response({ detail: { code: "AGENT_ROLE_FORBIDDEN", message: "Owner role required." } }, 403));
    }
    const body = request.postDataJSON() as { dataset_id?: string | null };
    state.selectedEvalDatasetIds.push(body.dataset_id ?? null);
    const next = evaluation("queued", state.evaluations.length + 1);
    next.draft_revision = state.draftRevision;
    next.dataset_id = body.dataset_id ?? null;
    next.dataset_version = body.dataset_id ? "release-v3" : null;
    next.dataset_manifest_hash = body.dataset_id ? "7".repeat(64) : null;
    state.evaluations = [next, ...state.evaluations];
    return route.fulfill(response(next, 201));
  }

  const executeMatch = pathname.match(
    new RegExp(`^/api/v1/agents/${AGENT_ID}/evals/([^/]+)/execute$`),
  );
  if (executeMatch && method === "POST") {
    const selected = state.evaluations.find(
      (item) => item.evaluation_id === executeMatch[1],
    );
    if (!selected) {
      return route.fulfill(response({ detail: { code: "AGENT_EVAL_NOT_FOUND" } }, 404));
    }
    if (selected.status === "queued") {
      const terminal = evaluation("passed", state.evaluations.length + 20);
      selected.status = "passed";
      selected.started_at = NOW;
      selected.completed_at = NOW;
      selected.gate_snapshot = terminal.gate_snapshot;
      selected.events = [
        selected.events[0],
        { ...terminal.events[1], evaluation_id: selected.evaluation_id },
        { ...terminal.events[2], evaluation_id: selected.evaluation_id },
      ];
    }
    return route.fulfill(response(selected));
  }

  const cancelMatch = pathname.match(
    new RegExp(`^/api/v1/agents/${AGENT_ID}/evals/([^/]+)/cancel$`),
  );
  if (cancelMatch && method === "POST") {
    const selected = state.evaluations.find(
      (item) => item.evaluation_id === cancelMatch[1],
    );
    if (!selected) {
      return route.fulfill(response({ detail: { code: "AGENT_EVAL_NOT_FOUND" } }, 404));
    }
    selected.status = "cancelled";
    selected.completed_at = NOW;
    selected.gate_snapshot.status = "cancelled";
    selected.events = [
      ...selected.events.filter((item) => item.status !== "passed"),
      {
        event_id: eventId(999, "cancelled"),
        evaluation_id: selected.evaluation_id,
        sequence: selected.events.length + 1,
        status: "cancelled",
        summary: {},
        created_at: NOW,
      },
    ];
    return route.fulfill(response(selected));
  }

  const diffMatch = pathname.match(new RegExp(`^/api/v1/agents/${AGENT_ID}/evals/([^/]+)/diff$`));
  if (diffMatch && method === "GET") {
    return route.fulfill(response(releaseDiff(diffMatch[1])));
  }

  if (pathname === `/api/v1/agents/${AGENT_ID}/publications` && method === "GET") {
    return route.fulfill(response(state.publications));
  }

  if (pathname === `/api/v1/agents/${AGENT_ID}/publish-events` && method === "GET") {
    return route.fulfill(response(state.events));
  }

  if (pathname === `/api/v1/agents/${AGENT_ID}/publish` && method === "POST") {
    state.publishCalls += 1;
    const key = request.headers()["idempotency-key"] || "";
    state.publishKeys.push(key);
    if (options.failFirstPublish && !state.failedPublish) {
      state.failedPublish = true;
      return route.fulfill(response({ detail: { code: "RELEASE_TEMPORARILY_UNAVAILABLE", message: "Promotion transaction was not committed." } }, 503));
    }
    if (state.role !== "owner") {
      return route.fulfill(response({ detail: { code: "AGENT_ROLE_FORBIDDEN", message: "Owner role required." } }, 403));
    }
    const body = request.postDataJSON() as { evaluation_id: string; reason: string; experiment_run_id?: string };
    state.publishRunIds.push(body.experiment_run_id ?? null);
    const selected = state.evaluations.find((item) => item.evaluation_id === body.evaluation_id);
    if (!selected || selected.status !== "passed" || selected.stale || selected.gate_snapshot.blocking_findings.length > 0) {
      return route.fulfill(response({ detail: { code: "AGENT_EVAL_STALE", message: "Evaluation is not publishable." } }, 409));
    }
    let createdVersion = state.versions.find((item) => item.agent_version_id === VERSION_THREE_ID);
    const replay = state.successfulPublishKeys.has(key);
    if (!createdVersion) {
      createdVersion = version(VERSION_THREE_ID, 3, selected.draft_revision, selected.evaluation_id);
      state.versions = [...state.versions, createdVersion];
    }
    if (!replay) {
      const prior = state.publications[0]?.version_id ?? null;
      state.publications = [publication(VERSION_THREE_ID, 3)];
      state.events = [...state.events, publishEvent(eventId(state.events.length + 100, "promote"), "promote", prior, VERSION_THREE_ID, body.reason)];
      state.successfulPublishKeys.add(key);
    }
    return route.fulfill(response({
      request_id: "publish-request",
      version: createdVersion,
      publication: state.publications[0],
      event: state.events.at(-1),
      idempotent_replay: replay,
    }));
  }

  if (pathname === `/api/v1/publications/${PUBLICATION_ID}/rollback` && method === "POST") {
    state.rollbackCalls += 1;
    const key = request.headers()["idempotency-key"] || "";
    state.rollbackKeys.push(key);
    const body = request.postDataJSON() as { target_version_id: string; reason: string };
    if (state.role !== "owner") {
      return route.fulfill(response({ detail: { code: "AGENT_ROLE_FORBIDDEN", message: "Owner role required." } }, 403));
    }
    const target = state.versions.find((item) => item.agent_version_id === body.target_version_id);
    if (!target) return route.fulfill(response({ detail: { code: "AGENT_VERSION_NOT_FOUND", message: "Version not found." } }, 404));
    const replay = state.successfulRollbackKeys.has(key);
    const prior = state.publications[0].version_id;
    const audit = replay
      ? state.events.find((item) => item.operation === "rollback" && item.to_version_id === target.agent_version_id)!
      : publishEvent(eventId(state.events.length + 200, "rollback"), "rollback", prior, target.agent_version_id, body.reason);
    if (!replay) {
      state.publications = [publication(target.agent_version_id, target.version_number)];
      state.events = [...state.events, audit];
      state.successfulRollbackKeys.add(key);
    }
    return route.fulfill(response({
      request_id: "rollback-request",
      version: target,
      publication: state.publications[0],
      event: audit,
      idempotent_replay: replay,
    }));
  }

  throw new Error(`Unhandled Agent release request: ${method} ${pathname}`);
}

async function installHarness(page: Page, options: HarnessOptions = {}): Promise<HarnessState> {
  await installClientAuth(page, {
    user_id: "owner-user",
    email: "owner@example.com",
    display_name: "Release Owner",
    permissions: ["console:dashboard:view", "conversation:playground:access", "console:eval:view"],
    effective_permissions: ["console:dashboard:view", "conversation:playground:access", "console:eval:view"],
  });
  const theme = options.theme ?? "light";
  await seedClientPrefs(page, {
    locale: "en-US",
    themeMode: theme,
    resolvedTheme: theme,
    darkMode: theme === "dark",
  });
  const statuses = options.statuses ?? ["queued", "running", "passed", "failed", "cancelled", "stale"];
  const evaluations = statuses.map((status, index) => evaluation(
    status,
    index + 1,
    options.blockingPassed && status === "passed",
  ));
  const state: HarnessState = {
    role: options.role ?? "owner",
    draftRevision: 8,
    currentSpec: structuredClone(spec),
    evaluations,
    versions: [version(VERSION_ONE_ID, 1, 5, null), version(VERSION_TWO_ID, 2, 7, evaluations.find((item) => item.status === "passed")?.evaluation_id ?? null)],
    publications: [publication()],
    events: [publishEvent("60000000-6000-4000-8000-600000000000", "promote", VERSION_ONE_ID, VERSION_TWO_ID, "Promote approved support policy")],
    publishCalls: 0,
    rollbackCalls: 0,
    publishKeys: [],
    publishRunIds: [],
    successfulPublishKeys: new Set(),
    rollbackKeys: [],
    selectedEvalDatasetIds: [],
    successfulRollbackKeys: new Set(),
    failedPublish: false,
  };

  await page.route("**/api/v1/**", async (route) => {
    const request = route.request();
    const pathname = new URL(request.url()).pathname;
    if (pathname.startsWith("/api/v1/agents/") || pathname.startsWith("/api/v1/publications/")) {
      return handleAgentApi(route, state, options);
    }
    if (pathname === "/api/v1/auth/me") {
      return route.fulfill(response({
        user_id: "owner-user",
        email: "owner@example.com",
        display_name: "Release Owner",
        roles: ["user"],
        permissions: ["console:dashboard:view", "conversation:playground:access", "console:eval:view"],
        effective_permissions: ["console:dashboard:view", "conversation:playground:access", "console:eval:view"],
        tier: "normal",
        force_password_change: false,
      }));
    }
    if (pathname === "/api/v1/assistant/models") return route.fulfill(response({ models: [{ id: "qwen3.7-plus", name: "qwen3.7-plus", provider: "dashscope", context_window: 131072, max_output_tokens: 8192, supports_vision: true, supports_tools: true }] }));
    if (pathname === "/api/v1/assistant/tools") return route.fulfill(response({ tools: [] }));
    if (pathname === "/api/v1/mcp/servers") return route.fulfill(response({ servers: [], total: 0 }));
    if (pathname === "/api/v1/skills") return route.fulfill(response({ skills: [], total: 0 }));
    if (pathname === "/api/v1/connectors/available") return route.fulfill(response([]));
    if (pathname === "/api/v1/connectors/mine") return route.fulfill(response([]));
    if (pathname === "/api/v1/knowledge/datasets") {
      return route.fulfill(response([]));
    }
    if (pathname === "/api/v1/eval/datasets") {
      return route.fulfill(response({
        datasets: options.withDataset ? [{
          dataset_id: DATASET_ID,
          tenant_id: "tenant-a",
          name: "Release quality set",
          description: "Approved local release cases.",
          version: "release-v3",
          schema: {},
          metadata: {},
          created_by: "eval-owner",
          updated_at: NOW,
        }] : [],
        total: options.withDataset ? 1 : 0,
        limit: 200,
        offset: 0,
      }));
    }
    if (pathname === "/api/v1/assistant/config") return route.fulfill(response({ default_model_id: "qwen3.7-plus", available_providers: ["dashscope"], kb_enabled: false, web_search_enabled: false }));
    if (pathname === "/api/v1/sessions" && request.method() === "GET") return route.fulfill(response([]));
    if (pathname === "/api/v1/setup/state") return route.fulfill(response({ configured: true, missing: [], mode: "environment", default_model: null }));
    throw new Error(`Unhandled API request: ${request.method()} ${pathname}`);
  });
  return state;
}

function watchTraffic(page: Page, allowedFailures: RegExp[] = []) {
  const errors: string[] = [];
  const badResponses: string[] = [];
  page.on("pageerror", (error) => errors.push(String(error)));
  page.on("console", (message) => {
    const expectedFailedRequest = allowedFailures.length > 0
      && /Failed to load resource: the server responded with a status of 503/.test(message.text());
    if (message.type() === "error" && !/favicon/i.test(message.text()) && !expectedFailedRequest) errors.push(message.text());
  });
  page.on("response", (item) => {
    if (item.status() < 400 || !item.url().includes("/api/v1/")) return;
    const label = `${item.status()} ${new URL(item.url()).pathname}`;
    if (!allowedFailures.some((pattern) => pattern.test(label))) badResponses.push(label);
  });
  return () => {
    expect(errors).toEqual([]);
    expect(badResponses).toEqual([]);
  };
}

async function expectNoHorizontalOverflow(page: Page) {
  expect(await page.evaluate(() => document.documentElement.scrollWidth <= document.documentElement.clientWidth + 1)).toBe(true);
}

async function capture(page: Page, name: string) {
  const dimensions = name.match(/-(\d+)x(\d+)$/);
  if (dimensions) {
    expect(page.viewportSize(), `${name} must match its labelled viewport`).toEqual({
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

async function captureFullPage(page: Page, name: string) {
  const directory = path.resolve(process.cwd(), "../reports/agent-studio/as-06-screenshots");
  await fs.mkdir(directory, { recursive: true });
  await page.screenshot({
    path: path.join(directory, `${name}.png`),
    fullPage: true,
    animations: "disabled",
  });
}

async function tabTo(page: Page, target: Locator, limit = 80) {
  await expect(target).toBeVisible();
  for (let index = 0; index < limit; index += 1) {
    if (await target.evaluate((element) => element === document.activeElement)) return;
    await page.keyboard.press("Tab");
  }
  throw new Error(`Keyboard focus did not reach ${await target.textContent()}`);
}

test.describe("Agent release evaluation", () => {
  test("renders no-dataset and every lifecycle state on exact desktop and mobile viewports", async ({ page }) => {
    const assertTraffic = watchTraffic(page);
    await page.setViewportSize({ width: 1440, height: 900 });
    await installHarness(page);
    await page.goto(`/agents/${AGENT_ID}/evals`, { waitUntil: "domcontentloaded" });

    await expect(page.getByTestId("agent-release-eval-panel")).toBeVisible();
    await expect(page.getByText("No production evaluation dataset is configured")).toBeVisible();
    for (const status of ["queued", "running", "passed", "failed", "cancelled", "stale"] as const) {
      await expect(page.getByTestId(`agent-eval-${status}`)).toBeVisible();
    }
    await expect(page.getByText("runtime_fingerprint_changed")).toBeVisible();
    await expect(page.getByText("model quality not evaluated", { exact: false }).first()).toBeVisible();
    const runButton = page.getByRole("button", { name: "Run evaluation" });
    await tabTo(page, runButton);
    await expect(runButton).toBeFocused();
    await assertNoBlockingA11yIssues(page, [".agent-studio"]);
    await expectNoHorizontalOverflow(page);
    await capture(page, "eval-lifecycle-no-dataset-1440x900");

    await page.setViewportSize({ width: 390, height: 844 });
    await expect(page.getByTestId("agent-release-eval-panel")).toBeVisible();
    await expectNoHorizontalOverflow(page);
    await assertNoBlockingA11yIssues(page, [".agent-studio"]);
    await capture(page, "eval-lifecycle-no-dataset-390x844");
    await captureFullPage(page, "eval-lifecycle-no-dataset-390-wide-full-page");
    assertTraffic();
  });

  test("uses the authorized Eval Dataset catalog and binds the selected version", async ({ page }) => {
    const assertTraffic = watchTraffic(page);
    await page.setViewportSize({ width: 1440, height: 900 });
    const state = await installHarness(page, { statuses: [], withDataset: true });
    await page.goto(`/agents/${AGENT_ID}/evals`, { waitUntil: "domcontentloaded" });

    await page.getByLabel("Dataset").click();
    await page.getByText("Release quality set · release-v3", { exact: true }).click();
    await page.getByRole("button", { name: "Run evaluation" }).click();

    await expect(page.getByTestId("agent-eval-passed")).toBeVisible();
    expect(state.selectedEvalDatasetIds).toEqual([DATASET_ID]);
    expect(state.evaluations[0].dataset_version).toBe("release-v3");
    expect(state.evaluations[0].dataset_manifest_hash).toBe("7".repeat(64));
    assertTraffic();
  });

  test("cancels a durable queued evaluation through the server endpoint", async ({ page }) => {
    const assertTraffic = watchTraffic(page);
    await page.setViewportSize({ width: 1440, height: 900 });
    const state = await installHarness(page, { statuses: ["queued"] });
    await page.goto(`/agents/${AGENT_ID}/evals`, { waitUntil: "domcontentloaded" });

    await page.getByRole("button", { name: "Cancel evaluation" }).click();

    await expect(page.getByTestId("agent-eval-cancelled")).toBeVisible();
    expect(state.evaluations[0].status).toBe("cancelled");
    expect(state.evaluations[0].events.at(-1)?.status).toBe("cancelled");
    assertTraffic();
  });

  test("server blocking findings keep the publish submit disabled", async ({ page }) => {
    await page.setViewportSize({ width: 1440, height: 900 });
    const state = await installHarness(page, { statuses: ["passed"], blockingPassed: true });
    await page.goto(`/agents/${AGENT_ID}/evals`, { waitUntil: "domcontentloaded" });
    await page.getByRole("button", { name: "Review and publish" }).click();
    const sheet = page.getByTestId("agent-publish-sheet");
    await expect(sheet).toBeVisible();
    await expect(sheet.getByText("RESOURCE_REVOKED")).toBeVisible();
    await expect(page.getByRole("button", { name: "Publish Version" })).toBeDisabled();
    await capture(page, "publish-server-blocked-1440x900");
    expect(state.publishCalls).toBe(0);
  });

  test("keeps the dark release surface accessible", async ({ page }) => {
    const assertTraffic = watchTraffic(page);
    await page.setViewportSize({ width: 1440, height: 900 });
    await installHarness(page, { statuses: ["passed"], withDataset: true, theme: "dark" });
    await page.goto(`/agents/${AGENT_ID}/evals`, { waitUntil: "domcontentloaded" });
    await page.getByRole("button", { name: "Review and publish" }).click();

    await expect(page.locator("html")).toHaveClass(/dark/);
    await assertNoBlockingA11yIssues(page, [".agent-publish-drawer"]);
    await expectNoHorizontalOverflow(page);
    await capture(page, "publish-diff-dark-1440x900");
    assertTraffic();
  });

  test("saving a changed Draft makes its prior evaluation stale and unpublishable", async ({ page }) => {
    const assertTraffic = watchTraffic(page);
    await page.setViewportSize({ width: 1440, height: 900 });
    const state = await installHarness(page, { statuses: ["passed"] });
    await page.goto(`/agents/${AGENT_ID}/evals`, { waitUntil: "domcontentloaded" });
    await expect(page.getByRole("button", { name: "Review and publish" })).toBeEnabled();

    await page.getByRole("button", { name: "Overview", exact: true }).click();
    await page.getByLabel("Description").fill("Changed after the release evaluation was recorded.");
    await page.locator(".agent-studio-actions").getByRole("button", { name: "Save draft" }).click();
    await expect(page.locator(".agent-save-state")).toHaveText("Saved");
    await page.getByRole("button", { name: "Eval & Publish", exact: true }).click();

    await expect(page.getByTestId("agent-eval-stale")).toBeVisible();
    await expect(page.getByRole("button", { name: "Review and publish" })).toHaveCount(0);
    await expect(page.getByRole("button", { name: "Run again" })).toBeVisible();
    await capture(page, "eval-stale-after-draft-save-1440x900");
    expect(state.draftRevision).toBe(9);
    expect(state.publishCalls).toBe(0);
    assertTraffic();
  });
});

test.describe("Agent publish and rollback", () => {
  test("links only an explicitly verified successful Agent Version run for this release dataset", async ({ page }) => {
    page.setDefaultTimeout(15_000);
    const state = await installHarness(page, { statuses: ["passed"], withDataset: true });
    const selectedEvaluation = state.evaluations[0];
    selectedEvaluation.dataset_id = DATASET_ID;
    selectedEvaluation.dataset_manifest_hash = "7".repeat(64);
    state.versions.push({ ...version(VERSION_THREE_ID, 3, 8, null), spec_hash: selectedEvaluation.spec_hash });
    const matchingRunId = "81818181-8181-4181-8181-818181818181";
    const wrongDatasetRunId = "82828282-8282-4282-8282-828282828282";
    const failedGateRunId = "84848484-8484-4484-8484-848484848484";
    await page.route("**/api/v1/eval/experiment-runs/*", async (route) => {
      const runId = new URL(route.request().url()).pathname.split("/").at(-1);
      return route.fulfill(response({
        run_id: runId,
        experiment_id: "83838383-8383-4383-8383-838383838383",
        tenant_id: "tenant-a",
        dataset_id: runId === wrongDatasetRunId ? "other-dataset" : DATASET_ID,
        dataset_manifest_hash: "7".repeat(64),
        status: "succeeded",
        run_mode: "live_candidate",
        target_snapshot: {
          candidate_type: "agent_version",
          agent_id: AGENT_ID,
          agent_version_id: VERSION_THREE_ID,
          agent_spec_hash: selectedEvaluation.spec_hash,
          model_id: spec.model.model_id,
          knowledge_dataset_ids: [],
        },
        score_summary: {},
        metrics: {
          gate: runId === failedGateRunId
            ? { profile: "agent_version_task_suite", status: "fail", reason: "known_hard_failure" }
            : { profile: "agent_version_task_suite", status: "warning", reason: "fixed_sample_only" },
        },
        created_by: "owner-user", created_at: NOW,
      }));
    });
    await page.goto(`/agents/${AGENT_ID}/evals`, { waitUntil: "domcontentloaded" });
    await page.getByRole("button", { name: "Review and publish" }).click();
    const sheet = page.getByTestId("agent-publish-sheet");
    const runInput = sheet.getByRole("searchbox", { name: "Experiment run ID" });
    const publishButton = page.getByRole("button", { name: "Publish Version" });

    await runInput.fill(wrongDatasetRunId);
    await sheet.getByRole("button", { name: "Verify run" }).click();
    await expect(sheet.getByText("The run's test set does not match this release evaluation.")).toBeVisible();
    await expect(publishButton).toBeDisabled();
    expect(state.publishCalls).toBe(0);

    await runInput.fill(failedGateRunId);
    await sheet.getByRole("button", { name: "Verify run" }).click();
    await expect(sheet.getByText("The run's quality gate does not permit linked publication.")).toBeVisible();
    await expect(publishButton).toBeDisabled();
    expect(state.publishCalls).toBe(0);

    await runInput.fill(matchingRunId);
    await sheet.getByRole("button", { name: "Verify run" }).click();
    await expect(sheet.getByTestId("agent-publish-selected-run")).toContainText(VERSION_THREE_ID);
    await expect(sheet.getByTestId("agent-publish-selected-run")).toContainText(DATASET_ID);
    await expect(publishButton).toBeEnabled();
    await publishButton.click();
    await expect(sheet.getByText("Version published")).toBeVisible();
    expect(state.publishRunIds).toEqual([matchingRunId]);
  });

  test("reuses the idempotency key, promotes once, preserves session pinning, and rolls back", async ({ page }) => {
    const assertTraffic = watchTraffic(page, [/^503 \/api\/v1\/agents\/.*\/publish$/]);
    await page.setViewportSize({ width: 1440, height: 900 });
    const state = await installHarness(page, { statuses: ["passed"], withDataset: true, failFirstPublish: true });
    await page.goto(`/agents/${AGENT_ID}/evals`, { waitUntil: "domcontentloaded" });
    await page.getByRole("button", { name: "Review and publish" }).click();

    const sheet = page.getByTestId("agent-publish-sheet");
    await expect(sheet).toBeVisible();
    await expect(sheet.getByText("Structured change summary")).toBeVisible();
    for (const section of ["Identity", "Instructions", "Model", "Capabilities and tools", "Skills", "Knowledge", "Memory and safety"]) {
      await expect(sheet.getByText(section, { exact: true })).toBeVisible();
    }
    await expect(sheet).not.toContainText(spec.instructions);
    await expect(sheet.getByText("Session pinning is preserved")).toBeVisible();
    await page.getByLabel("Release reason").fill("Promote the exact approved release candidate.");
    const publishButton = page.getByRole("button", { name: "Publish Version" });
    await expect(publishButton).toBeEnabled();
    await publishButton.click();
    await expect(page.getByText("RELEASE_TEMPORARILY_UNAVAILABLE", { exact: false })).toBeVisible();
    await publishButton.click();
    await expect(sheet.getByText("Version published")).toBeVisible();

    expect(state.publishKeys).toHaveLength(2);
    expect(state.publishKeys[0]).toBeTruthy();
    expect(state.publishKeys[1]).toBe(state.publishKeys[0]);
    expect(state.versions.filter((item) => item.agent_version_id === VERSION_THREE_ID)).toHaveLength(1);
    expect(state.events.filter((item) => item.to_version_id === VERSION_THREE_ID)).toHaveLength(1);
    expect(state.publications[0].version_id).toBe(VERSION_THREE_ID);

    await assertNoBlockingA11yIssues(page, [".agent-publish-drawer"]);
    await capture(page, "publish-diff-success-1440x900");
    await page.setViewportSize({ width: 390, height: 844 });
    await expectNoHorizontalOverflow(page);
    await capture(page, "publish-diff-success-390x844");
    await captureFullPage(page, "publish-diff-success-390-wide-full-page");

    await page.getByRole("button", { name: "Close" }).click();
    await page.setViewportSize({ width: 1440, height: 900 });
    await page.goto(`/agents/${AGENT_ID}/versions`, { waitUntil: "domcontentloaded" });
    await expect(page.getByText("Current target · v3")).toBeVisible();
    await expect(page.getByText("Existing sessions stay on their pinned Version", { exact: false })).toBeVisible();
    await expect(page.getByText("Published Version", { exact: true }).last()).toBeVisible();
    await capture(page, "versions-current-and-audit-1440x900");

    const versionOne = page.locator(".agent-version-history > article").filter({ hasText: "v1" });
    await versionOne.getByRole("button", { name: "Roll back HOSTED" }).click();
    const confirm = page.getByRole("button", { name: "Roll back channel" });
    await expect(confirm).toBeDisabled();
    await page.getByLabel("Rollback reason").fill("Restore the last healthy resource authorization set.");
    await expect(confirm).toBeEnabled();
    await confirm.click();
    await expect(page.getByText("Current target · v1")).toBeVisible();
    await expect(page.getByText("Rolled back channel", { exact: true })).toBeVisible();
    expect(state.rollbackCalls).toBe(1);
    expect(state.rollbackKeys[0]).toBeTruthy();
    expect(state.publications[0].version_id).toBe(VERSION_ONE_ID);
    expect(state.versions).toHaveLength(3);
    expect(state.events.filter((item) => item.operation === "rollback")).toHaveLength(1);
    await assertNoBlockingA11yIssues(page, [".agent-studio"]);
    await capture(page, "rollback-audited-success-1440x900");

    await page.getByRole("button", { name: "Eval & Publish", exact: true }).click();
    await page.getByRole("button", { name: "Review and publish" }).click();
    await page.getByLabel("Release reason").fill("Promote the same evaluated Version after rollback.");
    await page.getByRole("button", { name: "Publish Version" }).click();
    await expect(page.getByTestId("agent-publish-sheet").getByText("Version published")).toBeVisible();
    expect(state.publishKeys).toHaveLength(3);
    expect(state.publishKeys[2]).not.toBe(state.publishKeys[0]);
    expect(state.publications[0].version_id).toBe(VERSION_THREE_ID);
    expect(state.versions.filter((item) => item.agent_version_id === VERSION_THREE_ID)).toHaveLength(1);

    await page.getByRole("button", { name: "Close" }).click();
    await page.getByRole("button", { name: "Channels & Analytics", exact: true }).click();
    await expect(page.getByText("Current target · v3")).toBeVisible();
    await versionOne.getByRole("button", { name: "Roll back HOSTED" }).click();
    await page.getByLabel("Rollback reason").fill("Repeat the rollback as a new intentional operation.");
    await page.getByRole("button", { name: "Roll back channel" }).click();
    await expect(page.getByText("Current target · v1")).toBeVisible();
    expect(state.rollbackKeys).toHaveLength(2);
    expect(state.rollbackKeys[1]).not.toBe(state.rollbackKeys[0]);
    expect(state.events.filter((item) => item.operation === "rollback")).toHaveLength(2);
    assertTraffic();
  });

  test("blocks rollback to a Version that was never a channel target", async ({ page }) => {
    const assertTraffic = watchTraffic(page);
    await page.setViewportSize({ width: 1440, height: 900 });
    const state = await installHarness(page, { statuses: ["passed"] });
    state.versions.push(version(VERSION_THREE_ID, 3, 8, null));
    await page.goto(`/agents/${AGENT_ID}/versions`, { waitUntil: "domcontentloaded" });

    const unreleased = page.locator(".agent-version-history > article").filter({ hasText: "v3" });
    await expect(unreleased.getByRole("button", { name: "Roll back HOSTED" })).toBeDisabled();
    await unreleased.getByRole("button", { name: "Release evidence" }).click();
    await expect(unreleased.getByText("No promotion event targets this Version.")).toBeVisible();
    expect(state.rollbackCalls).toBe(0);
    assertTraffic();
  });
});

for (const role of ["editor", "viewer"] as const) {
  test(`${role} can inspect release evidence but cannot evaluate, publish, or roll back`, async ({ page }) => {
    const assertTraffic = watchTraffic(page);
    await page.setViewportSize({ width: 1440, height: 900 });
    const state = await installHarness(page, { role, statuses: ["passed"] });
    await page.goto(`/agents/${AGENT_ID}/evals`, { waitUntil: "domcontentloaded" });
    await expect(page.getByText("Owner permission required")).toBeVisible();
    await expect(page.getByRole("button", { name: "Run evaluation" })).toBeDisabled();
    await expect(page.getByRole("button", { name: "Review and publish" })).toBeDisabled();

    await page.goto(`/agents/${AGENT_ID}/versions`, { waitUntil: "domcontentloaded" });
    await expect(page.getByText("Release history is read-only for this role", { exact: false })).toBeVisible();
    await expect(page.getByRole("button", { name: "Roll back HOSTED" }).first()).toBeDisabled();
    expect(state.publishCalls).toBe(0);
    expect(state.rollbackCalls).toBe(0);
    assertTraffic();
  });
}
