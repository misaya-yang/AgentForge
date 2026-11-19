import test from "node:test";
import assert from "node:assert/strict";
import { mcpConfigDraft, normalizeMcpConfig, sameMcpConfig, saveReviewedMcpConfig, validateMcpConfigDraft, type TenantMcpConfig } from "./tenantMcpGovernance.ts";
const config: TenantMcpConfig = { tenant_id: "tenant-a", allowed_servers: ["docs"], max_connections: 10, server_overrides: { docs: { timeout: 30 } } };
test("new draft is restrictive and has native default connection limit", () => {
  assert.deepEqual(mcpConfigDraft(), { tenant_id: "", allowed_servers: "[]", max_connections: "5" });
});
test("round trip retains hidden overrides without adding them to editable draft", () => {
  const draft = mcpConfigDraft(config);
  assert.equal("server_overrides" in draft, false);
  assert.deepEqual(validateMcpConfigDraft(draft, config), { payload: config });
});
for (const servers of ["null", "{}", '["docs",1]', "[", '"docs"']) test(`invalid allowlist rejected: ${servers}`, () => {
  assert.equal(validateMcpConfigDraft({ ...mcpConfigDraft(config), allowed_servers: servers }).error, "list");
});
for (const limit of ["0", "101", "1.5", "-1", "1e1", "", " 1"]) test(`invalid connection limit rejected: ${limit}`, () => {
  assert.equal(validateMcpConfigDraft({ ...mcpConfigDraft(config), max_connections: limit }).error, "connections");
});
test("empty allowlist and boundary connection limits remain exact", () => {
  for (const max of [1, 100]) assert.deepEqual(validateMcpConfigDraft({ ...mcpConfigDraft(config), allowed_servers: "[]", max_connections: String(max) }).payload, { ...config, allowed_servers: [], max_connections: max, server_overrides: null });
});
test("existing tenant cannot be renamed", () => {
  assert.equal(validateMcpConfigDraft({ ...mcpConfigDraft(config), tenant_id: "other" }, config).error, "tenant");
});
test("tenant length is Unicode codepoint length", () => {
  assert.equal(validateMcpConfigDraft({ ...mcpConfigDraft(config), tenant_id: "a".repeat(65) }).error, "tenant");
  assert.ok(validateMcpConfigDraft({ ...mcpConfigDraft(config), tenant_id: "租".repeat(64) }).payload);
});
test("normalize native JSONB text and discard irrelevant row metadata", () => {
  assert.deepEqual(normalizeMcpConfig({ ...config, server_overrides: JSON.stringify(config.server_overrides), id: "123", created_at: "date" }), config);
});
for (const overrides of [undefined, "broken", "[]", "1", []]) test(`malformed override fails closed: ${String(overrides)}`, () => {
  assert.throws(() => normalizeMcpConfig({ ...config, server_overrides: overrides }));
});
test("JSONB object key reordering is equivalent, array reordering is not", () => {
  assert.ok(sameMcpConfig({ ...config, server_overrides: { x: 1, y: 2 } }, { ...config, server_overrides: { y: 2, x: 1 } }));
  assert.equal(sameMcpConfig({ ...config, allowed_servers: ["x", "y"] }, { ...config, allowed_servers: ["y", "x"] }), false);
});
test("save performs read, write, verified read in order", async () => {
  const order: string[] = [];
  const result = await saveReviewedMcpConfig(config, config, async tenant => { order.push(`read:${tenant}`); return config; }, async payload => { order.push("write"); assert.deepEqual(payload, config); });
  assert.deepEqual(result, config);
  assert.deepEqual(order, ["read:tenant-a", "write", "read:tenant-a"]);
});
test("changed baseline blocks write including changed hidden overrides", async () => {
  let writes = 0;
  await assert.rejects(saveReviewedMcpConfig(config, config, async () => ({ ...config, server_overrides: null }), async () => { writes++; }), /changed/);
  assert.equal(writes, 0);
});
test("creation refuses to overwrite a tenant absent from stale list", async () => {
  let writes = 0;
  await assert.rejects(saveReviewedMcpConfig(config, null, async () => config, async () => { writes++; }), /changed/);
  assert.equal(writes, 0);
});
test("read failure cannot authorize creation", async () => {
  let writes = 0;
  await assert.rejects(saveReviewedMcpConfig(config, null, async () => { throw new Error("offline"); }, async () => { writes++; }), /offline/);
  assert.equal(writes, 0);
});
test("uncertain write is neither retried nor reported successful", async () => {
  let writes = 0, reads = 0;
  await assert.rejects(saveReviewedMcpConfig(config, config, async () => { reads++; return config; }, async () => { writes++; throw new Error("uncertain"); }), /uncertain/);
  assert.equal(writes, 1); assert.equal(reads, 1);
});
test("readback mismatch cannot report success", async () => {
  let reads = 0;
  await assert.rejects(saveReviewedMcpConfig(config, config, async () => ++reads === 1 ? config : { ...config, max_connections: 9 }, async () => {}), /readback_mismatch/);
});
test("new config requires absent baseline and verifies empty override normalization", async () => {
  let reads = 0;
  const actual = { ...config, server_overrides: null };
  assert.deepEqual(await saveReviewedMcpConfig({ ...config, server_overrides: {} }, null, async () => ++reads === 1 ? null : actual, async () => {}), actual);
});
