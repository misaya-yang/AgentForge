import assert from "node:assert/strict";
import test from "node:test";
import { readFileSync } from "node:fs";
import vm from "node:vm";
import ts from "typescript";

// Execute the complete production component with controlled hook scheduling and
// controlled transport. These offline regressions do not claim browser or API acceptance.
const source = readFileSync(new URL("./ConnectorsPanel.tsx", import.meta.url), "utf8");
type Tree = { type: unknown; props: Record<string, unknown>; key?: unknown };
type Read = (url: string) => Promise<unknown>;
function deferred() {
  let resolve!: (value: unknown) => void;
  let reject!: (reason: Error) => void;
  const promise = new Promise<unknown>((yes, no) => { resolve = yes; reject = no; });
  return { promise, resolve, reject };
}
async function settle() { for (let i = 0; i < 30; i++) await Promise.resolve(); }
function all(tree: unknown, predicate: (node: Tree) => boolean): Tree[] {
  if (!tree || typeof tree !== "object") return [];
  if (Array.isArray(tree)) return tree.flatMap(node => all(node, predicate));
  const node = tree as Tree;
  return [...(predicate(node) ? [node] : []), ...all(node.props?.children, predicate)];
}
function host(personalPage = false, search = "?connected=forged&status=activated") {
  const slots: unknown[] = [], effects: (() => void)[] = [], cleanups: (() => void)[] = [];
  let cursor = 0;
  let read: Read = async () => [];
  let open = true;
  let mutate: Read = async () => ({});
  let confirmed = true;
  const alerts: string[] = [];
  const location = { href: "" };
  const calls: { method: string; url: string }[] = [], counts: number[] = [];
  const onCountChange = (count: number) => counts.push(count);
  const same = (a?: unknown[], b?: unknown[]) => a?.length === b?.length && a?.every((value, i) => Object.is(value, b?.[i]));
  const react = {
    useState(initial: unknown) {
      const index = cursor++;
      if (!(index in slots)) slots[index] = initial;
      return [slots[index], (value: unknown) => { slots[index] = typeof value === "function" ? value(slots[index]) : value; }];
    },
    useRef(initial: unknown) { const index = cursor++; return slots[index] ?? (slots[index] = { current: initial }); },
    useCallback(fn: () => void | Promise<void>, deps: unknown[]) {
      const index = cursor++;
      const old = slots[index] as { deps: unknown[]; fn: () => void | Promise<void> } | undefined;
      if (!old || !same(old.deps, deps)) slots[index] = { deps, fn };
      return (slots[index] as { fn: () => void | Promise<void> }).fn;
    },
    useEffect(fn: () => void | (() => void), deps: unknown[]) {
      const index = cursor++;
      if (!same(slots[index] as unknown[], deps)) {
        slots[index] = deps;
        effects.push(() => { cleanups[index]?.(); cleanups[index] = fn() || (() => {}); });
      }
    },
  };
  const jsx = (type: unknown, props: Record<string, unknown>, key?: unknown) => ({ type, props, key });
  let token = "session-a", pageKey: unknown;
  let scope: { id: string; isCurrent: () => boolean } | undefined;
  const authScope = () => { if (!scope || scope.id !== token) { const current = token; scope = { id: token, isCurrent: () => current === token }; } return scope; };
  const exports = {} as { default: (props: { open: boolean; onClose: () => void; onCountChange: (count: number) => void }) => Tree };
  vm.runInNewContext(ts.transpileModule(source, { compilerOptions: { jsx: ts.JsxEmit.ReactJSX, target: ts.ScriptTarget.ES2022, module: ts.ModuleKind.CommonJS } }).outputText, {
    exports,
    require(name: string) {
      if (name === "react") return react;
      if (name === "@/hooks/useAuthenticatedReadScope") return { useAuthenticatedReadScope: authScope };
      if (name === "react/jsx-runtime") return { jsx, jsxs: jsx };
      if (name === "react-i18next") return { useTranslation: () => ({ t: (key: string) => key }) };
      if (name === "lucide-react") return { Plug: "Plug" };
      if (name === "@/lib/api") return { api: {
        async get(url: string) { calls.push({ method: "GET", url }); return { data: await read(url) }; },
        async post(url: string) { calls.push({ method: "POST", url }); return { data: await mutate(url) }; },
        async delete(url: string) { calls.push({ method: "DELETE", url }); return { data: await mutate(url) }; },
      } };
      throw Error(name);
    },
    alert(message: string) { alerts.push(message); },
    confirm() { return confirmed; },
    window: { location },
  });
  const destinations: unknown[] = [];
  const pageExports = {} as { ConnectionsPage: () => Tree | null };
  if (personalPage) vm.runInNewContext(ts.transpileModule(
    readFileSync(new URL("../../Connections.tsx", import.meta.url), "utf8"),
    { compilerOptions: { jsx: ts.JsxEmit.ReactJSX, module: ts.ModuleKind.CommonJS } },
  ).outputText, { exports: pageExports, require(name: string) {
    if (name === "react/jsx-runtime") return { jsx, jsxs: jsx };
    if (name === "react-router-dom") return { useNavigate: () => (...args: unknown[]) => destinations.push(args), useSearchParams: () => [new URLSearchParams(search)] };
    if (name === "@/hooks/useAuthenticatedReadScope") return { useAuthenticatedReadScope: authScope };
    if (name === "@/pages/assistant/components/ConnectorsPanel") return { __esModule: true, default: exports.default };
    throw Error(name);
  }, window: { location: { search: "?connected=forged&status=activated" } } });
  const render = () => {
    cursor = 0;
    const page = personalPage ? pageExports.ConnectionsPage() : null;
    const wrapper = page
      ? (page.type as typeof exports.default)(page.props as Parameters<typeof exports.default>[0])
      : exports.default({ open, onClose() {}, onCountChange });
    if (pageKey !== wrapper.key) {
      cleanups.forEach(cleanup => cleanup?.());
      slots.length = 0; cleanups.length = 0; effects.length = 0;
      cursor = 0; pageKey = wrapper.key;
    }
    const tree = (wrapper.type as typeof exports.default)(wrapper.props as Parameters<typeof exports.default>[0]);
    effects.splice(0).forEach(effect => effect());
    return tree;
  };
  const nodes = (predicate: (node: Tree) => boolean) => all(render(), predicate);
  const text = () => JSON.stringify(render());
  return {
    calls, counts, destinations, render, nodes, text, alerts, location,
    setMutation(fn: Read) { mutate = fn; },
    setConfirmed(value: boolean) { confirmed = value; },
    setRead(fn: Read) { read = fn; },
    setToken(value: string) { token = value; render(); },
    setOpen(value: boolean) { open = value; render(); },
    cleanup() { cleanups.forEach(cleanup => cleanup?.()); },
    retry() {
      const button = nodes(node => node.type === "button" && node.props.children === "common.retry")[0];
      assert.ok(button, "Explicit catalog retry must be available");
      return button.props.onClick as () => void;
    },
    alert() { return nodes(node => node.props?.role === "alert").length > 0; },
  };
}
const connection = { provider: 'fixture', display_name: 'Fixture connection', enabled: true, connected: true };
test('unavailable initial catalog is not a successful empty catalog or zero-count observation', async () => { const h = host(); h.setRead(async () => { throw Error('controlled 503'); }); h.render(); await settle(); assert.equal(h.alert(), true); assert.ok(!h.text().includes('connectors.empty')); assert.deepEqual(h.counts, []); assert.equal(h.calls.length, 1); h.cleanup(); });
test('successful empty catalog remains a valid empty result', async () => { const h = host(); h.render(); await settle(); assert.equal(h.alert(), false); assert.ok(h.text().includes('connectors.empty')); assert.deepEqual(h.counts, [0]); h.cleanup(); });
test('failed refresh retains last count/config privately, hides stale status and actions, manual retry recovers', async () => { const h = host(); h.setRead(async (url) => url.endsWith('mcp-status') ? { mcp_active: true, tools: [] } : [connection]); h.render(); await settle(); assert.ok(h.text().includes('connectors.activated')); h.setOpen(false); h.setRead(async () => { throw Error('controlled 503'); }); h.setOpen(true); await settle(); assert.equal(h.alert(), true); assert.deepEqual(h.counts, [1]); assert.ok(!h.text().includes('connectors.activated')); assert.ok(!h.text().includes('Fixture connection')); const calls = h.calls.length; await settle(); h.render(); assert.equal(h.calls.length, calls, 'No automatic retry'); h.setRead(async (url) => url.endsWith('mcp-status') ? { mcp_active: true, tools: [] } : [connection]); h.retry()(); await settle(); assert.equal(h.alert(), false); assert.ok(h.text().includes('Fixture connection')); assert.deepEqual(h.counts, [1, 1]); assert.ok(h.calls.every(c => c.method === 'GET')); h.cleanup(); });
test('overlapping manual retries issue one GET', async () => { const h = host(); h.setRead(async () => { throw Error('controlled 503'); }); h.render(); await settle(); const pending = deferred(); h.setRead(() => pending.promise); const retry = h.retry(); retry(); retry(); assert.equal(h.calls.length, 2); pending.resolve([]); await settle(); assert.ok(h.text().includes('connectors.empty')); h.cleanup(); });
test('closed and reopened panel rejects prior read completion', async () => { const h = host(); const old = deferred(), current = deferred(); h.setRead(() => old.promise); h.render(); h.setOpen(false); h.setRead(() => current.promise); h.setOpen(true); current.resolve([]); await settle(); old.resolve([connection]); await settle(); assert.deepEqual(h.counts, [0]); assert.ok(h.text().includes('connectors.empty')); assert.equal(h.calls.length, 2, 'Obsolete read must not start MCP reads'); h.cleanup(); });
test('unmounted panel ignores late error and success callbacks', async () => { for (const rejects of [false, true]) {
    const h = host();
    const pending = deferred();
    h.setRead(() => pending.promise);
    h.render();
    h.cleanup();
    if (rejects)
        pending.reject(Error('controlled failure'));
    else
        pending.resolve([connection]);
    await settle();
    assert.deepEqual(h.counts, []);
    assert.equal(h.calls.length, 1);
} });
test('obsolete MCP failure cannot launch additional status reads', async () => { const h = host(); const status = deferred(); h.setRead(url => url.endsWith('mcp-status') ? status.promise : Promise.resolve([connection, { ...connection, provider: 'second' }])); h.render(); await settle(); assert.equal(h.calls.length, 2); h.cleanup(); status.reject(Error('controlled MCP failure')); await settle(); assert.equal(h.calls.length, 2); });
test('older failed catalog cannot hide successful reopened observation', async () => { const h = host(); const old = deferred(); h.setRead(() => old.promise); h.render(); h.setOpen(false); h.setRead(async () => []); h.setOpen(true); await settle(); old.reject(Error('obsolete controlled failure')); await settle(); assert.deepEqual(h.counts, [0]); assert.equal(h.alert(), false); assert.ok(h.text().includes('connectors.empty')); h.cleanup(); });

test('obsolete failure cannot release current read ownership or replace loading',async()=>{const h=host();const old=deferred(),current=deferred();h.setRead(async()=>{throw Error('initial')});h.render();await settle();const retry=h.retry();h.setRead(()=>old.promise);retry();h.setOpen(false);h.setRead(()=>current.promise);h.setOpen(true);old.reject(Error('stale failure'));await settle();assert.ok(h.text().includes('connectors.loading'));assert.equal(h.alert(),false);retry();assert.equal(h.calls.length,3,'Old finally must not clear new activeRead');current.resolve([]);await settle();assert.deepEqual(h.counts,[0]);h.cleanup()});

function statusRetry(h: ReturnType<typeof host>, label = connection.display_name) {
  const button = h.nodes(node => node.type === "button" && node.props["aria-label"] === `connectors.retryStatus ${label}`)[0];
  assert.ok(button, "Failed provider status needs an explicit GET-only retry");
  return button.props.onClick as () => void;
}
function hasActivation(h: ReturnType<typeof host>) {
  return h.nodes(node => node.type === "button" && node.props.children === "connectors.activate").length > 0;
}
test("failed MCP observation preserves connected catalog fact without inferring inactive tools", async () => {
  const h = host();
  h.setRead(async url => { if (url.endsWith("mcp-status")) throw Error("controlled provider failure"); return [connection]; });
  h.render(); await settle();
  assert.ok(h.text().includes("connectors.mcpReadFailed"));
  assert.ok(!h.text().includes("connectors.connectedNoTools"));
  assert.ok(!h.text().includes("connectors.notConnected"));
  assert.equal(hasActivation(h), false);
  assert.deepEqual(h.counts, [1]);
  const before = h.calls.length; h.render(); await settle();
  assert.equal(h.calls.length, before, "A failed MCP GET must not retry automatically");
  h.cleanup();
});
test("failed current MCP read never displays the last successful active badge", async () => {
  const h = host();
  h.setRead(async url => url.endsWith("mcp-status") ? { mcp_active: true, tools: [{ name: "fixture" }] } : [connection]);
  h.render(); await settle(); assert.ok(h.text().includes("connectors.activated"));
  h.setOpen(false);
  h.setRead(async url => { if (url.endsWith("mcp-status")) throw Error("controlled status unavailable"); return [connection]; });
  h.setOpen(true); await settle();
  assert.ok(h.text().includes("connectors.mcpReadFailed"));
  assert.ok(!h.text().includes("connectors.activated"));
  assert.equal(hasActivation(h), false);
  assert.deepEqual(h.counts, [1, 1]);
  h.cleanup();
});
test("explicit MCP retry is coalesced, GET-only and leaves catalog count intact", async () => {
  const h = host();
  h.setRead(async url => { if (url.endsWith("mcp-status")) throw Error("controlled failure"); return [connection]; });
  h.render(); await settle();
  const pending = deferred(); h.setRead(() => pending.promise);
  const retry = statusRetry(h); retry(); retry();
  assert.equal(h.calls.length, 3);
  assert.ok(h.text().includes("connectors.mcpLoading"));
  assert.equal(hasActivation(h), false);
  pending.resolve({ mcp_active: false, tools: [] }); await settle();
  assert.ok(h.text().includes("connectors.connectedNoTools"));
  assert.equal(hasActivation(h), true, "Only explicit inactive status may offer activation");
  assert.deepEqual(h.counts, [1]);
  assert.deepEqual(h.calls.map(call => call.url), ["/api/v1/connectors/available", "/api/v1/connectors/fixture/mcp-status", "/api/v1/connectors/fixture/mcp-status"]);
  assert.ok(h.calls.every(call => call.method === "GET"));
  h.cleanup();
});
test("per-provider status retry preserves an independently successful observation", async () => {
  const h = host(); const second = { ...connection, provider: "second", display_name: "Second connection" };
  h.setRead(async url => {
    if (url.includes("/fixture/mcp-status")) throw Error("one failed provider");
    if (url.endsWith("mcp-status")) return { mcp_active: true, tools: [] };
    return [connection, second];
  });
  h.render(); await settle();
  assert.ok(h.text().includes("connectors.activated"));
  assert.ok(h.text().includes("connectors.mcpReadFailed"));
  h.setRead(async () => ({ mcp_active: true, tools: [] })); statusRetry(h)(); await settle();
  assert.ok(!h.text().includes("connectors.mcpReadFailed"));
  assert.equal(h.nodes(node => node.type === "span" && node.props.children === "connectors.activated").length, 2);
  assert.equal(h.calls.filter(call => call.url.includes("/second/")).length, 1);
  assert.deepEqual(h.counts, [2]); h.cleanup();
});
test("obsolete MCP retry success cannot replace a reopened current failed observation", async () => {
  const h = host(); const failStatus = async (url: string) => { if (url.endsWith("mcp-status")) throw Error("controlled unavailable"); return [connection]; };
  h.setRead(failStatus); h.render(); await settle();
  const old = deferred(); h.setRead(() => old.promise); statusRetry(h)();
  h.setOpen(false); h.setRead(failStatus); h.setOpen(true); await settle();
  old.resolve({ mcp_active: true, tools: [] }); await settle();
  assert.ok(h.text().includes("connectors.mcpReadFailed"));
  assert.ok(!h.text().includes("connectors.activated"));
  assert.equal(hasActivation(h), false); h.cleanup();
});
test("obsolete MCP retry rejection cannot release a new same-provider retry", async () => {
  const h = host(); const failStatus = async (url: string) => { if (url.endsWith("mcp-status")) throw Error("controlled unavailable"); return [connection]; };
  h.setRead(failStatus); h.render(); await settle();
  const old = deferred(); h.setRead(() => old.promise); statusRetry(h)();
  h.setOpen(false); h.setRead(failStatus); h.setOpen(true); await settle();
  const current = deferred(); h.setRead(() => current.promise); const retry = statusRetry(h); retry();
  old.reject(Error("obsolete failure")); await settle();
  assert.ok(h.text().includes("connectors.mcpLoading"));
  const before = h.calls.length; retry(); assert.equal(h.calls.length, before);
  current.resolve({ mcp_active: true, tools: [] }); await settle();
  assert.ok(h.text().includes("connectors.activated")); h.cleanup();
});

test("nonboolean MCP status is unknown rather than inferred inactive or active", async () => {
  for (const status of [{}, { mcp_active: null }, { mcp_active: "false" }, null]) {
    const h = host();
    h.setRead(async url => url.endsWith("mcp-status") ? status : [connection]);
    h.render(); await settle();
    assert.ok(h.text().includes("connectors.mcpReadFailed"));
    assert.ok(!h.text().includes("connectors.connectedNoTools"));
    assert.ok(!h.text().includes("connectors.activated"));
    assert.equal(hasActivation(h), false);
    assert.deepEqual(h.counts, [1]);
    h.cleanup();
  }
});


test("OAuth personal page verifies current connection instead of trusting query and closes safely", async () => {
  const h = host(true);
  h.setRead(async () => [{ ...connection, connected: false }]);
  h.render(); await settle();
  assert.match(h.text(), /connectors.notConnected/);
  assert.doesNotMatch(h.text(), /connectors.activated/);
  assert.deepEqual(h.calls, [{ method: "GET", url: "/api/v1/connectors/available" }]);
  const close = h.nodes(node => node.type === "button" && node.props["aria-label"] === "connectors.closeAria")[0];
  (close.props.onClick as () => void)();
  assert.equal(JSON.stringify(h.destinations), JSON.stringify([["/", { replace: true }]]));
  h.cleanup();
});

test("OAuth personal page retains real panel catalog failure and read-only recovery", async () => {
  const h = host(true);
  h.setRead(async () => { throw new Error("catalog unavailable"); });
  h.render(); await settle();
  assert.match(h.text(), /connectors.loadFailed/);
  h.setRead(async url => url.endsWith("/available") ? [connection] : { mcp_active: false, tools: [] });
  h.retry()(); await settle();
  assert.match(h.text(), /connectors.connectedNoTools/);
  assert.ok(h.calls.every(call => call.method === "GET"));
  h.cleanup();
});


test("personal connection session replacement retires pending observations for the same user", async () => {
  const h = host(true), old = deferred();
  h.setRead(async () => old.promise);
  h.render();
  h.setRead(async () => []);
  h.setToken("session-b"); await settle();
  assert.match(h.text(), /connectors.empty/);
  old.resolve([connection]); await settle();
  assert.match(h.text(), /connectors.empty/);
  assert.doesNotMatch(h.text(), /Fixture connection/);
  assert.equal(h.calls.length, 2);
  assert.ok(h.calls.every(call => call.method === "GET"));
  h.cleanup();
});


test("personal denial notice is a fixed hint and only reads current connections", async () => {
  const h = host(true, "?status=authorization_failed&error_description=SECRET&connected=forged");
  h.setRead(async () => [connection]); h.render(); await settle();
  assert.ok(h.text().includes("connectors.authorizationReturnFailed"));
  assert.ok(h.text().includes("Fixture connection"));
  assert.ok(!h.text().includes("SECRET"));
  assert.ok(h.calls.every(call => call.method === "GET"));
  h.cleanup();
});
test("unknown and success return query hints cannot create a failure notice", async () => {
  for (const query of ["", "?status=connected", "?status=access_denied", "?error_description=SECRET"]) {
    const h = host(true, query); h.render(); await settle();
    assert.ok(!h.text().includes("connectors.authorizationReturnFailed")); h.cleanup();
  }
});


function action(h: ReturnType<typeof host>, label: string, name = connection.display_name) {
  const button = h.nodes(node => node.type === "button" && node.props["aria-label"] === `${label} ${name}`)[0];
  assert.ok(button, `${label} must be available for ${name}`);
  return button.props.onClick as () => Promise<void>;
}
const expired = { ...connection, connected: false, status: "expired" };
test("expired and locally revoked connections are distinct and only retained expired credentials offer disconnect", async () => {
  for (const status of ["expired", "revoked", null]) {
    const h = host(true); h.setRead(async () => [{ ...expired, status }]); h.render(); await settle();
    assert.ok(h.text().includes(status === "expired" ? "connectors.authorizationExpired" : status === "revoked" ? "connectors.authorizationRevoked" : "connectors.notConnected"));
    action(h, status ? "connectors.reconnect" : "connectors.connect");
    assert.equal(h.nodes(n => n.props?.["aria-label"] === `connectors.disconnect ${connection.display_name}`).length, status === "expired" ? 1 : 0);
    assert.equal(hasActivation(h), false); assert.equal(h.calls.length, 1); h.cleanup();
  }
});
test("expired disconnect confirmation cancellation has no request; success clears saved state through one authoritative read", async () => {
  const h = host(true); h.setRead(async () => [expired]); h.render(); await settle();
  h.setConfirmed(false); await action(h, "connectors.disconnect")(); assert.equal(h.calls.length, 1);
  h.setConfirmed(true); const pending = deferred(); h.setMutation(() => pending.promise);
  const click = action(h, "connectors.disconnect"); const first = click(); void click();
  assert.equal(h.calls.filter(c => c.method === "DELETE").length, 1);
  assert.ok(h.nodes(n => n.type === "button" && n.props["aria-label"] === `connectors.reconnect ${connection.display_name}`)[0].props.disabled);
  h.setRead(async () => [{ ...expired, status: "revoked" }]); pending.resolve({}); await first;
  assert.match(h.text(), /connectors.authorizationRevoked/); assert.equal(h.calls.length, 3); h.cleanup();
});
test("failed disconnect and failed readback never replay DELETE; explicit catalog retry recovers", async () => {
  const h = host(true); h.setRead(async () => [expired]); h.render(); await settle();
  h.setMutation(async () => { throw Error("controlled transport failure"); });
  h.setRead(async () => { throw Error("controlled catalog failure"); });
  await action(h, "connectors.disconnect")();
  assert.match(h.text(), /connectors.loadFailed/); assert.ok(h.alert());
  assert.equal(h.calls.filter(c => c.method === "DELETE").length, 1);
  h.setRead(async () => [{ ...expired, status: "revoked" }]); h.retry()(); await settle();
  assert.match(h.text(), /connectors.authorizationRevoked/);
  assert.equal(h.calls.filter(c => c.method === "DELETE").length, 1); h.cleanup();
});
test("successful disconnect with unavailable readback also retries GET only", async () => {
  const h = host(true); h.setRead(async () => [expired]); h.render(); await settle();
  h.setRead(async () => { throw Error("controlled catalog failure"); });
  await action(h, "connectors.disconnect")(); assert.match(h.text(), /connectors.loadFailed/);
  h.setRead(async () => []); h.retry()(); await settle(); assert.match(h.text(), /connectors.empty/);
  assert.equal(h.calls.filter(c => c.method === "DELETE").length, 1); h.cleanup();
});
test("reconnect is single-flight across providers and failed initiation allows explicit retry", async () => {
  const h = host(true); const second = { ...expired, provider: "second", display_name: "Second" };
  h.setRead(async () => [expired, second]); h.render(); await settle();
  const firstClick = action(h, "connectors.reconnect"), secondClick = action(h, "connectors.reconnect", "Second");
  const pending = deferred(); h.setRead(() => pending.promise);
  const first = firstClick(); void firstClick(); void secondClick(); assert.equal(h.calls.length, 2);
  pending.reject(Error("authorization unavailable")); await first; assert.ok(h.alert());
  h.setRead(async () => ({ auth_url: "https://example.invalid/authorize" }));
  await action(h, "connectors.reconnect")(); assert.equal(h.location.href, "https://example.invalid/authorize"); h.cleanup();
});
test("late reconnect after personal session replacement or panel reopen cannot redirect", async () => {
  for (const replaceSession of [true, false]) {
    const h = host(replaceSession); h.setRead(async () => [expired]); h.render(); await settle();
    const pending = deferred(); h.setRead(() => pending.promise); const operation = action(h, "connectors.reconnect")();
    h.setRead(async () => [expired]);
    if (replaceSession) h.setToken("session-b"); else { h.setOpen(false); h.setOpen(true); }
    await settle(); pending.resolve({ auth_url: "https://example.invalid/obsolete" }); await operation;
    assert.equal(h.location.href, "");
    assert.equal(h.nodes(n => n.props?.["aria-label"] === `connectors.reconnect ${connection.display_name}`)[0].props.disabled, false); h.cleanup();
  }
});
test("late DELETE and activation callbacks after reopen cannot overwrite, alert or refresh current observations", async () => {
  for (const kind of ["disconnect", "activate"]) for (const reject of [false, true]) {
    const h = host(); h.setRead(async url => url.endsWith("mcp-status") ? { mcp_active: false, tools: [] } : [connection]); h.render(); await settle();
    const pending = deferred(); h.setMutation(() => pending.promise); const operation = action(h, `connectors.${kind}`)();
    h.setOpen(false); h.setRead(async () => []); h.setOpen(true); await settle(); const before = h.calls.length;
    if (reject) pending.reject(Error("obsolete error")); else pending.resolve({ tool_count: 1 });
    await operation; assert.match(h.text(), /connectors.empty/); assert.equal(h.calls.length, before); assert.deepEqual(h.alerts, []); assert.equal(h.alert(), false); h.cleanup();
  }
});
test("disconnect retires a pending tool-status retry before authoritative readback", async () => {
  const h = host(); h.setRead(async url => { if (url.endsWith("mcp-status")) throw Error("status unavailable"); return [connection]; }); h.render(); await settle();
  const oldStatus = deferred(); h.setRead(() => oldStatus.promise); statusRetry(h)();
  h.setRead(async () => [{ ...expired, status: "revoked" }]); await action(h, "connectors.disconnect")();
  oldStatus.resolve({ mcp_active: true, tools: [] }); await settle();
  assert.match(h.text(), /connectors.authorizationRevoked/); assert.doesNotMatch(h.text(), /connectors.activated/);
  assert.equal(h.calls.filter(c => c.method === "DELETE").length, 1); h.cleanup();
});
test("activation single-flight reconciles success or failure instead of assuming a token remains connected", async () => {
  for (const reject of [false, true]) {
    const h = host(); h.setRead(async url => url.endsWith("mcp-status") ? { mcp_active: false, tools: [] } : [connection]); h.render(); await settle();
    const pending = deferred(); h.setMutation(() => pending.promise); const click = action(h, "connectors.activate"); const operation = click(); void click();
    h.setRead(async () => [expired]); if (reject) pending.reject(Error("authorization expired")); else pending.resolve({ tool_count: 1 });
    await operation; assert.match(h.text(), /connectors.authorizationExpired/); assert.equal(h.calls.filter(c => c.method === "POST").length, 1); assert.equal(hasActivation(h), false); h.cleanup();
  }
});

test("shared Assistant-style entrypoint retires pending mutations on authentication replacement", async () => {
  for (const kind of ["connect", "disconnect", "activate"]) for (const rejects of [false, true]) {
    const h = host();
    h.setRead(async url => url.endsWith("mcp-status") ? { mcp_active: false, tools: [] } : [kind === "connect" ? expired : connection]);
    h.render(); await settle();
    const pending = deferred();
    if (kind === "connect") h.setRead(() => pending.promise); else h.setMutation(() => pending.promise);
    const operation = action(h, `connectors.${kind === "connect" ? "reconnect" : kind}`)();
    h.setRead(async () => []); h.setToken("session-b"); await settle(); const before = h.calls.length;
    if (rejects) pending.reject(Error("obsolete session")); else pending.resolve({ auth_url: "https://example.invalid/obsolete", tool_count: 1 });
    await operation;
    assert.match(h.text(), /connectors.empty/); assert.equal(h.location.href, ""); assert.deepEqual(h.alerts, []);
    assert.equal(h.calls.length, before); assert.equal(h.alert(), false); h.cleanup();
  }
});
