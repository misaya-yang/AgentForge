import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import vm from "node:vm";
import ts from "typescript";
import type * as Form from "./guardrailsForm.ts";
const exported: Record<string, unknown> = {};
vm.runInNewContext(ts.transpileModule(readFileSync(new URL("./guardrailsForm.ts", import.meta.url), "utf8"), { compilerOptions: { target: ts.ScriptTarget.ES2022, module: ts.ModuleKind.CommonJS } }).outputText, { exports: exported, TextEncoder });
const form = exported as typeof Form;
function valid() {
  const policy = form.emptyPolicy();
  for (const stage of policy.stages) stage.rules.push({ id: "safe-rule", all: ["test"], any: [], min_any: 0, score: 1 });
  return policy;
}
test("new drafts remain disabled and cannot save empty stages", () => {
  assert.equal(form.emptyPolicy().enabled, false);
  assert.equal(form.validPolicy(form.emptyPolicy()), false);
  assert.equal(form.validPolicy(valid()), true);
});
test("three-stage, rule score, directions and byte budgets fail closed", () => {
  const mutations: Array<(p: ReturnType<typeof valid>) => void> = [
    p => { p.stages.pop(); }, p => { p.stages[1].stage = p.stages[0].stage; },
    p => { p.stages[0].directions = []; }, p => { p.stages[0].rules[0].score = 0; },
    p => { p.budget_ms = 501; }, p => { p.max_output_bytes = 1023; },
    p => { p.stages[0].rules[0].all = ["" ]; }, p => { p.stages[0].rules[0].all = ["界".repeat(700)]; },
    p => { p.stages[0].detectors = ["email"]; }, p => { p.stages[0].rules[0].any = ["x"]; },
  ];
  for (const mutate of mutations) { const p = valid(); mutate(p); assert.equal(form.validPolicy(p), false); }
});
test("backend-compatible labels and detectors preserve disabled policies", () => {
  const p = valid(); p.version = "version/a_b:1";
  p.stages[1].rules = []; p.stages[1].detectors = ["email"];
  assert.equal(form.validPolicy(p), true);
  p.version = "a b"; assert.equal(form.validPolicy(p), false);
});
