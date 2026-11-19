import assert from "node:assert/strict";
import test from "node:test";
import { initialRoutingPolicy, parseRoutingPolicy, routingPolicyFingerprint } from "./routingPolicyDraft.ts";
const policy = () => ({ ...initialRoutingPolicy("tenant"), policy_id: "p", version: "v1", candidates: [{ target: { provider: "provider", model: "model", version: "version" }, priority: 0, weight: 1 }] });
test("routing draft rejects cross-tenant policies, unsafe integers, absent candidates and duplicate targets", () => {
  const p = policy(); assert.equal(parseRoutingPolicy(JSON.stringify(p), "tenant").policy_id, "p");
  for (const patch of [{ tenant: "foreign" }, { max_cost_nanos: Number.MAX_SAFE_INTEGER + 1 }, { max_attempts: 0 }, { candidates: [] }, { candidates: [...p.candidates, ...p.candidates] }, { metric_max_age_ms: 0 }]) {
    assert.throws(() => parseRoutingPolicy(JSON.stringify({ ...p, ...patch }), "tenant"));
  }
});
test("readback comparison ignores JSON object ordering and retains meaningful changes", () => {
  const p = policy(), reordered = Object.fromEntries(Object.entries(p).reverse());
  assert.equal(routingPolicyFingerprint(p), routingPolicyFingerprint(reordered));
  assert.notEqual(routingPolicyFingerprint(p), routingPolicyFingerprint({ ...p, max_attempts: 2 }));
});

test("routing draft matches native unknown-field and capability-set rules", () => {
  const p = policy();
  assert.deepEqual(parseRoutingPolicy(JSON.stringify({ ...p, required_capabilities: ["z", "a"] }), "tenant").required_capabilities, ["a", "z"]);
  for (const patch of [{ fabricated: true }, { policy_id: " p " }, { required_capabilities: ["a", "a"] }, { candidates: [{ ...p.candidates[0], fabricated: true }] }]) {
    assert.throws(() => parseRoutingPolicy(JSON.stringify({ ...p, ...patch }), "tenant"));
  }
});
