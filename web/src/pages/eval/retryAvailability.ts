import type { EvalExperimentCaseResult } from "@/api/eval";

type RetryCase = Pick<EvalExperimentCaseResult, "status" | "execution_status" | "retry_available">;

/** Quality and model execution remain separate when judging does not complete. */
export function isFailedEvalCase(item: RetryCase): boolean {
  return item.status === "failed" || item.execution_status === "failed"
    || (item.status === "unscored" && item.execution_status === "succeeded");
}

export function canSelectEvalCaseForRetry(item: RetryCase): boolean {
  if (["queued", "running", "cancelled", "skipped"].includes(item.execution_status || "")) return false;
  if (typeof item.retry_available === "boolean") {
    return item.retry_available && isFailedEvalCase(item);
  }
  // Older servers lack the safety hint. Do not newly expose unscored judge retries.
  return item.status === "failed" || item.execution_status === "failed";
}
