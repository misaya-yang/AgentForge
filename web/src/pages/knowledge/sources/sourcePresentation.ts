import type { DatasetSources } from "@/api/knowledge";

export type ConnectorPresentationState =
  | "loading" | "load_failed" | "unknown" | "not_configured" | "available" | "unavailable";

export function connectorPresentationState(
  sources: DatasetSources | null,
  loading: boolean,
  failed: boolean
): ConnectorPresentationState {
  if (failed) return "load_failed";
  if (loading && !sources) return "loading";
  if (!sources) return "unknown";
  switch (sources.connector_status) {
    case "not_configured":
    case "available":
    case "unavailable":
      return sources.connector_status;
    default:
      return "unknown";
  }
}

export function bindingPresentationState(
  binding: DatasetSources["confluence_bindings"][number]
): "problem" | "last_success" | "never_succeeded" {
  if (binding.has_problem || ["error", "failed", "unavailable"].includes((binding.status || "").toLowerCase())) {
    return "problem";
  }
  return binding.last_success_at ? "last_success" : "never_succeeded";
}

export function canShowSuccessfulSyncCount(
  binding: DatasetSources["confluence_bindings"][number]
): boolean {
  return bindingPresentationState(binding) !== "problem" && !!binding.last_success_at
    && Number.isSafeInteger(binding.page_count) && binding.page_count >= 0;
}
