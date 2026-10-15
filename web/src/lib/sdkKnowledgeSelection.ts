/** UI admission only: the Gateway rechecks the caller's access on each run. */
export interface SdkKnowledgeOption {
  dataset_id: string;
  name: string;
  my_permission?: string;
}

export const MAX_SDK_KNOWLEDGE_DATASETS = 16;

export function visibleSdkKnowledgeDatasets(rows: readonly SdkKnowledgeOption[]): SdkKnowledgeOption[] {
  const unique = new Map<string, SdkKnowledgeOption>();
  for (const row of rows) {
    if (typeof row.dataset_id === "string" && /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/i.test(row.dataset_id) &&
        ["viewer", "editor", "owner"].includes(row.my_permission || "")) {
      unique.set(row.dataset_id.toLowerCase(), {
        dataset_id: row.dataset_id.toLowerCase(),
        name: typeof row.name === "string" && row.name.trim() ? row.name : row.dataset_id,
        my_permission: row.my_permission,
      });
    }
  }
  return [...unique.values()];
}

export function validateSdkKnowledgeSelection(
  selected: readonly string[],
  visible: readonly SdkKnowledgeOption[],
): string[] {
  const permitted = new Set(visibleSdkKnowledgeDatasets(visible).map((row) => row.dataset_id));
  if (selected.length > MAX_SDK_KNOWLEDGE_DATASETS || new Set(selected).size !== selected.length ||
      selected.some((id) => !permitted.has(id))) {
    throw new Error("invalid_sdk_knowledge_selection");
  }
  return [...selected];
}
