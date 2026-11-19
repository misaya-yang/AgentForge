/**
 * Shared first-run harness for the open-source e2e suite.
 *
 * The first-run feature (SetupBanner + SetupChecklist) fires a
 * GET /api/v1/setup/state query from every protected page via
 * AppLayout, so every open-source spec that mounts AppLayout must
 * mock it. This harness also blankets the dashboard's data endpoints
 * with empty shapes so /dashboard renders without a live backend.
 */
import type { Page } from "@playwright/test";
import { installClientAuth, seedClientPrefs } from "./helpers";

export interface FirstRunHarnessOptions {
  /** false → banner + checklist render; true → both stay hidden. */
  configured: boolean;
  permissions?: string[];
  roles?: string[];
}

const DASHBOARD_PERMISSIONS = [
  "console:dashboard:view",
  "console:services:view",
  "knowledge:dataset:view",
  "knowledge:dataset:create",
  "conversation:playground:access",
  "console:eval:view",
  "console:settings:view",
  "user:list",
  "user:edit",
];

function jsonResponse(body: unknown) {
  return {
    status: 200,
    contentType: "application/json",
    body: JSON.stringify(body),
  };
}

export function setupStatePayload(configured: boolean) {
  return {
    configured,
    missing: configured ? [] : ["dashscope"],
    mode: "ui",
    default_model: configured ? "qwen3.7-plus" : null,
  };
}

const EMPTY_DATE_RANGE = {
  start_date: "2026-06-01",
  end_date: "2026-06-30",
};

export async function installFirstRunHarness(page: Page, options: FirstRunHarnessOptions) {
  const permissions = options.permissions ?? DASHBOARD_PERMISSIONS;
  const roles = options.roles ?? ["user"];
  await seedClientPrefs(page, { locale: "en-US", themeMode: "light", resolvedTheme: "light", darkMode: false });
  await installClientAuth(page, {
    user_id: "first-run-user",
    email: "first-run@example.com",
    display_name: "First Run User",
    roles,
    permissions,
    effective_permissions: permissions,
  });

  await page.route("**/api/v1/**", async (route) => {
    const request = route.request();
    const path = new URL(request.url()).pathname;
    if (path === "/api/v1/auth/me") {
      return route.fulfill(jsonResponse({
        user_id: "first-run-user",
        email: "first-run@example.com",
        display_name: "First Run User",
        roles,
        permissions,
        effective_permissions: permissions,
        tier: "normal",
        force_password_change: false,
      }));
    }
    if (path === "/api/v1/setup/state") {
      return route.fulfill(jsonResponse(setupStatePayload(options.configured)));
    }
    // Dashboard data — empty shapes so the page renders without a backend.
    if (path === "/api/v1/services") return route.fulfill(jsonResponse([]));
    if (path === "/api/v1/users") return route.fulfill(jsonResponse({ items: [], total: 0 }));
    if (path === "/api/v1/api-keys") return route.fulfill(jsonResponse([]));
    if (path === "/api/v1/usage/summary") {
      return route.fulfill(jsonResponse({
        total_requests: 0,
        success_rate: 100,
        total_input_tokens: 0,
        total_output_tokens: 0,
        total_tokens: 0,
        total_cost_usd: 0,
        avg_latency_ms: 0,
        ...EMPTY_DATE_RANGE,
        data_status: "complete",
        data_freshness_minutes: 0,
      }));
    }
    if (path === "/api/v1/usage/breakdown" || path === "/api/v1/usage/performance-breakdown" || path === "/api/v1/usage/failure-breakdown") {
      return route.fulfill(jsonResponse({ items: [], ...EMPTY_DATE_RANGE }));
    }
    if (path === "/api/v1/usage/timeseries") {
      return route.fulfill(jsonResponse({ points: [], ...EMPTY_DATE_RANGE }));
    }
    if (path === "/api/v1/usage/traces") return route.fulfill(jsonResponse([]));
    if (path === "/api/v1/quota/users-overview") {
      return route.fulfill(jsonResponse({
        users: [],
        summary: { total: 0, blocked: 0, exceeded: 0, warning: 0, ok: 0 },
      }));
    }
    if (request.method() === "GET") return route.fulfill(jsonResponse({}));
    return route.fulfill(jsonResponse({}));
  });
}
