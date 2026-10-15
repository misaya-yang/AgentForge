import assert from "node:assert/strict";
import test from "node:test";

import { futurePublicExpiry } from "./agentReleaseExpiry.ts";

test("public expiry requires an explicitly chosen future instant", () => {
  const now = new Date("2026-09-28T12:00:00Z").getTime();
  assert.equal(futurePublicExpiry("", now), null);
  assert.equal(futurePublicExpiry("invalid", now), null);
  assert.equal(futurePublicExpiry("2026-09-28T12:00:00Z", now), null);
  assert.equal(futurePublicExpiry("2026-09-29T12:00:00Z", now), "2026-09-29T12:00:00.000Z");
});
