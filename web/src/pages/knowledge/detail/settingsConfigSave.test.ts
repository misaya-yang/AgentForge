import assert from "node:assert/strict";
import test from "node:test";
import { reconcileSettingsSave, restoreSettingsSave, settingsEditorUnchanged, stageSettingsSave } from "./settingsConfigSave.ts";
import type { DatasetConfig } from "../../../types/knowledge.ts";
const config = (retrieval: unknown) => ({ dataset_id: "a", retrieval }) as DatasetConfig;
test("retrieval reconciliation checks exact submitted nested subset and identity", () => {
  const attempt = { ...stageSettingsSave("retrieval", { top_k: 9, fusion: { alpha: 0.7 } }, [9]), mutationAccepted: true };
  assert.equal(reconcileSettingsSave(attempt, config({ top_k: 9, fusion: { alpha: 0.7, rrf_k: 91 } }), "a").phase, "reconciled");
  for (const data of [{ top_k: 5 }, { top_k: "9", fusion: { alpha: 0.7 } }, { top_k: 9, fusion: { alpha: 0.8 } }]) {
    assert.equal(reconcileSettingsSave(attempt, config(data), "a").phase, "conflict");
  }
  assert.throws(() => reconcileSettingsSave(attempt, config({}), "other"), /identity/);
});
test("submission snapshot is immutable and newer edits are not settled", () => {
  const payload = { top_k: 9 };
  const attempt = stageSettingsSave("retrieval", payload, { b: 2, a: 1 });
  payload.top_k = 10;
  assert.equal(attempt.submitted?.top_k, 9);
  assert.equal(settingsEditorUnchanged(attempt, { a: 1, b: 2 }), true);
  assert.equal(settingsEditorUnchanged(attempt, { a: 1, b: 3 }), false);
});
test("interrupted or malformed attempts remain recovery-only", () => {
  const attempt = stageSettingsSave("chunking", { chunk_size: 700 }, [700]);
  for (const phase of ["staged", "mutation_accepted", "readback_uncertain", "conflict"]) {
    assert.equal(restoreSettingsSave({ ...attempt, phase })?.phase, "readback_uncertain");
  }
  const malformed = restoreSettingsSave({ submitted: { top_k: 9 } });
  assert.equal(malformed?.phase, "readback_uncertain");
  assert.equal(malformed?.section, null);
  assert.equal(restoreSettingsSave(undefined), null);
});

test("matching GET after unknown mutation proves observed state, not original request completion", () => {
  const attempt = stageSettingsSave("retrieval", { top_k: 9 }, [9]);
  assert.equal(reconcileSettingsSave(attempt, config({ top_k: 9 }), "a").phase, "observed_match");
});
