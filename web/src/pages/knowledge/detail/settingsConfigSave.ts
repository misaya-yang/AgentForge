import type { DatasetConfig } from "../../../types/knowledge.ts";

export type SettingsSaveSection = "chunking" | "retrieval" | "embedding";
export type SettingsSavePhase = "staged" | "mutation_accepted" | "readback_uncertain" | "conflict" | "observed_match" | "reconciled" | "rejected";
export interface SettingsSaveAttempt {
  section: SettingsSaveSection | null;
  mutationAccepted: boolean;
  phase: SettingsSavePhase;
  submitted: Record<string, unknown> | null;
  editorSnapshot: string | null;
}

function record(value: unknown): value is Record<string, unknown> {
  return value !== null && typeof value === "object" && !Array.isArray(value);
}

/** Stable identity for editor values, independent of object insertion order. */
export function settingsSnapshot(value: unknown): string {
  if (Array.isArray(value)) return `[${value.map(settingsSnapshot).join(",")}]`;
  if (record(value)) return `{${Object.keys(value).sort().map(key => `${JSON.stringify(key)}:${settingsSnapshot(value[key])}`).join(",")}}`;
  return JSON.stringify(value) ?? "undefined";
}

export function stageSettingsSave(
  section: SettingsSaveSection,
  submitted: Record<string, unknown>,
  editor: unknown,
): SettingsSaveAttempt {
  return { section, mutationAccepted: false, phase: "staged", submitted: JSON.parse(JSON.stringify(submitted)), editorSnapshot: settingsSnapshot(editor) };
}

/** Never restore an interrupted write as permission to replay it. */
export function restoreSettingsSave(value: unknown): SettingsSaveAttempt | null {
  if (value === undefined || value === null) return null;
  if (record(value) && ["chunking", "retrieval", "embedding"].includes(String(value.section))
      && record(value.submitted) && Object.keys(value.submitted).length > 0
      && typeof value.editorSnapshot === "string"
      && ["staged", "mutation_accepted", "readback_uncertain", "conflict", "observed_match"].includes(String(value.phase))) {
    return { section: value.section as SettingsSaveSection, mutationAccepted: value.mutationAccepted === true || value.phase === "mutation_accepted", phase: "readback_uncertain",
      submitted: value.submitted, editorSnapshot: value.editorSnapshot };
  }
  // A malformed recovery marker must not silently unlock another write.
  return { section: null, mutationAccepted: false, phase: "readback_uncertain", submitted: null, editorSnapshot: null };
}

function containsSubmitted(actual: unknown, expected: unknown): boolean {
  if (record(expected)) return record(actual) && Object.keys(expected).every(key =>
    Object.prototype.hasOwnProperty.call(actual, key) && containsSubmitted(actual[key], expected[key]));
  return settingsSnapshot(actual) === settingsSnapshot(expected);
}

/** Reads prove only the submitted section, never unrelated current values. */
export function reconcileSettingsSave(attempt: SettingsSaveAttempt, config: DatasetConfig, datasetId: string): SettingsSaveAttempt {
  if (config.dataset_id !== datasetId) throw new Error("Dataset config identity mismatch");
  const match = attempt.section !== null && attempt.submitted !== null
    && containsSubmitted(config[attempt.section], attempt.submitted);
  return { ...attempt, phase: match ? (attempt.mutationAccepted ? "reconciled" : "observed_match") : "conflict" };
}

export function settingsEditorUnchanged(attempt: SettingsSaveAttempt, editor: unknown): boolean {
  return attempt.editorSnapshot !== null && settingsSnapshot(editor) === attempt.editorSnapshot;
}
