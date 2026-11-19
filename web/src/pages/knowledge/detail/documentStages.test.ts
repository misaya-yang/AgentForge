// The browser app type gate intentionally omits Node globals; these imports
// are supplied by the `node --test` runtime used for this file.
// @ts-expect-error -- node built-in types are outside tsconfig.app.json.
import assert from "node:assert/strict";
// @ts-expect-error -- node built-in types are outside tsconfig.app.json.
import { test } from "node:test";

import {
  buildStageTimings,
  formatStageDuration,
  lastStartedDocumentStage,
  runningStageDurationMs,
} from "./documentStages.ts";

test("buildStageTimings returns empty for missing document or timestamps", () => {
  assert.deepEqual(buildStageTimings(null), []);
  assert.deepEqual(buildStageTimings(undefined), []);
  assert.deepEqual(buildStageTimings({}), []);
  // Unparseable timestamps are treated as absent, not as crashes.
  assert.deepEqual(
    buildStageTimings({ parsing_started_at: "not-a-date" }),
    []
  );
});

test("failed documents report only their last recorded stage", () => {
  assert.equal(lastStartedDocumentStage({ parsing_started_at: "2026-08-28T10:00:00Z" }), "parsing");
  assert.equal(lastStartedDocumentStage({ parsing_started_at: "2026-08-28T10:00:00Z", splitting_started_at: "2026-08-28T10:00:12Z" }), "splitting");
  assert.equal(lastStartedDocumentStage({ indexing_started_at: "2026-08-28T10:00:46Z" }), "indexing");
  assert.equal(lastStartedDocumentStage({
    indexing_started_at: "2026-08-28T10:00:46Z",
    parsing_started_at: "2026-08-28T10:05:00Z",
  }), "parsing");
  assert.equal(lastStartedDocumentStage({ parsing_started_at: "invalid" }), null);
  assert.equal(lastStartedDocumentStage(null), null);
});

test("buildStageTimings covers the full pipeline with ended stages", () => {
  const timings = buildStageTimings({
    parsing_started_at: "2026-08-28T10:00:00Z",
    splitting_started_at: "2026-08-28T10:00:12Z",
    indexing_started_at: "2026-08-28T10:00:46Z",
    completed_at: "2026-08-28T10:01:52Z",
  });
  assert.deepEqual(
    timings.map((t) => ({ stage: t.stage, durationMs: t.durationMs, running: t.running })),
    [
      { stage: "parsing", durationMs: 12_000, running: false },
      { stage: "splitting", durationMs: 34_000, running: false },
      { stage: "indexing", durationMs: 66_000, running: false },
    ]
  );
});

test("buildStageTimings marks the open tail stage as running", () => {
  const timings = buildStageTimings({
    parsing_started_at: "2026-08-28T10:00:00Z",
    splitting_started_at: "2026-08-28T10:00:12Z",
    indexing_started_at: "2026-08-28T10:00:46Z",
  });
  assert.equal(timings.length, 3);
  assert.equal(timings[2].stage, "indexing");
  assert.equal(timings[2].durationMs, null);
  assert.equal(timings[2].running, true);
  assert.equal(timings[1].running, false);
});

test("buildStageTimings skips stages without a start timestamp", () => {
  // Some writers stamp indexing only; partial data must not invent stages.
  const timings = buildStageTimings({
    indexing_started_at: "2026-08-28T10:00:46Z",
    completed_at: "2026-08-28T10:01:52Z",
  });
  assert.deepEqual(
    timings.map((t) => t.stage),
    ["indexing"]
  );
});

test("buildStageTimings falls back to completed_at when the next stamp is missing", () => {
  const timings = buildStageTimings({
    parsing_started_at: "2026-08-28T10:00:00Z",
    completed_at: "2026-08-28T10:00:30Z",
  });
  assert.equal(timings.length, 1);
  assert.equal(timings[0].stage, "parsing");
  assert.equal(timings[0].durationMs, 30_000);
  assert.equal(timings[0].running, false);
});

test("buildStageTimings clamps clock-skew negatives to zero", () => {
  const timings = buildStageTimings({
    parsing_started_at: "2026-08-28T10:00:30Z",
    splitting_started_at: "2026-08-28T10:00:00Z",
  });
  assert.equal(timings[0].durationMs, 0);
});

test("formatStageDuration renders compact durations", () => {
  assert.equal(formatStageDuration(200), "<1s");
  assert.equal(formatStageDuration(1_000), "1s");
  assert.equal(formatStageDuration(42_000), "42s");
  assert.equal(formatStageDuration(120_000), "2m");
  assert.equal(formatStageDuration(185_000), "3m 5s");
  assert.equal(formatStageDuration(3_600_000), "1h 00m");
  assert.equal(formatStageDuration(3_725_000), "1h 02m");
  assert.equal(formatStageDuration(Number.NaN), "-");
  assert.equal(formatStageDuration(-5), "-");
});

test("runningStageDurationMs measures from the start timestamp", () => {
  const timing = {
    stage: "indexing" as const,
    startedAt: "2026-08-28T10:00:46Z",
    durationMs: null,
    running: true,
  };
  const now = Date.parse("2026-08-28T10:01:42Z");
  assert.equal(runningStageDurationMs(timing, now), 56_000);
  // A clock before the start timestamp must not go negative.
  assert.equal(runningStageDurationMs(timing, Date.parse("2026-08-28T09:00:00Z")), 0);
});
