export const ACTIVE_RUN_METADATA_KEY = "assistant_active_run";

export function shouldBlockDuringRunRestore(
  metadata: Record<string, unknown> | null | undefined,
): boolean {
  const marker = metadata?.[ACTIVE_RUN_METADATA_KEY];
  if (!marker || typeof marker !== "object" || Array.isArray(marker)) return false;
  const runId = (marker as Record<string, unknown>).run_id;
  return typeof runId === "string" && runId.trim().length > 0;
}
