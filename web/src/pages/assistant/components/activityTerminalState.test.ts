import assert from "node:assert/strict";
import test from "node:test";
import { hasTerminalActivity, terminalActivitySteps } from "./activityTerminalState.ts";
import type { TimelineStepData } from "./TimelineStep.tsx";

const step = (id: string, status: "running" | "completed" | "error"): TimelineStepData => ({ kind: "tool", id, icon: "code", title: id, body: "running old text", status });

test("a cancelled/failed run preserves completed results and exposes uncertain unfinished tools", () => {
  const old = [step("done", "completed"), step("failed", "error"), step("started", "running"), step("process-tool-approval", "running")];
  const result = terminalActivitySteps(old, true, new Set(["approval"]), "Result unknown", "Not executed");
  assert.equal(result[0], old[0]);
  assert.equal(result[1], old[1]);
  assert.deepEqual(result.slice(2).map((s) => [s.kind === "tool" ? s.status : "", s.body]), [["unknown", "Result unknown"], ["not_executed", "Not executed"]]);
  assert.equal(old[2].body, "running old text");
});

test("live tools keep running and repeated terminal projection is idempotent", () => {
  const steps = [step("started", "running")];
  assert.equal(terminalActivitySteps(steps, false, new Set(), "Unknown", "Not executed"), steps);
  const terminal = terminalActivitySteps(steps, true, new Set(), "Unknown", "Not executed");
  assert.deepEqual(terminalActivitySteps(terminal, true, new Set(), "Unknown", "Not executed"), terminal);
});

test("an explicit cancelled or failed message wins over a stale approval hold", () => {
  const summary = { status: "blocked", tools: [] } as const;
  for (const status of ["cancelled", "failed"] as const) {
    assert.equal(hasTerminalActivity({ status, processSummary: { ...summary, tools: [] } }), true);
  }
  assert.equal(hasTerminalActivity({ processSummary: { ...summary, tools: [] } }), false);
});
