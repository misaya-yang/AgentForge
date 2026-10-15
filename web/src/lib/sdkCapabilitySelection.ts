/** UI admission only; current capability authority is always checked by Gateway. */
export interface SdkCapabilityOption {
  id: string;
  name: string;
  title?: string;
  summary?: string;
  description: string;
  category: string;
  risk: string;
  capabilityKind?: string;
  effect?: string;
  approvalPolicy?: string;
  readOnly?: boolean;
}

export const MAX_SDK_CAPABILITIES = 16;
const excludedNames = new Set([
  "read_attachment", "context_compact", "update_user_memory", "tool_search",
  "tool_describe", "tool_call", "spawn_subagent", "search_knowledge_base",
]);

export function visibleSdkCapabilities(rows: readonly SdkCapabilityOption[]): SdkCapabilityOption[] {
  const unique = new Map<string, SdkCapabilityOption>();
  for (const row of rows) {
    if (typeof row.id !== "string" || row.id !== row.name ||
        !/^[a-zA-Z0-9][a-zA-Z0-9._:-]{0,127}$/.test(row.id) || excludedNames.has(row.id) ||
        ["mcp", "skill", "platform_tool_discovery"].includes(row.capabilityKind || "") ||
        !((row.effect === "read" && row.readOnly === true && row.approvalPolicy === "never") ||
          (["write", "unknown"].includes(row.effect || "") && row.readOnly === false &&
            ["on_request", "always"].includes(row.approvalPolicy || "")))) continue;
    unique.set(row.id, row);
  }
  return [...unique.values()];
}

export function validateSdkCapabilitySelection(
  selected: readonly string[], visible: readonly SdkCapabilityOption[],
): string[] {
  const permitted = new Set(visibleSdkCapabilities(visible).map((row) => row.id));
  if (selected.length > MAX_SDK_CAPABILITIES || new Set(selected).size !== selected.length ||
      selected.some((id) => !permitted.has(id))) throw new Error("invalid_sdk_capability_selection");
  return [...selected];
}
