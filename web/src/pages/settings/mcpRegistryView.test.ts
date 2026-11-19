import assert from "node:assert/strict";
import test from "node:test";
import { registryConnections, registryServers, registryTools } from "./mcpRegistryView.ts";
const connection = () => ({ connection_id: "11111111-1111-1111-1111-111111111111", server_id: "server", principal_type: "user_delegated", enabled: true, revoked_at: null, expires_at: null, credential: "MUST_NOT_PROJECT" });
const tool = () => ({ tool_id: "tool", server_id: "server", runtime_name: "search", snapshot_id: "snapshot", schema_hash: "a".repeat(64), schema_version: 1, risk_level: "low", enabled: true, read_only: true, input_schema: { properties: { query: { type: "string", default: "MUST_NOT_PROJECT" } }, required: ["query"] } });
test("MCP discovery selects an enabled unexpired connection from the exact server and projects no credentials", () => {
  const row = connection(); const actual = registryConnections({ connections: [row] }, "server");
  assert.equal(actual.length, 1); assert.equal(actual[0].principal_type, "user_delegated"); assert.equal("credential" in actual[0], false);
  assert.deepEqual(registryConnections({ connections: [{ ...row, enabled: false }, { ...row, expires_at: "2000-01-01T00:00:00Z" }, { ...row, revoked_at: "2026-01-01" }] }, "server"), []);
  for (const patch of [{ server_id: "foreign" }, { connection_id: "" }, { principal_type: "invented" }, { expires_at: "not-a-date" }]) assert.throws(() => registryConnections({ connections: [{ ...row, ...patch }] }, "server"));
});
test("MCP malformed reads remain unavailable rather than masquerading as an empty snapshot", () => {
  assert.throws(() => registryTools({}, "server")); assert.deepEqual(registryTools({ tools: [] }, "server"), []);
  assert.throws(() => registryServers({ servers: [{ server_id: "server" }] }));
  for (const patch of [{ server_id: "foreign" }, { schema_hash: "invented" }, { schema_version: 0 }, { input_schema: { required: "bad" } }]) assert.throws(() => registryTools({ tools: [{ ...tool(), ...patch }] }, "server"));
  assert.equal(registryTools({ tools: [tool()] }, "server")[0].snapshot_id, "snapshot");
});
