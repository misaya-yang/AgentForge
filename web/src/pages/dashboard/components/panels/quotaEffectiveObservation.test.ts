import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import vm from "node:vm";
import ts from "typescript";
import type { QuotaUserOverviewItem, QuotaUsersOverviewResponse } from "../../../../api/usage";

type Node = { type: unknown; props: Record<string, unknown> };
function nodes(value: unknown, predicate: (node: Node) => boolean): Node[] {
  if (!value || typeof value !== "object") return [];
  if (Array.isArray(value)) return value.flatMap(item => nodes(item, predicate));
  const node = value as Node;
  return [...(predicate(node) ? [node] : []), ...nodes(node.props?.children, predicate)];
}
function text(value: unknown): string {
  if (value == null) return "";
  if (typeof value !== "object") return String(value);
  if (Array.isArray(value)) return value.map(text).join("");
  return text((value as Node).props?.children);
}
async function render(data: QuotaUsersOverviewResponse) {
  const calls: string[] = [];
  const translations: { key: string; values: Record<string, unknown> }[] = [];
  const modules: Record<string, unknown> = {};
  function load(file: string): Record<string, unknown> {
    const exports: Record<string, unknown> = {};
    vm.runInNewContext(ts.transpileModule(readFileSync(new URL(file, import.meta.url), "utf8"), {
      compilerOptions: { target: ts.ScriptTarget.ES2022, module: ts.ModuleKind.CommonJS, jsx: ts.JsxEmit.ReactJSX },
    }).outputText, { exports, URLSearchParams, require(name: string) {
      assert.ok(name in modules, `Unexpected dependency ${name}`); return modules[name];
    } });
    return exports;
  }
  let query: Promise<unknown> | undefined;
  modules["@/lib/api"] = { api: { async get(url: string) { calls.push(url); return { data }; } } };
  const api = load("../../../../api/usage.ts");
  const jsx = (type: unknown, props: Record<string, unknown>) => ({ type, props });
  Object.assign(modules, {
    antd: Object.fromEntries(["Table", "Progress", "Tag", "Select", "Tooltip", "Button"].map(key => [key, key])),
    "@tanstack/react-query": { useQuery(options: { queryFn: () => Promise<unknown> }) {
      query = options.queryFn(); return { data, isLoading: false, refetch() {} };
    } },
    "@ant-design/icons": {}, "../PanelWrapper": { PanelWrapper: "PanelWrapper" },
    "../../DashboardContext": { useDashboardContext: () => ({ lastRefresh: new Date("2026-10-12T12:00:00Z") }) },
    "@/store/useAppStore": { useAppStore: () => ({ darkMode: false }) }, "@/api/usage": api,
    react: { useState: (value: unknown) => [value, () => {}] }, "react/jsx-runtime": { jsx, jsxs: jsx },
    "react-i18next": { useTranslation: () => ({ t(key: string, values: Record<string, unknown> = {}) {
      translations.push({ key, values }); return `${key}${Object.keys(values).length ? JSON.stringify(values) : ""}`;
    } }) },
    "../../hooks/useDashboardEntityLabels": { useDashboardEntityLabels: () => ({ resolveUserLabel: (value: string) => value }) },
    "../../styles": { getColors: () => ({}) },
  });
  const panel = load("./UserQuotaPanel.tsx").UserQuotaPanel as () => unknown;
  const tree = panel(); await query;
  const table = nodes(tree, node => node.type === "Table")[0];
  type Column = { key: string; render: (value: unknown, row: QuotaUserOverviewItem) => unknown };
  const columns = table.props.columns as Column[];
  const row = (table.props.dataSource as QuotaUserOverviewItem[])[0];
  const daily = columns.find(column => column.key === "daily")!.render(null, row);
  const status = columns.find(column => column.key === "status")!.render(row.status, row);
  return { calls, daily, status, translations, table, row };
}
function response(observed = true): QuotaUsersOverviewResponse {
  const item: QuotaUserOverviewItem = {
    user_id: "fixture-user", daily_tokens_used: 110, daily_tokens_limit: 100,
    monthly_cost_used_cents: 0, monthly_cost_limit_cents: null,
    monthly_tokens_used: 0, monthly_tokens_limit: null, is_blocked: false,
    overage_strategy: "hard_block", status: "ok",
  };
  if (observed) item.limit_observation = {
    observed_at: "2026-10-12T12:00:00+00:00",
    effective: { daily_tokens: 200, monthly_tokens: null, monthly_cost_cents: null },
    sources: { daily_tokens: "configured_plus_temporary_boost", monthly_tokens: "unlimited", monthly_cost_cents: "unlimited" },
    temporary_boost: { extra_tokens: 100, extra_cost_cents: 0, expires_at: "2099-01-01T00:00:00+00:00", active: true },
  };
  return { users: [item], summary: { total: 1, blocked: 0, exceeded: 0, warning: 0, ok: 1 } };
}
test("actual API/panel show effective snapshot and retain configured base provenance", async () => {
  const result = await render(response());
  assert.deepEqual(result.calls, ["/api/v1/quota/users-overview?limit=20&sort_by=daily_tokens"]);
  assert.ok(text(result.daily).includes("110/200"));
  assert.ok(Math.abs(Number(nodes(result.daily, node => node.type === "Progress")[0].props.percent) - 55) < 1e-9);
  assert.ok(text(result.daily).includes('configuredLimit{"value":"100"}'));
  assert.ok(text(result.daily).includes("effectiveSnapshot"));
  const detail = result.translations.find(item => item.key.endsWith("observationDetail"))!;
  assert.equal(detail.values.observed, "2026-10-12T12:00:00+00:00");
  assert.equal(detail.values.extra, "100");
  assert.equal(detail.values.active, "dashboard.userQuota.boostActive");
  assert.equal(text(result.status), "dashboard.userQuota.status.normal");
});
test("older API response is explicitly configured-only, never labelled effective", async () => {
  const result = await render(response(false));
  assert.ok(text(result.daily).includes("110/100"));
  assert.ok(text(result.daily).includes("configuredOnly"));
  assert.ok(!text(result.daily).includes("effectiveSnapshot"));
});
for (const limit of [null, 0]) {
  test(`effective ${limit} retains unlimited presentation`, async () => {
    const data = response(); data.users[0].limit_observation!.effective.daily_tokens = limit;
    const result = await render(data);
    assert.ok(text(result.daily).includes("∞"));
    assert.equal(nodes(result.daily, node => node.type === "Progress").length, 0);
  });
}
test("stored zero plus active boost displays finite effective limit and configured unlimited", async () => {
  const data = response(); data.users[0].daily_tokens_limit = 0;
  data.users[0].limit_observation!.effective.daily_tokens = 100;
  const result = await render(data);
  assert.ok(text(result.daily).includes("110/100"));
  assert.ok(text(result.daily).includes('configuredLimit{"value":"∞"}'));
});
test("expired boost retains provenance and displays configured effective value", async () => {
  const data = response(); const observation = data.users[0].limit_observation!;
  observation.effective.daily_tokens = 100; observation.sources.daily_tokens = "configured";
  observation.temporary_boost.active = false; observation.temporary_boost.expires_at = "2000-01-01T00:00:00+00:00";
  const result = await render(data);
  assert.ok(text(result.daily).includes("110/100"));
  assert.equal(result.translations.find(item => item.key.endsWith("observationDetail"))!.values.active, "dashboard.userQuota.boostInactive");
});
// Optional offline roundtrip receipt generated by executing the actual Python API.
// Ordinary suite execution has no cross-language process or I/O dependency.
if (process.env.QUOTA_OBSERVATION_RECEIPT) {
  const receipt = JSON.parse(readFileSync(process.env.QUOTA_OBSERVATION_RECEIPT, "utf8")) as {
    results: { case: string; overview: QuotaUsersOverviewResponse }[];
  };
  for (const fixture of receipt.results) test(`serialized Python response → actual API/panel: ${fixture.case}`, async () => {
    const result = await render(fixture.overview);
    const observation = result.row.limit_observation!;
    assert.ok(observation);
    assert.ok(text(result.daily).includes("effectiveSnapshot"));
    const limit = observation.effective.daily_tokens;
    if (limit == null || limit === 0) assert.ok(text(result.daily).includes("∞"));
    else assert.ok(text(result.daily).includes(`${result.row.daily_tokens_used}/${limit}`));
    assert.ok(result.translations.some(item => item.key.endsWith("configuredLimit")));
  });
}
