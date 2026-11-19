/** Diagnostic export is an allowlist, never a redaction of the stored task. */
export type TaskDiagnosticState = "pending" | "processing" | "completed" | "failed" | "cancelled" | "unknown";
export type ResultObservation = "not_requested" | "loading" | "available" | "unavailable" | "unknown";
export interface DiagnosticReceipt { task_id: string; status: string }
const states: readonly string[] = ["pending", "processing", "completed", "failed", "cancelled"];

export function taskDiagnosticState(status: string): TaskDiagnosticState {
  return states.includes(status) ? status as TaskDiagnosticState : "unknown";
}

export function taskDiagnosticSummary(receipt: DiagnosticReceipt, result: ResultObservation, observedAt: string) {
  // Only the canonical UUID task identity is exportable. Never copy a search
  // string, arbitrary server text, a service name, or any result/error payload.
  const taskId = /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/i.test(receipt.task_id)
    ? receipt.task_id : null;
  const state = taskDiagnosticState(receipt.status);
  return JSON.stringify({
    schema: "task-diagnostic/v1",
    task_id: taskId,
    task_id_omitted: taskId === null,
    receipt_status: state,
    result_read: state === "completed" ? result : "not_requested",
    snapshot_created_at: observedAt,
    scope: "client_observation_only",
    side_effects: "not_verified",
  }, null, 2);
}
