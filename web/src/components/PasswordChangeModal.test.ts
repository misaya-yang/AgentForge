import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import vm from "node:vm";
import ts from "typescript";

// Actual component and authenticated-view hook, controlled React scheduling and
// password transport. Only synthetic values; no browser, account or credential I/O.
type Tree = { type: unknown; key?: string; props: Record<string, unknown> };
type Owner = { slots: unknown[]; cleanups: Map<number, () => void> };
type Auth = { token: string | null; isAuthenticated: boolean; user: { user_id: string; roles: string[]; permissions: string[]; tier: string } | null };
function nodes(tree: unknown, type: string): Tree[] {
  if (!tree || typeof tree !== "object") return [];
  if (Array.isArray(tree)) return tree.flatMap(item => nodes(item, type));
  const node = tree as Tree;
  return [...(node.type === type ? [node] : []), ...nodes(node.props?.children, type)];
}
function deferred() {
  let resolve!: () => void;
  let reject!: (error: unknown) => void;
  const promise = new Promise<void>((yes, no) => { resolve = yes; reject = no; });
  return { promise, resolve, reject };
}
function host() {
  const outer: Owner = { slots: [], cleanups: new Map() };
  let inner: Owner | undefined; let innerKey: unknown;
  let owner = outer; let cursor = 0;
  const effects: (() => void)[] = [];
  const listeners = new Set<(state: Auth, previous: Auth) => void>();
  let auth: Auth = { token: "fixture-session-A", isAuthenticated: true,
    user: { user_id: "fixture-member", roles: ["user"], permissions: [], tier: "normal" } };
  let clears = 0; let completions = 0; let closes = 0; let successes = 0; let posts = 0; let uuid = 0;
  const request = deferred();
  const replace = (next: Auth) => { const previous = auth; auth = next; listeners.forEach(fn => fn(auth, previous)); };
  const clearAuth = () => { clears++; replace({ token: null, user: null, isAuthenticated: false }); };
  const store = Object.assign((select?: (value: Auth) => unknown) => select ? select(auth) : { ...auth, clearAuth }, {
    getState: () => ({ ...auth, clearAuth }),
    subscribe(fn: (state: Auth, previous: Auth) => void) { listeners.add(fn); return () => listeners.delete(fn); },
  });
  const same = (a?: unknown[], b?: unknown[]) => !!a && !!b && a.length === b.length && a.every((item, i) => Object.is(item, b[i]));
  const react = {
    useState(initial: unknown) {
      const target = owner; const i = cursor++;
      if (!(i in target.slots)) target.slots[i] = typeof initial === "function" ? initial() : initial;
      return [target.slots[i], (value: unknown) => { target.slots[i] = typeof value === "function" ? value(target.slots[i]) : value; }];
    },
    useRef(initial: unknown) { const i = cursor++; return owner.slots[i] ?? (owner.slots[i] = { current: initial }); },
    useMemo(compute: () => unknown, deps: unknown[]) {
      const i = cursor++; const previous = owner.slots[i] as { deps: unknown[]; value: unknown } | undefined;
      if (!previous || !same(previous.deps, deps)) owner.slots[i] = { deps, value: compute() };
      return (owner.slots[i] as { value: unknown }).value;
    },
    useCallback(fn: unknown, deps: unknown[]) { return react.useMemo(() => fn, deps); },
    useLayoutEffect(run: () => void | (() => void), deps: unknown[]) {
      const i = cursor++; const target = owner;
      if (!same(target.slots[i] as unknown[] | undefined, deps)) {
        target.slots[i] = deps;
        effects.push(() => { target.cleanups.get(i)?.(); const cleanup = run(); if (cleanup) target.cleanups.set(i, cleanup); });
      }
    },
  };
  const jsx = (type: unknown, props: Record<string, unknown>, key?: string) => ({ type, props, key });
  const modules: Record<string, unknown> = {
    react, "react/jsx-runtime": { jsx, jsxs: jsx },
    antd: { message: { success() { successes++; } } },
    "react-i18next": { useTranslation: () => ({ t: (key: string) => key }) },
    "@/store/useAuthStore": { useAuthStore: store },
    "@/lib/utils": { generateUUID: () => `fixture-view-${++uuid}` },
    "@/api/auth": { changePassword: () => { posts++; return request.promise; } },
  };
  function load(path: string) {
    const exports: Record<string, (props: unknown) => Tree | null> = {};
    vm.runInNewContext(ts.transpileModule(readFileSync(new URL(path, import.meta.url), "utf8"), {
      compilerOptions: { module: ts.ModuleKind.CommonJS, target: ts.ScriptTarget.ES2022, jsx: ts.JsxEmit.ReactJSX },
    }).outputText, { exports, require(id: string) {
      if (id.startsWith("@/components/ui/")) return new Proxy({}, { get: (_target, key) => key });
      assert.ok(id in modules, id); return modules[id];
    } });
    return exports;
  }
  modules["@/lib/sdkAuthorityScope"] = load("../lib/sdkAuthorityScope.ts");
  modules["@/hooks/useAuthenticatedReadScope"] = load("../hooks/useAuthenticatedReadScope.ts");
  const component = load("./PasswordChangeModal.tsx").PasswordChangeModal;
  const dispose = (target?: Owner) => target?.cleanups.forEach(fn => fn());
  const props = { open: true, allowClose: true, onComplete: () => { completions++; }, onClose: () => { closes++; } };
  function render() {
    owner = outer; cursor = 0;
    let tree = component(props);
    if (tree && typeof tree.type === "function") {
      if (!inner || innerKey !== tree.key) { dispose(inner); inner = { slots: [], cleanups: new Map() }; innerKey = tree.key; }
      owner = inner; cursor = 0; tree = tree.type(tree.props);
    } else if (!tree) { dispose(inner); inner = undefined; }
    effects.splice(0).forEach(fn => fn());
    return tree;
  }
  function fill() {
    const values = ["SyntheticOld1!", "SyntheticNew2!", "SyntheticNew2!"];
    nodes(render(), "Input").forEach((node, i) => (node.props.onChange as (event: unknown) => void)({ target: { value: values[i] } }));
    return render();
  }
  return { props, render, fill, request, replace, get auth() { return auth; },
    counts: () => ({ clears, completions, closes, successes, posts }),
    unmount() { dispose(inner); dispose(outer); },
  };
}
const submit = (tree: Tree | null) => (nodes(tree, "form")[0].props.onSubmit as (event: unknown) => Promise<void>)({ preventDefault() {} });

test("current password change submits once and requires a fresh login", async () => {
  const h = host(); const tree = h.fill(); const first = submit(tree); const duplicate = submit(tree);
  assert.equal(h.counts().posts, 1);
  assert.equal(nodes(h.render(), "Button").every(node => node.props.disabled), true);
  h.request.resolve(); await first; await duplicate;
  assert.deepEqual(h.counts(), { posts: 1, clears: 1, completions: 1, closes: 0, successes: 1 });
  assert.equal(h.render(), null);
});

test("replacement or ABA authority retires old submit and completion without a render", async () => {
  for (const outcome of ["success", "failure"]) for (const change of ["token", "roles", "logout-restore"]) {
    const h = host(); const tree = h.fill(); const pending = submit(tree); const original = h.auth;
    if (change === "token") h.replace({ ...original, token: "fixture-session-B" });
    if (change === "roles") h.replace({ ...original, user: { ...original.user!, roles: ["admin"] } });
    if (change === "logout-restore") { h.replace({ token: null, user: null, isAuthenticated: false }); h.replace(original); }
    assert.equal(h.counts().posts, 1);
    if (outcome === "success") h.request.resolve(); else h.request.reject(new Error("fixture rejection"));
    await pending; await submit(tree);
    assert.deepEqual(h.counts(), { posts: 1, clears: 0, completions: 0, closes: 0, successes: 0 });
    assert.equal(nodes(h.render(), "Input").every(node => node.props.value === ""), true);
  }
});

test("close and reopen discard fields and retire old callbacks", async () => {
  const h = host(); const old = h.fill();
  (nodes(old, "Button").find(node => node.props.type === "button")!.props.onClick as () => void)();
  await submit(old); assert.equal(h.counts().posts, 0); assert.equal(h.counts().closes, 1);
  h.props.open = false; assert.equal(h.render(), null);
  h.props.open = true;
  assert.equal(nodes(h.render(), "Input").every(node => node.props.value === ""), true);
  await submit(old); assert.equal(h.counts().posts, 0);
});

test("pending request and forced change cannot be dismissed", async () => {
  for (const forced of [true, false]) {
    const h = host(); h.props.allowClose = !forced; const tree = h.fill(); const pending = forced ? undefined : submit(tree);
    const rendered = h.render(); const content = nodes(rendered, "DialogContent")[0];
    assert.equal(content.props.showCloseButton, false);
    let prevented = 0;
    for (const event of ["onEscapeKeyDown", "onInteractOutside"]) (content.props[event] as (e: unknown) => void)({ preventDefault() { prevented++; } });
    (nodes(rendered, "Dialog")[0].props.onOpenChange as (open: boolean) => void)(false);
    for (const button of nodes(tree, "Button")) if (button.props.type === "button") (button.props.onClick as () => void)();
    assert.equal(prevented, 2); assert.equal(h.counts().closes, 0);
    h.request.resolve(); await pending;
  }
});

test("unmount ignores both late outcomes and retired submissions", async () => {
  for (const success of [true, false]) {
    const h = host(); const tree = h.fill(); const pending = submit(tree); h.unmount();
    if (success) h.request.resolve(); else h.request.reject(new Error("fixture rejection"));
    await pending; await submit(tree);
    assert.deepEqual(h.counts(), { posts: 1, clears: 0, completions: 0, closes: 0, successes: 0 });
  }
});

test("current rejected change remains recoverable without automatic replay", async () => {
  const h = host(); const pending = submit(h.fill());
  h.request.reject({ response: { status: 400, data: { detail: "Current password is incorrect" } } }); await pending;
  assert.equal(h.counts().posts, 1); assert.equal(h.counts().clears, 0);
  assert.equal(nodes(h.render(), "Button").every(node => !node.props.disabled), true);
  assert.ok(nodes(h.render(), "div").some(node => node.props.children === "Current password is incorrect"));
});


test("first login mounts the password form against the newly authenticated view", async () => {
  const h = host(); const member = h.auth;
  h.replace({ token: null, user: null, isAuthenticated: false }); h.props.open = false;
  assert.equal(h.render(), null);
  h.replace(member); h.props.open = true;
  const pending = submit(h.fill()); assert.equal(h.counts().posts, 1);
  h.request.resolve(); await pending; assert.equal(h.counts().completions, 1);
});

test("external close during pending request cannot alter a reopened form", async () => {
  for (const success of [true, false]) {
    const h = host(); const pending = submit(h.fill());
    h.props.open = false; h.render(); h.props.open = true; h.render();
    const reopened = h.fill();
    if (success) h.request.resolve(); else h.request.reject(new Error("fixture timeout"));
    await pending;
    assert.equal(h.counts().clears, 0); assert.equal(h.counts().completions, 0);
    assert.deepEqual(nodes(h.render(), "Input").map(n => n.props.value), nodes(reopened, "Input").map(n => n.props.value));
    assert.equal(nodes(h.render(), "Button").every(n => !n.props.disabled), true);
  }
});

test("unknown password result clears fields, blocks replay and offers sign-in recovery", async () => {
  for (const failure of [new Error("fixture timeout"), { response: { status: 503 } }, { response: { status: 408 } }]) {
    const h = host(); const original = h.fill(); const pending = submit(original);
    h.request.reject(failure); await pending; await submit(original);
    const tree = h.render(); assert.equal(h.counts().posts, 1);
    assert.equal(nodes(tree, "Input").every(n => n.props.value === "" && n.props.disabled), true);
    assert.ok(nodes(tree, "div").some(n => n.props.children === "auth.passwordChangeUnknown"));
    const content = nodes(tree, "DialogContent")[0];
    assert.equal(content.props.showCloseButton, false);
    let prevented = 0;
    for (const event of ["onEscapeKeyDown", "onInteractOutside"]) (content.props[event] as (e: unknown) => void)({ preventDefault() { prevented++; } });
    (nodes(tree, "Dialog")[0].props.onOpenChange as (open: boolean) => void)(false);
    (nodes(original, "Button").find(n => n.props.type === "button")!.props.onClick as () => void)();
    assert.equal(prevented, 2); assert.equal(h.counts().closes, 0);
    const recovery = nodes(tree, "Button").find(n => n.props.children === "auth.passwordChangeSignIn")!;
    (recovery.props.onClick as () => void)();
    assert.equal(h.counts().clears, 1); assert.equal(h.counts().completions, 1); assert.equal(h.counts().successes, 0);
  }
});
