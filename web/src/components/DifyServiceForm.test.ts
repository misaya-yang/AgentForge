/* eslint-disable @typescript-eslint/no-explicit-any -- Controlled production-component/transport fixture shapes. */
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import vm from "node:vm";
import ts from "typescript";

type Element = { type: any; props: Record<string, any>; key?: string };
type Auth = { token: string | null; user: {
  user_id: string; roles: string[]; permissions: string[]; tier: string;
} | null };
type Instance = { type: any; slots: any[]; cursor: number; mounted: boolean };
type Effect = { kind: "effect"; phase: "layout" | "passive"; deps?: unknown[];
  create: () => void | (() => void); cleanup?: () => void; pending: boolean };

function deferred<T>() {
  let resolve!: (value: T) => void;
  let reject!: (error: Error) => void;
  const promise = new Promise<T>((yes, no) => { resolve = yes; reject = no; });
  return { promise, resolve, reject };
}

/**
 * Evaluate the real parent, public Dify wrapper, scope helper and gateway helper.
 * Only React hooks, UI primitives and the API transport are doubled. The renderer
 * preserves component/key identity, stable setters, dependency-aware memo/effects,
 * deletion cleanup and layout-before-passive commit ordering. Closed dialogs and
 * inactive tabs unmount their children, as the production Radix primitives do.
 * These are callback/ownership tests, not browser keyboard or pointer acceptance.
 */
function host() {
  let auth: Auth = { token: "fixture-auth-original", user: {
    user_id: "owner", roles: ["admin"], permissions: ["console:services:edit"], tier: "enterprise",
  } };
  let current: Instance | null = null;
  let instances = new Map<string, Instance>();
  let tree: Element | null = null;
  let dirty = true;
  let rootPresent = true;
  let contentPresent = true;
  let registered = 0;
  let uuid = 0;
  const stateWrites: { mounted: boolean }[] = [];
  const writes: { url: string; body: Record<string, any>; reply: ReturnType<typeof deferred<any>> }[] = [];
  const sameDeps = (a?: unknown[], b?: unknown[]) => !!a && !!b &&
    a.length === b.length && a.every((value, index) => Object.is(value, b[index]));
  const nextSlot = (): [Instance, number] => {
    assert.ok(current, "Hooks must belong to a rendered component.");
    return [current, current.cursor++];
  };
  function effect(phase: Effect["phase"], create: Effect["create"], deps?: unknown[]) {
    const [owner, index] = nextSlot();
    const previous = owner.slots[index] as Effect | undefined;
    if (!previous) owner.slots[index] = { kind: "effect", phase, create, deps, pending: true };
    else if (!sameDeps(previous.deps, deps)) {
      previous.create = create; previous.deps = deps; previous.pending = true;
    }
  }
  const react = {
    useState(initial: any) {
      const [owner, index] = nextSlot();
      if (!(index in owner.slots)) {
        const slot: { value: any; set: (next: any) => void } = {
          value: typeof initial === "function" ? initial() : initial, set: () => {},
        };
        slot.set = (next: any) => {
          stateWrites.push({ mounted: owner.mounted });
          if (!owner.mounted) return;
          const value = typeof next === "function" ? next(slot.value) : next;
          if (!Object.is(value, slot.value)) { slot.value = value; dirty = true; }
        };
        owner.slots[index] = slot;
      }
      const slot = owner.slots[index];
      return [slot.value, slot.set];
    },
    useRef(initial: any) {
      const [owner, index] = nextSlot();
      return owner.slots[index] ?? (owner.slots[index] = { current: initial });
    },
    useMemo(create: () => any, deps?: unknown[]) {
      const [owner, index] = nextSlot();
      const previous = owner.slots[index];
      if (!previous || !sameDeps(previous.deps, deps)) owner.slots[index] = { value: create(), deps };
      return owner.slots[index].value;
    },
    useEffect: (create: Effect["create"], deps?: unknown[]) => effect("passive", create, deps),
    useLayoutEffect: (create: Effect["create"], deps?: unknown[]) => effect("layout", create, deps),
  };
  const element = (type: any, props: Record<string, any>, key?: unknown): Element =>
    ({ type, props, key: key == null ? undefined : String(key) });
  const translate = (key: string, fallback?: unknown) => typeof fallback === "string" ? fallback : key;
  const modules: Record<string, any> = {
    react,
    "react/jsx-runtime": { jsx: element, jsxs: element },
    "react-i18next": { useTranslation: () => ({ t: translate }) },
    yaml: { default: { parse: () => { throw new Error("YAML is outside this fixture."); } } },
    "@/lib/utils": { generateUUID: () => `fixture-scope-${++uuid}` },
    "@/store/useAuthStore": { useAuthStore: Object.assign(
      (select: (state: any) => any) => select({ ...auth, hasPermission: () => true }),
      { getState: () => auth },
    ) },
    "@/components/SdkAgentServiceForm": { SdkAgentServiceForm: "SdkAgentServiceForm" },
    "@/components/ConfigEditor": { ConfigEditor: "ConfigEditor" },
    "@/lib/api": { api: {
      post: async (url: string, body: Record<string, any>) => {
        const reply = deferred<any>();
        writes.push({ url, body: structuredClone(body), reply });
        return { data: await reply.promise };
      },
      get: async () => { throw new Error("Dify registration must not introduce a preflight read."); },
    } },
  };
  for (const [module, names] of [
    ["button", ["Button"]], ["input", ["Input"]], ["label", ["Label"]],
    ["dialog", ["Dialog", "DialogContent", "DialogDescription", "DialogHeader", "DialogTitle", "DialogTrigger"]],
    ["tabs", ["Tabs", "TabsContent", "TabsList", "TabsTrigger"]],
  ] as const) modules[`@/components/ui/${module}`] = Object.fromEntries(names.map(name => [name, name]));
  function evaluate(path: string) {
    const exports: Record<string, any> = {};
    const source = readFileSync(new URL(path, import.meta.url), "utf8");
    vm.runInNewContext(ts.transpileModule(source, {
      compilerOptions: { module: ts.ModuleKind.CommonJS, jsx: ts.JsxEmit.ReactJSX },
    }).outputText, { exports, Error, URL, require: (name: string) => {
      assert.ok(modules[name], `Unexpected production import: ${name}`); return modules[name];
    } });
    return exports;
  }
  modules["@/lib/sdkAuthorityScope"] = evaluate("../lib/sdkAuthorityScope.ts");
  modules["@/api/gateway"] = evaluate("../api/gateway.ts");
  modules["@/components/DifyServiceForm"] = evaluate("./DifyServiceForm.tsx");
  const { ServiceForm } = evaluate("./ServiceForm.tsx");

  function flush() {
    let passes = 0;
    do {
      assert.ok(++passes <= 30, "The component effects must reach a stable commit.");
      dirty = false;
      const next = new Map<string, Instance>();
      function render(node: any, path: string, context: { open?: boolean; tab?: string }): any {
        if (Array.isArray(node)) return node.map((child, index) => render(child, `${path}/${index}`, context));
        if (!node || typeof node !== "object" || !("props" in node)) return node;
        const identity = `${path}/${node.key ?? "unkeyed"}`;
        if (typeof node.type === "function") {
          const previous = instances.get(identity);
          const owner = previous?.type === node.type ? previous : {
            type: node.type, slots: [], cursor: 0, mounted: true,
          };
          next.set(identity, owner);
          owner.cursor = 0;
          const parent = current; current = owner;
          let output: any;
          try { output = node.type(node.props); } finally { current = parent; }
          return { ...node, props: { ...node.props, children: render(output, `${identity}/output`, context) } };
        }
        if (node.type === "Dialog") context = { ...context, open: node.props.open };
        if (node.type === "DialogContent" && (!context.open || !contentPresent)) return null;
        if (node.type === "Tabs") context = { ...context, tab: node.props.value };
        if (node.type === "TabsContent" && node.props.value !== context.tab) return null;
        return { ...node, props: { ...node.props, children: render(node.props.children, `${identity}/children`, context) } };
      }
      tree = rootPresent ? render(element(ServiceForm, { onRegistered: () => { registered++; } }), "root", {}) : null;
      const removed = [...instances.entries()].filter(([key, owner]) => next.get(key) !== owner).map(([, owner]) => owner);
      // Mark every deleted instance inactive before any cleanup can enqueue state.
      for (const owner of removed) owner.mounted = false;
      const committed = [...next.values()].reverse();
      for (const phase of ["layout", "passive"] as const) {
        for (const owner of removed) for (const slot of owner.slots) {
          if (slot?.kind === "effect" && slot.phase === phase) { slot.cleanup?.(); slot.cleanup = undefined; }
        }
        for (const owner of committed) for (const slot of owner.slots) {
          if (slot?.kind === "effect" && slot.phase === phase && slot.pending) {
            slot.cleanup?.(); slot.cleanup = undefined;
          }
        }
        for (const owner of committed) for (const slot of owner.slots) {
          if (slot?.kind === "effect" && slot.phase === phase && slot.pending) {
            slot.pending = false;
            const cleanup = slot.create();
            slot.cleanup = typeof cleanup === "function" ? cleanup : undefined;
          }
        }
      }
      instances = next;
    } while (dirty);
    return tree;
  }
  function nodes() {
    const all: Element[] = [];
    function visit(node: any) {
      if (Array.isArray(node)) node.forEach(visit);
      else if (node?.props) { all.push(node); visit(node.props.children); }
    }
    visit(flush()); return all;
  }
  function find(predicate: (node: Element) => boolean) {
    const found = nodes().find(predicate); assert.ok(found, "Expected a rendered production element."); return found;
  }
  const dialog = () => find(node => node.type === "Dialog");
  const tabs = () => find(node => node.type === "Tabs");
  const field = (id: string) => find(node => node.props.id === id);
  const change = (id: string, value: string) => { field(id).props.onChange({ target: { value } }); flush(); };
  const fill = (suffix = "original") => {
    change("dify-service-id", `dify-${suffix}`);
    change("dify-name", `Dify ${suffix}`);
    change("dify-base-url", "https://dify.example.test/v1/");
    change("dify-api-key", `fixture-app-key-${suffix}`);
  };
  const submitHandler = () => find(node => node.type === "form").props.onSubmit;
  const submit = () => submitHandler()({ preventDefault() {} }) as Promise<void>;
  const settle = (index: number, outcome: "success" | "failure") => {
    const write = writes[index]; assert.ok(write, "Expected a deferred production API request.");
    if (outcome === "success") write.reply.resolve({ service_id: write.body.service_id, status: "registered" });
    else write.reply.reject(Object.assign(new Error("fixture-secret-upstream-body"), {
      response: { status: 500, data: { detail: "fixture-secret-upstream-body" } },
      config: { headers: { Authorization: "fixture-secret-upstream-body" } },
    }));
  };
  const assertBusy = (busy: boolean) => {
    const triggers = nodes().filter(node => node.type === "TabsTrigger");
    assert.deepEqual(triggers.map(node => node.props.value), ["simple", "dify", "sdk", "advanced"]);
    for (const trigger of triggers) assert.equal(trigger.props.disabled, busy, `${trigger.props.value} trigger busy state`);
  };
  flush(); dialog().props.onOpenChange(true); flush(); tabs().props.onValueChange("dify"); flush();
  return {
    writes, stateWrites, nodes, find, dialog, tabs, field, change, fill, submit, submitHandler, settle, flush, assertBusy,
    get registered() { return registered; },
    get auth() { return auth; },
    changeAuth(next: Auth) { auth = next; dirty = true; },
    unmount() { rootPresent = false; flush(); },
    forceCloseAndReopen() {
      // Simulate an external content teardown, then a genuine controlled close and
      // reopen of the SAME parent. Old callbacks still target that parent's state.
      contentPresent = false; flush();
      dialog().props.onOpenChange(false); flush();
      assert.equal(dialog().props.open, false, "Child teardown must release parent busy ownership.");
      contentPresent = true;
      dialog().props.onOpenChange(true); flush();
    },
  };
}

const boundaries: { name: string; change: (auth: Auth) => Auth }[] = [
  { name: "account", change: auth => ({ ...auth, user: { ...auth.user!, user_id: "other-owner" } }) },
  { name: "roles", change: auth => ({ ...auth, user: { ...auth.user!, roles: ["viewer"] } }) },
  { name: "permissions", change: auth => ({ ...auth, user: { ...auth.user!, permissions: [] } }) },
  { name: "tier", change: auth => ({ ...auth, user: { ...auth.user!, tier: "normal" } }) },
  { name: "credential", change: auth => ({ ...auth, token: "fixture-auth-replacement" }) },
  { name: "logout", change: () => ({ token: null, user: null }) },
];

test("Dify dispatch clears the app key and fences duplicate submits while preserving the legacy request", async () => {
  const h = host(); h.fill();
  const handler = h.submitHandler();
  const first = handler({ preventDefault() {} });
  await handler({ preventDefault() {} });
  assert.equal(h.writes.length, 1);
  assert.equal(h.field("dify-api-key").props.value, "", "The app key must clear before transport settles.");
  await h.submit(); assert.equal(h.writes.length, 1);
  assert.equal(h.writes[0].url, "/api/v1/services");
  assert.deepEqual(h.writes[0].body, {
    service_id: "dify-original", name: "Dify original", service_type: "conversational",
    supported_modes: ["sync", "stream"], status: "active", connector_type: "http",
    connector_config: { base_url: "https://dify.example.test/v1", headers: { Authorization: "Bearer fixture-app-key-original" } },
    accepted_content_types: ["text"], output_content_types: ["text"], session_enabled: true,
    max_retries: 0, metadata: { adapter_type: "dify_chat" },
  });
  h.settle(0, "success"); await first;
  assert.equal(h.registered, 1); assert.equal(h.dialog().props.open, false);
});

test("pending Dify disables every parent tab and fences direct onValueChange calls", async () => {
  const h = host(); h.fill(); const saving = h.submit(); h.assertBusy(true);
  for (const value of ["simple", "sdk", "advanced"]) {
    h.tabs().props.onValueChange(value);
    assert.equal(h.tabs().props.value, "dify", "Disabling triggers alone is insufficient.");
  }
  assert.equal(h.writes.length, 1); h.settle(0, "failure"); await saving; h.assertBusy(false);
});

for (const dismissal of ["Escape", "outside pointer"]) {
  test(`pending Dify rejects the dialog close callback used by ${dismissal} dismissal`, async () => {
    const h = host(); h.fill(); const saving = h.submit();
    h.dialog().props.onOpenChange(false);
    assert.equal(h.dialog().props.open, true);
    assert.equal(h.tabs().props.value, "dify");
    h.settle(0, "failure"); await saving;
    h.dialog().props.onOpenChange(false); assert.equal(h.dialog().props.open, false);
  });
}

test("pending Dify disables its cancel button and every editable field", async () => {
  const h = host(); h.fill(); const saving = h.submit();
  const cancel = h.find(node => node.type === "Button" && node.props.type === "button");
  assert.equal(cancel.props.disabled, true);
  cancel.props.onClick(); assert.equal(h.dialog().props.open, true);
  for (const id of ["dify-service-id", "dify-name", "dify-base-url", "dify-api-key"]) assert.equal(h.field(id).props.disabled, true);
  assert.equal(h.find(node => node.props.type === "submit").props.disabled, true);
  h.settle(0, "failure"); await saving;
});

for (const outcome of ["success", "failure"] as const) {
  test(`old Dify ${outcome} after forced close/reopen cannot close, error or unlock the newer pending form`, async () => {
    const h = host(); h.fill(); const oldSave = h.submit(); h.flush();
    h.forceCloseAndReopen(); h.assertBusy(false);
    assert.equal(h.field("dify-service-id").props.value, "");
    assert.equal(h.field("dify-api-key").props.value, "");
    h.fill("newer"); const newSave = h.submit(); h.assertBusy(true);
    const writesBefore = h.stateWrites.length;
    h.settle(0, outcome); await oldSave;
    assert.equal(h.registered, 0);
    assert.equal(h.stateWrites.length, writesBefore, "A detached request must not publish state or parent busy callbacks.");
    assert.equal(h.dialog().props.open, true); h.assertBusy(true);
    assert.equal(h.field("dify-service-id").props.value, "dify-newer");
    assert.equal(h.nodes().some(node => node.props.role === "alert"), false);
    h.settle(1, "success"); await newSave;
    assert.equal(h.registered, 1); assert.equal(h.dialog().props.open, false); assert.equal(h.writes.length, 2);
  });

  test(`Dify ${outcome} after full parent unmount cannot publish callbacks or state`, async () => {
    const h = host(); h.fill(); const saving = h.submit(); h.flush(); h.unmount();
    const writesBefore = h.stateWrites.length;
    h.settle(0, outcome); await saving;
    assert.equal(h.registered, 0);
    assert.equal(h.stateWrites.length, writesBefore, "Unmounted request completion must be inert.");
    assert.equal(h.flush(), null);
  });

  test(`old Dify ${outcome} after credential remount cannot unlock the current authority's pending form`, async () => {
    const h = host(); h.fill(); const oldSave = h.submit(); h.flush();
    h.changeAuth({ ...h.auth, token: "fixture-auth-replacement" }); h.flush();
    h.assertBusy(false);
    assert.equal(h.field("dify-service-id").props.value, "");
    assert.equal(h.field("dify-api-key").props.value, "");
    h.fill("new-authority"); const newSave = h.submit(); h.assertBusy(true);
    const writesBefore = h.stateWrites.length;
    h.settle(0, outcome); await oldSave;
    assert.equal(h.registered, 0);
    assert.equal(h.stateWrites.length, writesBefore, "The old request must not release the new authority's parent busy state.");
    assert.equal(h.dialog().props.open, true); h.assertBusy(true);
    assert.equal(h.field("dify-service-id").props.value, "dify-new-authority");
    assert.equal(h.nodes().some(node => node.props.role === "alert"), false);
    h.settle(1, "success"); await newSave;
    assert.equal(h.registered, 1); assert.equal(h.dialog().props.open, false);
  });

  for (const boundary of boundaries) {
    test(`Dify ${outcome} observes ${boundary.name} change before rerender and cannot publish stale state`, async () => {
      const h = host(); h.fill(); const saving = h.submit(); h.flush();
      const writesBefore = h.stateWrites.length;
      h.changeAuth(boundary.change(h.auth));
      // Intentionally no render/effect cleanup until after the transport resolves.
      h.settle(0, outcome); await saving;
      assert.equal(h.registered, 0);
      assert.equal(h.stateWrites.length, writesBefore, "Completion must read current authority synchronously.");
      h.flush();
      assert.equal(h.dialog().props.open, true); h.assertBusy(false);
      assert.equal(h.field("dify-service-id").props.value, "");
      assert.equal(h.field("dify-api-key").props.value, "");
      assert.equal(h.nodes().some(node => node.props.role === "alert"), false);
      assert.equal(h.writes.length, 1);
    });
  }
}

for (const boundary of boundaries) {
  test(`${boundary.name} remount discards Dify drafts and secrets without exposing auth in React keys`, () => {
    const h = host(); h.fill();
    h.changeAuth(boundary.change(h.auth)); h.flush();
    assert.equal(h.field("dify-service-id").props.value, "");
    assert.equal(h.field("dify-name").props.value, "");
    assert.equal(h.field("dify-base-url").props.value, "https://api.dify.ai/v1");
    assert.equal(h.field("dify-api-key").props.value, "");
    const keys = JSON.stringify(h.nodes().map(node => node.key));
    assert.equal(keys.includes("fixture-auth"), false);
    assert.equal(keys.includes("fixture-app-key"), false);
    assert.equal(h.writes.length, 0); h.assertBusy(false);
  });
}

for (const boundary of boundaries.filter(value => ["permissions", "credential"].includes(value.name))) {
  test(`retained Dify submit callback cannot dispatch after ${boundary.name} changes before rerender`, async () => {
    const h = host(); h.fill(); const submit = h.submitHandler();
    h.changeAuth(boundary.change(h.auth));
    const submission = submit({ preventDefault() {} });
    assert.equal(h.writes.length, 0);
    await submission;
    h.flush(); assert.equal(h.field("dify-api-key").props.value, "");
  });
}

test("current Dify failure is sanitized, releases parent busy and permits an explicit retry with the existing nonsecret draft", async () => {
  const h = host(); h.fill(); const first = h.submit(); h.settle(0, "failure"); await first;
  assert.equal(h.registered, 0); assert.equal(h.dialog().props.open, true); h.assertBusy(false);
  assert.equal(h.field("dify-api-key").props.value, "");
  assert.equal(h.field("dify-service-id").props.value, "dify-original");
  assert.equal(h.field("dify-name").props.value, "Dify original");
  assert.equal(h.field("dify-base-url").props.value, "https://dify.example.test/v1/");
  assert.ok(h.find(node => node.props.role === "alert").props.children);
  assert.equal(JSON.stringify(h.flush()).includes("fixture-secret-upstream-body"), false);
  assert.equal(JSON.stringify(h.flush()).includes("fixture-app-key-original"), false);
  assert.equal(h.find(node => node.props.type === "submit").props.disabled, false);
  // Clearing the key means retry cannot silently reuse the old credential.
  const missingKey = h.submit(); assert.equal(h.writes.length, 1); await missingKey;
  h.change("dify-api-key", "fixture-app-key-retry"); const retry = h.submit(); h.assertBusy(true);
  assert.equal(h.field("dify-api-key").props.value, "");
  assert.equal(h.nodes().some(node => node.props.role === "alert"), false);
  assert.equal(h.writes[1].url, "/api/v1/services");
  assert.equal(h.writes[1].body.service_id, "dify-original");
  assert.equal(h.writes[1].body.connector_config.headers.Authorization, "Bearer fixture-app-key-retry");
  h.settle(1, "success"); await retry;
  assert.equal(h.registered, 1); assert.equal(h.dialog().props.open, false);
});

test("current Dify success closes once and releases busy ownership for a fresh empty reopen", async () => {
  const h = host(); h.fill(); const saving = h.submit(); h.assertBusy(true);
  h.settle(0, "success"); await saving;
  assert.equal(h.registered, 1); assert.equal(h.dialog().props.open, false);
  h.dialog().props.onOpenChange(true); h.flush();
  assert.equal(h.dialog().props.open, true); h.assertBusy(false);
  assert.equal(h.field("dify-service-id").props.value, "");
  assert.equal(h.field("dify-api-key").props.value, "");
  assert.equal(h.nodes().some(node => node.props.role === "alert"), false);
  assert.equal(h.writes.length, 1);
});

test("leaving an idle Dify tab unmounts its secret and draft before returning", () => {
  const h = host(); h.fill();
  h.tabs().props.onValueChange("simple"); assert.equal(h.tabs().props.value, "simple");
  h.tabs().props.onValueChange("dify");
  assert.equal(h.field("dify-service-id").props.value, "");
  assert.equal(h.field("dify-api-key").props.value, "");
  assert.equal(h.writes.length, 0); h.assertBusy(false);
});

test("combined parent busy fence also preserves SDK ownership and resets before returning to Dify", () => {
  const h = host(); h.tabs().props.onValueChange("sdk");
  const sdk = h.find(node => node.type === "SdkAgentServiceForm");
  sdk.props.onBusyChange(true); h.assertBusy(true);
  h.tabs().props.onValueChange("dify"); assert.equal(h.tabs().props.value, "sdk");
  h.dialog().props.onOpenChange(false); assert.equal(h.dialog().props.open, true);
  sdk.props.onBusyChange(false); h.assertBusy(false);
  h.tabs().props.onValueChange("dify"); assert.equal(h.tabs().props.value, "dify");
  assert.equal(h.field("dify-api-key").props.value, "");
});
