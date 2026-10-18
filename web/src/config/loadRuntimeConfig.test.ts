import assert from "node:assert/strict";
import test from "node:test";
import { loadRuntimeConfig } from "./loadRuntimeConfig.ts";

test("bootstrap awaits same-origin uncached JSON and projects only public string settings", async () => {
  let applied: unknown;
  let release!: (response: Response) => void;
  const pending = loadRuntimeConfig(async (url, options) => {
    assert.equal(url, "/runtime-config.json");
    assert.equal(options?.credentials, "same-origin");
    assert.equal(options?.cache, "no-store");
    assert.equal(options?.redirect, "error");
    assert.ok(options?.signal);
    return new Promise(resolve => { release = resolve; });
  }, config => { applied = config; });
  assert.equal(applied, undefined);
  release(Response.json({ apiBaseUrl: "/api", supportEmail: "支持@example.test", telemetryEndpoint: "/api/v1/telemetry/browser", privateToken: "never-copy", __proto__: { polluted: true } }));
  await pending;
  assert.deepEqual(applied, { apiBaseUrl: "/api", supportEmail: "支持@example.test", telemetryEndpoint: "/api/v1/telemetry/browser" });
});

test("missing, malformed and unavailable configuration never mount an app with fallback authority", async () => {
  for (const data of [null, [], "script", { apiUrl: {} }, { sseDebug: true }]) {
    await assert.rejects(loadRuntimeConfig(async () => Response.json(data), () => assert.fail("must not apply invalid data")), /Invalid runtime/);
  }
  await assert.rejects(loadRuntimeConfig(async () => new Response("missing", { status: 404 }), () => assert.fail()), /unavailable/);
  await assert.rejects(loadRuntimeConfig(async () => new Response("<html>SPA fallback</html>"), () => assert.fail()), SyntaxError);
  await assert.rejects(loadRuntimeConfig(async () => { throw new Error("network failed"); }, () => assert.fail()), /network failed/);
});
