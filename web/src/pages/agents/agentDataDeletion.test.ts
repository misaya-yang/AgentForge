import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import vm from "node:vm";
import ts from "typescript";
import { sdkAuthorityScope } from "../../lib/sdkAuthorityScope.ts";

type Tree = { type: unknown; key?: string; props: Record<string, unknown> };
type Dialog = { onOk: () => Promise<void>; onCancel: () => void; afterClose?: () => void; content: string };
type Lane = { slots: unknown[]; cleanups: Array<() => void>; cursor: number };
function nodes(value: unknown): Tree[] {
  if (!value || typeof value !== "object") return [];
  if (Array.isArray(value)) return value.flatMap(nodes);
  const node = value as Tree;
  if (!node.props) return Object.values(value).flatMap(nodes);
  return [node, ...Object.values(node.props).flatMap(nodes)];
}
function text(value: unknown): string {
  if (typeof value === "string" || typeof value === "number") return String(value);
  if (Array.isArray(value)) return value.map(text).join(" ");
  if (!value || typeof value !== "object") return "";
  const node = value as Tree;
  if (node.type === "Tabs" && Array.isArray(node.props?.items)) return node.props.items.map(item => text((item as { children?: unknown }).children)).join(" ");
  return node.props ? ["children", "title", "description"].map(key => text(node.props[key])).join(" ") : Object.values(value).map(text).join(" ");
}
function deferred() {
  let resolve!: (value: unknown) => void; let reject!: (reason: unknown) => void;
  const promise = new Promise<unknown>((yes, no) => { resolve = yes; reject = no; });
  return { promise, resolve, reject };
}
function result(changes: Record<string, unknown> = {}) {
  return { deletion_id: "deletion-a", tenant_id: "tenant-a", agent_id: "agent-a", scope: "retention",
    subject_user_id: null, status: "completed", deleted_counts: { traces: 0, sessions: 2, policy: { secret: "hidden-inventory" } },
    error_code: null, attempt_count: 1, completed_at: "2026-10-15T00:00:00Z", ...changes };
}
function host() {
  const lanes = new Map<string, Lane>(); let active: Lane; let effects: Array<() => void> = [];
  let used = new Set<string>(); let uuid = 0; let agentId = "agent-a"; let role = "owner"; let analyticsRole: string | null = null; const queryErrors = new Map<string, unknown>();
  let auth = { token: "account-a", isAuthenticated: true, user: { user_id: "user-a", tenant_id: "tenant-a", permissions: ["agents:read"], roles: ["owner"], tier: "normal" } };
  const subscribers = new Set<(state: typeof auth, previous: typeof auth) => void>();
  const dialogs: Dialog[] = []; const writes: unknown[][] = []; const notices: string[] = [];
  const queries: Array<{ queryKey: unknown[]; queryFn: () => Promise<unknown> }> = [];
  let write: (...args: unknown[]) => Promise<unknown> = async () => result();
  let refresh = async () => ({}); let destroyed = 0; let refreshes = 0;
  const policy = { legal_hold: false, trace_retention_days: 90 };
  const react = {
    useState(initial: unknown) { const lane = active; const i = lane.cursor++; if (!(i in lane.slots)) lane.slots[i] = initial; return [lane.slots[i], (value: unknown) => { lane.slots[i] = typeof value === "function" ? value(lane.slots[i]) : value; }]; },
    useRef(initial: unknown) { const lane = active; const i = lane.cursor++; return lane.slots[i] ?? (lane.slots[i] = { current: initial }); },
    useMemo(make: () => unknown, deps: unknown[]) { const lane = active; const i = lane.cursor++; const old = lane.slots[i] as { value: unknown; deps: unknown[] } | undefined; if (!old || deps.some((value, n) => !Object.is(value, old.deps[n]))) lane.slots[i] = { value: make(), deps }; return (lane.slots[i] as { value: unknown }).value; },
    useLayoutEffect(effect: () => void | (() => void), deps: unknown[]) { const lane = active; const i = lane.cursor++; const old = lane.slots[i] as unknown[] | undefined; if (!old || deps.some((value, n) => !Object.is(value, old[n]))) { lane.slots[i] = deps; effects.push(() => { lane.cleanups[i]?.(); lane.cleanups[i] = effect() || (() => {}); }); } },
  };
  Object.assign(react, { useEffect: react.useLayoutEffect, useCallback: (fn: unknown, deps: unknown[]) => react.useMemo(() => fn, deps) });
  const jsx = (type: unknown, props: Record<string, unknown>, key?: string) => ({ type, props, key });
  const named = new Proxy({}, { get: (_target, key) => String(key) });
  const t = (key: string, values: Record<string, unknown> = {}) => key + Object.entries(values).map(([k, v]) => ` ${k}=${v}`).join("");
  const api = {
    requestAgentDataDeletion: (...args: unknown[]) => { writes.push(args); return write(...args); },
    agentErrorDetail: (error: unknown) => ({ message: "read failed", status: (error as { status?: number })?.status }),
    getAgent: async () => ({ name: "Agent", caller_role: role }),
    getAgentAnalytics: async () => ({ metrics: {}, traces: [], caller_role: role }),
    listAgentVersions: async () => [], listAgentPublications: async () => [],
    listAgentAuditEvents: async () => ({ events: [], total: 0 }), getAgentGovernance: async () => policy,
    updateAgentGovernance: async () => policy, invalidateAgentCache: async () => ({ deleted_cache_rows: 0 }), revokeAllAgentCredentials: async () => ({}),
  };
  const modules: Record<string, unknown> = {
    "@/components/DeferredTable": { DeferredTable: "Table" },
    react, "react/jsx-runtime": { jsx, jsxs: jsx }, "lucide-react": named,
    "react-router-dom": { useParams: () => ({ agentId }), useNavigate: () => () => {}, Link: "Link" },
    "react-i18next": { useTranslation: () => ({ t, i18n: { resolvedLanguage: "en-US" } }) },
    antd: { ...Object.fromEntries(["Alert", "Button", "Empty", "Input", "InputNumber", "Pagination", "Select", "Skeleton", "Switch", "Table", "Tabs", "Tag"].map(n => [n, n])),
      Form: { Item: "FormItem" }, Typography: { Text: "Text", Title: "Title" },
      App: { useApp: () => ({ modal: { confirm: (dialog: Dialog) => { dialogs.push(dialog); return { destroy: () => { destroyed++; dialog.afterClose?.(); } }; } },
        message: { success: (v: string) => notices.push(v), error: (v: string) => notices.push(v), warning: (v: string) => notices.push(v) } }) } },
    "@tanstack/react-query": {
      useQueryClient: () => ({ setQueryData: () => {} }),
      useQuery: (options: { queryKey: unknown[]; queryFn: () => Promise<unknown> }) => {
        queries.push(options); const key = options.queryKey; const kind = key.includes("analytics") ? "analytics" : key.includes("governance") ? "governance" : "other";
        const data = key.includes("governance") ? policy : key.includes("versions") || key.includes("publications") ? [] : key.includes("audits") ? { events: [], total: 0 } : { name: "Agent", caller_role: kind === "analytics" ? analyticsRole ?? role : role, metrics: {}, traces: [] };
        return { data, isLoading: false, isFetching: false, error: queryErrors.get(kind) || null, refetch: async () => { refreshes++; try { return await refresh(); } catch (error) { queryErrors.set(kind, error); throw error; } } };
      },
      useMutation: (options: { mutationFn: (input: unknown) => Promise<unknown>; onSuccess: (value: unknown) => void; onError: (error: unknown) => void }) => {
        const run = async (input: unknown) => { try { const value = await options.mutationFn(input); options.onSuccess(value); return value; } catch (error) { options.onError(error); throw error; } };
        return { mutate: (input: unknown) => { void run(input).catch(() => {}); }, mutateAsync: run, isPending: false };
      },
    },
    "@/store/useAuthStore": { useAuthStore: Object.assign((select: (value: typeof auth) => unknown) => select(auth), { getState: () => auth, subscribe: (fn: (value: typeof auth, previous: typeof auth) => void) => { subscribers.add(fn); return () => subscribers.delete(fn); } }) },
    "@/lib/sdkAuthorityScope": { sdkAuthorityScope }, "@/lib/utils": { generateUUID: () => `opaque-${++uuid}` }, "@/api/agents": api, "./agent-studio.css": {},
  };
  for (const [name, path] of [["@/hooks/useAuthenticatedReadScope", "../../hooks/useAuthenticatedReadScope.ts"], ["./useAgentDataDeletion", "./useAgentDataDeletion.ts"], ["./AgentDataDeletionPanel", "./AgentDataDeletionPanel.tsx"], ["page", "./AgentAnalyticsPage.tsx"]]) {
    const exported = {};
    vm.runInNewContext(ts.transpileModule(readFileSync(new URL(path, import.meta.url), "utf8"), { compilerOptions: { target: ts.ScriptTarget.ES2022, module: ts.ModuleKind.CommonJS, jsx: ts.JsxEmit.ReactJSX } }).outputText, {
      exports: exported, require: (id: string) => { assert.ok(id in modules, id); return modules[id]; }, crypto: { randomUUID: () => `opaque-${++uuid}` },
    }); modules[name] = exported;
  }
  function expand(value: unknown, parent = "root"): unknown {
    if (Array.isArray(value)) return value.map((v, i) => expand(v, `${parent}:${i}`));
    if (!value || typeof value !== "object") return value;
    const node = value as Tree;
    if (!node.props) return Object.fromEntries(Object.entries(value).map(([k, v]) => [k, expand(v, `${parent}:${k}`)]));
    if (typeof node.type === "function") {
      const key = `${parent}:${node.type.name}:${node.key || ""}`; used.add(key);
      if (!lanes.has(key)) lanes.set(key, { slots: [], cleanups: [], cursor: 0 });
      active = lanes.get(key)!; active.cursor = 0;
      return expand(node.type(node.props), key);
    }
    return { ...node, props: Object.fromEntries(Object.entries(node.props).map(([k, v]) => [k, typeof v === "function" ? v : expand(v, `${parent}:${k}`)])) };
  }
  const render = () => {
    effects = []; used = new Set(); queries.length = 0;
    const output = expand(jsx((modules.page as { AgentAnalyticsPage: unknown }).AgentAnalyticsPage, {}));
    for (const [key, lane] of lanes) if (!used.has(key)) { lane.cleanups.forEach(fn => fn()); lanes.delete(key); }
    effects.forEach(fn => fn()); return output;
  };
  return { dialogs, writes, notices, policy, queries,
    render() { render(); return render(); },
    button(key: string) { const found = nodes(this.render()).find(n => n.type === "Button" && text(n).includes(key)); assert.ok(found, key); return found; },
    click(key: string) { const button = this.button(key); assert.notEqual(button.props.disabled, true); (button.props.onClick as () => void)(); },
    setWrite(fn: typeof write) { write = fn; }, setRefresh(fn: typeof refresh) { refresh = fn; },
    setAnalyticsRole(value: string) { analyticsRole = value; }, setQueryError(kind: string, error: unknown) { queryErrors.set(kind, error); },
    changeAgent(id: string) { agentId = id; }, changeRole(value: string) { role = value; },
    changeAuth(value: string) { const previous = auth; auth = { ...auth, token: value }; subscribers.forEach(fn => fn(auth, previous)); },
    unmount() { lanes.forEach(lane => lane.cleanups.forEach(fn => fn())); lanes.clear(); },
    get destroyed() { return destroyed; }, get refreshes() { return refreshes; },
  };
}
const deleteButton = "agents.analytics.deleteData";
const retryButton = "agents.analytics.deletionRecovery.retryAction";

test("uncertain deletion retains its request identity for explicit continuation", async () => {
  const h = host(); h.setWrite(async () => { throw new Error("private transport detail"); });
  h.click(deleteButton); await h.dialogs[0].onOk().catch(() => {});
  const shown = text(h.render()); assert.match(shown, /deletionRecovery.unknown/); assert.doesNotMatch(shown, /private transport detail/);
  h.click(retryButton); h.setWrite(async () => result()); await h.dialogs[1].onOk();
  assert.equal(h.writes.length, 2); assert.deepEqual(h.writes[0], h.writes[1]);
});
test("accepted deletion receipt remains visible after analytics refresh failure", async () => {
  const h = host(); h.setRefresh(async () => { throw new Error("read failed"); });
  h.click(deleteButton); await h.dialogs[0].onOk();
  const shown = text(h.render()); assert.match(shown, /deletion-a/); assert.match(shown, /deletionRecovery.refreshFailed/);
  assert.match(shown, /traces\s*:\s*0\b/); assert.doesNotMatch(shown, /hidden-inventory/);
});
test("explicit reconfirmation reuses the original deletion request key", async () => {
  const h = host(); h.setWrite(async () => { throw new Error("lost response"); });
  h.click(deleteButton); await h.dialogs[0].onOk().catch(() => {});
  const hasRetry = nodes(h.render()).some(n => n.type === "Button" && text(n).includes(retryButton));
  h.click(hasRetry ? retryButton : deleteButton); await h.dialogs[1].onOk().catch(() => {});
  assert.equal(h.writes.length, 2);
  assert.deepEqual(h.writes[1], h.writes[0], "continuation must reuse the original request, not broaden its cleanup inventory");
});
test("confirmation and submission are single-flight even before rerender", async () => {
  const h = host(); const pending = deferred(); h.setWrite(() => pending.promise);
  const click = h.button(deleteButton).props.onClick as () => void;
  click(); click(); assert.equal(h.dialogs.length, 1);
  const first = h.dialogs[0].onOk(); await h.dialogs[0].onOk(); h.dialogs[0].onCancel();
  assert.equal(h.writes.length, 1); assert.equal(h.dialogs.length, 1);
  pending.resolve(result()); await first; assert.match(text(h.render()), /deletion-a/);
});
test("confirmation freezes scope and trimmed subject and describes destructive continuation", async () => {
  const h = host(); const tree = h.render();
  const scope = nodes(tree).find(n => n.props["aria-label"] === "agents.analytics.deletionScope")!;
  (scope.props.onChange as (value: string) => void)("user");
  const subject = nodes(h.render()).find(n => n.props["aria-label"] === "agents.analytics.subjectUser")!;
  (subject.props.onChange as (event: { target: { value: string } }) => void)({ target: { value: " user-one " } });
  h.setWrite(async () => { throw new Error("lost"); }); h.click(deleteButton);
  (scope.props.onChange as (value: string) => void)("tenant"); h.render();
  await h.dialogs[0].onOk();
  assert.equal((h.writes[0][1] as { scope: string }).scope, "user");
  assert.equal((h.writes[0][1] as { subject_user_id: string }).subject_user_id, "user-one");
  h.click(retryButton); assert.match(h.dialogs[1].content, /retryDescription/); assert.match(h.dialogs[1].content, /user-one/);
  await h.dialogs[1].onOk(); assert.deepEqual(h.writes[1], h.writes[0]);
});
for (const change of ["auth", "aba", "agent", "role", "unmount"]) {
  for (const submitted of [false, true]) {
    test(`${change} retires ${submitted ? "submitted" : "unsubmitted"} deletion ownership`, async () => {
      const h = host(); const pending = deferred(); h.setWrite(() => pending.promise); h.click(deleteButton);
      const completion = submitted ? h.dialogs[0].onOk() : undefined;
      if (change === "auth" || change === "aba") { h.changeAuth("account-b"); if (change === "aba") h.changeAuth("account-a"); }
      if (change === "agent") h.changeAgent("agent-b");
      if (change === "role") h.changeRole("viewer");
      if (change === "unmount") h.unmount(); else h.render();
      if (!submitted) await h.dialogs[0].onOk();
      pending.resolve(result()); await completion;
      assert.equal(h.writes.length, submitted ? 1 : 0); assert.equal(h.refreshes, 0);
      if (change !== "unmount") assert.doesNotMatch(text(h.render()), /deletion-a/);
    });
  }
}
test("retained confirmation refuses synchronous auth replacement before rerender", async () => {
  const h = host(); h.click(deleteButton); h.changeAuth("account-b"); await h.dialogs[0].onOk();
  assert.equal(h.writes.length, 0);
});
test("legal hold before dispatch blocks the retained confirmation", async () => {
  const h = host(); h.click(deleteButton); h.policy.legal_hold = true; h.render(); await h.dialogs[0].onOk();
  assert.equal(h.writes.length, 0);
});
test("cancelled confirmation cannot dispatch later and releases the next confirmation", async () => {
  const h = host(); h.click(deleteButton); h.dialogs[0].onCancel(); await h.dialogs[0].onOk();
  assert.equal(h.writes.length, 0); h.click(deleteButton); assert.equal(h.dialogs.length, 2);
});
for (const status of ["pending", "failed", "blocked", "completed"]) {
  test(`receipt preserves ${status} and only permits appropriate explicit continuation`, async () => {
    const h = host(); h.setWrite(async () => result({ status, completed_at: status === "failed" || status === "pending" ? null : "2026-10-15" }));
    h.click(deleteButton); await h.dialogs[0].onOk(); const shown = text(h.render());
    assert.match(shown, new RegExp(`deletion\\.${status}`));
    const continuation = nodes(h.render()).some(n => n.type === "Button" && text(n).includes(retryButton));
    assert.equal(continuation, status === "pending" || status === "failed"); assert.equal(h.writes.length, 1);
  });
}
test("terminal failed receipt never advertises retry, and new operation is explicit", async () => {
  const h = host(); h.setWrite(async () => result({ status: "failed" })); h.click(deleteButton); await h.dialogs[0].onOk();
  assert.ok(!nodes(h.render()).some(n => n.type === "Button" && text(n).includes(retryButton)));
  h.click("agents.analytics.deletionRecovery.newOperation"); assert.equal(h.writes.length, 1);
  h.click(deleteButton); await h.dialogs[1].onOk(); assert.notEqual((h.writes[0][1] as { idempotency_key: string }).idempotency_key, (h.writes[1][1] as { idempotency_key: string }).idempotency_key);
});
for (const wrong of [{ agent_id: "other" }, { scope: "user", subject_user_id: "other" }, { status: "invented" }, { deletion_id: "" }]) {
  test(`malformed receipt ${JSON.stringify(wrong)} retains uncertainty without readback`, async () => {
    const h = host(); h.setWrite(async () => result(wrong)); h.click(deleteButton); await h.dialogs[0].onOk();
    assert.match(text(h.render()), /deletionRecovery.unknown/); assert.equal(h.refreshes, 0);
  });
}
test("changed receipt identity on continuation cannot replace the retained receipt", async () => {
  const h = host(); h.setWrite(async () => result({ status: "failed", completed_at: null })); h.click(deleteButton); await h.dialogs[0].onOk();
  h.setWrite(async () => result({ deletion_id: "wrong-deletion" })); h.click(retryButton); await h.dialogs[1].onOk();
  const shown = text(h.render()); assert.match(shown, /deletionRecovery.unknown/); assert.match(shown, /deletion-a/); assert.doesNotMatch(shown, /wrong-deletion/);
});
test("read cache identity changes with authentication and old reads refuse replacement identity", async () => {
  const h = host(); h.render(); const previous = [...h.queries];
  h.changeAuth("account-b"); h.render();
  assert.notDeepEqual(h.queries.map(q => q.queryKey), previous.map(q => q.queryKey));
  await assert.rejects(previous[0].queryFn(), /Retired analytics view/);
});

for (const status of ["pending", "failed", "completed"]) {
  test(`background analytics 404 retains ${status} receipt and original continuation`, async () => {
    const h = host(); h.setWrite(async () => result({ status, completed_at: status === "completed" ? "done" : null }));
    h.setRefresh(async () => { throw { status: 404 }; });
    h.click(deleteButton); await h.dialogs[0].onOk();
    assert.match(text(h.render()), /deletion-a/); assert.match(text(h.render()), /analytics.loadError/);
    h.setQueryError("governance", new Error("policy refresh failed"));
    assert.match(text(h.render()), /deletion-a/); assert.match(text(h.render()), /analytics.policyError/);
    if (status !== "completed") { h.click(retryButton); await h.dialogs[1].onOk(); assert.deepEqual(h.writes[1], h.writes[0]); }
  });
}
for (const submitted of [false, true]) {
  test(`analytics viewer overrides cached agent owner for ${submitted ? "submitted" : "unsubmitted"} deletion`, async () => {
    const h = host(); const pending = deferred(); h.setWrite(() => pending.promise);
    h.click(deleteButton); const completion = submitted ? h.dialogs[0].onOk() : undefined;
    h.setAnalyticsRole("viewer"); h.render();
    if (completion) { pending.resolve(result()); await completion; }
    else await h.dialogs[0].onOk();
    assert.equal(h.writes.length, submitted ? 1 : 0); assert.equal(h.refreshes, 0);
    assert.doesNotMatch(text(h.render()), /deletion-a/);
  });
}
test("analytics access denial fences a retained confirmation with cached owner data", async () => {
  const h = host(); h.click(deleteButton); h.setQueryError("analytics", { status: 403 }); h.render();
  await h.dialogs[0].onOk(); assert.equal(h.writes.length, 0);
});
