import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import vm from "node:vm";
import ts from "typescript";
import { QueryClient, QueryObserver } from "@tanstack/react-query";

// Production page, catalog hook/API mapper, status panel and composer. React
// scheduling, session/stream hooks and HTTP are controlled; no browser/provider.
type Tree = { type: unknown; props: Record<string, unknown> };
type Service = { service_id: string; service_name: string; enabled: boolean; service_type?: string; metadata?: Record<string, unknown> };
const service = (id = "available", enabled = true): Service => ({ service_id: id, service_name: id, enabled });
const noop = () => {};
const settle = () => new Promise<void>(resolve => setImmediate(resolve));
function deferred<T>() { let resolve!: (value: T) => void; let reject!: (reason: Error) => void;
  const promise = new Promise<T>((yes, no) => { resolve = yes; reject = no; }); return { promise, resolve, reject }; }
function all(value: unknown, predicate: (node: Tree) => boolean): Tree[] {
  if (!value || typeof value !== "object") return [];
  if (Array.isArray(value)) return value.flatMap(child => all(child, predicate));
  const node = value as Tree;
  return [...(predicate(node) ? [node] : []), ...all(node.props?.children, predicate)];
}
function host(selected: string | undefined = "available") {
  let live: Service[] = [service()]; let read = async () => ({ services: live });
  let requests = 0; let sends = 0; let stops = 0; let creates = 0; let streaming = false; let canManage = false;
  let sending = async () => {};
  const sent: unknown[] = [];
  const custom: unknown[] = [];
  const auth = { token: "fixture-token", user: { user_id: "owner", roles: [], permissions: [] as string[], tier: "free" }, hasPermission: (permission: string) => { assert.equal(permission, "console:services:view"); return canManage; } };
  const frames = new Map<string, unknown[]>(); let slots: unknown[]; let cursor = 0;
  let shortcuts: { enabled: boolean; onNewChat?: () => void; onStop?: () => void };
  const store = { selectedServiceId: selected, activeSessionId: "session", playgroundSidebarOpen: true, localTitles: {},
    setSelectedServiceId(id: string) { store.selectedServiceId = id; }, setActiveSessionId: noop, setPlaygroundSidebarOpen: noop, setLocalTitles: noop };
  const messages = [{ role: "user", content: "Prior question" }, { role: "assistant", content: "Prior answer", status: "completed" }];
  const session = { messages, setMessages: noop, sessions: [], sessionEnabled: true, setSessionEnabled: noop,
    handleNewSession: () => { creates++; }, handleStopStreaming: () => { stops++; streaming = false; },
    currentRequestIdRef: { current: 0 }, abortControllerRef: { current: null }, messagesRef: { current: messages },
  };
  const client = new QueryClient({ defaultOptions: { queries: { retry: false, gcTime: Infinity } } });
  let observer: QueryObserver; let unsubscribe: (() => void) | undefined;
  const react = {
    useState(initial: unknown) { const own = slots; const index = cursor++; if (!(index in own)) own[index] = typeof initial === "function" ? initial() : initial;
      return [own[index], (next: unknown) => { own[index] = typeof next === "function" ? next(own[index]) : next; }]; },
    useMemo: (fn: () => unknown, deps: unknown[]) => { const index = cursor++; const prior = slots[index] as { deps: unknown[]; value: unknown } | undefined;
      if (!prior || deps.some((value, i) => !Object.is(value, prior.deps[i]))) slots[index] = { deps, value: fn() };
      return (slots[index] as { value: unknown }).value; }, useCallback: (fn: unknown) => fn, useEffect: noop, useLayoutEffect: noop,
    useRef(initial: unknown) { const index = cursor++; return slots[index] ?? (slots[index] = { current: initial }); },
  };
  const jsx = (type: unknown, props: Record<string, unknown>) => ({ type, props });
  const named = new Proxy({}, { get: (_target, key) => String(key) });
  const modules: Record<string, unknown> = {
    react, "react/jsx-runtime": { jsx, jsxs: jsx }, "react-router-dom": { Link: "Link" }, "lucide-react": named,
    "react-i18next": { useTranslation: () => ({ t: (key: string) => key, i18n: { language: "en-US" } }) },
    "@/lib/utils": { cn: (...values: unknown[]) => values.filter(Boolean).join(" "), getPlaygroundErrorMessage: () => "Safe error" },
    "@/hooks/use-toast": { toast: { error: noop, success: noop } }, "@/components/ChatWindow": { ChatWindow: "ChatWindow" },
    "@/store/useAppStore": { useAppStore: Object.assign(() => store, { getState: () => store }) },
    "@/store/useAuthStore": { useAuthStore: Object.assign((select: (state: unknown) => unknown) => select(auth), { getState: () => auth }) },
    "@/features/chat/shortcuts": { useChatShortcuts: (options: typeof shortcuts) => { shortcuts = options; } },
    "@/utils/intl": { formatDateTime: () => "Time" }, "@/hooks/usePasteImage": { usePasteImage: () => noop },
    "@/api/files": { uploadFileWithProgress: () => { throw new Error("Unexpected upload"); } },
    "@/api/architecture": {}, "@/lib/api": { api: { get: async (url: string) => { assert.equal(url, "/api/v1/proxy"); requests++; return { data: await read() }; } } },
    "@tanstack/react-query": { useQuery: (options: ConstructorParameters<typeof QueryObserver>[1]) => {
      if (!observer) { observer = new QueryObserver(client, options); unsubscribe = observer.subscribe(noop); }
      else observer.setOptions(options);
      return observer.getCurrentResult();
    } },
    "./hooks/usePlaygroundSessions": { usePlaygroundSessions: () => ({ ...session, loading: streaming }) },
    "./hooks/usePlaygroundStream": { usePlaygroundStream: () => ({ handleSend: async (input: unknown, fields: unknown) => { sends++; sent.push(input); custom.push(fields); await sending(); }, uiStreamingActive: streaming, resolvedToolCallsMode: "full" }) },
    "./hooks/useScrollToBottom": { useScrollToBottom: () => ({ scrollRef: { current: null }, stickToBottomRef: { current: false } }) },
  };
  function load(path: string) {
    const exports: Record<string, (props: Record<string, unknown>) => Tree> = {};
    const source = readFileSync(new URL(path, import.meta.url), "utf8").replaceAll("import.meta.env.DEV", "false");
    vm.runInNewContext(ts.transpileModule(source, { compilerOptions: { jsx: ts.JsxEmit.ReactJSX, module: ts.ModuleKind.CommonJS, target: ts.ScriptTarget.ES2022 } }).outputText,
      { exports, require: (name: string) => { if (name.startsWith("@/components/ui/")) return named; assert.ok(name in modules, `Unexpected import ${name}`); return modules[name]; },
        window: { localStorage: { getItem: () => null } }, Error, Date });
    return exports;
  }
  modules["@/lib/sdkAuthorityScope"] = load("../../lib/sdkAuthorityScope.ts");
  modules["./hooks/useDifyInputs"] = load("./hooks/useDifyInputs.ts");
  modules["@/api/gateway"] = load("../../api/gateway.ts");
  modules["@/hooks/useServices"] = load("../../hooks/useServices.ts");
  modules["@/components/MultimodalInput"] = load("../../components/MultimodalInput.tsx");
  modules["./PlaygroundServiceStatus"] = load("./PlaygroundServiceStatus.tsx");
  const page = load("./index.tsx").PlaygroundPage;
  function renderNode(node: unknown, path: string): unknown {
    if (!node || typeof node !== "object") return node;
    if (Array.isArray(node)) return node.map((child, index) => renderNode(child, `${path}:${index}`));
    const value = node as Tree;
    if (typeof value.type === "function") { const key = `${path}:${value.type.name}`; slots = frames.get(key) ?? []; frames.set(key, slots); cursor = 0; return renderNode(value.type(value.props), key); }
    return { ...value, props: { ...value.props, children: renderNode(value.props?.children, `${path}:children`) } };
  }
  const render = () => renderNode({ type: page, props: {} }, "root");
  const nodes = (predicate: (node: Tree) => boolean) => all(render(), predicate);
  const node = (type: string) => { const found = nodes(value => value.type === type)[0]; assert.ok(found, type); return found; };
  const button = (label: string) => { const found = nodes(value => value.type === "Button" && value.props["aria-label"] === label)[0]; assert.ok(found, label); return found; };
  return { store, session, render, node, button, sent, custom, auth, nodes, setSending(value: typeof sending) { sending = value; },
    get requests() { return requests; }, get sends() { return sends; }, get creates() { return creates; }, get stops() { return stops; }, get shortcuts() { render(); return shortcuts; },
    text: () => JSON.stringify(render()), setManage(value: boolean) { canManage = value; }, setStreaming(value: boolean) { streaming = value; },
    setRead(value: typeof read) { read = value; }, setLive(value: Service[]) { live = value; },
    type(value: string) { (nodes(value => value.type === "Textarea" && value.props.id !== "dify-custom-inputs")[0].props.onChange as (event: unknown) => void)({ target: { value } }); },
    choose(id: string) { (node("Select").props.onValueChange as (value: string) => void)(id); },
    async start() { render(); await settle(); render(); },
    refresh() { const value = nodes(item => item.type === "Button" && ["playground.serviceEntry.refresh", "playground.serviceEntry.refreshing"].includes(String(item.props.children)))[0]; assert.ok(value); assert.equal(value.props.disabled, false); (value.props.onClick as () => void)(); },
    refetch: () => observer.refetch(),
    dispose() { unsubscribe?.(); client.clear(); },
  };
}

test("catalog loading, failed read, explicit retry and selection reach one explicit send", async () => {
  const h = host(""); try {
    const first = deferred<{ services: Service[] }>(); h.setRead(() => first.promise);
    assert.match(h.text(), /serviceEntry.loadingTitle/); assert.equal(h.node("Textarea").props.disabled, true);
    first.reject(new Error("raw-error-with-fixture-secret")); await settle();
    assert.match(h.text(), /serviceEntry.errorTitle/); assert.doesNotMatch(h.text(), /raw-error-with-fixture-secret|serviceEntry.emptyTitle/);
    const second = deferred<{ services: Service[] }>(); h.setRead(() => second.promise); h.refresh();
    assert.equal(h.sends, 0); assert.equal(h.creates, 0);
    second.resolve({ services: [service()] }); await settle(); assert.match(h.text(), /serviceEntry.unselectedTitle/);
    h.choose("available"); h.type("Diagnostic question");
    assert.equal(h.node("Textarea").props.disabled, false); assert.equal(h.sends, 0);
    await (h.button("common.send").props.onClick as () => Promise<void>)();
    assert.equal(h.sends, 1); assert.equal(JSON.stringify(h.sent), '[[{"type":"text","data":"Diagnostic question"}]]');
    assert.equal(h.requests, 2);
  } finally { h.dispose(); }
});

for (const admin of [false, true]) test(`successful empty/disabled catalog has ${admin ? "Services link" : "contact-admin guidance"}`, async () => {
  const h = host(); try { h.setManage(admin); h.setLive([service("disabled", false)]); await h.start();
    assert.match(h.text(), /serviceEntry.emptyTitle/); assert.doesNotMatch(h.text(), /serviceEntry.errorTitle/);
    assert.equal(h.node("Select").props.disabled, true); assert.equal(h.node("Textarea").props.disabled, true);
    assert.equal(h.text().includes('"to":"/services"'), admin); assert.equal(h.text().includes("serviceEntry.contactAdmin"), !admin);
    assert.equal(h.sends, 0); assert.equal(h.creates, 0);
  } finally { h.dispose(); }
});

test("unavailable selection preserves draft and history until explicit service choice", async () => {
  const h = host(); try { await h.start(); h.type("Keep my draft"); h.setLive([service("another")]); await h.refetch();
    assert.match(h.text(), /serviceEntry.unavailableTitle/); assert.equal(h.store.selectedServiceId, "available");
    assert.equal(h.node("Textarea").props.value, "Keep my draft"); assert.equal(h.node("Textarea").props.disabled, true);
    assert.equal(h.node("ChatWindow").props.messages, h.session.messages); assert.equal(h.node("ChatWindow").props.onRegenerate, undefined);
    assert.equal(h.shortcuts.onNewChat, undefined);
    h.choose("another"); assert.equal(h.node("Textarea").props.value, "Keep my draft"); assert.equal(h.node("Textarea").props.disabled, false);
    assert.equal(h.sends, 0); assert.equal(h.creates, 0);
  } finally { h.dispose(); }
});

test("failed background refresh retains draft, gates new work and preserves button and shortcut Stop", async () => {
  const h = host(); try { await h.start(); h.type("Unsent draft"); h.setStreaming(true);
    h.setRead(async () => { throw new Error("sensitive-transport-detail"); }); await h.refetch();
    assert.match(h.text(), /serviceEntry.errorTitle/); assert.doesNotMatch(h.text(), /sensitive-transport-detail/);
    assert.equal(h.node("Textarea").props.value, "Unsent draft"); assert.equal(h.shortcuts.enabled, true); assert.equal(h.shortcuts.onNewChat, undefined);
    const stop = h.button("assistant.stopGenerating"); assert.notEqual(stop.props.disabled, true); (stop.props.onClick as () => void)(); assert.equal(h.stops, 1);
    h.setStreaming(true); h.shortcuts.onStop?.(); assert.equal(h.stops, 2);
    h.setRead(async () => ({ services: [service()] })); h.refresh(); await settle();
    assert.equal(h.node("Textarea").props.value, "Unsent draft"); assert.equal(h.node("Textarea").props.disabled, false);
    assert.equal(h.sends, 0); assert.equal(h.creates, 0);
  } finally { h.dispose(); }
});

test("in-progress successful refresh keeps the last observed enabled target usable", async () => {
  const h = host(); try { await h.start(); h.type("Existing draft"); const pending = deferred<{ services: Service[] }>(); h.setRead(() => pending.promise);
    const reading = h.refetch(); assert.equal(h.node("Textarea").props.disabled, false); assert.equal(h.sends, 0);
    pending.resolve({ services: [] }); await reading; assert.match(h.text(), /serviceEntry.emptyTitle/); assert.equal(h.node("Textarea").props.value, "Existing draft");
  } finally { h.dispose(); }
});

test("all service-entry states and actions resolve in both production locale roots", () => {
  for (const locale of ["en-US", "zh-CN"]) {
    const data = JSON.parse(readFileSync(new URL(`../../i18n/locales/${locale}.json`, import.meta.url), "utf8"));
    for (const state of ["loading", "error", "empty", "unavailable", "unselected"]) {
      for (const field of ["Title", "Description"]) assert.equal(typeof data.playground.serviceEntry[`${state}${field}`], "string");
    }
    for (const key of ["manage", "contactAdmin", "refresh", "refreshing"]) assert.ok(data.playground.serviceEntry[key]);
    assert.equal(data.help.sections.playground.serviceEntry, undefined);
  }
});

const difyService = (): Service => ({ ...service(), service_type: "conversational", metadata: { adapter_type: "dify_chat" } });
const customEditor = (h: ReturnType<typeof host>) => { const node = h.nodes(node => node.props.id === "dify-custom-inputs")[0]; assert.ok(node); return node; };
const changeCustom = (h: ReturnType<typeof host>, value: string) => (customEditor(h).props.onChange as (event: unknown) => void)({ target: { value } });
test("production Dify page validates before Send and preserves the prompt until valid submission", async () => {
  const h = host(); try { h.setLive([difyService()]); await h.start(); h.type("Question");
    changeCustom(h, '{"private":"unfinished');
    assert.equal(customEditor(h).props["aria-invalid"], true); assert.equal(h.button("common.send").props.disabled, true);
    (h.node("ChatWindow").props.onRegenerate as () => void)(); assert.equal(h.sends, 0); assert.equal(h.creates, 0);
    assert.match(h.text(), /playground.difyInputs.invalid/);
    changeCustom(h, '{"topic":"value","count":2,"enabled":false}');
    assert.equal(h.button("common.send").props.disabled, false);
    await (h.button("common.send").props.onClick as () => Promise<void>)();
    assert.equal(h.sends, 1); assert.equal(JSON.stringify(h.sent[0]), '[{"type":"text","data":"Question"}]');
    assert.equal(JSON.stringify(h.custom[0]), '{"topic":"value","count":2,"enabled":false}');
  } finally { h.dispose(); }
});
test("Dify editor locks during a run; Stop remains available and clear restores omitted inputs", async () => {
  const h = host(); try { h.setLive([difyService()]); await h.start(); changeCustom(h, '{"topic":"keep"}'); h.setStreaming(true);
    assert.equal(customEditor(h).props.disabled, true); changeCustom(h, '{"topic":"late"}');
    assert.equal(customEditor(h).props.value, '{"topic":"keep"}');
    (h.button("assistant.stopGenerating").props.onClick as () => void)(); assert.equal(h.stops, 1);
    const clear = h.nodes(node => node.type === "Button" && node.props.children === "playground.difyInputs.clear")[0];
    (clear.props.onClick as () => void)(); assert.equal(customEditor(h).props.value, "");
    h.type("Question"); await (h.button("common.send").props.onClick as () => Promise<void>)(); assert.equal(h.custom[0], undefined);
  } finally { h.dispose(); }
});
for (const change of ["service", "session", "token", "permissions", "memory"]) test(`Dify page retires the custom draft on ${change} change`, async () => {
  const h = host(); try { h.setLive([difyService(), { ...difyService(), service_id: "other", service_name: "other" }]); await h.start(); changeCustom(h, '{"private":"old"}');
    const regenerate = h.node("ChatWindow").props.onRegenerate as () => void;
    if (change === "service") h.choose("other"); else if (change === "session") h.store.activeSessionId = "other-session";
    else if (change === "token") h.auth.token = "other-token"; else if (change === "permissions") h.auth.user.permissions = ["changed"];
    else h.session.sessionEnabled = false;
    assert.equal(customEditor(h).props.value, ""); regenerate(); assert.equal(h.sends, 0);
  } finally { h.dispose(); }
});
for (const config of [
  { metadata: { adapter_type: "dify" } },
  { metadata: { adapter_type: "openai_agents" } },
  { metadata: { adapter_type: "dify_chat", proxy_mode: "transparent" } },
  { service_type: "langgraph", metadata: { adapter_type: "dify_chat" } },
]) test(`custom editor is absent for unsupported/contradictory config ${JSON.stringify(config)}`, async () => {
  const h = host(); try { h.setLive([{ ...service(), ...config }]); await h.start();
    assert.equal(h.nodes(node => node.props.id === "dify-custom-inputs").length, 0);
    h.type("Question"); await (h.button("common.send").props.onClick as () => Promise<void>)(); assert.equal(h.custom[0], undefined);
  } finally { h.dispose(); }
});
test("Dify input guidance and validation are translated in both locale roots", () => {
  for (const locale of ["en-US", "zh-CN"]) {
    const data = JSON.parse(readFileSync(new URL(`../../i18n/locales/${locale}.json`, import.meta.url), "utf8"));
    for (const key of ["label", "help", "invalid", "clear"]) assert.equal(typeof data.playground.difyInputs[key], "string");
  }
});
for (const change of ["service", "session", "token", "permissions"]) test(`Dify pre-rerender ${change} mutation prevents captured regenerate callback dispatch`, async () => {
  const h = host(); try { h.setLive([difyService()]); await h.start(); changeCustom(h, '{"private":"old"}');
    const regenerate = h.node("ChatWindow").props.onRegenerate as () => void;
    if (change === "service") h.store.selectedServiceId = "other";
    else if (change === "session") h.store.activeSessionId = "other";
    else if (change === "token") h.auth.token = "other";
    else h.auth.user.permissions = ["other"];
    regenerate(); assert.equal(h.sends, 0, "No rerender may be needed to retire stale dispatch");
  } finally { h.dispose(); }
});
test("Dify same-tick regenerate callbacks dispatch once and release only after settlement", async () => {
  const h = host(); const pending = deferred<void>(); try { h.setLive([difyService()]); await h.start(); changeCustom(h, '{"field":"value"}');
    h.setSending(() => pending.promise); const regenerate = h.node("ChatWindow").props.onRegenerate as () => void;
    regenerate(); regenerate(); assert.equal(h.sends, 1);
    pending.resolve(); await settle(); regenerate(); assert.equal(h.sends, 2);
  } finally { pending.resolve(); h.dispose(); }
});
test("Dify same-tick pending send fences captured editor and clear callbacks", async () => {
  const h = host(); const pending = deferred<void>(); try { h.setLive([difyService()]); await h.start(); changeCustom(h, '{"field":"original"}');
    const edit = customEditor(h).props.onChange as (event: unknown) => void;
    const clear = h.nodes(node => node.type === "Button" && node.props.children === "playground.difyInputs.clear")[0].props.onClick as () => void;
    h.setSending(() => pending.promise); (h.node("ChatWindow").props.onRegenerate as () => void)();
    edit({ target: { value: '{"field":"late"}' } }); clear();
    assert.equal(customEditor(h).props.value, '{"field":"original"}'); assert.equal(h.sends, 1);
  } finally { pending.resolve(); await settle(); h.dispose(); }
});
