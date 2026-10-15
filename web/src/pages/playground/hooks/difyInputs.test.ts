import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import vm from "node:vm";
import ts from "typescript";
import { sdkAuthorityScope } from "../../../lib/sdkAuthorityScope.ts";

type Draft = { text: string; invalid: boolean; value?: Record<string, unknown>; setText: (value: string) => void; isCurrent: () => boolean; beginSend: () => (() => void) | undefined };
function host() {
  let cursor = 0;
  const slots: unknown[] = [];
  const cleanup: (() => void)[] = [];
  const auth = { token: "fixture-token", user: { user_id: "owner", roles: [] as string[], permissions: [] as string[], tier: "free" } };
  const options: [string | undefined, string | undefined, boolean, boolean] = ["service", "session", true, true];
  const react = {
    useRef(initial: unknown) { const index = cursor++; return slots[index] ?? (slots[index] = { current: initial }); },
    useState(initial: unknown) { const index = cursor++; if (!(index in slots)) slots[index] = initial; return [slots[index], (value: unknown) => { slots[index] = value; }]; },
    useMemo(fn: () => unknown, deps: unknown[]) { const index = cursor++; const prior = slots[index] as { deps: unknown[]; value: unknown } | undefined;
      if (!prior || deps.some((value, i) => !Object.is(value, prior.deps[i]))) slots[index] = { deps, value: fn() }; return (slots[index] as { value: unknown }).value; },
    useLayoutEffect(fn: () => () => void) { const index = cursor++; if (!(index in slots)) { slots[index] = true; cleanup.push(fn()); } },
  };
  const modules: Record<string, unknown> = { react, "@/lib/sdkAuthorityScope": { sdkAuthorityScope },
    "@/store/useAppStore": { useAppStore: { getState: () => ({ selectedServiceId: options[0], activeSessionId: options[1] }) } },
    "@/store/useAuthStore": { useAuthStore: Object.assign((select: (state: typeof auth) => unknown) => select(auth), { getState: () => auth }) } };
  const exports: { useDifyInputs?: (...args: typeof options) => Draft; parseDifyInputs?: (text: string) => Record<string, unknown> | undefined } = {};
  vm.runInNewContext(ts.transpileModule(readFileSync(new URL("./useDifyInputs.ts", import.meta.url), "utf8"), { compilerOptions: { module: ts.ModuleKind.CommonJS, target: ts.ScriptTarget.ES2022 } }).outputText,
    { exports, require: (name: string) => { assert.ok(name in modules, name); return modules[name]; }, Error });
  return { auth, options, parse: exports.parseDifyInputs!, render: () => { cursor = 0; return exports.useDifyInputs!(...options); }, unmount: () => cleanup.forEach(fn => fn()) };
}
for (const text of ["{", "secret-invalid", "null", "[]", '"text"', "1", "true", '{"n":1e400}', '{"nested":[1e400]}']) {
  test(`custom input parser rejects invalid object ${text}`, () => { const h = host(); assert.throws(() => h.parse(text)); h.render().setText(text); assert.equal(h.render().invalid, true); });
}
test("empty draft is omitted; JSON types and prototype-shaped keys remain ordinary data", () => {
  const h = host(); assert.equal(h.parse(" \n"), undefined);
  const source = '{"text":"value","count":3,"enabled":false,"none":null,"nested":[{"x":1}],"__proto__":{"polluted":true},"constructor":{"prototype":{"polluted":true}}}';
  const value = h.parse(source)!; assert.equal(JSON.stringify(value), source);
  assert.equal(Object.hasOwn(value, "__proto__"), true); assert.equal(({} as Record<string, unknown>).polluted, undefined);
  assert.equal(JSON.stringify(h.parse('{"large":"' + "a".repeat(1024 * 1024) + '"}')).length, 1024 * 1024 + 12, "No arbitrary editor cap");
});
for (const change of ["service", "session", "enabled", "memory", "token", "user", "permissions", "roles", "tier"] as const) {
  test(`custom draft clears and stale callbacks retire across ${change} changes and return`, () => {
    const h = host(); h.render().setText('{"private":"old"}'); const old = h.render(); assert.equal(old.invalid, false);
    const previous = [...h.options] as typeof h.options; const token = h.auth.token; const user = h.auth.user;
    if (change === "service") h.options[0] = "other";
    else if (change === "session") h.options[1] = "other";
    else if (change === "enabled") h.options[2] = false;
    else if (change === "memory") h.options[3] = false;
    else if (change === "token") h.auth.token = "new-token";
    else h.auth.user = { ...user, ...(change === "user" ? { user_id: "new-owner" } : change === "permissions" ? { permissions: ["new"] } : change === "roles" ? { roles: ["new"] } : { tier: "pro" }) };
    assert.equal(h.render().text, ""); assert.equal(old.isCurrent(), false); old.setText('{"late":true}'); assert.equal(h.render().text, "");
    h.options.splice(0, 4, ...previous); h.auth.token = token; h.auth.user = user;
    assert.equal(h.render().text, ""); assert.equal(h.render().value, undefined);
  });
}
test("auth mutation before render and unmount retire existing callbacks", () => {
  const h = host(); h.render().setText('{"field":"old"}'); const old = h.render(); h.auth.token = "new";
  assert.equal(old.isCurrent(), false); old.setText('{"late":true}'); assert.equal(h.render().text, "");
  const latest = h.render(); h.unmount(); assert.equal(latest.isCurrent(), false);
});
