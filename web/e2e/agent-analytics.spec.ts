import fs from "node:fs/promises";
import path from "node:path";

import { expect, test, type Page, type Route } from "@playwright/test";

import {
  assertNoBlockingA11yIssues,
  installClientAuth,
  seedClientPrefs,
} from "./support/helpers";

const AGENT_ID = "11111111-1111-4111-8111-111111111111";
const VERSION_ID = "22222222-2222-4222-8222-222222222222";
const PUBLICATION_ID = "33333333-3333-4333-8333-333333333333";
const TRACE_ID = "44444444-4444-4444-8444-444444444444";
const NOW = "2026-07-18T08:00:00.000Z";
const SCREENSHOTS = path.resolve(process.cwd(), "../reports/agent-studio/as-08-screenshots");

type CallerRole = "owner" | "editor" | "viewer";

interface HarnessOptions {
  role?: CallerRole;
  analyticsState?: "populated" | "empty" | "error";
  retentionLimited?: boolean;
}

interface HarnessState {
  analyticsUrls: string[];
  cacheInvalidations: number;
  policyUpdates: number;
  credentialRevocations: number;
  deletions: number;
}

function json(body: unknown, status = 200) {
  return { status, contentType: "application/json", body: JSON.stringify(body) };
}

async function capture(page: Page, name: string) {
  await fs.mkdir(SCREENSHOTS, { recursive: true });
  await page.screenshot({ path: path.join(SCREENSHOTS, name), fullPage: true });
}

function agent(role: CallerRole) {
  return {
    tenant_id: "tenant-a",
    agent_id: AGENT_ID,
    slug: "support-operations",
    name: "Support Operations",
    description: "Routes support work under explicit policies.",
    owner_id: "owner-a",
    status: "active",
    caller_role: role,
    draft_revision: 7,
    created_at: NOW,
    updated_at: NOW,
  };
}

function policy() {
  return {
    tenant_id: "tenant-a",
    agent_id: AGENT_ID,
    trace_retention_days: 30,
    runtime_retention_days: 30,
    attachment_retention_days: 7,
    legal_hold: false,
    principal_requests_per_minute: 60,
    principal_requests_per_day: 5000,
    ip_requests_per_minute: 120,
    ip_requests_per_day: 10000,
    publication_requests_per_minute: 500,
    publication_requests_per_day: 50000,
    max_agents_per_tenant: 100,
    max_active_publications: 10,
    max_concurrent_runs: 25,
    max_daily_tokens: 10000000,
    max_daily_mcp_calls: 100000,
    max_storage_bytes: 10737418240,
    alert_threshold_percent: 80,
    cache_epoch: 3,
    updated_by: "owner-a",
    created_at: NOW,
    updated_at: NOW,
  };
}

async function fulfillAgentApi(
  route: Route,
  state: HarnessState,
  options: HarnessOptions,
) {
  const request = route.request();
  const url = new URL(request.url());
  const path = url.pathname;
  const method = request.method();
  const role = options.role || "owner";

  if (path === `/api/v1/agents/${AGENT_ID}` && method === "GET") {
    return route.fulfill(json(agent(role)));
  }
  if (path === `/api/v1/agents/${AGENT_ID}/versions` && method === "GET") {
    return route.fulfill(json([{
      tenant_id: "tenant-a",
      agent_version_id: VERSION_ID,
      agent_id: AGENT_ID,
      version_number: 4,
      schema_version: "agent-spec/v1",
      spec: {},
      spec_hash: "version-hash",
      source_draft_id: "55555555-5555-4555-8555-555555555555",
      source_draft_revision: 7,
      created_by: "owner-a",
      created_at: NOW,
    }]));
  }
  if (path === `/api/v1/agents/${AGENT_ID}/publications` && method === "GET") {
    return route.fulfill(json([{
      tenant_id: "tenant-a",
      publication_id: PUBLICATION_ID,
      agent_id: AGENT_ID,
      channel: "api",
      public_id: "support-api",
      version_id: VERSION_ID,
      auth_mode: "token",
      policy: {},
      status: "active",
      created_by: "owner-a",
      updated_by: "owner-a",
      created_at: NOW,
      updated_at: NOW,
    }]));
  }
  if (path === `/api/v1/agents/${AGENT_ID}/analytics` && method === "GET") {
    state.analyticsUrls.push(url.toString());
    if (options.analyticsState === "error") {
      return route.fulfill(json({ detail: { code: "AGENT_STORAGE_UNAVAILABLE", message: "Metrics store is unavailable." } }, 503));
    }
    const traces = options.analyticsState === "empty" ? [] : [{
      trace_id: TRACE_ID,
      agent_id: AGENT_ID,
      agent_version_id: VERSION_ID,
      publication_id: PUBLICATION_ID,
      channel: "api",
      session_id: "session-safe",
      status: "succeeded",
      model_id: "qwen3.7-plus",
      total_latency_ms: 842,
      total_tokens: 93,
      total_cost_cents: 0,
      input_preview: "Authorization: [REDACTED] classify this request",
      output_preview: "Safe routed response",
      redaction_state: { sensitive_fields: "removed" },
      started_at: NOW,
      created_at: NOW,
    }];
    return route.fulfill(json({
      agent_id: AGENT_ID,
      caller_role: role,
      metrics: {
        total_runs: traces.length,
        succeeded_runs: traces.length,
        failed_runs: 0,
        sessions: traces.length,
        success_rate: traces.length ? 1 : null,
        avg_latency_ms: traces.length ? 842 : 0,
        p50_latency_ms: traces.length ? 842 : 0,
        p95_latency_ms: traces.length ? 842 : 0,
        avg_ttft_ms: traces.length ? 210 : 0,
        p50_ttft_ms: traces.length ? 210 : 0,
        p95_ttft_ms: traces.length ? 210 : 0,
        total_tokens: traces.length ? 93 : 0,
        total_cost_cents: 0,
        tool_calls: traces.length ? 10 : 0,
        tool_succeeded: traces.length ? 9 : 0,
        tool_success_rate: traces.length ? 0.9 : null,
        knowledge_queries: traces.length ? 4 : 0,
        knowledge_hits: traces.length ? 3 : 0,
        knowledge_hit_rate: traces.length ? 0.75 : null,
        feedback_count: traces.length ? 5 : 0,
        positive_feedback_count: traces.length ? 4 : 0,
        feedback_positive_rate: traces.length ? 0.8 : null,
        breakdown: traces.length ? [{ channel: "api", run_count: traces.length }] : [],
        retention_limited: options.retentionLimited ?? true,
        retention: {
          trace_retention_days: 30,
          legal_hold: false,
          last_retention_cleanup_at: options.retentionLimited === false ? null : NOW,
        },
      },
      traces,
      total: traces.length,
      limit: 20,
      offset: Number(url.searchParams.get("offset") || 0),
      filters: {},
    }));
  }
  if (path === `/api/v1/agents/${AGENT_ID}/audit-events` && method === "GET") {
    return route.fulfill(json({
      events: [{
        id: 81,
        user_id: "owner-a",
        action: "publication_promote",
        status: "success",
        agent_id: AGENT_ID,
        agent_version_id: VERSION_ID,
        publication_id: PUBLICATION_ID,
        channel: "api",
        request_summary: { authorization: "[REDACTED]" },
        response_summary: { status: "promoted" },
        redaction_state: { sensitive_fields: "removed" },
        created_at: NOW,
      }],
      total: 1,
      limit: 20,
      offset: 0,
    }));
  }
  if (path === `/api/v1/agents/${AGENT_ID}/governance` && method === "GET") {
    return route.fulfill(json(policy()));
  }
  if (path === `/api/v1/agents/${AGENT_ID}/governance` && method === "PUT") {
    state.policyUpdates += 1;
    return route.fulfill(json({ ...policy(), ...(request.postDataJSON() as object) }));
  }
  if (path.endsWith("/governance/cache:invalidate") && method === "POST") {
    state.cacheInvalidations += 1;
    return route.fulfill(json({ request_id: "request-cache", cache_epoch: 4, deleted_cache_rows: 2 }));
  }
  if (path.endsWith("/governance/credentials:revoke") && method === "POST") {
    state.credentialRevocations += 1;
    return route.fulfill(json({ request_id: "request-revoke", revoked: { api_tokens: 1 } }));
  }
  if (path.endsWith("/governance/data-deletions") && method === "POST") {
    state.deletions += 1;
    return route.fulfill(json({
      deletion_id: "66666666-6666-4666-8666-666666666666",
      tenant_id: "tenant-a",
      agent_id: AGENT_ID,
      scope: "retention",
      subject_user_id: null,
      status: "completed",
      deleted_counts: { traces: 1 },
      error_code: null,
      requested_by: "owner-a",
      requested_at: NOW,
      completed_at: NOW,
    }));
  }
  return route.fulfill(json({ detail: { code: "NOT_FOUND", message: path } }, 404));
}

async function installHarness(page: Page, options: HarnessOptions = {}) {
  const state: HarnessState = {
    analyticsUrls: [],
    cacheInvalidations: 0,
    policyUpdates: 0,
    credentialRevocations: 0,
    deletions: 0,
  };
  await seedClientPrefs(page, { locale: "en-US", themeMode: "light", resolvedTheme: "light" });
  await installClientAuth(page, {
    user_id: "owner-a",
    display_name: "Operations Owner",
    roles: [options.role === "owner" ? "tenant_admin" : "user"],
    permissions: [
      "console:dashboard:view",
      "console:eval:view",
      "conversation:playground:access",
      "knowledge:dataset:view",
    ],
    effective_permissions: [
      "console:dashboard:view",
      "console:eval:view",
      "conversation:playground:access",
      "knowledge:dataset:view",
    ],
  });
  await page.route("**/api/v1/agents/**", (route) => fulfillAgentApi(route, state, options));
  return state;
}

test("owner can filter redacted traces and operate governance on desktop", async ({ page }) => {
  await page.setViewportSize({ width: 1440, height: 900 });
  const consoleErrors: string[] = [];
  page.on("console", (entry) => { if (entry.type() === "error") consoleErrors.push(entry.text()); });
  const state = await installHarness(page, { role: "owner", retentionLimited: true });
  await page.goto(`/agents/${AGENT_ID}/analytics`);

  await expect(page.getByTestId("agent-analytics-page")).toBeVisible();
  await expect(page.getByRole("heading", { name: "Support Operations" })).toBeVisible();
  await expect(page.getByText("Results are retention-limited")).toBeVisible();
  await expect(page.getByText("842 ms").first()).toBeVisible();
  await expect(page.getByText("210 ms")).toBeVisible();
  await expect(page.getByText("90%")).toBeVisible();
  await expect(page.getByText("75%")).toBeVisible();
  await expect(page.getByText("80%")).toBeVisible();
  await expect(page.getByText("Runs by channel")).toBeVisible();
  await expect(page.getByText("Authorization: [REDACTED] classify this request").first()).toBeVisible();
  await expect(page.locator("body")).not.toContainText("sk-live-never-render");

  await page.getByRole("combobox", { name: "Channel" }).click();
  await page.locator(".ant-select-dropdown:visible .ant-select-item-option-content", { hasText: /^api$/ }).click();
  await expect.poll(() => state.analyticsUrls.at(-1) || "").toContain("channel=api");
  const traceLink = page.getByRole("link", { name: /44444444/ }).first();
  await expect(traceLink).toHaveAttribute("href", new RegExp(`trace_id=${TRACE_ID}`));
  await capture(page, "analytics-desktop-traces.png");

  await page.getByRole("tab", { name: "Audit" }).click();
  await expect(page.getByText("publication_promote")).toBeVisible();
  await expect(page.locator("body")).not.toContainText("authorization");
  await capture(page, "analytics-desktop-audit.png");

  await page.getByRole("tab", { name: "Governance" }).click();
  await expect(page.getByRole("heading", { name: "Retention and legal hold" })).toBeVisible();
  await page.getByRole("button", { name: "Save policy" }).click();
  await expect.poll(() => state.policyUpdates).toBe(1);
  await page.getByRole("button", { name: "Invalidate Agent cache" }).click();
  await expect.poll(() => state.cacheInvalidations).toBe(1);
  await capture(page, "analytics-desktop-governance.png");
  await assertNoBlockingA11yIssues(page, ["main.agent-analytics"]);
  expect(consoleErrors).toEqual([]);
});

test("mobile editor receives responsive traces and least-privilege controls", async ({ page }) => {
  await page.setViewportSize({ width: 390, height: 844 });
  await installHarness(page, { role: "editor", retentionLimited: false });
  await page.goto(`/agents/${AGENT_ID}/analytics`);

  await expect(page.getByTestId("agent-analytics-page")).toBeVisible();
  await expect(page.locator(".agent-trace-cards article")).toBeVisible();
  await expect(page.locator(".agent-trace-table")).toBeHidden();
  await page.getByRole("tab", { name: "Governance" }).click();
  await expect(page.getByText("Owner access required")).toBeVisible();
  await expect(page.getByRole("button", { name: "Save policy" })).toHaveCount(0);
  await page.keyboard.press("Tab");
  await expect(page.locator(":focus")).toBeVisible();
  await capture(page, "analytics-mobile-editor.png");
  await assertNoBlockingA11yIssues(page, ["main.agent-analytics"]);
});

test("viewer sees empty state without audit or destructive controls", async ({ page }) => {
  await installHarness(page, { role: "viewer", analyticsState: "empty", retentionLimited: false });
  await page.goto(`/agents/${AGENT_ID}/analytics`);
  await expect(page.getByText("No traces match these filters.")).toBeVisible();
  await page.getByRole("tab", { name: "Audit" }).click();
  await expect(page.getByText("Owner access required")).toBeVisible();
  await expect(page.getByRole("button", { name: /Delete runtime data/ })).toHaveCount(0);
});

test("analytics exposes a retryable error state", async ({ page }) => {
  await installHarness(page, { analyticsState: "error" });
  await page.goto(`/agents/${AGENT_ID}/analytics`);
  await expect(page.getByText("Agent analytics could not be loaded")).toBeVisible();
  await expect(page.getByText("Metrics store is unavailable.")).toBeVisible();
  await expect(page.getByRole("button", { name: "Retry" })).toBeVisible();
});

test("frontend flag hides Agent Studio while Assistant, Knowledge, Eval, and Share remain mounted", async ({ page }) => {
  const consoleErrors: string[] = [];
  page.on("console", (entry) => { if (entry.type() === "error") consoleErrors.push(entry.text()); });
  await page.addInitScript(() => {
    window.__AI_GATEWAY_RUNTIME_CONFIG__ = { agentStudioEnabled: "false" };
  });
  await page.route("**/api/v1/**", async (route) => {
    const url = new URL(route.request().url());
    if (url.pathname === "/api/v1/assistant/shares/flag-compatible") {
      return route.fulfill(json({
        share_code: "flag-compatible",
        title: "Feature flag compatibility",
        snapshot: { messages: [], artifacts: [], model_id: "qwen3.7-plus", shared_at: NOW },
        message_count: 0,
        artifact_count: 0,
        view_count: 1,
        created_at: NOW,
        expires_at: null,
      }));
    }
    if (url.pathname.endsWith("/models")) return route.fulfill(json({ models: [] }));
    if (url.pathname.endsWith("/tools")) return route.fulfill(json({ tools: [] }));
    if (url.pathname.includes("/sessions")) return route.fulfill(json([]));
    if (url.pathname === "/api/v1/connectors/mine" || url.pathname === "/api/v1/connectors/available") return route.fulfill(json([]));
    if (url.pathname === "/api/v1/skills") return route.fulfill(json({ skills: [] }));
    if (url.pathname === "/api/v1/mcp/servers") return route.fulfill(json({ servers: [] }));
    if (url.pathname.endsWith("/datasets")) return route.fulfill(json([]));
    if (url.pathname.endsWith("/traces")) return route.fulfill(json({ traces: [], total: 0, limit: 50, offset: 0 }));
    return route.fulfill(json({}));
  });
  await installClientAuth(page, {
    permissions: ["console:dashboard:view", "console:eval:view", "conversation:playground:access", "knowledge:dataset:view"],
    effective_permissions: ["console:dashboard:view", "console:eval:view", "conversation:playground:access", "knowledge:dataset:view"],
  });

  await page.goto("/assistant");
  await expect(page.locator(".assistant-v2").first()).toBeVisible();
  await expect(page.getByRole("link", { name: "Agents" })).toHaveCount(0);
  await page.goto("/knowledge");
  await expect(page).toHaveURL(/\/knowledge$/);
  await expect(page.locator("main")).toBeVisible();
  await page.goto("/eval");
  await expect(page.getByTestId("eval-console")).toBeVisible();
  await page.goto("/share/flag-compatible");
  await expect(page.getByRole("heading", { name: "Feature flag compatibility" })).toBeVisible();
  await page.goto("/agents");
  await expect(page.getByText("404", { exact: true })).toBeVisible();
  expect(consoleErrors).toEqual([]);
});
