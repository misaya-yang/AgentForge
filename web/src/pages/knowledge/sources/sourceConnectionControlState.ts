import type { SourceCredentialIntent, SourceConnectionChange, SourceConnectionControl, SourceConnectionImpact } from "@/api/knowledge";

export const controlKey = (userId: string, datasetId: string) => `kb-source-control:${JSON.stringify([userId, datasetId])}`;
const state = (value: unknown) => value === "active" || value === "disabled";
export function readControlChange(key: string): SourceConnectionChange | SourceCredentialIntent | null {
  const raw = sessionStorage.getItem(key);
  if (!raw) return null;
  const value = JSON.parse(raw);
  if (!value || typeof value.connection_id !== "string" || !value.connection_id ||
      typeof value.expected_revision !== "string" || !value.expected_revision ||
      !state(value.expected_status) || (value.operation !== "credentials" && (!state(value.status) || value.expected_status === value.status))) throw Error("Invalid recovery state");
  if (value.operation === "credentials") return { operation: "credentials", connection_id: value.connection_id, expected_revision: value.expected_revision, expected_status: value.expected_status };
  return { connection_id: value.connection_id, expected_revision: value.expected_revision, expected_status: value.expected_status, status: value.status };
}
export function validateControl(value: unknown, id?: string): SourceConnectionControl {
  const row = value as SourceConnectionControl;
  if (!row || typeof row.connection_id !== "string" || !row.connection_id || (id && row.connection_id !== id) ||
      [row.name, row.domain, row.status, row.sync_mode, row.revision].some(v => typeof v !== "string" || !v)) throw Error("Invalid control receipt");
  return row;
}
export function validateImpact(value: unknown, id: string): SourceConnectionImpact {
  const impact = value as SourceConnectionImpact;
  validateControl(impact?.connection, id);
  if (!Number.isSafeInteger(impact.binding_count) || impact.binding_count < 0 ||
      !Number.isSafeInteger(impact.hidden_count) || impact.hidden_count < 0 || !Array.isArray(impact.bindings) ||
      !(impact.next_offset === null || (Number.isSafeInteger(impact.next_offset) && impact.next_offset > 0)) ||
      impact.bindings.some(b => !b || typeof b.binding_id !== "string" || typeof b.dataset_id !== "string" ||
        !(b.space_name === null || typeof b.space_name === "string") || !(b.status === null || typeof b.status === "string"))) throw Error("Invalid impact receipt");
  return impact;
}
