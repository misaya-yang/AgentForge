import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import vm from "node:vm";
import ts from "typescript";

type Node = { type: unknown; props: Record<string, unknown> & { children?: unknown; step?: { id: string }; onClick?: () => void; onApprove?: () => void; onReject?: () => void }; key?: string };
function nodes(value: unknown): Node[] {
  if (Array.isArray(value)) return value.flatMap(nodes);
  if (!value || typeof value !== "object") return [];
  const node = value as Node;
  return [node, ...nodes(node.props?.children)];
}
const step = (i: number, status = "completed") => ({ kind: "tool", id: `tool-${i}`, icon: "other", title: `Tool ${i}`, body: "", status });
const history = (count = 1000) => Array.from({ length: count }, (_, i) => step(i));
function host(language = "en-US") {
  const translations = JSON.parse(readFileSync(new URL(`../../../i18n/locales/${language}.json`, import.meta.url), "utf8"));
  const t = (key: string, options: Record<string, unknown> = {}) => {
    const text = key.split(".").reduce((value, part) => value?.[part], translations) ?? options.defaultValue ?? key;
    return String(text).replace(/\{\{(\w+)\}\}/g, (_, name) => String(options[name]));
  };
  let state: unknown[] = []; let index = 0;
  const jsx = (type: unknown, props: Node["props"], key?: string) => ({ type, props, key });
  const modules: Record<string, unknown> = {
    react: {
      useState(initial: unknown) { const slot = index++; if (!(slot in state)) state[slot] = typeof initial === "function" ? initial() : initial; return [state[slot], (next: unknown) => { state[slot] = next; }]; },
      useMemo: (fn: () => unknown) => fn(),
    },
    "react/jsx-runtime": { jsx, jsxs: jsx },
    "react-i18next": { useTranslation: () => ({ t }) },
    "./TimelineStep": { TimelineStep: "TimelineStep" },
    "./activityTheme": { T: {}, ui: {}, ensureActivityStyles() {} },
    "./activityIcons": { Icon: "Icon" },
    "./buildTimeline": { buildTimeline: (message: { steps: unknown[] }) => ({ steps: message.steps, totalDurationMs: 10 }) },
    "./ToolApprovalCard": { ToolApprovalCard: "ToolApprovalCard" },
  };
  function compile(path: string) {
    const exports: Record<string, unknown> = {};
    vm.runInNewContext(ts.transpileModule(readFileSync(new URL(path, import.meta.url), "utf8"), { compilerOptions: { jsx: ts.JsxEmit.ReactJSX, module: ts.ModuleKind.CommonJS, target: ts.ScriptTarget.ES2022 } }).outputText,
      { exports, require(id: string) { assert.ok(id in modules, id); return modules[id]; } });
    return exports;
  }
  modules["../assistantOutcome"] = compile("../assistantOutcome.ts");
  modules["./activityTerminalState"] = compile("./activityTerminalState.ts");
  const timeline = compile("./ActivityTimeline.tsx").ActivityTimeline as (props: unknown) => Node;
  modules["./ActivityTimeline"] = { ActivityTimeline: timeline };
  const panel = compile("./ActivityPanel.tsx").ActivityPanel as (props: unknown) => Node;
  return {
    timeline, t,
    render(steps: unknown[], id = "run-a", running = true) { index = 0; return nodes(timeline({ steps, message: { id }, running })); },
    panel(message: unknown, onToolApproval: (...args: unknown[]) => void) { index = 0; return nodes(panel({ open: true, message, onClose() {}, onToolApproval })); },
    reset() { state = []; },
  };
}
const rows = (tree: Node[]) => tree.filter(node => node.type === "TimelineStep");
const ids = (tree: Node[]) => rows(tree).map(node => node.props.step!.id);
function click(tree: Node[], label: string) { const button = tree.find(node => node.type === "button" && node.props.children === label); assert.ok(button); assert.equal(button.props.disabled, false); assert.equal(button.props.type, "button"); button.props.onClick!(); }

for (const language of ["en-US", "zh-CN"]) test(`1000 loaded steps have bounded, complete chronological navigation (${language})`, () => {
  const h = host(language); const steps = history(); let tree = h.render(steps);
  assert.equal(rows(tree).length, 50); assert.deepEqual(ids(tree), steps.slice(950).map(s => s.id));
  const seen = new Set(ids(tree));
  for (let i = 0; i < 19; i++) { click(tree, h.t("assistant.activity.earlier")); tree = h.render(steps); assert.equal(rows(tree).length, 50); ids(tree).forEach(id => seen.add(id)); }
  assert.equal(seen.size, 1000); assert.deepEqual(ids(tree), steps.slice(0, 50).map(s => s.id));
  assert.equal(tree.find(n => n.props.children === h.t("assistant.activity.earlier"))?.props.disabled, true);
  click(tree, h.t("assistant.activity.later")); tree = h.render(steps); assert.deepEqual(ids(tree), steps.slice(50, 100).map(s => s.id));
  click(tree, h.t("assistant.activity.latest")); tree = h.render(steps); assert.deepEqual(ids(tree), steps.slice(950).map(s => s.id));
  assert.ok(tree.some(n => n.props.children === h.t("assistant.activity.loadedRange", { start: 951, end: 1000, count: 1000 })));
  assert.equal(tree.find(n => n.type === "nav")?.props["aria-label"], h.t("assistant.activity.navigation"));
});

test("initial and historical identity stay fixed on append; Latest is an explicit jump", () => {
  const h = host(); const steps = history(); let tree = h.render(steps);
  const initial = ids(tree);
  tree = h.render([...steps, step(1000, "running")]); assert.deepEqual(ids(tree), initial);
  click(tree, h.t("assistant.activity.earlier")); tree = h.render(steps); const original = ids(tree);
  const longer = [...steps, step(1000, "running")]; tree = h.render(longer); assert.deepEqual(ids(tree), original);
  tree = h.render([step(-1), ...longer]); assert.deepEqual(ids(tree), original);
  click(tree, h.t("assistant.activity.latest")); tree = h.render(longer); assert.equal(ids(tree).at(-1), "tool-1000");
  tree = h.render([...longer, step(1001, "running")]); assert.equal(ids(tree).at(-1), "tool-1000"); assert.equal(rows(tree).length, 50);
  click(tree, h.t("assistant.activity.latest")); tree = h.render([...longer, step(1001, "running")]); assert.equal(ids(tree).at(-1), "tool-1001");
});

test("short/empty histories, shrinking histories and message switches cannot retain an invalid page", () => {
  const h = host(); assert.equal(rows(h.render([])).length, 0); assert.equal(h.render(history(50)).some(n => n.type === "nav"), false);
  assert.equal(ids(h.render(history()))[0], "tool-0");
  h.reset(); let tree = h.render(history()); click(tree, h.t("assistant.activity.earlier"));
  tree = h.render(history(), "run-b"); assert.equal(ids(tree)[0], "tool-950");
  tree = h.render(history(5)); assert.equal(rows(tree).length, 5);
});

test("paging preserves each error, unknown and not-executed marker and independently streaming reasoning", () => {
  const h = host(); const steps = [...history(996), step(996, "error"), step(997, "unknown"), step(998, "not_executed"), { kind: "thinking", id: "thinking", title: "Thinking", body: "Public summary", streaming: true }];
  const visible = rows(h.render(steps)); assert.equal(visible.at(-4)?.props.isError, true);
  for (const row of visible.slice(-3, -1)) { assert.equal(row.props.isCompleted, false); assert.equal(row.props.isRunning, false); }
  assert.equal(visible.at(-1)?.props.isRunning, true); assert.equal(rows(h.render(steps, "run-a", false)).at(-1)?.props.isRunning, false);
});

test("actual drawer exposes pending approval before history and forwards existing authorization bindings", () => {
  const h = host(); const decisions: unknown[][] = [];
  const message = { id: "run-a", role: "assistant", content: "", isStreaming: false, sourceAccessRevoked: true, status: "pending", steps: history(), processSummary: { status: "blocked", runtimeThreadId: "thread-a", tools: [{ id: "old-tool", name: "read_file", status: "approval_required", approvalId: "approval-a" }] } };
  const tree = h.panel(message, (...args) => decisions.push(args)); const approval = tree.find(n => n.type === "ToolApprovalCard"); const timeline = tree.find(n => n.type === h.timeline);
  assert.ok(approval); assert.ok(timeline); assert.ok(tree.indexOf(approval) < tree.indexOf(timeline));
  assert.equal(approval.props.sourceAccessRevoked, true); assert.equal(approval.props.runtimeThreadId, "thread-a");
  approval.props.onApprove!(); approval.props.onReject!(); assert.deepEqual(decisions, [["run-a", "old-tool", "approval-a", true], ["run-a", "old-tool", "approval-a", false]]);
  assert.equal(timeline.key, "run-a"); assert.equal(timeline.props.message, message);
  const switched = h.panel({ ...message, id: "run-b" }, () => {}); assert.equal(switched.find(n => n.type === h.timeline)?.key, "run-b");
  const unknown = h.panel({ ...message, outcomeUncertain: true }, () => {});
  assert.equal(unknown.find(n => n.type === h.timeline)?.props.running, false);
  assert.ok(unknown.some(n => typeof n.props.children === "string" && n.props.children.includes(h.t("assistant.outcome.unknown.title"))));
  for (const status of ["cancelled", "failed", "completed"]) {
    const terminal = h.panel({ ...message, status, processSummary: { ...message.processSummary, status } }, () => {});
    assert.equal(terminal.some(n => n.type === "ToolApprovalCard"), false);
    assert.equal(terminal.find(n => n.type === h.timeline)?.props.running, false);
  }
});
