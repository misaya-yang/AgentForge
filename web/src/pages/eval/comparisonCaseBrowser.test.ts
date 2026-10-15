import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import vm from "node:vm";
import ts from "typescript";
import * as cases from "./comparisonCaseBrowser.ts";
import type { EvalExperimentRunComparisonResponse } from "../../api/eval.ts";

type Tree = { type: unknown; key?: string; props: Record<string, unknown> };
function nodes(value: unknown): Tree[] {
  if (Array.isArray(value)) return value.flatMap(nodes);
  if (!value || typeof value !== "object") return [];
  const tree = value as Tree; return [tree, ...nodes(tree.props?.children)];
}
function text(value: unknown): string {
  if (typeof value === "string" || typeof value === "number") return String(value);
  if (Array.isArray(value)) return value.map(text).join(" ");
  return value && typeof value === "object" ? text((value as Tree).props?.children) : "";
}
function button(tree: Tree, key: string) {
  const value = nodes(tree).find(node => node.type === "Button" && text(node) === `eval.comparison.${key}`);
  assert.ok(value, key); return value.props as { disabled?: boolean; onClick: () => void };
}
function rows(count: number) { return Array.from({ length: count }, (_, position) => ({ case_id: `case-${String(position + 1).padStart(3, "0")}`, status: "regressed", baseline_trace_id: `baseline-${position + 1}`, candidate_trace_id: `candidate-${position + 1}`, baseline_output: `old-${position + 1}`, candidate_output: `new-${position + 1}` })); }
function response(items: Array<Record<string, unknown>>): EvalExperimentRunComparisonResponse {
  return { baseline_run_id: "baseline", candidate_run_id: "candidate", baseline_summary: {}, candidate_summary: {}, deltas: {}, regression_summary: { regressed_case_count: 101 }, case_diffs: items };
}
function host(initial: EvalExperimentRunComparisonResponse) {
  let comparison = initial; let uuid = 0; let childKey: string | undefined;
  const opened: string[] = [];
  type Hooks = { slots: unknown[]; cursor: number; cleanups: Array<() => void>; effects: Array<() => void> };
  const parent: Hooks = { slots: [], cursor: 0, cleanups: [], effects: [] };
  const child: Hooks = { slots: [], cursor: 0, cleanups: [], effects: [] }; let active = parent;
  const react = {
    useState(value: unknown) { const owner = active; const index = owner.cursor++; if (!(index in owner.slots)) owner.slots[index] = value; return [owner.slots[index], (next: unknown) => { owner.slots[index] = typeof next === "function" ? next(owner.slots[index]) : next; }]; },
    useRef(value: unknown) { const index = active.cursor++; return active.slots[index] ?? (active.slots[index] = { current: value }); },
    useMemo(factory: () => unknown, deps: unknown[]) { const index = active.cursor++; const previous = active.slots[index] as { deps: unknown[]; value: unknown } | undefined;
      if (!previous || deps.some((item, i) => !Object.is(item, previous.deps[i]))) active.slots[index] = { deps, value: factory() }; return (active.slots[index] as { value: unknown }).value; },
    useLayoutEffect(effect: () => () => void, deps: unknown[]) { const owner = active; const index = owner.cursor++; const previous = owner.slots[index] as unknown[] | undefined;
      if (!previous || deps.some((item, i) => !Object.is(item, previous[i]))) { owner.slots[index] = deps; owner.effects.push(() => { owner.cleanups[index]?.(); owner.cleanups[index] = effect(); }); } },
  };
  const jsx = (type: unknown, props: Record<string, unknown>, key?: string) => ({ type, props, key });
  const named = new Proxy({}, { get: (_, key) => String(key) });
  const modules: Record<string, unknown> = {
    react, "react/jsx-runtime": { jsx, jsxs: jsx }, "lucide-react": named,
    antd: { ...Object.fromEntries(["Alert", "Button", "Descriptions", "Input", "Select", "Segmented", "Tag"].map(key => [key, key])), Empty: Object.assign(() => {}, { PRESENTED_IMAGE_SIMPLE: "empty" }) },
    "react-i18next": { useTranslation: () => ({ t: (key: string, values?: unknown) => values && typeof values === "object" ? `${key}:${JSON.stringify(values)}` : key }) },
    "../comparisonCaseBrowser": cases, "../agentVersionTarget": { agentVersionTarget: () => null },
    "@/lib/utils": { generateUUID: () => `response-${++uuid}` },
  };
  function compile(path: string) {
    const exports: Record<string, (props: unknown) => Tree> = {};
    vm.runInNewContext(ts.transpileModule(readFileSync(new URL(path, import.meta.url), "utf8"), {
      compilerOptions: { module: ts.ModuleKind.CommonJS, jsx: ts.JsxEmit.ReactJSX, target: ts.ScriptTarget.ES2022 },
    }).outputText, { exports, require: (id: string) => { assert.ok(id in modules, id); return modules[id]; } }); return exports;
  }
  const browser = compile("./components/ComparisonCaseBrowser.tsx"); modules["./ComparisonCaseBrowser"] = browser;
  const panel = compile("./components/ExperimentRunComparison.tsx");
  return {
    opened, replace(value: EvalExperimentRunComparisonResponse) { comparison = value; },
    render() {
      active = parent; active.cursor = 0;
      const tree = panel.ExperimentRunComparison({ comparison, runs: [], onOpenTrace: (id: string) => opened.push(id) });
      const entry = nodes(tree).find(node => node.type === browser.ComparisonCaseBrowser); assert.ok(entry);
      if (entry.key !== childKey) { child.cleanups.forEach(cleanup => cleanup?.()); child.slots = []; child.cleanups = []; childKey = entry.key; }
      active = child; active.cursor = 0; const view = browser.ComparisonCaseBrowser(entry.props); child.effects.splice(0).forEach(effect => effect()); return view;
    },
    unmount() { child.cleanups.forEach(cleanup => cleanup?.()); },
  };
}
function articles(tree: Tree) { return nodes(tree).filter(node => node.type === "article"); }
function change(tree: Tree, control: string, value: unknown) {
  const node = nodes(tree).find(item => item.type === control); assert.ok(node);
  (node.props.onChange as (value: unknown) => void)(control === "Input" ? { target: { value } } : value);
}

test("full returned list pages beyond50 without merging duplicate case or Trace IDs", () => {
  const input = rows(103); input[102] = { ...input[102], case_id: input[0].case_id, candidate_trace_id: input[0].candidate_trace_id };
  const original = JSON.stringify(input); const ordered = cases.comparisonCaseRows(input); const visited: number[] = [];
  for (let offset = 0; offset < input.length; offset += 25) visited.push(...cases.comparisonCaseWindow(ordered, "", undefined, offset).rows.map(row => row.position));
  assert.equal(new Set(visited).size, 103); assert.equal(JSON.stringify(input), original);
});
test("unknown and legacy classification states remain filterable; no missing ID fabrication in search", () => {
  const input = cases.comparisonCaseRows([{ case_id: "One", classification: "custom-state" }, { case_id: "Two" }, { status: "unscored" }]);
  assert.equal(cases.comparisonCaseWindow(input, "one", "custom-state", 0).filtered, 1);
  assert.equal(cases.comparisonCaseWindow(input, "", "unknown", 0).filtered, 1);
  assert.equal(cases.comparisonCaseWindow(input, "case-3", undefined, 0).filtered, 0);
});
test("page boundaries and malformed offsets remain bounded", () => {
  const input = cases.comparisonCaseRows(rows(101));
  for (const offset of [-1, Infinity, NaN, 1.5]) assert.equal(cases.comparisonCaseWindow(input, "", undefined, offset).offset, 0);
  const last = cases.comparisonCaseWindow(input, "", undefined, 9999); assert.equal(last.start, 101); assert.equal(last.end, 101); assert.equal(last.hasNext, false);
  const empty = cases.comparisonCaseWindow(input, "missing", undefined, 100); assert.equal(empty.start, 0); assert.equal(empty.end, 0); assert.equal(empty.offset, 0);
});
test("production browser reaches all103 returned cases with bounded DOM and correct last Trace IDs", () => {
  const h = host(response(rows(103))); const ids: string[] = [];
  for (let page = 0; page < 5; page++) {
    const tree = h.render(); assert.ok(articles(tree).length <= 25); ids.push(...articles(tree).map(item => String(item.key)));
    if (page < 4) button(tree, "nextCases").onClick();
    else { assert.equal(button(tree, "nextCases").disabled, true); const last = articles(tree).at(-1)!; button(last, "baselineTrace").onClick(); button(last, "candidateTrace").onClick(); }
  }
  assert.equal(new Set(ids).size, 103); assert.deepEqual(h.opened, ["baseline-103", "candidate-103"]); h.unmount();
});
test("production local case-ID and unknown-status filters reset page and keep returned count", () => {
  const input = rows(103); input[102].status = "custom-state"; const h = host(response(input)); button(h.render(), "nextCases").onClick();
  change(h.render(), "Input", "case-103"); let tree = h.render(); assert.equal(articles(tree).length, 1); assert.equal(button(tree, "previousCases").disabled, true);
  assert.ok(text(tree).includes('"returned":103')); change(tree, "Input", ""); change(h.render(), "Select", "custom-state"); tree = h.render();
  assert.equal(articles(tree).length, 1); assert.ok(text(tree).includes("case-103")); const select = nodes(tree).find(node => node.type === "Select")!;
  assert.ok((select.props.options as Array<{ value: string }>).some(option => option.value === "custom-state")); h.unmount();
});
test("zero matches show zero range and can clear filter without a new request", () => {
  const h = host(response(rows(103))); change(h.render(), "Input", "not-present"); let tree = h.render(); assert.equal(articles(tree).length, 0);
  assert.ok(text(tree).includes('"start":0,"end":0,"filtered":0,"returned":103')); assert.equal(button(tree, "nextCases").disabled, true);
  change(tree, "Input", ""); tree = h.render(); assert.equal(articles(tree).length, 25); h.unmount();
});
for (const samePair of [true, false]) {
  test(`response replacement resets filters and page even for same pair=${samePair}`, () => {
    const old = response(rows(103)); const h = host(old); button(h.render(), "nextCases").onClick(); change(h.render(), "Select", "regressed"); button(h.render(), "nextCases").onClick();
    const replacement = response([{ case_id: "replacement", status: "improved", candidate_trace_id: "new-trace" }]); if (!samePair) replacement.candidate_run_id = "other-candidate";
    h.replace(replacement); const tree = h.render(); assert.equal(articles(tree).length, 1); assert.ok(text(tree).includes("replacement")); assert.equal(button(tree, "previousCases").disabled, true);
    assert.equal(nodes(tree).find(node => node.type === "Select")?.props.value, undefined); h.unmount();
  });
}
test("old response Trace callbacks are inert after replacement or unmount", () => {
  const h = host(response(rows(1))); const old = button(articles(h.render())[0], "candidateTrace"); h.replace(response(rows(1))); h.render(); old.onClick(); assert.deepEqual(h.opened, []);
  const current = button(articles(h.render())[0], "candidateTrace"); h.unmount(); current.onClick(); assert.deepEqual(h.opened, []);
});
test("paging/filtering never rewrites comparison scores or source response", () => {
  const input = response(rows(103)); const before = JSON.stringify(input); const h = host(input); button(h.render(), "nextCases").onClick(); change(h.render(), "Input", "case-100"); h.render(); assert.equal(JSON.stringify(input), before); h.unmount();
});
