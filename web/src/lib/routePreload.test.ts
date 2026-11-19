import assert from "node:assert/strict";
import test from "node:test";
import { createRoutePreloader } from "./routePreload.ts";
test("route intent preloads its longest matching route once without prefix collisions", async () => {
  const calls: string[] = [];
  const preload = createRoutePreloader({ "/agents": async () => { calls.push("list"); }, "/agents/new": async () => { calls.push("create"); } });
  await preload("/agents/new"); await preload("/agents/new"); await preload("/agents/existing");
  assert.deepEqual(calls, ["create", "list"]); assert.equal(preload("/agents-not-a-route"), undefined);
});
test("failed navigation preloads can be tried again by a later explicit intent", async () => {
  let attempts = 0;
  const preload = createRoutePreloader({ "/eval": async () => { if (++attempts === 1) throw new Error("offline"); } });
  await preload("/eval"); await preload("/eval"); await preload("/eval"); assert.equal(attempts, 2);
});
