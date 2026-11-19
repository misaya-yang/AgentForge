import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import vm from "node:vm";
import ts from "typescript";
import { sdkAuthorityScope } from "../../lib/sdkAuthorityScope.ts";

type Tree = { type: unknown; props: Record<string, unknown>; key?: string };
const id = "00000000-0000-0000-0000-000000000001";
const other = "00000000-0000-0000-0000-000000000002";
function nodes(tree: unknown): Tree[] {
  if (Array.isArray(tree)) return tree.flatMap(nodes);
  if (!tree || typeof tree !== "object") return [];
  const node = tree as Tree; return [node, ...nodes(node.props?.children)];
}
function text(tree: unknown): string {
  if (typeof tree === "string") return tree;
  if (Array.isArray(tree)) return tree.map(text).join(" ");
  return tree && typeof tree === "object" ? text((tree as Tree).props?.children) : "";
}
function deferred<T>() { let resolve!: (value: T) => void; let reject!: (error: Error) => void; const promise = new Promise<T>((yes, no) => { resolve = yes; reject = no; }); return { promise, resolve, reject }; }
const settle = () => new Promise(resolve => setTimeout(resolve, 0));
function button(tree: Tree, key: string) {
  const node = nodes(tree).find(node => node.type === "button" && text(node) === `tasks.scheduleCreation.${key}`);
  assert.ok(node, key); return node.props;
}
function host() {
  const auth = { token: "synthetic-a", user: { user_id: "actor-a", roles: [], permissions: [], tier: "normal" } };
  const calls: { method: string; path: string; body?: Record<string, unknown> }[] = [];
  let accepted: Record<string, unknown> = {};
  const response = (body: Record<string, unknown>) => ({ task_id: id, title: String(body.title).trim(), message: body.message, model_id: body.model_id, status: "active", next_run_at: body.run_at, interval_seconds: body.interval_seconds ?? null, max_retries: body.max_retries, retry_count: 0 });
  let write: (body: Record<string, unknown>) => Promise<unknown> = async body => ({ task: response(body) });
  let read: () => Promise<unknown> = async () => ({ task: accepted });
  let params = new URLSearchParams("tab=scheduled&schedule_before_id=kept");
  type Slot = { value?: unknown; deps?: unknown[]; cleanup?: () => void };
  let outer: Slot[] = [], editor: Slot[] = [], form: Slot[] = [], slots: Slot[] = outer;
  let cursor = 0, editorKey: string | undefined, formKey: string | undefined, next = 0;
  let effects: Array<() => void> = [];
  let history: Tree | undefined;
  const same = (a?: unknown[], b?: unknown[]) => a?.length === b?.length && a?.every((value, i) => Object.is(value, b?.[i]));
  const jsx = (type: unknown, props: Tree["props"], key?: string) => ({ type, props, key });
  const authHook = Object.assign((selector: (value: typeof auth) => unknown) => selector(auth), { getState: () => auth });
  const modules: Record<string, unknown> = {
    react: {
      useState(initial: unknown) { const slot = slots[cursor++] ??= { value: initial }; return [slot.value, (value: unknown) => { slot.value = typeof value === "function" ? value(slot.value) : value; }]; },
      useRef(initial: unknown) { const slot = slots[cursor++] ??= { value: { current: initial } }; return slot.value; },
      useMemo(make: () => unknown, deps: unknown[]) { const slot = slots[cursor++] ??= {}; if (!same(slot.deps, deps)) { slot.value = make(); slot.deps = deps; } return slot.value; },
      useEffect(effect: () => (() => void), deps: unknown[]) { const slot = slots[cursor++] ??= {}; if (!same(slot.deps, deps)) { effects.push(() => { slot.cleanup?.(); slot.cleanup = effect(); }); slot.deps = deps; } },
    },
    "react/jsx-runtime": { jsx, jsxs: jsx },
    "react-router-dom": { Link: "a", useSearchParams: () => [params] },
    "react-i18next": { useTranslation: () => ({ t: (key: string, options?: unknown) => key + (options && typeof options === "object" ? ` ${JSON.stringify(options)}` : "") }) },
    "@/store/useAuthStore": { useAuthStore: authHook },
    "@/lib/sdkAuthorityScope": { sdkAuthorityScope },
    "@/lib/utils": { generateUUID: () => `visit-${++next}` },
    "@/components/ui/button": { Button: "button" }, "@/components/ui/input": { Input: "input" }, "@/components/ui/textarea": { Textarea: "textarea" },
    "@/components/ui/card": { Card: "card", CardHeader: "card-header", CardTitle: "card-title", CardContent: "card-content" },
    "./ScheduledTaskActions": { ScheduledTaskActions: "schedule-actions" },
    "./ScheduledHistoryBrowser": { ScheduledHistoryBrowser: "history" },
    "@/lib/api": { api: {
      async post(path: string, body: Record<string, unknown>) { calls.push({ method: "POST", path, body }); const data = await write(body); accepted = response(body); return { data }; },
      async get(path: string) { calls.push({ method: "GET", path }); return { data: await read() }; },
    } },
  };
  function load(relative: string) {
    const exports: Record<string, (...args: unknown[]) => unknown> = {};
    vm.runInNewContext(ts.transpileModule(readFileSync(new URL(relative, import.meta.url), "utf8"), { compilerOptions: { target: ts.ScriptTarget.ES2022, module: ts.ModuleKind.CommonJS, jsx: ts.JsxEmit.ReactJSX } }).outputText, {
      exports, require(name: string) { assert.ok(name in modules, name); return modules[name]; }, URLSearchParams, Date, TextEncoder, Error,
    }); return exports;
  }
  modules["@/api/scheduledTasks"] = load("../../api/scheduledTasks.ts");
  const workflow = load("./scheduledCreation.ts"); modules["../scheduledCreation"] = workflow;
  const component = load("./components/ScheduledTaskCreation.tsx"); modules["./ScheduledTaskCreation"] = component;
  const parent = load("./components/ScheduledTasksTab.tsx");
  function cleanup() { for (const slot of form) slot.cleanup?.(); form = []; }
  return {
    auth, calls, workflow, response,
    write(fn: typeof write) { write = fn; }, read(fn: typeof read) { read = fn; },
    navigate(query: string) { params = new URLSearchParams(query); },
    unmount() { cleanup(); outer = []; editor = []; editorKey = undefined; formKey = undefined; },
    history: () => history,
    render() {
      slots = outer; cursor = 0; const root = parent.ScheduledTasksTab() as Tree;
      if (root.key !== editorKey) { cleanup(); editor = []; editorKey = root.key; formKey = undefined; }
      slots = editor; cursor = 0; const tree = (root.type as (props: Tree["props"]) => Tree)(root.props);
      history = nodes(tree).find(node => node.type === "history");
      const child = nodes(tree).find(node => node.type === component.ScheduledTaskCreation);
      if (!child) { cleanup(); return tree; }
      if (child.key !== formKey) { cleanup(); formKey = child.key; }
      slots = form; cursor = 0; const rendered = component.ScheduledTaskCreation(child.props) as Tree;
      for (const effect of effects) effect(); effects = []; return rendered;
    },
  };
}
function field(h: ReturnType<typeof host>, name: string) { const node = nodes(h.render()).find(node => node.props.name === name); assert.ok(node, name); return node.props; }
function change(h: ReturnType<typeof host>, name: string, value: string) { (field(h, name).onChange as (event: unknown) => void)({ target: { value } }); }
function fill(h: ReturnType<typeof host>) {
  for (const [name, value] of Object.entries({ title: "  Daily review  ", message: "Keep these exact instructions", model: "existing-model", runAt: "2099-01-02T03:04", interval: "60", retries: "2" })) change(h, name, value);
}
function submit(h: ReturnType<typeof host>) { const form = nodes(h.render()).find(node => node.type === "form"); assert.ok(form); (form.props.onSubmit as (event: unknown) => void)({ preventDefault() {} }); }

test("actual mounted form submits once, retains exact input, reads exact acknowledgement ID and links it", async () => {
  const h = host(); fill(h); const titleHandler = field(h, "title").onChange as (event: unknown) => void;
  submit(h); submit(h); titleHandler({ target: { value: "late edit" } }); await settle();
  assert.equal(h.calls.filter(call => call.method === "POST").length, 1);
  assert.equal(h.calls[0].path, "/api/v1/scheduled-tasks");
  assert.equal(h.calls[0].body?.title, "  Daily review  "); assert.equal(h.calls[0].body?.interval_seconds, 3600);
  assert.equal(field(h, "title").value, "  Daily review  ");
  assert.equal(h.calls[1].path, `/api/v1/scheduled-tasks/${id}`);
  assert.match(text(h.render()), /scheduleCreation.accepted/); assert.match(text(h.render()), /scheduleCreation.observed/);
  const link = nodes(h.render()).find(node => node.type === "a"); assert.ok(link);
  const params = new URLSearchParams(String(link.props.to)); assert.equal(params.get("task"), id); assert.equal(params.get("schedule_before_id"), "kept");
});

test("lost POST response remains locked and read-only history review never attributes a matching title", async () => {
  const h = host(); h.write(async () => { throw new Error("Lost response"); }); fill(h); submit(h); await settle();
  assert.match(text(h.render()), /scheduleCreation.uncertain/); assert.equal(h.calls.length, 1);
  submit(h); assert.equal(h.calls.length, 1); assert.equal(field(h, "message").value, "Keep these exact instructions");
  const revision = h.history()?.props.revision; (button(h.render(), "review").onClick as () => void)(); h.render();
  assert.equal(h.history()?.props.revision, Number(revision) + 1); assert.equal(h.calls.length, 1);
  assert.doesNotMatch(text(h.render()), /scheduleCreation.another|scheduleCreation.open/);
});

for (const malformed of ["missing", "id", "input"]) test(`malformed ${malformed} acknowledgement never authorizes an identity GET`, async () => {
  const h = host(); h.write(async body => ({ task: malformed === "missing" ? {} : { ...h.response(body), ...(malformed === "id" ? { task_id: "invented" } : { message: "different request" }) } }));
  fill(h); submit(h); await settle(); assert.match(text(h.render()), /scheduleCreation.uncertain/); assert.equal(h.calls.length, 1);
});

test("accepted identity survives failed or wrong-ID reads and refresh never repeats POST", async () => {
  const h = host(); h.read(async () => { throw new Error("read unavailable"); }); fill(h); submit(h); await settle();
  assert.match(text(h.render()), /scheduleCreation.accepted/); assert.match(text(h.render()), /scheduleCreation.readFailed/);
  h.read(async () => ({ task: { ...h.response(h.calls[0].body!), task_id: other } })); (button(h.render(), "refresh").onClick as () => void)(); await settle();
  assert.match(text(h.render()), /scheduleCreation.readFailed/);
  h.read(async () => ({ task: { ...h.response(h.calls[0].body!), status: "completed" } })); (button(h.render(), "refresh").onClick as () => void)(); await settle();
  assert.match(text(h.render()), /"status":"completed"/); assert.equal(h.calls.filter(call => call.method === "POST").length, 1);
});

for (const phase of ["POST", "GET"]) test(`unmount and return fences late ${phase} and never replays prior input`, async () => {
  const h = host(), pending = deferred<unknown>();
  if (phase === "POST") h.write(() => pending.promise); else h.read(() => pending.promise);
  fill(h); submit(h); await settle(); const old = h.calls[0].body!;
  h.unmount(); h.render(); pending.resolve({ task: h.response(old) }); await settle();
  assert.equal(field(h, "title").value, ""); assert.doesNotMatch(text(h.render()), /scheduleCreation.accepted|scheduleCreation.observed/);
  assert.equal(h.calls.length, phase === "POST" ? 1 : 2);
});

for (const change of ["token", "authority"]) test(`${change} transition fences a pending creation response`, async () => {
  const h = host(), pending = deferred<unknown>(); h.write(() => pending.promise); fill(h); submit(h);
  if (change === "token") h.auth.token = "synthetic-b"; else h.auth.user = { ...h.auth.user, tier: "changed" };
  h.render(); pending.resolve({ task: h.response(h.calls[0].body!) }); await settle();
  assert.equal(h.calls.length, 1); assert.equal(field(h, "title").value, ""); assert.doesNotMatch(text(h.render()), /scheduleCreation.accepted/);
});

test("history navigation keeps the attempted form locked, and a verified receipt permits only a fresh blank form", async () => {
  const h = host(); fill(h); submit(h); await settle();
  h.navigate(`tab=scheduled&task=${other}`); h.render(); submit(h); assert.equal(h.calls.filter(call => call.method === "POST").length, 1);
  (button(h.render(), "another").onClick as () => void)(); h.render();
  assert.equal(field(h, "title").value, ""); assert.equal(field(h, "message").value, ""); assert.equal(h.calls.filter(call => call.method === "POST").length, 1);
});

test("invalid local inputs do not consume a submission or call POST", async () => {
  const h = host(); fill(h); change(h, "retries", "4"); submit(h); assert.equal(h.calls.length, 0); assert.match(text(h.render()), /scheduleCreation.invalid/);
  change(h, "retries", "0"); submit(h); await settle(); assert.equal(h.calls.filter(call => call.method === "POST").length, 1);
});
