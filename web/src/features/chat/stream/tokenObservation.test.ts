import assert from "node:assert/strict";
import test from "node:test";
import { observeTokenUsage, restoreTokenObservation } from "./tokenObservation.ts";

test("reported zero is valid; missing fields and partial totals remain unknown", () => {
  const empty = { tokenUsageSource: "unknown" as const };
  assert.deepEqual(observeTokenUsage(empty, undefined), empty);
  assert.deepEqual(observeTokenUsage(empty, { input_tokens: 0 }), { inputTokens: 0, tokenUsageSource: "reported" });
  assert.deepEqual(observeTokenUsage(empty, { input_tokens: 0, output_tokens: 0 }), {
    inputTokens: 0, outputTokens: 0, tokenUsageSource: "reported",
  });
});
test("partial observations merge received fields without inventing a total or replacing a received total", () => {
  let stats = observeTokenUsage({ tokenUsageSource: "unknown" }, { input_tokens: 10, total_tokens: 99 });
  stats = observeTokenUsage(stats, { output_tokens: 1 });
  assert.deepEqual(stats, { inputTokens: 10, outputTokens: 1, totalTokens: 99, tokenUsageSource: "reported" });
  assert.deepEqual(observeTokenUsage(stats, { duration_ms: 10 }), stats);
});
test("invalid values never become measured zero; valid aliases keep canonical precedence", () => {
  for (const value of [null, "", "2.5", "1e2", "0x10", -1, 0.5, Infinity, NaN, Number.MAX_SAFE_INTEGER + 1]) {
    assert.equal(observeTokenUsage({ tokenUsageSource: "unknown" }, { input_tokens: value }).inputTokens, undefined);
  }
  const value = observeTokenUsage({ tokenUsageSource: "unknown" }, { input_tokens: 0, prompt_tokens: 9, completion_tokens: 3 });
  assert.deepEqual(value, { inputTokens: 0, outputTokens: 3, tokenUsageSource: "reported" });
});
test("history without provenance retains valid counts but cannot be relabeled reported or measured", () => {
  assert.deepEqual(restoreTokenObservation({ input_tokens: 0, output_tokens: 4, total_tokens: 4 }), {
    inputTokens: 0, outputTokens: 4, totalTokens: 4, tokenUsageSource: "unknown",
  });
  assert.equal(restoreTokenObservation({ token_usage_source: "measured", total_tokens: 4 }).tokenUsageSource, "unknown");
});
test("new history round-trips reported fields and separately labeled text estimates", () => {
  assert.deepEqual(restoreTokenObservation({ input_tokens: 0, token_usage_source: "reported",
    text_token_estimate: { input_tokens: 3, output_tokens: 5, total_tokens: 8 }, cost: 9 }), {
    inputTokens: 0, tokenUsageSource: "reported", textTokenEstimate: { inputTokens: 3, outputTokens: 5, totalTokens: 8 },
  });
  assert.equal(restoreTokenObservation({ text_token_estimate: { input_tokens: 3, output_tokens: 5, total_tokens: 9 } }).textTokenEstimate, undefined);
});
test("additive Gateway provenance remains per-component and never replaces received amounts", () => {
  let observation = observeTokenUsage({ tokenUsageSource: "unknown" }, { input_tokens: 0, output_tokens: 7, total_tokens: 7,
    token_provenance: { input_tokens: "upstream", output_tokens: "estimated", total_tokens: "derived" } });
  assert.deepEqual(observation.tokenProvenance, { input_tokens: "upstream", output_tokens: "estimated", total_tokens: "derived" });
  observation = observeTokenUsage(observation, { output_tokens: 8 });
  assert.deepEqual(observation.tokenProvenance, { input_tokens: "upstream", total_tokens: "derived" });
  assert.equal(observation.totalTokens, 7); assert.equal(observation.outputTokens, 8);
});
test("provenance alone, invalid labels and invalid counts cannot create measured usage", () => {
  assert.equal(observeTokenUsage({ tokenUsageSource: "unknown" }, { token_provenance: { input_tokens: "upstream" } }).inputTokens, undefined);
  assert.equal(observeTokenUsage({ tokenUsageSource: "unknown" }, { input_tokens: -1, token_provenance: { input_tokens: "upstream" } }).tokenProvenance, undefined);
  assert.equal(observeTokenUsage({ tokenUsageSource: "unknown" }, { input_tokens: 1, token_provenance: { input_tokens: "measured" } }).tokenProvenance, undefined);
});
test("new Gateway histories retain additive provenance without relabeling old histories", () => {
  const restored = restoreTokenObservation({ input_tokens: 0, output_tokens: 3, total_tokens: 3,
    token_provenance: { input_tokens: "upstream", output_tokens: "estimated", total_tokens: "derived" } });
  assert.equal(restored.tokenUsageSource, "reported");
  assert.equal(restored.tokenProvenance?.input_tokens, "upstream");
  assert.equal(restoreTokenObservation({ input_tokens: 0 }).tokenUsageSource, "unknown");
});

test("legacy decimal integer strings retain valid counts and explicit zero", () => {
  const observation = observeTokenUsage({ tokenUsageSource: "unknown" }, { input_tokens: " 0 ", output_tokens: "12", total_tokens: "+12" });
  assert.deepEqual(observation, { inputTokens: 0, outputTokens: 12, totalTokens: 12, tokenUsageSource: "reported" });
  assert.equal(restoreTokenObservation({ input_tokens: "0", output_tokens: "12", total_tokens: "12" }).totalTokens, 12);
  assert.equal(restoreTokenObservation({ input_tokens: "0", output_tokens: "12", total_tokens: "12" }).tokenUsageSource, "unknown");
});
