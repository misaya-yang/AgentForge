import assert from "node:assert/strict";
import test from "node:test";
import { cancelledReceiptFlags } from "./cancelledReceipt.ts";

test("cancelled original run adopts a late uncertain receipt without borrowing another run", () => {
  const other = { metadata: { runtime_run_id: "other", process_summary: { outcome_uncertain: true } } };
  assert.deepEqual(cancelledReceiptFlags([other], "original"), { outcomeUncertain: false, sourceAccessRevoked: false });
  const original = { metadata: { runtime_run_id: "original", process_summary: { status: "cancelled", outcome_uncertain: true } } };
  assert.deepEqual(cancelledReceiptFlags([original, other], "original"), { outcomeUncertain: true, sourceAccessRevoked: false });
});

test("source restriction remains independent of execution uncertainty", () => {
  assert.deepEqual(cancelledReceiptFlags([{ metadata: { runtime_run_id: "original", source_access_revoked: true } }], "original"), { outcomeUncertain: false, sourceAccessRevoked: true });
});
