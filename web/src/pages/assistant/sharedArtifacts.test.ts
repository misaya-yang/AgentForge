import assert from "node:assert/strict";
import { test } from "node:test";
import { placeSharedArtifacts } from "./sharedArtifacts.ts";

test("old snapshots show unlinked authorized artifacts once", () => {
  const pdf = { artifact_id: "pdf" };
  assert.deepEqual(placeSharedArtifacts([pdf, pdf], [{ role: "assistant" }]), { byMessage: [[]], remaining: [pdf] });
});
test("placements exclude nonmembers, malformed IDs and duplicates across messages", () => {
  const pdf = { artifact_id: "pdf" }, image = { artifact_id: "image" };
  const result = placeSharedArtifacts([pdf, image], [
    { role: "user", metadata: { artifact_ids: ["image"] } },
    { role: "assistant", metadata: { artifact_ids: ["pdf", "other-owner", "pdf", 1] } },
    { role: "assistant", metadata: { artifact_ids: ["pdf"] } },
    { role: "assistant", metadata: { artifact_ids: "image" } },
  ]);
  assert.deepEqual(result, { byMessage: [[], [pdf], [], []], remaining: [image] });
  assert.deepEqual(placeSharedArtifacts([], [{ role: "assistant", metadata: { artifact_ids: ["pdf"] } }]), { byMessage: [[]], remaining: [] });
});
