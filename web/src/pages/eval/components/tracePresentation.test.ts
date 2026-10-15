// @ts-expect-error -- Node types are outside tsconfig.app.json.
import assert from "node:assert/strict";
// @ts-expect-error -- Node types are outside tsconfig.app.json.
import { test } from "node:test";
import { formatTraceCost, formatTraceTokens } from "./tracePresentation.ts";

test("missing or partial measurements do not render as zero usage or free calls", () => {
  assert.equal(formatTraceTokens({ total_tokens: 0, metadata: {} }), "—");
  assert.equal(formatTraceCost({ total_cost_cents: 0, metadata: {} }), "—");
  const metadata = { runtime_model_usage: { dispatched_calls: 2, tokens_complete: false } };
  assert.equal(formatTraceTokens({ total_tokens: 40, metadata }), "—");
  assert.equal(formatTraceCost({ total_cost_cents: 10, metadata }), "—");
});

test("complete receipts preserve measured zero and small positive costs", () => {
  const metadata = { runtime_model_usage: { tokens_complete: true, cost_microusd: 0 } };
  assert.equal(formatTraceTokens({ total_tokens: 0, metadata }), "0");
  assert.equal(formatTraceCost({ total_cost_cents: 0, metadata }), "$0.00");
  assert.equal(formatTraceCost({ total_cost_cents: 0, metadata: { runtime_model_usage: { cost_microusd: 17 } } }), "<$0.01");
  assert.equal(formatTraceCost({ total_cost_cents: 125, metadata: {} }), "$1.25");
});
