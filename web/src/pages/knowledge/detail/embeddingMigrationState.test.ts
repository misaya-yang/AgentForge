// The browser app type gate intentionally omits Node globals; these imports
// are supplied by the `node --test` runtime used for this file.
// @ts-expect-error -- node built-in types are outside tsconfig.app.json.
import assert from "node:assert/strict";
// @ts-expect-error -- node built-in types are outside tsconfig.app.json.
import { test } from "node:test";

import {
  canStartEmbeddingMigration,
  getEmbeddingMigrationActions,
  getEmbeddingMigrationControls,
  getEmbeddingMigrationProgress,
  selectEmbeddingMigrationJob,
  shouldPollEmbeddingMigration,
} from "./embeddingMigrationState.ts";

test("selectEmbeddingMigrationJob prefers live and falls back to terminal history", () => {
  const live = { state: "backfilling" };
  const latest = { state: "completed" };
  assert.equal(
    selectEmbeddingMigrationJob({ live_migration: live, latest_migration: latest }),
    live
  );
  assert.equal(
    selectEmbeddingMigrationJob({ live_migration: null, latest_migration: latest }),
    latest
  );
  assert.equal(
    selectEmbeddingMigrationJob({ recent_migrations: [{ state: "rolled_back" }] })?.state,
    "rolled_back"
  );
});

test("getEmbeddingMigrationProgress uses authoritative pending and enabled counts", () => {
  assert.deepEqual(
    getEmbeddingMigrationProgress({
      live_migration: { state: "backfilling", totals: {} },
      pending_chunks: 25,
      enabled_chunks: 100,
    }),
    { completed: 75, pending: 25, total: 100, percent: 75 }
  );
  assert.deepEqual(
    getEmbeddingMigrationProgress({
      live_migration: null,
      latest_migration: {
        state: "completed",
        totals: { verified_enabled_chunks: 8 },
      },
      pending_chunks: null,
      enabled_chunks: 8,
    }),
    { completed: 8, pending: 0, total: 8, percent: 100 }
  );
});

test("getEmbeddingMigrationActions covers every operator state", () => {
  assert.deepEqual(getEmbeddingMigrationActions("shadow_build", 9), ["backfill", "abort"]);
  assert.deepEqual(getEmbeddingMigrationActions("backfilling", 0), [
    "backfill",
    "verify",
    "abort",
  ]);
  assert.deepEqual(getEmbeddingMigrationActions("failed", 3), ["backfill", "abort"]);
  assert.deepEqual(getEmbeddingMigrationActions("verified", 0), ["gate", "abort"]);
  assert.deepEqual(getEmbeddingMigrationActions("gating", 0), ["gate", "abort"]);
  assert.deepEqual(getEmbeddingMigrationActions("gate_failed", 0), ["gate", "abort"]);
  assert.deepEqual(getEmbeddingMigrationActions("ready", 0), ["cutover", "abort"]);
  assert.deepEqual(getEmbeddingMigrationActions("completed", 0), ["rollback"]);
  assert.deepEqual(getEmbeddingMigrationActions("rolled_back", 0), ["abort"]);
  assert.deepEqual(getEmbeddingMigrationActions("abandoned", null), []);
});

test("start and polling policies do not treat terminal failures as success", () => {
  assert.equal(canStartEmbeddingMigration(null), true);
  assert.equal(canStartEmbeddingMigration("completed"), true);
  assert.equal(canStartEmbeddingMigration("abandoned"), true);
  assert.equal(canStartEmbeddingMigration("failed"), false);
  assert.equal(shouldPollEmbeddingMigration("backfilling"), true);
  assert.equal(shouldPollEmbeddingMigration("gating"), true);
  assert.equal(shouldPollEmbeddingMigration("failed"), false);
  assert.equal(shouldPollEmbeddingMigration("completed"), false);
});

test("durable jobs constrain controls from one state derivation", () => {
  const queued = {
    job_id: "job-1",
    migration_id: "migration-1",
    action: "backfill" as const,
    state: "queued",
  };
  assert.deepEqual(
    getEmbeddingMigrationControls("shadow_build", 10, queued, "migration-1"),
    {
      actions: ["abort"],
      actionJobState: "queued",
      actionJobActive: true,
      canStart: false,
    }
  );
  assert.deepEqual(
    getEmbeddingMigrationControls("completed", 0, queued, "migration-new"),
    {
      actions: [],
      actionJobState: "queued",
      actionJobActive: true,
      canStart: false,
    }
  );
  assert.deepEqual(
    getEmbeddingMigrationControls(
      "failed",
      3,
      { ...queued, state: "failed", error: "provider timeout" },
      "migration-1"
    ),
    {
      actions: ["abort"],
      actionJobState: "failed",
      actionJobActive: false,
      canStart: false,
    }
  );
});
