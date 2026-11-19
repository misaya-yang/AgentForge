import assert from "node:assert/strict";
import { test } from "node:test";

import { resolveArtifactIdsByMessageIndex } from "./sessionArtifactHydration.ts";

test("unbound persisted DOCX is attached to the nearest assistant response", () => {
  const resolved = resolveArtifactIdsByMessageIndex(
    [
      { id: "u1", role: "user", createdAt: "2026-08-30T12:00:00Z" },
      { id: "a1", role: "assistant", createdAt: "2026-08-30T12:00:03Z" },
      { id: "a2", role: "assistant", createdAt: "2026-08-30T12:01:00Z" },
    ],
    [{
      artifact_id: "art_docx",
      created_at: "2026-08-30T12:00:02Z",
      source: "ai",
    }],
  );
  assert.deepEqual(resolved.get(1), ["art_docx"]);
});

test("explicit artifact ownership wins and user uploads are not rebound", () => {
  const resolved = resolveArtifactIdsByMessageIndex(
    [{ id: "a1", role: "assistant", _artifactIds: ["explicit"] }],
    [
      { artifact_id: "explicit", source: "ai" },
      { artifact_id: "upload", source: "user" },
    ],
  );
  assert.deepEqual(resolved.get(0), ["explicit"]);
});
