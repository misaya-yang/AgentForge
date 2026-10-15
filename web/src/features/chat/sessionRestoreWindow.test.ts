import assert from "node:assert/strict";
import { test } from "node:test";

import { shouldBlockDuringRunRestore } from "./sessionRestoreWindow.ts";

test("active-run restore stays conservatively blocked until reconciliation", () => {
  assert.equal(
    shouldBlockDuringRunRestore({
      assistant_active_run: { run_id: "run-active", updated_at: "2026-01-01T00:00:00Z" },
    }),
    true,
  );
  assert.equal(shouldBlockDuringRunRestore({ assistant_active_run: {} }), false);
  assert.equal(shouldBlockDuringRunRestore(undefined), false);
});
