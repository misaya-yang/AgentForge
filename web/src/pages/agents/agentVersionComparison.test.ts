import assert from "node:assert/strict";
import test from "node:test";
import type { AgentDetail, AgentVersion } from "../../types/agents";
import { createDefaultAgentSpec } from "../../types/agents.ts";
import { compareAgentVersions, sameVersionValue, validateVersionRead } from "./agentVersionComparisonState.ts";

function version(id = "v1"): AgentVersion {
  return { tenant_id: "tenant-a", agent_id: "agent-a", agent_version_id: id, version_number: id === "v1" ? 1 : 2,
    schema_version: "agent-spec/v1", spec: createDefaultAgentSpec(), spec_hash: `hash-${id}`,
    source_draft_id: "draft-a", source_draft_revision: 1, created_by: "user-a", created_at: "2026-10-13T00:00:00Z" };
}
const changed = (a: AgentVersion, b: AgentVersion) => compareAgentVersions(a, b).filter(row => row.changed);
const agent = { agent_id: "agent-a", tenant_id: "tenant-a" } as AgentDetail;

test("different immutable identities with identical saved config do not imply configuration or quality changes", () => {
  assert.equal(changed(version(), version("v2")).length, 0);
});
test("explicit direction preserves full instruction, model and knowledge changes", () => {
  const a = version(); const b = version("v2");
  b.spec.instructions = "Answer only from the supplied files.\nExplain conflicts.";
  b.spec.model.model_id = "candidate-model";
  b.spec.knowledge = [{ dataset_id: "dataset-a", retrieval_config: { mode: "tool", top_k: 3 } }];
  const rows = changed(a, b);
  assert.equal(rows.find(row => row.field === "instructions")?.before.text, a.spec.instructions);
  assert.equal(rows.find(row => row.field === "instructions")?.after.text, b.spec.instructions);
  assert.equal(rows.find(row => row.field === "dataset_id")?.before.state, "absent");
  assert.equal(rows.find(row => row.field === "dataset_id")?.after.text, "dataset-a");
  const inverse = changed(b, a);
  assert.equal(inverse.length, rows.length);
  assert.equal(inverse.find(row => row.field === "dataset_id")?.after.state, "absent");
});
test("object field order does not count but list order and duplicates remain significant", () => {
  assert.equal(sameVersionValue({ b: 2, a: 1 }, { a: 1, b: 2 }), true);
  assert.equal(sameVersionValue(["a", "b"], ["b", "a"]), false);
  assert.equal(sameVersionValue(["a", "a"], ["a"]), false);
  assert.equal(sameVersionValue({}, { model_id: null }), false);
});
test("missing legacy values are not replaced by current defaults and null/empty/absent stay distinct", () => {
  const a = version(); const b = version("v2");
  delete (a.spec as Partial<typeof a.spec>).outcome;
  b.spec.model.provider_id = null;
  delete a.spec.model.provider_id;
  b.spec.instructions = "";
  const rows = changed(a, b);
  assert.equal(rows.find(row => row.field === "goal")?.before.state, "absent");
  assert.equal(rows.find(row => row.field === "goal")?.after.text, "");
  assert.equal(rows.find(row => row.field === "provider_id")?.after.text, "null");
  assert.equal(rows.find(row => row.field === "instructions")?.after.text, "");
});
test("equal reported hashes never suppress different returned values", () => {
  const a = version(); const b = version("v2");
  b.spec_hash = a.spec_hash; b.spec.model.temperature = 0.7;
  assert.equal(changed(a, b).find(row => row.field === "temperature")?.after.text, "0.7");
});
test("opaque capability, unknown memory and extension contents never leave the comparison projection", () => {
  const a = version(); const b = version("v2");
  a.spec.capabilities = [{ type: "connector", resource_id: "mail", config: { token: "baseline-secret" } }];
  b.spec.capabilities = [{ type: "connector", resource_id: "mail", config: { token: "candidate-secret" } }];
  a.spec.memory.hidden = { private_history: "baseline-private" };
  b.spec.memory.hidden = { private_history: "candidate-private" };
  (b.spec as unknown as Record<string, unknown>)["sensitive-field-name"] = "unknown-secret";
  const rows = changed(a, b); const serialized = JSON.stringify(rows);
  for (const marker of ["baseline-secret", "candidate-secret", "private_history", "baseline-private", "candidate-private", "sensitive-field-name", "unknown-secret"]) assert.ok(!serialized.includes(marker), marker);
  assert.equal(rows.find(row => row.field === "config")?.before.state, "withheld");
  assert.ok(rows.some(row => row.section === "memory" && row.field === "other"));
  assert.ok(rows.some(row => row.section === "other"));
});
test("malformed known values and credential-bearing icon URLs are not recursively rendered", () => {
  const a = version(); const b = version("v2");
  (b.spec.model as unknown as Record<string, unknown>).model_id = { secret: "nested-secret" };
  b.spec.identity.icon_url = "https://example.com/icon?access_token=secret-url";
  const rows = changed(a, b);
  assert.equal(rows.find(row => row.field === "model_id")?.after.state, "unavailable");
  assert.equal(rows.find(row => row.field === "icon_url")?.after.state, "withheld");
  assert.ok(!JSON.stringify(rows).includes("secret"));
});
test("capability versions and schema changes plus known retrieval settings remain visible", () => {
  const a = version(); const b = version("v2");
  a.spec.capabilities = [{ type: "skill", resource_id: "report", resource_version: "1", schema_hash: "old", config: {} }];
  b.spec.capabilities = [{ type: "skill", resource_id: "report", resource_version: "2", schema_hash: "new", config: {} }];
  a.spec.knowledge = [{ dataset_id: "kb-a", retrieval_config: { top_k: 3, include_images: false } }];
  b.spec.knowledge = [{ dataset_id: "kb-a", retrieval_config: { top_k: 7, include_images: true } }];
  assert.deepEqual(changed(a, b).map(row => row.field), ["resource_version", "schema_hash", "top_k", "include_images"]);
});
test("entire missing section and empty stored section are still distinguishable", () => {
  const a = version(); const b = version("v2");
  delete (a.spec as Partial<typeof a.spec>).memory; b.spec.memory = {};
  const rows = changed(a, b);
  assert.ok(rows.some(row => row.field === "structure" && row.before.state === "absent" && row.after.state === "withheld"));
});
test("read validation binds all rows to fresh Agent and tenant and rejects duplicate IDs", () => {
  for (const rows of [[{ ...version(), tenant_id: "tenant-b" }], [{ ...version(), agent_id: "agent-b" }], [version(), version()]]) {
    assert.throws(() => validateVersionRead("agent-a", agent, rows));
  }
  assert.throws(() => validateVersionRead("agent-a", { ...agent, agent_id: "agent-b" }, [version()]));
  assert.throws(() => validateVersionRead("agent-a", agent, [{ ...version(), spec: null } as unknown as AgentVersion]));
});
test("authorized empty read remains empty and sorting cannot mutate source snapshots", () => {
  const rows = [version(), version("v2")]; const frozen = JSON.stringify(rows);
  assert.deepEqual(validateVersionRead("agent-a", agent, rows).map(row => row.agent_version_id), ["v2", "v1"]);
  assert.equal(JSON.stringify(rows), frozen);
  assert.deepEqual(validateVersionRead("agent-a", agent, []), []);
  compareAgentVersions(rows[0], rows[1]); assert.equal(JSON.stringify(rows), frozen);
});


test("equal redacted read models do not reveal original opaque values or verify reported hashes", () => {
  const a = version(); const b = version("v2");
  a.spec.capabilities = [{ type: "connector", resource_id: "mail", config: { token: "[REDACTED]" } }];
  b.spec.capabilities = [{ type: "connector", resource_id: "mail", config: { token: "[REDACTED]" } }];
  assert.notEqual(a.spec_hash, b.spec_hash);
  assert.equal(changed(a, b).length, 0);
  assert.ok(!JSON.stringify(compareAgentVersions(a, b)).includes("[REDACTED]"));
});


test("historical malformed memory mode is unavailable rather than coerced to a valid setting", () => {
  const a = version(); const b = version("v2"); b.spec.memory.mode = ["off"];
  const row = changed(a, b).find(row => row.section === "memory" && row.field === "mode");
  assert.equal(row?.after.state, "unavailable");
});
test("render metadata must be safe scalar identity and revision fields", () => {
  for (const bad of [{ agent_version_id: ["v1"] }, { agent_version_id: {} }, { spec_hash: {} }, { created_at: {} },
    { created_at: "not-a-time" }, { source_draft_revision: "1" }, { version_number: 0 }, { schema_version: null }]) {
    assert.throws(() => validateVersionRead("agent-a", agent, [{ ...version(), ...bad } as unknown as AgentVersion]));
  }
});
