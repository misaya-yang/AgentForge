import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import vm from "node:vm";
import ts from "typescript";
test("deferred tabs load at first visit and retain their mounted subtree after leaving", () => {
  let visited: unknown, initialized = false; const effects: (() => void)[] = [];
  const exports: Record<string, (props: unknown) => unknown> = {};
  const jsx = (type: unknown, props: unknown) => ({ type, props });
  vm.runInNewContext(ts.transpileModule(readFileSync(new URL("./DeferredTab.tsx", import.meta.url), "utf8"), { compilerOptions: { module: ts.ModuleKind.CommonJS, jsx: ts.JsxEmit.ReactJSX } }).outputText, {
    exports, require: (name: string) => name === "react/jsx-runtime" ? { jsx, jsxs: jsx } : {
      Suspense: "Suspense", useState(value: unknown) { if (!initialized) { initialized = true; visited = value; } return [visited, (next: unknown) => { visited = next; }]; }, useEffect(run: () => void) { effects.push(run); },
    },
  });
  assert.equal(exports.DeferredTab({ active: false, children: "draft" }), null);
  effects.splice(0).forEach(effect => effect());
  assert.ok(exports.DeferredTab({ active: true, children: "draft" })); effects.splice(0).forEach(effect => effect());
  const retained = exports.DeferredTab({ active: false, children: "draft" }) as { props: { children: unknown } };
  assert.equal(retained.props.children, "draft");
});
