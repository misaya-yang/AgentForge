import assert from "node:assert/strict";
import test from "node:test";
import { itemKindKey, relativeTime, sourceLabel } from "./format.ts";

const now = Date.parse("2026-11-13T12:00:00Z");

test("relative time stays in the viewer locale", () => {
  const recent = new Date(now - 14 * 60_000).toISOString();
  assert.match(relativeTime(recent, now, "zh-CN"), /14/);
  assert.match(relativeTime(recent, now, "en-US"), /14/);
  assert.equal(relativeTime("not-a-date", now, "en-US"), "");
});

test("source shows the space only when someone else shared the item", () => {
  const shared = { source: "native", shared: true, created_by: "other", space_name: "Studio" };
  assert.equal(sourceLabel(shared, "me"), "Studio");
  assert.equal(sourceLabel({ ...shared, created_by: "me" }, "me"), "—");
  assert.equal(sourceLabel({ ...shared, shared: false }, "me"), "—");
  assert.equal(sourceLabel({ ...shared, source: "connector:drive" }, "me"), "Studio");
  assert.equal(itemKindKey("slide"), "slide");
  assert.equal(itemKindKey("unknown"), "file");
});
