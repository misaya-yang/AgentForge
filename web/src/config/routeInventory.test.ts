import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import { consoleRouteInventory } from "./routeInventory.ts";
test("every mounted route has a concrete accessibility case, without skips", () => {
  const router = readFileSync(new URL("../router.tsx", import.meta.url), "utf8");
  const mounted = [...router.matchAll(/<Route\s+path="([^"]+)"/g)].map(match => match[1]).sort();
  assert.deepEqual(consoleRouteInventory.map(([route]) => route).sort(), mounted);
  for (const [, fixture] of consoleRouteInventory) assert.doesNotMatch(fixture, /[:*]/);
  const gate = readFileSync(new URL("../../e2e/all-routes-accessibility.spec.ts", import.meta.url), "utf8");
  assert.doesNotMatch(gate, /test\.skip|test\.fixme/); assert.match(gate, /wcag22aa/);
});
