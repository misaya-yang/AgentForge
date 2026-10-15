import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import vm from "node:vm";
import ts from "typescript";
import * as navigation from "./taskNavigation.ts";

type Tree = { type: string | ((props: Record<string, unknown>) => Tree); props: Record<string, unknown> };
const source = readFileSync(new URL("./components/TaskInboxTab.tsx", import.meta.url), "utf8");
const compiled = ts.transpileModule(source, {
  compilerOptions: { module: ts.ModuleKind.CommonJS, jsx: ts.JsxEmit.ReactJSX },
}).outputText;

// Execute the actual inbox and row functions. React/router/query/UI primitives are
// controlled doubles; this is visible tree evidence, not browser/layout acceptance.
function render(progress: unknown, status = "processing", language = "en-US", omitted = false) {
  const locale = JSON.parse(readFileSync(new URL(`../../i18n/locales/${language}.json`, import.meta.url), "utf8"));
  const row: Record<string, unknown> = {
    task_id: "task-progress-observation", service_id: "service-a", status,
    created_at: "2026-10-14T00:00:00Z", ...(omitted ? {} : { progress }),
  };
  const params = new URLSearchParams("tab=inbox&task_status=processing&task_offset=50");
  const calls: unknown[][] = [];
  const readScope = { id: "progress-read-scope", isCurrent: () => true };
  const modules: Record<string, unknown> = {
    "react/jsx-runtime": { jsx: (type: Tree["type"], props: Tree["props"]) => ({ type, props }), jsxs: (type: Tree["type"], props: Tree["props"]) => ({ type, props }) },
    "react-router-dom": { Link: "a", useSearchParams: () => [params, () => { throw new Error("Unexpected navigation mutation"); }] },
    "react-i18next": { useTranslation: () => ({
      t: (key: string) => key.split(".").reduce((value, part) => value?.[part], locale) ?? key,
      i18n: { language },
    }) },
    "lucide-react": new Proxy({}, { get: (_, key) => String(key) }),
    "@/components/ui/badge": { Badge: "badge" },
    "@/components/ui/button": { Button: "button" },
    "@/components/ui/card": { Card: "card", CardContent: "card-content" },
    "@/hooks/useTasks": { useTasks: (...args: unknown[]) => { calls.push(args); return { data: [row], refetch() { throw new Error("Unexpected read"); } }; } },
    "@/hooks/useAuthenticatedReadScope": { useAuthenticatedReadScope: () => readScope },
    "../taskNavigation": navigation,
  };
  const exports: { TaskInboxTab?: () => Tree } = {};
  vm.runInNewContext(compiled, { exports, require(name: string) { assert.ok(name in modules, name); return modules[name]; } });
  function expand(value: unknown): unknown {
    if (Array.isArray(value)) return value.map(expand);
    if (!value || typeof value !== "object") return value;
    const node = value as Tree;
    if (typeof node.type === "function") return expand(node.type(node.props));
    return { ...node, props: { ...node.props, children: expand(node.props.children) } };
  }
  return { tree: expand(exports.TaskInboxTab!()), calls, readScope };
}
function nodes(value: unknown): Tree[] {
  if (Array.isArray(value)) return value.flatMap(nodes);
  if (!value || typeof value !== "object") return [];
  const node = value as Tree;
  return [node, ...nodes(node.props.children)];
}
function text(value: unknown): string {
  if (typeof value === "string" || typeof value === "number") return String(value);
  if (Array.isArray(value)) return value.map(text).join("");
  return value && typeof value === "object" ? text((value as Tree).props.children) : "";
}
function progressText(tree: unknown) {
  const displays = nodes(tree).filter(node => node.props.className === "mt-1 font-mono text-sm tabular-nums");
  assert.equal(displays.length, 1);
  return text(displays[0]);
}

const statuses = ["pending", "processing", "completed", "failed", "cancelled", "future-state"];
for (const status of statuses) {
  test(`missing progress stays visibly unknown for ${status}`, () => {
    for (const omitted of [false, true]) {
      const { tree, calls, readScope } = render(undefined, status, "en-US", omitted);
      assert.equal(progressText(tree), "Not reported");
      assert.deepEqual(calls, [["processing", 50, readScope]]);
      const link = nodes(tree).find(node => node.type === "a");
      assert.ok(link);
      const target = new URLSearchParams(String(link.props.to).slice(1));
      assert.equal(target.get("task_id"), "task-progress-observation");
      assert.equal(target.get("task_status"), "processing");
      assert.equal(target.get("task_offset"), "50");
    }
  });
}

test("null, coercible values and invalid numbers never manufacture a percentage", () => {
  for (const value of [null, "", "0", "1", "25", "bad", true, false, [], [1], {}, { value: 1 }, NaN, Infinity, -Infinity, -0.01, -1, 100.01, 101]) {
    assert.equal(progressText(render(value).tree), "Not reported", `value=${String(value)}`);
  }
});

test("existing finite numeric mapping and rounding remain compatible in every state", () => {
  const cases = [[0, "0%"], [-0, "0%"], [0.0049, "0%"], [0.005, "1%"], [0.5, "50%"], [0.999, "100%"], [1, "100%"], [1.01, "1%"], [49.49, "49%"], [49.5, "50%"], [99.9, "100%"], [100, "100%"]] as const;
  for (const status of statuses) for (const [value, expected] of cases) {
    assert.equal(progressText(render(value, status).tree), expected, `${status} ${value}`);
  }
});

test("unknown progress is visible in both supported locales", () => {
  assert.equal(progressText(render(null, "completed", "en-US").tree), "Not reported");
  assert.equal(progressText(render(null, "completed", "zh-CN").tree), "未提供");
});

test("refreshing observations can move between known and unknown without sticky presentation", () => {
  for (const [value, expected] of [[0.25, "25%"], [null, "Not reported"], [75, "75%"], ["75", "Not reported"], [0, "0%"]]) {
    assert.equal(progressText(render(value).tree), expected);
  }
});
